// SPDX-License-Identifier: GPL-3.0-only
//! mDNS advertiser. Publishes a `_wyoming._tcp.local.` /
//! `_fono._tcp.local.` service for the running daemon so other Fono
//! instances (and Home Assistant satellites, Rhasspy, …) can discover
//! and connect without manual host:port entry.
//!
//! The advertiser is **opt-in** — it only runs when the matching
//! `[server.*].enabled = true` block is set. On graceful daemon
//! shutdown the [`AdvertiserHandle::shutdown`] path sends a goodbye
//! packet so peers evict immediately rather than waiting for TTL.

use std::collections::HashMap;
use std::net::IpAddr;

use anyhow::{Context, Result};
use mdns_sd::{ServiceDaemon, ServiceInfo};
use tracing::{debug, warn};

use super::txt::{format_caps, KEY_AUTH, KEY_CAPS, KEY_MODEL, KEY_PATH, KEY_PROTO, KEY_VERSION};
use super::PeerKind;

/// User-facing advertise spec. Translated into a `ServiceInfo` and
/// registered with the shared [`mdns_sd::ServiceDaemon`].
#[derive(Debug, Clone)]
pub struct AdvertiseSpec {
    pub kind: PeerKind,
    /// Friendly instance name. Typically `fono-<hostname>`. mDNS
    /// guarantees uniqueness per service type per LAN.
    pub instance_name: String,
    /// Local hostname to publish (`<host>.local.`). `mdns-sd` does
    /// not append the trailing dot for you; the helper below adds
    /// one when missing.
    pub hostname: String,
    /// Service port — `10300` for Wyoming, `10301` default for Fono.
    pub port: u16,
    /// IP addresses the daemon should publish in A/AAAA records.
    /// Empty = ask `mdns-sd` to auto-detect every non-loopback
    /// interface and keep the published A/AAAA records updated as
    /// addresses change.
    pub addresses: Vec<IpAddr>,
    /// Protocol revision string for the `proto` TXT key
    /// (`"wyoming/1"` / `"fono/1"`).
    pub proto: String,
    /// Server version (the `version` TXT key).
    pub version: String,
    /// Capabilities — published as a comma-joined `caps` TXT key.
    pub caps: Vec<String>,
    /// Optional primary model hint for the `model` TXT key.
    pub model: Option<String>,
    /// Whether peers must include a bearer token. `true` ⇒ `auth=token`,
    /// `false` ⇒ `auth=none`.
    pub auth_required: bool,
    /// Optional WebSocket path (Fono-native only). `Some("/fono/v1")`.
    pub path: Option<String>,
}

/// Handle returned by [`Advertiser::register`]. Drop or call
/// [`AdvertiserHandle::shutdown`] to unregister + send goodbye.
pub struct AdvertiserHandle {
    daemon: ServiceDaemon,
    fullname: String,
}

impl AdvertiserHandle {
    /// Best-effort unregister + goodbye packet. Failures are logged
    /// at `warn!` and otherwise swallowed — daemon shutdown must not
    /// fail because the LAN noticed slowly.
    pub async fn shutdown(self) {
        match self.daemon.unregister(&self.fullname) {
            Ok(rx) => {
                // Drain the unregister-status channel so the daemon
                // really did emit the goodbye. Bounded wait — on
                // local LAN this completes in ms.
                if let Err(e) = tokio::task::spawn_blocking(move || rx.recv()).await {
                    debug!(
                        target: "fono::discovery",
                        "advertiser unregister wait failed: {e:#}"
                    );
                }
            }
            Err(e) => warn!(target: "fono::discovery", "advertiser unregister failed: {e:#}"),
        }
    }

    /// Fullname registered with the daemon (mostly for diagnostics +
    /// integration tests that need to recognise their own publication).
    #[must_use]
    pub fn fullname(&self) -> &str {
        &self.fullname
    }
}

impl Drop for AdvertiserHandle {
    fn drop(&mut self) {
        // Synchronous best-effort. If the daemon is alive this fires
        // a goodbye; otherwise we're already unwinding so logging is
        // sufficient.
        if let Err(e) = self.daemon.unregister(&self.fullname) {
            debug!(
                target: "fono::discovery",
                fullname = %self.fullname,
                "advertiser sync drop unregister failed: {e:#}"
            );
        }
    }
}

/// Façade for publishing one mDNS service. Stateless beyond the
/// daemon handle.
pub struct Advertiser {
    daemon: ServiceDaemon,
}

impl Advertiser {
    #[must_use]
    pub fn new(daemon: ServiceDaemon) -> Self {
        Self { daemon }
    }

    /// Publish the service described by `spec`. The returned handle
    /// keeps the publication alive; dropping it sends a goodbye.
    pub fn register(&self, spec: AdvertiseSpec) -> Result<AdvertiserHandle> {
        let mut props: HashMap<String, String> = HashMap::new();
        if !spec.proto.is_empty() {
            props.insert(KEY_PROTO.into(), spec.proto.clone());
        }
        if !spec.version.is_empty() {
            props.insert(KEY_VERSION.into(), spec.version.clone());
        }
        if !spec.caps.is_empty() {
            props.insert(KEY_CAPS.into(), format_caps(&spec.caps));
        }
        if let Some(m) = &spec.model {
            if !m.is_empty() {
                props.insert(KEY_MODEL.into(), m.clone());
            }
        }
        props.insert(
            KEY_AUTH.into(),
            if spec.auth_required {
                "token".into()
            } else {
                "none".into()
            },
        );
        if let Some(p) = &spec.path {
            if !p.is_empty() {
                props.insert(KEY_PATH.into(), p.clone());
            }
        }

        let host = ensure_trailing_dot(&spec.hostname);
        let mut info = ServiceInfo::new(
            spec.kind.service_type(),
            &spec.instance_name,
            &host,
            spec.addresses.as_slice(),
            spec.port,
            Some(props),
        )
        .context("building mdns ServiceInfo")?;
        if spec.addresses.is_empty() {
            info = info.enable_addr_auto();
        }
        let fullname = info.get_fullname().to_string();
        self.daemon
            .register(info)
            .with_context(|| format!("mdns register({fullname})"))?;
        debug!(
            target: "fono::discovery",
            kind = ?spec.kind,
            %fullname,
            port = spec.port,
            "advertiser publishing"
        );
        Ok(AdvertiserHandle {
            daemon: self.daemon.clone(),
            fullname,
        })
    }
}

fn ensure_trailing_dot(host: &str) -> String {
    if host.ends_with('.') {
        host.to_string()
    } else {
        format!("{host}.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_trailing_dot_idempotent() {
        assert_eq!(ensure_trailing_dot("kitchen.local"), "kitchen.local.");
        assert_eq!(ensure_trailing_dot("kitchen.local."), "kitchen.local.");
    }
}
