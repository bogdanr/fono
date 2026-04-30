// SPDX-License-Identifier: GPL-3.0-only
//! Wire codec for Fono's networking protocols.
//!
//! Two protocols share a single framing layer:
//!
//! - **Wyoming** ([upstream spec](https://github.com/OHF-Voice/wyoming)) —
//!   raw TCP, JSONL header + optional UTF-8 data block + optional binary
//!   payload. Used for STT/TTS/wake interop with the Rhasspy + Home
//!   Assistant ecosystem.
//! - **Fono-native** — same `Frame` body, but carried over WebSocket
//!   binary messages so a browser tab can be a first-class client.
//!   Covers LLM cleanup, history mirror, and app-context routing —
//!   the parts Wyoming has no event types for.
//!
//! This crate is transport-agnostic: it knows how to serialise and
//! parse a [`Frame`] over any [`AsyncRead`]/[`AsyncWrite`] pair. The
//! WebSocket and TCP I/O glue lives in the `fono-net` crate (Slice 2+).
//!
//! See `plans/2026-04-29-2026-04-29-client-server-wyoming-fono-and-mdns-v2.md`.

pub mod arm;
pub mod fono;
pub mod frame;
pub mod wyoming;

pub use arm::Arm;
pub use frame::{Frame, FrameError};
