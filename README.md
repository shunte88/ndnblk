# ndnblk

100% Rust — bulk download all files from a Nitroflare folder.

## How it works

1. **Scrape** — POSTs to Nitroflare's `ajax/folder.php` endpoint with the folder's `userId` and `folder` token, paginating at 100 items per page to collect every `(file_id, filename)` pair
2. **Resolve** — calls the Nitroflare API v2 `getDownloadLink` endpoint (sequentially, with 300 ms spacing) to get a direct CDN download URL for each file
3. **Download** — runs `aria2c` for each file with up to 4 concurrent downloads, 16 connections per server, and 20 splits per file

## Features

- **Skip existing** — if a file already exists at the destination and is > 1 KB it is skipped; safe to re-run against the same folder to resume interrupted batches
- **Error-page detection** — Nitroflare occasionally returns a small error body (e.g. `ERROR: Wrong IP`) instead of the actual file; ndnblk detects files < 1 KB after a "successful" download, deletes the stub, and retries
- **Retry logic** — up to 3 attempts per file with a 3 s delay between retries
- **Tor support** — `--tor` routes all traffic (scraping, API calls, downloads) through a local Tor SOCKS5h proxy on `localhost:9050`
- **Structured logging** — dual output to console and a timestamped log file in `./logs/`
- **Random user-agent rotation** — via `rand_agents`

## Requirements

- [aria2c](https://aria2.github.io/) — must be on `PATH`
- A Nitroflare premium account
- (Optional) [Tor](https://www.torproject.org/) running locally on port 9050

## Configuration

Set the following environment variables (the wrapper script sources them from `~/.config/postinsta`):

| Variable | Description |
|---|---|
| `NTFLR_USERNAME` | Nitroflare account email |
| `NTFLR_PREMIUM` | Nitroflare account password |

## Usage

```
ndnblk <FOLDER_URL> [OPTIONS]

Arguments:
  <FOLDER_URL>  Nitroflare folder URL

Options:
  -d, --dir <DIR>   Output directory [default: ~/Downloads]
      --tor         Route all traffic through Tor (localhost:9050)
  -v, --verbose     Increase log verbosity (-v = debug, -vv = trace)
  -h, --help        Print help
  -V, --version     Print version
```

### Examples

```bash
# Download a folder to ~/Downloads
./ndnblk https://nitroflare.com/folder/10973/L02xNQV9USFBPLkQ=

# Download to a specific directory via Tor
./ndnblk https://nitroflare.com/folder/10973/L02xNQV9USFBPLkQ= --tor -d /mnt/media

# Re-run to pick up any failures (already-completed files are skipped)
./ndnblk https://nitroflare.com/folder/10973/L02xNQV9USFBPLkQ=
```

## Build

```bash
cargo build --release
```

The wrapper script `./ndnblk` runs the debug build and sources credentials automatically.

## License

MIT — (c) 2025-26 Stuart Hunter (shunte88)
