# Contributing

Thanks for taking a look. This document covers setup, the test flow, and the
conventions the CI pipeline enforces.

## Setup

- Rust **1.89 or newer** (edition 2024). `rust-toolchain.toml` pins `stable`.
- Git submodules. The protobuf definitions live in `steamdepot/proto`:

  ```bash
  git clone --recurse-submodules https://github.com/EliCunninghamDev/steamdepot.git
  # or, in an existing checkout:
  git submodule update --init --recursive
  ```

- No system `protoc`: the build uses `protoc-bin-vendored`.
- No Steam account is needed for the standard test suite.

## Checks

CI runs these, and `scripts/ci.sh` runs the same set locally:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked
```

Please run `./scripts/ci.sh` before opening a pull request.

## Test layout

Tests are split by what they need:

- **Unit tests** live next to the code. Use them for parsers, crypto,
  framing, checksums, and other pure logic.
- **Mock CM tests** (`steamdepot/tests/mock_cm.rs`) drive the real connection,
  login, PICS, and service-method code against a scripted WebSocket server in
  `steamdepot/tests/common/mod.rs`.
- **Mock CDN tests** (`steamdepot/tests/cdn_download.rs`) exercise manifest
  parsing, filename decryption, directory preparation, chunk decryption, and
  checksum verification against a tiny local HTTP server.
- **CLI tests** (`steamdepot-cli/tests/cli.rs`) run the binary and assert
  fail-fast argument validation. They never touch the network.
- **Live tests** (`steamdepot/tests/live_steam.rs`) are `#[ignore]`d and talk to
  real Steam infrastructure. Run them with `scripts/test-live.sh` and the
  variables from `.env.example`.

When adding a feature that talks to Steam, prefer extending the mock CM/CDN
fixtures over adding a live test. Live tests should stay small and tolerant of
account-specific state.

## Code style

- `cargo fmt` is authoritative; do not hand-format around it.
- Clippy runs with `-D warnings`. If a lint is wrong for a specific call site,
  add a narrowly scoped `#[allow(...)]` with a short comment explaining why.
- Public API changes need doc comments. `cargo doc` treats broken intra-doc
  links as errors.
- Avoid adding dependencies when the standard library or an existing dependency
  covers the need. New dependencies should be added to `[workspace.dependencies]`
  and inherited by the crates that use them.

## Commits and pull requests

- Keep commits focused and describe the behavior change, not the files touched.
- Include tests with bug fixes. If a fix is only observable against real Steam,
  say so in the PR description.
- Update `CHANGELOG.md` under `Unreleased` for user-visible changes.
- CI must be green before merge.
