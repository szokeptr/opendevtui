# Releasing

Releases are driven from GitHub Actions with the `Release` workflow.

## One-time setup

1. Create a public tap repository named `homebrew-tap` under the release owner, for example `szokeptr/homebrew-tap`.
2. Add a repository secret named `HOMEBREW_TAP_TOKEN` to this repository. It must be able to push to the tap repository.
3. If the default branch is protected in a way that blocks `GITHUB_TOKEN` pushes, add `RELEASE_TOKEN` with permission to push commits and tags to this repository.

## Release flow

1. Open GitHub Actions.
2. Run the `Release` workflow.
3. Enter a SemVer version without a leading `v`, for example `0.2.0`.
4. Leave `publish_homebrew` enabled to update the tap formula.

The workflow:

- updates `Cargo.toml` and `Cargo.lock`
- commits `Release vX.Y.Z` when the crate version changed
- creates an annotated `vX.Y.Z` tag
- builds Linux x86_64, macOS Intel, and macOS Apple Silicon archives
- creates or updates the GitHub release with checksums
- updates `Formula/opendevtui.rb` in the configured Homebrew tap

After the workflow finishes, users can install with:

```sh
brew install szokeptr/tap/opendevtui
```

The Homebrew formula builds from the tagged source release with `cargo install --locked`, so the committed `Cargo.lock` is part of the release contract.
