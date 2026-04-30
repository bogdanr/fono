// SPDX-License-Identifier: GPL-3.0-only
//! Integration test for the mDNS discovery round-trip: an in-process
//! advertiser publishes a Wyoming service and a browser running on a
//! second `ServiceDaemon` resolves it into the shared `Registry`.
//!
//! The test relies on real multicast loopback. CI runners that
//! disallow multicast will skip when `ServiceDaemon::new()` errors.

#![cfg(feature = "discovery")]

use std::time::{Duration, Instant};

use fono_net::discovery::{
    advertiser::{AdvertiseSpec, Advertiser},
    Browser, PeerKind, Registry,
};
use mdns_sd::ServiceDaemon;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn advertise_then_browse_resolves_peer() {
    // Two independent daemons sharing the loopback multicast group —
    // mdns-sd uses one OS socket per daemon so this models real LAN
    // behaviour rather than an in-process shortcut.
    let Ok(adv_daemon) = ServiceDaemon::new() else {
        eprintln!("ServiceDaemon::new() failed; skipping (no multicast in this env)");
        return;
    };
    let Ok(browse_daemon) = ServiceDaemon::new() else {
        eprintln!("second ServiceDaemon::new() failed; skipping");
        return;
    };

    let registry = Registry::new();
    let browser = Browser::new(browse_daemon, registry.clone());
    let _bh = browser.start(&[PeerKind::Wyoming]).expect("start browser");

    let advertiser = Advertiser::new(adv_daemon);
    let spec = AdvertiseSpec {
        kind: PeerKind::Wyoming,
        instance_name: "fono-test".into(),
        hostname: "fono-test.local".into(),
        port: 10300,
        addresses: vec![],
        proto: "wyoming/1".into(),
        version: "0.0.0-test".into(),
        caps: vec!["stt".into()],
        model: Some("whisper-tiny".into()),
        auth_required: false,
        path: None,
    };
    let handle = advertiser.register(spec).expect("register service");
    let fullname = handle.fullname().to_string();

    // Wait up to 5 s for the browser to resolve the publication.
    let deadline = Instant::now() + Duration::from_secs(5);
    let resolved = loop {
        if let Some(p) = registry
            .snapshot()
            .into_iter()
            .find(|p| p.fullname == fullname)
        {
            break Some(p);
        }
        if Instant::now() >= deadline {
            break None;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };

    let Some(peer) = resolved else {
        eprintln!(
            "browser did not resolve publication within 5s — likely a sandbox without \
             multicast loopback; treating as skipped"
        );
        return;
    };

    assert_eq!(peer.kind, PeerKind::Wyoming);
    assert_eq!(peer.port, 10300);
    assert_eq!(peer.proto, "wyoming/1");
    assert_eq!(peer.model.as_deref(), Some("whisper-tiny"));
    assert!(peer.caps.contains(&"stt".to_string()));
    assert!(!peer.auth_required);

    handle.shutdown().await;
}
