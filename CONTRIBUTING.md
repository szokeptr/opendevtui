# Contributing

OpenDevTUI is maintained as a personal project. Small fixes, bug reports, and focused improvements are welcome.

## Development

Install a stable Rust toolchain, then run:

```sh
cargo fmt
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

The app is Unix-only for now because it uses Unix process groups to manage service shutdown.

## Pull Requests

- Keep changes focused on one behavior or cleanup at a time.
- Add or update tests when changing config parsing, process management, editor behavior, or key handling.
- Avoid committing local workspace configs from `.opendevtui/`.
- Note any terminal/platform assumptions in the PR description.
