# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
semantic versioning.

## [Unreleased]

### Fixed

- Regular file permissions are now derived from the manifest on every download:
  the executable flag is applied in both directions (a stale `+x` from an older
  manifest is cleared) and the read-only flag is honored. Read-only files are
  kept writable until their chunks are written, and re-downloads make an
  existing read-only file writable before truncating it.

## [0.4.0] - 2026-11-23

### Added

- Offline test environment: a scripted mock Steam CM (WebSocket) and a local
  mock CDN (HTTP) with fixtures for manifests, encrypted filenames, and
  encrypted chunks.
- Integration tests covering anonymous login, PICS product info, depot keys,
  CDN server discovery, manifest parsing, directory preparation, chunk
  download/decrypt/verify, and CLI argument validation.
- `#[ignore]`d live tests plus `scripts/test-live.sh` and `.env.example`.
- CI workflow for formatting, clippy, tests (Linux/macOS/Windows), MSRV (1.85),
  and rustdoc; Dependabot configuration; `scripts/ci.sh`.
- `CONTRIBUTING.md`, `CHANGELOG.md`, `.editorconfig`, `rustfmt.toml`,
  `rust-toolchain.toml`, and a `.env.example`.

### Changed

- `CmConnection::connect` accepts a complete WebSocket URL in addition to a
  bare CM host, which is what enables the mock-CM tests.
- `CdnServer` gained a `scheme` field and a `url` helper so CDN requests can
  target local HTTP servers.
- Workspace crates share version, metadata, and dependencies
  (`[workspace.package]`, `[workspace.dependencies]`, `[workspace.lints]`).
- Builds use a vendored `protoc`; a system protobuf compiler is no longer
  required.
- CLI option combinations are validated before any network access.

### Fixed

- Manifest filenames are validated before use, preventing absolute paths or
  `..` components from writing outside the install directory.
- `login_with_token` now returns a session marked as authenticated; previously
  the returned copy still said `authenticated: false` even though the stored
  session was updated.
- `steam-mail` only decodes quoted-printable bodies when the message declares
  that transfer encoding, so literal `=` characters in URLs are no longer
  corrupted.
- Quoted-printable decoding now preserves non-ASCII UTF-8 bytes.
- The repository URL in crate metadata pointed at a misspelled repository.
- Documentation examples no longer produce broken intra-doc links.

### Removed

- Unused `ecb` dependency.
