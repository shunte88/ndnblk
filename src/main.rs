/*
 *  main.rs
 *
 *  ndnblk - download all files from a Nitroflare folder via aria2c
 *      (c) 2025-26 Stuart Hunter (shunte88)
 *
 *      This program is free software: you can redistribute it and/or modify
 *      it under the terms of the GNU General Public License as published by
 *      the Free Software Foundation, either version 3 of the License, or
 *      (at your option) any later version.
 */

use clap::{ArgAction, Parser};
use log::{debug, error, info, warn, LevelFilter};
use log4rs::append::console::ConsoleAppender;
use log4rs::append::file::FileAppender;
use log4rs::config::{Appender, Config, Root};
use log4rs::encode::pattern::PatternEncoder;
use rand_agents::user_agent;
use reqwest::{Client, ClientBuilder, Proxy};
use serde_json::Value;
use std::{
    env,
    error::Error,
    fs,
    time::Duration,
};
use futures::{stream, StreamExt};
use tokio::{process::Command, time::sleep};
use chrono::Local;

type BoxResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

const DEFAULT_DIR: &str = "/home/stuart/Downloads";
const MAX_CONCURRENCY: usize = 4;
const LOG_PATTERN: &str = "{d(%Y-%m-%d %H:%M:%S)(utc)} {h({l})} {m}{n}";
const NTFURL_API: &str = "https://nitroflare.com/api/v2";

// CLI

#[derive(Parser, Debug)]
#[command(name = "ndnblk", version, about = "NF folder bulk downloader", author = "Stuart Hunter (shunte88)")]
struct Args {
    /// Nitroflare folder URL
    #[arg(value_name = "FOLDER_URL")]
    folder_url: String,

    /// Route all traffic through Tor (SOCKS5h on localhost:9050)
    #[arg(long)]
    tor: bool,

    /// Output directory
    #[arg(short, long, default_value = DEFAULT_DIR)]
    dir: String,

    /// Verbosity (-v = debug, -vv = trace)
    #[arg(short, long, action = ArgAction::Count)]
    verbose: u8,
}

// Structs

#[derive(Clone, Debug)]
struct DownloadItem {
    file_id: String,
    filename: String,
    nf_down: String, // resolved download URL
    dir: String,
}

#[derive(Clone, Debug)]
struct TorManager {
    active: bool,
    proxy_str: String,
}

impl TorManager {
    fn start(use_tor: bool) -> Self {
        let proxy_str = if use_tor {
            "socks5h://localhost:9050".to_string()
        } else {
            String::new()
        };
        Self { active: use_tor, proxy_str }
    }

    fn add_proxy(&self, builder: ClientBuilder) -> ClientBuilder {
        if self.active {
            builder.proxy(Proxy::all(&self.proxy_str).unwrap())
        } else {
            builder
        }
    }
}

#[derive(Clone, Debug)]
struct NFDown {
    client: Client,
    ux: String,
    px: String,
}

impl NFDown {
    fn init(ux: &str, px: &str, tunnel: &TorManager) -> BoxResult<Self> {
        let client = tunnel
            .add_proxy(
                Client::builder()
                    .user_agent(user_agent())
                    .timeout(Duration::from_secs(20)),
            )
            .build()?;
        Ok(Self { client, ux: ux.to_string(), px: px.to_string() })
    }

    async fn get_download_url(&self, file_id: &str) -> BoxResult<String> {
        let url = format!("{NTFURL_API}/getDownloadLink");
        let params = [
            ("user", self.ux.as_str()),
            ("premiumKey", self.px.as_str()),
            ("file", file_id),
        ];

        for attempt in 1..=3u32 {
            let res = self.client.get(&url).query(&params).send().await;
            match res {
                Ok(resp) if resp.status().is_success() => {
                    let j: Value = resp.json().await?;
                    if let Some(dl_url) = j
                        .get("result")
                        .and_then(|r| r.get("url"))
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                    {
                        return Ok(dl_url.to_string());
                    }
                    let code = j.get("code").and_then(|v| v.as_i64()).unwrap_or(0);
                    if code == 12 {
                        warn!("Rate-limited (error 12) for {file_id}");
                    } else {
                        warn!("Unexpected API response for {file_id}: {j:?}");
                    }
                    return Ok(String::new());
                }
                Ok(resp) => warn!("HTTP {} attempt {attempt} for {file_id}", resp.status()),
                Err(e) => warn!("Request error attempt {attempt} for {file_id}: {e}"),
            }
            sleep(Duration::from_millis(500 * attempt as u64)).await;
        }
        Ok(String::new())
    }
}

