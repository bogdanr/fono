// SPDX-License-Identifier: GPL-3.0-only
//! Networking primitives for Fono — server side plus discovery helpers.
//!
//! Slice 3 of `plans/2026-04-29-2026-04-29-client-server-wyoming-fono-and-mdns-v2.md`
//! ships the Wyoming-protocol *server* here. The corresponding *client*
//! lives in `fono-stt::wyoming` (see the v2.2 plan amendment). Slice 4
//! adds mDNS browser/advertiser; Slice 5/6 add the Fono-native protocol.

#[cfg(feature = "discovery")]
pub mod discovery;

#[cfg(feature = "wyoming-server")]
pub mod wyoming;

#[cfg(feature = "wyoming-server")]
pub use wyoming::server::{WyomingServer, WyomingServerConfig, WyomingServerHandle};
