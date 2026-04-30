# OpenDevTUI

OpenDevTUI is a Unix terminal UI for managing project-local development services from one screen. It can start, stop, restart, edit, and tail logs for configured commands such as `npm run dev`, `docker compose up`, or shell scripts.

## Requirements

- Rust toolchain
- Unix-like terminal environment
- Commands you configure for services, such as `npm`, `docker`, or `bash`

## Run Locally

```sh
cargo run
```

The app reads and writes its workspace configuration at:

```text
.opendevtui/config
```

## Keyboard Shortcuts

- `a`: add a service
- `e`: edit selected service
- `v`: edit raw TOML config
- `d`: delete selected service
- `s`: start selected service
- `x`: stop selected service
- `r`: restart selected service
- `j` / `k`: move selection
- `Tab`: switch between services and logs
- `w`: toggle log wrapping
- `Shift+C`: clear selected service logs
- `q` or `Ctrl+C`: quit

## Example Config

```toml
version = 1

[[services]]
id = "web"
name = "Web app"
cwd = "."
command = "npm"
args = ["run", "dev"]
autostart = true

[[services]]
id = "worker"
name = "Background worker"
cwd = "."
command = "bash"
args = ["scripts/worker.sh"]
autostart = false
```

## Server Deployment

OpenDevTUI is a terminal application, not an HTTP web server. The normal server deployment is to install it on a Linux VPS and run it over SSH.

### Option 1: Install from GitHub on a VPS

After the repository is pushed to GitHub:

```sh
ssh user@your-server
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
cargo install --git git@github.com:szokeptr/opendevtui.git
cd /path/to/your/workspace
opendevtui
```

### Option 2: Build a Release Binary

```sh
cargo build --release
scp target/release/opendevtui user@your-server:/usr/local/bin/opendevtui
ssh user@your-server
cd /path/to/your/workspace
opendevtui
```

### Option 3: Browser Access

If you need access from a browser, put the TUI behind a terminal gateway such as `ttyd` or `wetty` and protect it with SSH/VPN or strong authentication. Do not expose a process-control TUI directly to the public internet.

## Development

```sh
cargo fmt
cargo test
```
