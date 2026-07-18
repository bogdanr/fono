// SPDX-License-Identifier: GPL-3.0-only
//! Web settings server round-trip: exercises the inbound-auth gate, the
//! API-key management routes, and the `/api/doctor` route with a real
//! HTTP client against stub hooks.
//!
//! Note on auth: loopback callers are always trusted (no bootstrap
//! lockout), so over a real loopback socket every request is admitted
//! regardless of the `auth_enabled` toggle. The non-loopback 401 path is
//! unit-tested exhaustively in `fono_net::auth::tests`; here we assert the
//! loopback-trust behaviour and that the management routes are wired.

use std::sync::{Arc, Mutex};

use fono_net::web_settings::{DoctorFn, WebSettingsConfig, WebSettingsHooks, WebSettingsServer};

fn stub_hooks() -> WebSettingsHooks {
    let doctor: DoctorFn = Arc::new(|| {
        Box::pin(async {
            Ok(serde_json::json!({
                "version": "0.0.0",
                "variant": "cpu",
                "generated_at": 1,
                "aggregate": "warn",
                "sections": [{
                    "title": "Audio",
                    "checks": [
                        { "label": "input device", "detail": "default", "severity": "ok" },
                        { "label": "wpctl", "detail": "not found", "severity": "warn" },
                    ],
                }],
            }))
        })
    });
    // Minimal in-memory API-key store so the management routes have real
    // create → list behaviour to exercise. `next_id` mints sequential ids.
    let keys: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let next_id = Arc::new(std::sync::atomic::AtomicI64::new(1));
    let list_keys = {
        let keys = Arc::clone(&keys);
        Arc::new(move || Ok(serde_json::json!({ "keys": *keys.lock().unwrap() })))
    };
    let create_key = {
        let keys = Arc::clone(&keys);
        let next_id = Arc::clone(&next_id);
        Arc::new(move |name: &str, _exp: Option<i64>| {
            let id = next_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let view = serde_json::json!({
                "id": id, "name": name, "masked": "fono_sk_\u{2026}abcd",
                "created_at": 1, "expires_at": null, "last_used_at": null,
                "revoked": false, "usage_day": 0, "usage_month": 0,
            });
            keys.lock().unwrap().push(view.clone());
            Ok(serde_json::json!({ "key": view, "secret": "fono_sk_secretsecret" }))
        })
    };
    WebSettingsHooks {
        get_config: Arc::new(|| Ok(serde_json::json!({}))),
        put_config: Arc::new(|_| Box::pin(async { Ok(String::new()) })),
        set_secret: Arc::new(|_, _| Ok(())),
        get_vocabulary: Arc::new(|| Ok(serde_json::json!({ "vocabulary": [] }))),
        put_vocabulary: Arc::new(|_| Ok(String::new())),
        meta: Arc::new(|| serde_json::json!({})),
        doctor,
        speak: Arc::new(|_| Box::pin(async { Err("speech disabled in test".to_string()) })),
        list_api_keys: list_keys,
        create_api_key: create_key,
        update_api_key: Arc::new(|_, _| Ok(serde_json::json!({ "key": {} }))),
        delete_api_key: Arc::new(|_| Ok(())),
        list_speakers: Arc::new(|| Ok(serde_json::json!({ "speakers": [] }))),
        rename_speaker: Arc::new(|_, _| Ok(())),
        delete_speaker: Arc::new(|_| Ok(())),
        enroll_speaker: Arc::new(|_| {
            Box::pin(async { Err("enrollment disabled in test".to_string()) })
        }),
    }
}

async fn start(auth_enabled: bool) -> fono_net::web_settings::WebSettingsHandle {
    let cfg =
        WebSettingsConfig { bind: "127.0.0.1".into(), port: 0, auth_enabled, loopback_only: true };
    // A verifier that rejects everything: proves loopback is trusted even
    // when no token could ever pass.
    let verifier: fono_net::AuthVerifier = Arc::new(|_tok: &str| None);
    let usage: fono_net::UsageSink = Arc::new(|_id| {});
    WebSettingsServer::new(cfg, stub_hooks())
        .with_auth(verifier, usage)
        .start()
        .await
        .expect("server start")
}

