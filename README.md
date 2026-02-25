# steamdepot-rs

A Rust library and CLI application for downloading from Steam content servers.

## Goals

- **Container-friendly** -- minimal runtime dependencies and configurable logging to better support containerized environments.
- **Library-first design** -- `steamdepot` exposes a reusable library crate; `steamdepot-cli` provides a ready-made command-line interface on top of it.

## Project Structure

| Crate | Description |
|---|---|
| `steamdepot` | Core library for authenticating with Steam and downloading depot content. |
| `steamdepot-cli` | CLI frontend for `steamdepot`. |
| `steam-mail` | Minimal SMTP server for extracting Steam Guard codes from incoming emails. |

## Usage

```bash
# Fetch product info for an app
steamdepot-cli --app-id 440

# Download an app's depots (resolve depots, fetch keys, download manifests and chunks)
steamdepot-cli --download 232250 --fetch-manifests --install-dir /tmp/steamtest

# Filter by OS and branch
steamdepot-cli --download 232250 --os linux --branch public --fetch-manifests --install-dir /tmp/steamtest

# Authenticated login with a refresh token
steamdepot-cli --username myaccount --token 'eyAidH...' --download 3557020 --fetch-manifests --install-dir /tmp/steamtest

# JSON log output (one JSON object per line)
steamdepot-cli --logmode json --download 232250 --fetch-manifests --install-dir /tmp/steamtest
```

### JSON log types

With `--logmode json`, every line is a JSON object with a `"type"` field:

| Type | Description |
|---|---|
| `cm_servers` | CM server list fetched |
| `connecting` | Connecting to a CM server |
| `login` | Logged in (steam_id, session_id, cell_id) |
| `plan_start` | Starting download plan resolution |
| `plan` | Resolved depots with keys |
| `cdn_servers` | CDN server pool size |
| `manifest` | Manifest metadata (files, sizes, chunks) |
| `prepare` | Creating directory tree |
| `prepared` | Directory tree created (dirs, files, symlinks) |
| `progress` | Chunk download progress (bytes_downloaded, bytes_total, pct) |
| `complete` | Depot download finished |
| `error` | An error occurred |
| `disconnected` | Session closed |

## steam-mail

`steam-mail` is a standalone crate that runs a minimal SMTP server to capture Steam Guard email codes. Point your email domain's MX record at the server, and it will extract the 5-character code from incoming Steam Guard emails.

```rust
use steam_mail::SteamMailServer;

let mut server = SteamMailServer::new("0.0.0.0:2525").await?;
println!("SMTP listening on {}", server.local_addr());

// Blocks until a Steam Guard email arrives
let code = server.recv_code().await.unwrap();
println!("Got Steam Guard code: {}", code);
```

This is useful for automating the auth flow: call `steamdepot::login::begin_auth_session()`, wait for the guard code via `steam-mail`, then submit it with `submit_guard_code()`.

## Acknowledgements

This project is based on the work performed by the [SteamRE](https://github.com/SteamRE) team and their [Depot Downloader](https://github.com/SteamRE/DepotDownloader) and [SteamKit2](https://github.com/SteamRE/SteamKit) projects.

## License

LGPL-2.1
