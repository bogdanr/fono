// SPDX-License-Identifier: GPL-3.0-only
//! Web settings server round-trip: exercises the token gate and the
//! `/api/doctor` route with a real HTTP client against stub hooks.

use std::sync::Arc;

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
    WebSettingsHooks {
        get_config: Arc::new(|| Ok(serde_json::json!({}))),
        put_config: Arc::new(|_| Box::pin(async { Ok(String::new()) })),
        set_secret: Arc::new(|_, _| Ok(())),
        get_vocabulary: Arc::new(|| Ok(serde_json::json!({ "vocabulary": [] }))),
        put_vocabulary: Arc::new(|_| Ok(String::new())),
        meta: Arc::new(|| serde_json::json!({})),
        doctor,
        speak: Arc::new(|_| Box::pin(async { Err("speech disabled in test".to_string()) })),
    }
}

async fn start(auth_token: Option<&str>) -> fono_net::web_settings::WebSettingsHandle {
    let cfg = WebSettingsConfig {
        bind: "127.0.0.1".into(),
        port: 0,
        auth_token: auth_token.map(str::to_owned),
        loopback_only: true,
    };
    WebSettingsServer::new(cfg, stub_hooks()).start().await.expect("server start")
}

#[tokio::test]
async fn doctor_route_is_token_gated() {
    let handle = start(Some("s3cret")).await;
    let base = format!("http://{}", handle.local_addr());
    let client = reqwest::Client::new();

    // No token -> 401.
    let r = client.get(format!("{base}/api/doctor")).send().await.expect("send");
    assert_eq!(r.status(), 401);

    // Bearer header -> 200 with the structured report.
    let r = client
        .get(format!("{base}/api/doctor"))
        .header("Authorization", "Bearer s3cret")
        .send()
        .await
        .expect("send");
    assert_eq!(r.status(), 200);
    let body: serde_json::Value = r.json().await.expect("json");
    assert_eq!(body["aggregate"], "warn");
    assert_eq!(body["sections"][0]["checks"][1]["severity"], "warn");

    // Query-parameter token (how the browser page authenticates) -> 200.
    let r = client.get(format!("{base}/api/doctor?token=s3cret")).send().await.expect("send");
    assert_eq!(r.status(), 200);

    // Static assets stay open (they hold no state).
    let r = client.get(format!("{base}/")).send().await.expect("send");
    assert_eq!(r.status(), 200);

    handle.shutdown().await;
}

#[tokio::test]
async fn doctor_route_open_without_token() {
    let handle = start(None).await;
    let base = format!("http://{}", handle.local_addr());
    let r = reqwest::get(format!("{base}/api/doctor")).await.expect("send");
    assert_eq!(r.status(), 200);
    handle.shutdown().await;
}
