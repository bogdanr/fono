// SPDX-License-Identifier: GPL-3.0-only
//! Networking primitives for Fono — server side plus discovery helpers.
//!
//! The Wyoming-protocol *server* lives here; the corresponding *client*
//! lives in `fono-stt::wyoming`. This crate also carries the mDNS
//! browser/advertiser and the Fono-native protocol.

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
    AssistantProvider, LlmServer, LlmServerConfig, LlmServerHandle, ModelFacts, TranscribeProvider,
    TranscribeRequest, UpstreamProvider,
};

#[cfg(feature = "web-settings")]
pub mod web_settings;

#[cfg(feature = "web-settings")]
pub use web_settings::{WebSettingsConfig, WebSettingsHandle, WebSettingsHooks, WebSettingsServer};
