// SPDX-License-Identifier: GPL-3.0-only
//! Networking primitives for Fono — server side plus discovery helpers.
//!
//! Slice 3 of `plans/2026-04-29-2026-04-29-client-server-wyoming-fono-and-mdns-v2.md`
//! ships the Wyoming-protocol *server* here. The corresponding *client*
//! lives in `fono-stt::wyoming` (see the v2.2 plan amendment). Slice 4
//! adds mDNS browser/advertiser; Slice 5/6 add the Fono-native protocol.

#[cfg(feature = "discovery")]
pub mod discovery;

#[cfg(any(feature = "llm-server", feature = "web-settings"))]
pub mod auth;

#[cfg(any(feature = "llm-server", feature = "web-settings"))]
pub use auth::{AuthVerifier, KeyId, UsageSink};

#[cfg(feature = "wyoming-server")]
pub mod wyoming;

#[cfg(feature = "wyoming-server")]
pub use wyoming::server::{WyomingServer, WyomingServerConfig, WyomingServerHandle};

#[cfg(feature = "llm-server")]
pub mod llm_server;

#[cfg(feature = "llm-server")]
pub use llm_server::{
    AssistantProvider, LlmServer, LlmServerConfig, LlmServerHandle, TranscribeProvider,
    TranscribeRequest, UpstreamProvider,
};

#[cfg(feature = "web-settings")]
pub mod web_settings;

#[cfg(feature = "web-settings")]
pub use web_settings::{WebSettingsConfig, WebSettingsHandle, WebSettingsHooks, WebSettingsServer};
