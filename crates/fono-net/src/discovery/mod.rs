// SPDX-License-Identifier: GPL-3.0-only
//! mDNS / DNS-SD autodiscovery for LAN Fono and Wyoming servers.
//!
//! Slice 4 of `plans/2026-04-29-2026-04-29-client-server-wyoming-fono-and-mdns-v2.md`.
//!
//! Two service types are watched and (optionally) advertised:
//!
//! * `_wyoming._tcp.local.` — the de-facto Wyoming service type used by
//!   wyoming-faster-whisper, Home Assistant satellites, Rhasspy, and
//!   any future Fono daemon hosting `[server.wyoming]`.
//! * `_fono._tcp.local.` — Fono-native protocol (Slice 5/6,
//!   WebSocket-based). Reserved here so the registry can already
//!   surface peers when Slice 6 lands; the advertiser only publishes
//!   the kinds the daemon currently serves.
//!
//! All discovery state is **ephemeral** — the registry is an
//! `Arc<RwLock<HashMap>>`. Restart Fono and the LAN is rediscovered
//! from scratch. There is no on-disk cache by design (eliminates a
//! whole class of stale-state failure modes).

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

pub mod advertiser;
pub mod browser;
pub mod txt;

pub use advertiser::Advertiser;
pub use browser::Browser;

/// Default Wyoming service type. Matches wyoming-faster-whisper,
/// Rhasspy, and Home Assistant.
pub const WYOMING_SERVICE_TYPE: &str = "_wyoming._tcp.local.";

/// Fono-native service type (Slice 5/6 — WebSocket).
pub const FONO_SERVICE_TYPE: &str = "_fono._tcp.local.";

/// Peers older than this are evicted from the registry on every
/// browser tick. Real mDNS daemons send periodic refreshes well
/// inside this window; the eviction is defence-in-depth for hosts
/// that vanish without a goodbye packet (suspended laptop, killed
/// container).
pub const PEER_TTL: Duration = Duration::from_secs(120);

/// Which Fono protocol family a discovered peer speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PeerKind {
    /// `_wyoming._tcp.local.` — speaks the Wyoming protocol on raw
    /// TCP. Compatible with `fono-stt::wyoming::WyomingStt`.
    Wyoming,
    /// `_fono._tcp.local.` — speaks the Fono-native protocol over
    /// WebSocket. Slice 5/6.
    Fono,
}

impl PeerKind {
    /// mDNS service type string this kind subscribes to.
    #[must_use]
    pub fn service_type(self) -> &'static str {
        match self {
            Self::Wyoming => WYOMING_SERVICE_TYPE,
            Self::Fono => FONO_SERVICE_TYPE,
        }
    }

    /// Best-effort classify a service type string.
    #[must_use]
    pub fn from_service_type(ty: &str) -> Option<Self> {
        match ty {
            WYOMING_SERVICE_TYPE => Some(Self::Wyoming),
            FONO_SERVICE_TYPE => Some(Self::Fono),
            _ => None,
        }
    }
}

/// One LAN peer the browser has resolved. Cheap to clone (single
/// `String` allocation per field).
#[derive(Debug, Clone)]
pub struct DiscoveredPeer {
    /// Wyoming or Fono-native.
    pub kind: PeerKind,
    /// mDNS instance fullname (`fono-A._wyoming._tcp.local.`). Used as
    /// the registry key — guaranteed unique per advertiser.
    pub fullname: String,
    /// Friendly instance name (`fono-A`) — the part before the type.
    pub name: String,
    /// Resolved hostname (typically `<host>.local.`).
    pub hostname: String,
    /// First resolved address; clients connect via `hostname` unless
    /// DNS-SD is broken on their side, in which case this fallback
    /// keeps things working.
    pub address: Option<IpAddr>,
    /// Service port (`10300` for Wyoming, `10301` default for Fono).
    pub port: u16,
    /// `proto` TXT key — protocol-specific revision. Wyoming uses
    /// `wyoming/1`; Fono uses `fono/1`.
    pub proto: String,
    /// `version` TXT key — server version string for diagnostics.
    pub version: String,
    /// `caps` TXT key, comma-split. E.g. `["stt"]`, `["stt","llm"]`.
    pub caps: Vec<String>,
    /// `model` TXT key — a primary model hint (Wyoming only). The
    /// browser pre-caches this so the tray menu can render
    /// `Wyoming · kitchen-pc.local (whisper-small)` without a side
    /// channel.
    pub model: Option<String>,
    /// `auth` TXT key — `"token"` if the peer expects a pre-shared
    /// bearer; `"none"` (or missing) otherwise.
    pub auth_required: bool,
    /// `path` TXT key — WebSocket path for Fono-native peers
    /// (e.g. `/fono/v1`). Always `None` for Wyoming.
    pub path: Option<String>,
    /// Last time we saw a `ServiceResolved` for this fullname. Used
    /// by [`Registry::evict_stale`].
    pub last_seen: Instant,
}

impl DiscoveredPeer {
    /// `host:port` form suitable for `tcp://...` URI construction.
    #[must_use]
    pub fn host_port(&self) -> String {
        format!("{}:{}", self.hostname.trim_end_matches('.'), self.port)
    }

    /// Human-readable label for the tray submenu.
    #[must_use]
    pub fn tray_label(&self) -> String {
        let host = self.hostname.trim_end_matches('.');
        match (self.kind, self.model.as_deref()) {
            (PeerKind::Wyoming, Some(model)) => format!("Wyoming · {host} ({model})"),
            (PeerKind::Wyoming, None) => format!("Wyoming · {host}"),
            (PeerKind::Fono, _) => format!("Fono server · {host}"),
        }
    }
}

/// Shared registry of currently-known LAN peers, keyed by mDNS
/// fullname. Cheap to clone — the inner `Arc` is reference-counted.
#[derive(Debug, Clone, Default)]
pub struct Registry {
    inner: Arc<RwLock<HashMap<String, DiscoveredPeer>>>,
}

impl Registry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Take a snapshot of the current peer list, sorted by
    /// `(kind, hostname, port)` for stable tray menu ordering.
    #[must_use]
    pub fn snapshot(&self) -> Vec<DiscoveredPeer> {
        let mut out: Vec<_> = self
            .inner
            .read()
            .expect("registry lock poisoned")
            .values()
            .cloned()
            .collect();
        out.sort_by(|a, b| {
            (a.kind as u8, a.hostname.as_str(), a.port).cmp(&(b.kind as u8, &b.hostname, b.port))
        });
        out
    }

    /// Number of currently-known peers (across all kinds).
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.read().expect("registry lock poisoned").len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Insert / refresh a peer. Replaces any existing entry with the
    /// same fullname. Returns `true` if this is a new fullname.
    pub fn upsert(&self, peer: DiscoveredPeer) -> bool {
        let mut g = self.inner.write().expect("registry lock poisoned");
        g.insert(peer.fullname.clone(), peer).is_none()
    }

    /// Remove a peer by fullname. Returns the removed entry, if any.
    pub fn remove(&self, fullname: &str) -> Option<DiscoveredPeer> {
        self.inner
            .write()
            .expect("registry lock poisoned")
            .remove(fullname)
    }

    /// Evict peers whose `last_seen` is older than [`PEER_TTL`].
    /// Returns the number of evictions.
    pub fn evict_stale(&self) -> usize {
        let now = Instant::now();
        let mut g = self.inner.write().expect("registry lock poisoned");
        let before = g.len();
        g.retain(|_, p| now.duration_since(p.last_seen) <= PEER_TTL);
        before - g.len()
    }
}
