# OpenDevTUI

OpenDevTUI is a Unix terminal UI for managing project-local development services from one screen. It starts, stops, restarts, edits, and tails logs for configured commands such as `npm run dev`, `docker compose up`, or shell scripts.

It is meant for local development workspaces and SSH sessions on trusted machines.

## Requirements

- Rust toolchain, when building from source
- Unix-like terminal environment
- Commands you configure for services, such as `npm`, `docker`, or `bash`

## Install

Install from GitHub:

```sh
cargo install --git https://github.com/szokeptr/opendevtui.git
```

Or clone and run locally:

```sh
git clone https://github.com/szokeptr/opendevtui.git
cd opendevtui
cargo run
```

## Usage

Run `opendevtui` from the root of the workspace whose services you want to manage.

```sh
opendevtui
opendevtui --help
opendevtui --version
```

OpenDevTUI reads and writes workspace configuration at:

```text
.opendevtui/config
```

The config is local to that workspace. It is ignored by this repository by default because service definitions can contain machine-specific paths, commands, or environment values.

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

Environment variables can be added to a service:

```toml
[[services]]
id = "api"
name = "API"
cwd = "."
command = "cargo"
args = ["run"]
env = { RUST_LOG = "debug" }
autostart = false
```

## Development

```sh
cargo fmt
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

Releases are automated through GitHub Actions. See [docs/releasing.md](docs/releasing.md).

## License

MIT
