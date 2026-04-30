// SPDX-License-Identifier: GPL-3.0-only
//! mDNS browser. One tokio task per service type maintaining the
//! shared [`super::Registry`].
//!
//! The browser is **always-on** when the `discovery` cargo feature is
//! compiled in and the daemon is running. It consumes only passive
//! multicast traffic — no outbound queries beyond a periodic refresh
//! handled internally by `mdns-sd` — so cost is negligible even on
//! battery-powered laptops.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use futures::FutureExt;
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tracing::{debug, trace, warn};

use super::txt::{
    parse_auth, parse_caps, KEY_AUTH, KEY_CAPS, KEY_MODEL, KEY_PATH, KEY_PROTO, KEY_VERSION,
};
use super::{DiscoveredPeer, PeerKind, Registry};

/// Cadence at which the browser sweeps the registry for stale peers
/// (peers whose `last_seen` is older than [`super::PEER_TTL`]).
const EVICTION_TICK: Duration = Duration::from_secs(15);

/// Handle returned by [`Browser::start`]. Drop or call
/// [`BrowserHandle::shutdown`] to stop the browse loop.
pub struct BrowserHandle {
    shutdown_tx: Option<oneshot::Sender<()>>,
    join: Option<JoinHandle<()>>,
}

impl BrowserHandle {
    /// Cancel the browse task. Existing entries in the registry are
    /// left intact — callers that want a clean slate should drop the
    /// `Registry` clone after shutdown.
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(h) = self.join.take() {
            let _ = h.await;
        }
    }
}

impl Drop for BrowserHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

/// mDNS browser façade. Constructed with a shared
/// [`mdns_sd::ServiceDaemon`] handle and a [`Registry`] to populate.
pub struct Browser {
    daemon: ServiceDaemon,
    registry: Registry,
}

impl Browser {
    #[must_use]
    pub fn new(daemon: ServiceDaemon, registry: Registry) -> Self {
        Self { daemon, registry }
    }

    /// Spawn one browse task per requested [`PeerKind`]. Returns a
    /// handle that, when dropped, cancels every spawned task. The
    /// returned future resolves once the browse subscriptions have
    /// been established, so callers can immediately
    /// `registry.snapshot()` without races.
    pub fn start(self, kinds: &[PeerKind]) -> Result<BrowserHandle> {
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
        let mut receivers = Vec::with_capacity(kinds.len());
        for kind in kinds {
            let rx = self
                .daemon
                .browse(kind.service_type())
                .with_context(|| format!("mdns browse({})", kind.service_type()))?;
            receivers.push((*kind, rx));
        }
        let registry = self.registry;
        let join = tokio::spawn(async move {
            let mut eviction_tick = tokio::time::interval(EVICTION_TICK);
            // Skip the immediate first tick — registry is empty at startup.
            eviction_tick.tick().await;
            loop {
                tokio::select! {
                    biased;
                    _ = &mut shutdown_rx => {
                        debug!(target: "fono::discovery", "browser shutdown requested");
                        return;
                    }
                    _ = eviction_tick.tick() => {
                        let evicted = registry.evict_stale();
                        if evicted > 0 {
                            debug!(
                                target: "fono::discovery",
                                evicted, "evicted stale peers from registry"
                            );
                        }
                    }
                    res = recv_first(&receivers) => {
                        match res {
                            Some((kind, ServiceEvent::ServiceResolved(info))) => {
                                if let Some(peer) = peer_from_info(kind, &info) {
                                    let inserted = registry.upsert(peer.clone());
                                    if inserted {
                                        debug!(
                                            target: "fono::discovery",
                                            kind = ?kind,
                                            fullname = %peer.fullname,
                                            host = %peer.hostname,
                                            port = peer.port,
                                            "peer discovered"
                                        );
                                    } else {
                                        trace!(
                                            target: "fono::discovery",
                                            fullname = %peer.fullname,
                                            "peer refreshed"
                                        );
                                    }
                                }
                            }
                            Some((_, ServiceEvent::ServiceRemoved(_, fullname))) => {
                                if registry.remove(&fullname).is_some() {
                                    debug!(
                                        target: "fono::discovery",
                                        %fullname, "peer goodbye"
                                    );
                                }
                            }
                            Some((_, ServiceEvent::SearchStarted(ty))) => {
                                debug!(target: "fono::discovery", %ty, "browse started");
                            }
                            Some((_, ServiceEvent::SearchStopped(ty))) => {
                                debug!(target: "fono::discovery", %ty, "browse stopped");
                            }
                            Some((_, ServiceEvent::ServiceFound(_, _))) => {
                                // Resolution still in progress; nothing
                                // to record yet.
                            }
                            None => {
                                // All receivers closed unexpectedly.
                                warn!(
                                    target: "fono::discovery",
                                    "all mdns browse receivers closed; exiting"
                                );
                                return;
                            }
                        }
                    }
                }
            }
        });

        Ok(BrowserHandle {
            shutdown_tx: Some(shutdown_tx),
            join: Some(join),
        })
    }
}

/// Receive the next event from any of the per-kind browse channels.
/// Returns `None` when every channel is closed.
async fn recv_first(
    receivers: &[(PeerKind, mdns_sd::Receiver<ServiceEvent>)],
) -> Option<(PeerKind, ServiceEvent)> {
    let mut futs = Vec::with_capacity(receivers.len());
    for (kind, rx) in receivers {
        let kind = *kind;
        let rx = rx.clone();
        futs.push(async move { rx.recv_async().await.ok().map(|ev| (kind, ev)) }.boxed());
    }
    if futs.is_empty() {
        return None;
    }
    let (winner, _idx, _rest) = futures::future::select_all(futs).await;
    winner
}

fn peer_from_info(kind: PeerKind, info: &ServiceInfo) -> Option<DiscoveredPeer> {
    let fullname = info.get_fullname().to_string();
    let port = info.get_port();
    if port == 0 {
        return None;
    }
    let hostname = info.get_hostname().to_string();
    let address = info.get_addresses().iter().next().copied();
    let proto = info
        .get_property_val_str(KEY_PROTO)
        .unwrap_or("")
        .to_string();
    let version = info
        .get_property_val_str(KEY_VERSION)
        .unwrap_or("")
        .to_string();
    let caps = info
        .get_property_val_str(KEY_CAPS)
        .map(parse_caps)
        .unwrap_or_default();
    let model = info
        .get_property_val_str(KEY_MODEL)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let auth_required = info
        .get_property_val_str(KEY_AUTH)
        .and_then(parse_auth)
        .unwrap_or(false);
    let path = info
        .get_property_val_str(KEY_PATH)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    // Friendly instance name = fullname minus its `.<service-type>` tail.
    let name = info
        .get_fullname()
        .strip_suffix(&format!(".{}", info.get_type()))
        .map_or_else(|| fullname.clone(), str::to_owned);
    Some(DiscoveredPeer {
        kind,
        fullname,
        name,
        hostname,
        address,
        port,
        proto,
        version,
        caps,
        model,
        auth_required,
        path,
        last_seen: Instant::now(),
    })
}