// Folder scraping

/// Parse userId and folder from https://nitroflare.com/folder/{userId}/{folder}
fn parse_folder_url(folder_url: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = folder_url.trim_end_matches('/').rsplitn(3, '/').collect();
    if parts.len() >= 2 {
        Some((parts[1].to_string(), parts[0].to_string()))
    } else {
        None
    }
}

/// POST to ajax/folder.php and return all (file_id, filename) pairs across all pages.
async fn collect_folder_links(client: &Client, folder_url: &str) -> BoxResult<Vec<(String, String)>> {
    let (user_id, folder) = parse_folder_url(folder_url)
        .ok_or_else(|| format!("Cannot parse folder URL: {folder_url}"))?;

    const PER_PAGE: usize = 100;
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut page: usize = 1;

    loop {
        let params = [
            ("userId", user_id.as_str()),
            ("folder",  folder.as_str()),
            ("page",    &page.to_string()),
            ("perPage", &PER_PAGE.to_string()),
        ];
        let j: Value = client
            .post("https://nitroflare.com/ajax/folder.php")
            .form(&params)
            .send().await?
            .json().await?;

        let total = j.get("total").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        if total == 0 {
            warn!("Folder empty or not found");
            break;
        }

        if page == 1 {
            let name = j.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            let total_pages = (total + PER_PAGE - 1) / PER_PAGE;
            info!("Folder \"{name}\": {total} file(s), {total_pages} page(s)");
        }

        if let Some(files) = j.get("files").and_then(|v| v.as_array()) {
            for file in files {
                let url  = file.get("url") .and_then(|v| v.as_str()).unwrap_or("");
                let name = file.get("name").and_then(|v| v.as_str()).unwrap_or("");
                // url is "view/FILE_ID/encoded_name" — extract the id segment
                if let Some(file_id) = url.split('/').nth(1) {
                    if !file_id.is_empty() && !name.is_empty() {
                        pairs.push((file_id.to_string(), name.to_string()));
                    }
                }
            }
        }

        let total_pages = (total + PER_PAGE - 1) / PER_PAGE;
        if page >= total_pages { break; }
        page += 1;
        sleep(Duration::from_millis(300)).await;
    }

    Ok(pairs)
}

// aria2c

async fn aria2c_download(item: &DownloadItem) -> BoxResult<bool> {
    fs::create_dir_all(&item.dir)?;
    let dest = std::path::Path::new(&item.dir).join(&item.filename);
    let a = vec![
        "--dir".to_string(),            item.dir.clone(),
        "--out".to_string(),            item.filename.clone(),
        "--max-connection-per-server=16".to_string(),
        "--max-concurrent-downloads=16".to_string(),
        "--split=20".to_string(),
        "--continue=true".to_string(),
        "--auto-file-renaming=true".to_string(),
        "--allow-overwrite=true".to_string(),
        "--summary-interval=0".to_string(),
        "--quiet".to_string(),
        item.nf_down.clone(),
    ];
    // Skip if already downloaded successfully
    if fs::metadata(&dest).map(|m| m.len() > 1024).unwrap_or(false) {
        info!("Skipping (exists): {}", item.filename);
        return Ok(true);
    }

    debug!("aria2c {}", a.join(" "));

    for attempt in 1..=3u32 {
        if attempt > 1 {
            let _ = fs::remove_file(&dest); // remove error-page stub before retry
            sleep(Duration::from_secs(3)).await;
            info!("Retrying ({attempt}/3): {}", item.filename);
        }

        let status = Command::new("aria2c").args(&a).status().await?;

        if !status.success() {
            warn!("aria2c exited {} attempt {attempt} for {}", status, item.filename);
            continue;
        }

        // aria2c exited 0 — check the file isn't a tiny error-page response
        match fs::metadata(&dest) {
            Ok(m) if m.len() < 1024 => {
                warn!("Attempt {attempt}: {} is {} bytes — looks like an error response, retrying",
                    item.filename, m.len());
            }
            Ok(_) => return Ok(true),
            Err(e) => warn!("Attempt {attempt}: cannot stat {}: {e}", item.filename),
        }
    }

    error!("All attempts failed for {}", item.filename);
    Ok(false)
}

