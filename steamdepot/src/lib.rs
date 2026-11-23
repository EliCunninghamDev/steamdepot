//! # steamdepot
//!
//! A library for downloading content from Steam's content network.
//!
//! The crate is organized around the same stages SteamKit2 and DepotDownloader
//! use:
//!
//! 1. [`steam_api::cm_list`] — fetch CM server endpoints over HTTPS.
//! 2. [`connection::CmConnection`] — connect to a CM over WebSocket and frame
//!    protobuf messages.
//! 3. [`login`] / [`auth`] — log in anonymously or with a refresh token, with
//!    Steam Guard support for password/passwordless flows.
//! 4. [`pics`] — fetch product info and depot decryption keys.
//! 5. [`depot`] — resolve an app's depots and manifest IDs.
//! 6. [`cdn`] — fetch CDN servers, manifests, and auth tokens.
//! 7. [`download`] — prepare the install tree and download/verify chunks.
//!
//! ## Protocol framing
//!
//! [`connection::CmConnection::connect`] accepts either a bare endpoint
//! (`cms1.example.com`) or a complete WebSocket URL. The latter is what the
//! test suite uses to point at a local mock CM.
//!
//! ## Testing
//!
//! Unit tests live next to the code they cover. Integration tests under
//! `tests/` run fully offline against a mock CM and a mock CDN. Tests that need
//! real Steam infrastructure are `#[ignore]`d and run via
//! `scripts/test-live.sh`.
#![forbid(unsafe_code)]

pub mod auth;
pub mod cdn;
pub mod connection;
pub mod crypto;
pub mod depot;
pub mod download;
pub mod emsg;
pub mod error;
pub mod keyvalues;
pub mod login;
pub mod pics;
pub mod proto;
pub mod session;
pub mod steam_api;