#[tokio::test]
async fn loopback_is_trusted_even_with_auth_on() {
    let handle = start(true).await;
    let base = format!("http://{}", handle.local_addr());
    let client = reqwest::Client::new();

    // Loopback with auth on and a reject-all verifier → still admitted.
    let r = client.get(format!("{base}/api/doctor")).send().await.expect("send");
    assert_eq!(r.status(), 200);
    let body: serde_json::Value = r.json().await.expect("json");
    assert_eq!(body["aggregate"], "warn");
    assert_eq!(body["sections"][0]["checks"][1]["severity"], "warn");

    // Static assets stay open (they hold no state).
    let r = client.get(format!("{base}/")).send().await.expect("send");
    assert_eq!(r.status(), 200);

    handle.shutdown().await;
}

#[tokio::test]
async fn doctor_route_open_with_auth_off() {
    let handle = start(false).await;
    let base = format!("http://{}", handle.local_addr());
    let r = reqwest::get(format!("{base}/api/doctor")).await.expect("send");
    assert_eq!(r.status(), 200);
    handle.shutdown().await;
}

#[tokio::test]
async fn api_keys_create_then_list_round_trip() {
    let handle = start(true).await;
    let base = format!("http://{}", handle.local_addr());
    let client = reqwest::Client::new();

    // Initially empty.
    let r = client.get(format!("{base}/api/apikeys")).send().await.expect("send");
    assert_eq!(r.status(), 200);
    let body: serde_json::Value = r.json().await.expect("json");
    assert_eq!(body["keys"].as_array().unwrap().len(), 0);

    // Create one — the plaintext secret is returned exactly once.
    let r = client
        .post(format!("{base}/api/apikeys"))
        .header("content-type", "application/json")
        .body(serde_json::json!({ "name": "laptop" }).to_string())
        .send()
        .await
        .expect("send");
    assert_eq!(r.status(), 200);
    let body: serde_json::Value = r.json().await.expect("json");
    assert!(body["secret"].as_str().unwrap().starts_with("fono_sk_"));
    assert_eq!(body["key"]["name"], "laptop");

    // It now appears in the list.
    let r = client.get(format!("{base}/api/apikeys")).send().await.expect("send");
    let body: serde_json::Value = r.json().await.expect("json");
    assert_eq!(body["keys"].as_array().unwrap().len(), 1);
    assert_eq!(body["keys"][0]["name"], "laptop");

    handle.shutdown().await;
}

#[tokio::test]
async fn speakers_list_and_mutations_round_trip() {
    let handle = start(true).await;
    let base = format!("http://{}", handle.local_addr());
    let client = reqwest::Client::new();

    // Metadata listing is served (the stub reports an empty roster).
    let r = client.get(format!("{base}/api/speakers")).send().await.expect("send");
    assert_eq!(r.status(), 200);
    let body: serde_json::Value = r.json().await.expect("json");
    assert_eq!(body["speakers"].as_array().unwrap().len(), 0);

    // Rename accepts a JSON name and reports success.
    let r = client
        .patch(format!("{base}/api/speakers/1"))
        .header("content-type", "application/json")
        .body(serde_json::json!({ "name": "Ada" }).to_string())
        .send()
        .await
        .expect("send");
    assert_eq!(r.status(), 200);

    // A non-numeric id is a client error, not a 500.
    let r = client
        .patch(format!("{base}/api/speakers/notanid"))
        .header("content-type", "application/json")
        .body(serde_json::json!({ "name": "Ada" }).to_string())
        .send()
        .await
        .expect("send");
    assert_eq!(r.status(), 400);

    // Delete by id succeeds.
    let r = client.delete(format!("{base}/api/speakers/1")).send().await.expect("send");
    assert_eq!(r.status(), 200);

    // Enrollment is wired: the stub rejects, surfacing a 422 (not a 404),
    // proving `POST /api/speakers` reaches the enroll hook.
    let r = client
        .post(format!("{base}/api/speakers"))
        .header("content-type", "application/json")
        .body(serde_json::json!({ "name": "Ada", "audio_pcm16": "AAA=" }).to_string())
        .send()
        .await
        .expect("send");
    assert_eq!(r.status(), 422);

    handle.shutdown().await;
}