// helpers

fn getenv(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}

// main

#[tokio::main]
async fn main() -> BoxResult<()> {

    let args = Args::parse();

    // Logging
    fs::create_dir_all("./logs")?;
    let stdout = ConsoleAppender::builder()
        .encoder(Box::new(PatternEncoder::new(LOG_PATTERN)))
        .build();
    let log_file = FileAppender::builder()
        .encoder(Box::new(PatternEncoder::new(LOG_PATTERN)))
        .build(format!("./logs/ndnblk.{}.log", Local::now().timestamp()))
        .unwrap();
    let loglev = match args.verbose {
        0 => LevelFilter::Info,
        1 => LevelFilter::Debug,
        _ => LevelFilter::Trace,
    };
    log4rs::init_config(
        Config::builder()
            .appender(Appender::builder().build("stdout", Box::new(stdout)))
            .appender(Appender::builder().build("file",   Box::new(log_file)))
            .build(Root::builder().appender("stdout").appender("file").build(loglev))
            .unwrap(),
    )
    .unwrap();

    // Credentials
    let ux = getenv("NTFLR_USERNAME", "");
    let px = getenv("NTFLR_PREMIUM", "");
    if ux.is_empty() {
        error!("NTFLR_USERNAME not set");
        return Ok(());
    }

    let tunnel = TorManager::start(args.tor);
    if tunnel.active {
        info!("Tor active (socks5h://localhost:9050)");
    }

    let scrape_client = tunnel
        .add_proxy(
            Client::builder()
                .user_agent(user_agent())
                .timeout(Duration::from_secs(30)),
        )
        .build()?;

    let nfd = NFDown::init(&ux, &px, &tunnel)?;

    // Step 1: collect (file_id, filename) pairs via ajax/folder.php
    info!("Collecting links from {}", args.folder_url);
    let pairs = collect_folder_links(&scrape_client, &args.folder_url).await?;
    if pairs.is_empty() {
        warn!("No files found — check the folder URL");
        return Ok(());
    }
    info!("Found {} file(s)", pairs.len());

    // Step 2: build download items
    let mut items: Vec<DownloadItem> = pairs
        .into_iter()
        .map(|(file_id, filename)| DownloadItem {
            file_id,
            filename,
            nf_down: String::new(),
            dir: args.dir.clone(),
        })
        .collect();

    // Step 3: resolve download URLs via NF API (sequential, gentle)
    info!("Resolving download URLs...");
    for item in &mut items {
        match nfd.get_download_url(&item.file_id).await {
            Ok(url) if !url.is_empty() => {
                debug!("{} -> ok", item.filename);
                item.nf_down = url;
            }
            Ok(_) => warn!("No URL returned for {} ({})", item.filename, item.file_id),
            Err(e) => warn!("API error for {}: {e}", item.file_id),
        }
        sleep(Duration::from_millis(300)).await;
    }

    let ready: Vec<&DownloadItem> = items.iter().filter(|i| !i.nf_down.is_empty()).collect();
    info!("{}/{} file(s) ready", ready.len(), items.len());
    if ready.is_empty() {
        warn!("Nothing to download");
        return Ok(());
    }

    // Step 4: aria2c downloads (bounded concurrency)
    let results = stream::iter(ready.iter().map(|item| async move {
        info!("Starting: {}", item.filename);
        match aria2c_download(item).await {
            Ok(true)  => { info!("Done: {}",   item.filename); true  }
            Ok(false) => { error!("Failed: {}", item.filename); false }
            Err(e)    => { error!("Error {}: {e}", item.filename); false }
        }
    }))
    .buffer_unordered(MAX_CONCURRENCY)
    .collect::<Vec<_>>()
    .await;

    let ok = results.iter().filter(|&&b| b).count();
    info!("Complete: {ok}/{} downloaded successfully", results.len());

    Ok(())
}
