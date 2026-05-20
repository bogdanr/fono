// SPDX-License-Identifier: GPL-3.0-only
//! No-op overlay backend.
//!
//! Acks every command, draws nothing, exits the worker thread on
//! `Shutdown` (or sender drop). Acts as the terminal fallback in the
//! candidate walk so [`crate::backend::spawn_overlay`] always
//! succeeds — even on a headless host with no `DISPLAY` and no
//! `WAYLAND_DISPLAY`.

use std::sync::mpsc::channel;

use fono_core::config::WaveformStyle;

use crate::backend::{BackendCapabilities, BackendId, OverlayCmd, SpawnedBackend};

pub fn spawn(_style: WaveformStyle) -> SpawnedBackend {
    let (tx, rx) = channel::<OverlayCmd>();
    let join = std::thread::Builder::new()
        .name("fono-overlay-noop".into())
        .spawn(move || {
            // Drain commands at trace level so logs reflect what
            // the orchestrator pushed even when there's no surface.
            while let Ok(cmd) = rx.recv() {
                match cmd {
                    OverlayCmd::Shutdown => break,
                    other => tracing::trace!("overlay(noop): {other:?}"),
                }
            }
        })
        .expect("spawn fono-overlay-noop thread");
    SpawnedBackend {
        id: BackendId::Noop,
        capabilities: BackendCapabilities {
            transparency: false,
            client_positioning: false,
            focus_passthrough: true,
            click_passthrough: true,
        },
        tx,
        waker: Box::new(|| {}),
        join,
    }
}
