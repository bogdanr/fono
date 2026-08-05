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

// One stub per hook, so the length tracks the hook count rather than any
// complexity worth splitting up.
#[allow(clippy::too_many_lines)]
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
    let (list_keys, create_key) = api_key_stubs();
    // Minimal in-memory tool catalogue: one tool whose `enabled` flag the
    // PATCH route flips, so the round-trip proves the wiring rather than a
    // hard-coded reply.
    let tool_enabled = Arc::new(Mutex::new(true));
    // The learned phrases, so `/api/shortcuts` has real add → list → forget
    // behaviour to exercise. Reported through `/api/tools`, which is the same
    // door the page reads them from.
    let phrases: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let list_tools = {
        let flag = Arc::clone(&tool_enabled);
        let phrases = Arc::clone(&phrases);
        Arc::new(move || {
            Ok(serde_json::json!({
                "servers": [{ "name": "ha", "url": "http://ha/sse", "configured": true }],
                "tools": [{
                    "source": "ha", "name": "HassTurnOn", "enabled": *flag.lock().unwrap(),
                    "available": true, "capability": "safe",
                    "verify_class": "post_condition",
                }],
                "shortcuts": phrases.lock().unwrap().iter()
                    .map(|p| serde_json::json!({ "phrase": p }))
                    .collect::<Vec<_>>(),
            }))
        })
    };
    let edit_shortcut: fono_net::web_settings::EditShortcutFn = {
        let phrases = Arc::clone(&phrases);
        Arc::new(move |phrase: &str, also: Option<&str>| {
            let mut held = phrases.lock().unwrap();
            match also {
                Some(also) => held.push(also.to_string()),
                None => held.retain(|p| p != phrase),
            }
            drop(held);
            Ok(())
        })
    };
    let set_tool_enabled = {
        let flag = Arc::clone(&tool_enabled);
        Arc::new(move |_source: &str, name: &str, enabled: bool| {
            if name != "HassTurnOn" {
                return Err(format!("unknown tool {name}"));
            }
            *flag.lock().unwrap() = enabled;
            Ok(())
        })
    };
    let (list_dictation, list_threads, get_thread, delete_history) = history_stubs();
    WebSettingsHooks {
        get_config: Arc::new(|| Ok(serde_json::json!({}))),
        put_config: Arc::new(|_| Box::pin(async { Ok(String::new()) })),
        set_secret: Arc::new(|_, _| Ok(())),
        get_vocabulary: Arc::new(|| Ok(serde_json::json!({ "vocabulary": [] }))),
        put_vocabulary: Arc::new(|_| Ok(String::new())),
        meta: Arc::new(|| serde_json::json!({})),
        doctor,
        prompt_cache: Arc::new(|| {
            Ok(serde_json::json!({ "caches": [{
                "role": "assistant", "model": "stub", "runtime": "abcd1234",
                "max_entries": 10, "max_bytes": 268_435_456u64,
                "entries_pinned": 1, "entries_evictable": 1, "entries_free": 9,
                "bytes_pinned": 2048, "bytes_evictable": 1024,
                "bytes_free": 268_434_432u64, "bytes_resident": 3072,
                "nodes": [], "unplaced": [], "verdicts": {}, "counters": {},
            }] }))
        }),
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
        calibrate_speaker: Arc::new(|_, _| {
            Box::pin(async { Err("calibration disabled in test".to_string()) })
        }),
        list_utterances: Arc::new(|_| {
            Ok(serde_json::json!({ "utterances": [], "suggested_prune": [] }))
        }),
        delete_utterance: Arc::new(|_, _| Ok(())),
        list_tools,
        set_tool_enabled,
        discover_tools: Arc::new(|_probe| {
            Box::pin(async { Err("no MCP server reachable in test".to_string()) })
        }),
        edit_shortcut,
        probe_llm: Arc::new(|_spec| {
            Box::pin(async { Err("no LLM server reachable in test".to_string()) })
        }),
        list_dictation,
        list_threads,
        get_thread,
        delete_history,
    }
}

/// Minimal in-memory API-key store so the management routes have real
/// create → list behaviour to exercise. `next_id` mints sequential ids.
fn api_key_stubs() -> (fono_net::web_settings::ListApiKeysFn, fono_net::web_settings::CreateApiKeyFn)
{
    let keys: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let next_id = Arc::new(std::sync::atomic::AtomicI64::new(1));
    let list_keys: fono_net::web_settings::ListApiKeysFn = {
        let keys = Arc::clone(&keys);
        Arc::new(move || Ok(serde_json::json!({ "keys": *keys.lock().unwrap() })))
    };
    let create_key: fono_net::web_settings::CreateApiKeyFn = {
        let keys = Arc::clone(&keys);
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
    (list_keys, create_key)
}

/// In-memory stand-ins for the four `/api/history/*` hooks. Dictation is
/// backed by real mutable state so the delete verbs have something to
/// change; conversations are fixed fixtures, since the routes only ever
/// read them.
fn history_stubs() -> (
    fono_net::web_settings::ListDictationFn,
    fono_net::web_settings::ListThreadsFn,
    fono_net::web_settings::GetThreadFn,
    fono_net::web_settings::DeleteHistoryFn,
) {
    // One entry with a detected speaker and one without, so the response
    // shape is exercised both ways.
    let dict: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(vec![
        serde_json::json!({
            "id": 1, "ts": 100, "raw": "hello world", "cleaned": "Hello, world.",
            "app_class": "kitty", "app_title": "zsh", "stt_backend": "whisper",
            "language": "en", "speaker": "Radu",
        }),
        serde_json::json!({
            "id": 2, "ts": 200, "raw": "second note", "cleaned": null,
            "app_class": null, "app_title": null, "stt_backend": "whisper",
            "language": "en", "speaker": null,
        }),
    ]));
    let list_dictation: fono_net::web_settings::ListDictationFn = {
        let dict = Arc::clone(&dict);
        Arc::new(move |q, limit| {
            let hits: Vec<_> = dict
                .lock()
                .unwrap()
                .iter()
                .filter(|e| q.is_empty() || e["raw"].as_str().is_some_and(|r| r.contains(q)))
                .take(limit)
                .cloned()
                .collect();
            Ok(serde_json::json!({ "entries": hits }))
        })
    };
    let delete_history: fono_net::web_settings::DeleteHistoryFn = {
        let dict = Arc::clone(&dict);
        Arc::new(move |kind, id| {
            if kind != "dictation" {
                return Ok(0);
            }
            let mut rows = dict.lock().unwrap();
            let before = rows.len();
            match id {
                Some(id) => rows.retain(|e| e["id"].as_i64() != Some(id)),
                None => rows.clear(),
            }
            Ok(before - rows.len())
        })
    };
    let list_threads: fono_net::web_settings::ListThreadsFn = Arc::new(|_limit| {
        Ok(serde_json::json!({ "threads": [{
            "id": 7, "started_at": 100, "last_at": 160, "ended": true,
            "turn_count": 2, "preview": "what is the time",
            "speakers": ["Radu"], "backend": "openai", "model": "gpt-4o-mini",
        }] }))
    });
    let get_thread: fono_net::web_settings::GetThreadFn = Arc::new(|id| {
        if id != 7 {
            return Err("no such conversation".to_string());
        }
        Ok(serde_json::json!({ "turns": [
            { "ordinal": 0, "role": "user", "text": "what is the time",
              "ts": 100, "speaker": "Radu", "latency_ms": null, "partial": false },
            { "ordinal": 1, "role": "assistant", "text": "just past four",
              "ts": 101, "speaker": null, "latency_ms": 820, "partial": false },
        ] }))
    });
    (list_dictation, list_threads, get_thread, delete_history)
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

/// The prompt-cache route is wired and, unlike the doctor report, answers
/// without spawning a probe — so the page can refresh it as a conversation
/// runs.
#[tokio::test]
async fn prompt_cache_route_reports_occupancy() {
    let handle = start(true).await;
    let base = format!("http://{}", handle.local_addr());
    let client = reqwest::Client::new();

    let r = client.get(format!("{base}/api/promptcache")).send().await.expect("send");
    assert_eq!(r.status(), 200);
    let body: serde_json::Value = r.json().await.expect("json");
    assert_eq!(body["caches"][0]["role"], "assistant");
    assert_eq!(body["caches"][0]["entries_pinned"], 1);
    assert_eq!(body["caches"][0]["bytes_resident"], 3072);

    handle.shutdown().await;
}

/// The settings page must serve the same φ favicon the website uses, so a
/// pinned tab looks identical in both places.
#[tokio::test]
async fn favicon_is_served_as_svg() {
    let handle = start(true).await;
    let base = format!("http://{}", handle.local_addr());
    let client = reqwest::Client::new();

    for path in ["/favicon.svg", "/favicon.ico"] {
        let r = client.get(format!("{base}{path}")).send().await.expect("send");
        assert_eq!(r.status(), 200, "{path} should be served");
        assert_eq!(
            r.headers().get("content-type").and_then(|v| v.to_str().ok()),
            Some("image/svg+xml"),
            "{path} should be typed as SVG"
        );
        let body = r.text().await.expect("body");
        assert!(body.contains("<svg"), "{path} should be an SVG document");
        assert!(body.contains("#c4533a"), "{path} should use the Fono red from the website");
    }

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

/// Deselecting a tool must survive the round-trip: the list route reports
/// the user's choice back, not the default.
#[tokio::test]
async fn tool_can_be_deselected_and_the_choice_is_reported_back() {
    let handle = start(true).await;
    let base = format!("http://{}", handle.local_addr());
    let client = reqwest::Client::new();

    // Discovered tools start enabled.
    let r = client.get(format!("{base}/api/tools")).send().await.expect("send");
    assert_eq!(r.status(), 200);
    let body: serde_json::Value = r.json().await.expect("json");
    assert_eq!(body["tools"][0]["name"], "HassTurnOn");
    assert_eq!(body["tools"][0]["enabled"], true);

    // Switch it off.
    let r = client
        .patch(format!("{base}/api/tools"))
        .header("content-type", "application/json")
        .body(r#"{"source":"ha","name":"HassTurnOn","enabled":false}"#)
        .send()
        .await
        .expect("send");
    assert_eq!(r.status(), 200);

    let r = client.get(format!("{base}/api/tools")).send().await.expect("send");
    let body: serde_json::Value = r.json().await.expect("json");
    assert_eq!(body["tools"][0]["enabled"], false);

    // A body missing `enabled` is a client error, not a silent no-op.
    let r = client
        .patch(format!("{base}/api/tools"))
        .header("content-type", "application/json")
        .body(r#"{"source":"ha","name":"HassTurnOn"}"#)
        .send()
        .await
        .expect("send");
    assert_eq!(r.status(), 400);

    handle.shutdown().await;
}

/// A server we cannot reach must surface as an error the UI can show, not a
/// silent success.
#[tokio::test]
async fn discovery_failure_is_reported() {
    let handle = start(true).await;
    let base = format!("http://{}", handle.local_addr());
    let r = reqwest::Client::new()
        .post(format!("{base}/api/tools/discover"))
        .send()
        .await
        .expect("send");
    assert_eq!(r.status(), 502);
    handle.shutdown().await;
}

/// The two edits offered on a learned phrase reach the store, and are told
/// apart by the verb.
///
/// Asserted through the page's own door — the phrase list comes back on
/// `/api/tools` — because a route that returns 200 while changing nothing is
/// exactly the failure this is here to catch. Deleting is a `DELETE`, not a
/// `POST` with a flag, so a request that loses its body cannot be read as
/// "forget it".
#[tokio::test]
async fn a_learned_phrase_can_gain_a_wording_and_be_forgotten() {
    let handle = start(true).await;
    let base = format!("http://{}", handle.local_addr());
    let client = reqwest::Client::new();
    let phrases = |body: &serde_json::Value| {
        body["shortcuts"]
            .as_array()
            .expect("the page is told about phrases")
            .iter()
            .map(|s| s["phrase"].as_str().unwrap_or_default().to_string())
            .collect::<Vec<_>>()
    };

    let r = client
        .post(format!("{base}/api/shortcuts"))
        .header("content-type", "application/json")
        .body(r#"{"like":"turn on the hall lamp","phrase":"aprinde lampa"}"#)
        .send()
        .await
        .expect("send");
    assert_eq!(r.status(), 200);
    let body: serde_json::Value =
        client.get(format!("{base}/api/tools")).send().await.expect("send").json().await.unwrap();
    assert_eq!(phrases(&body), ["aprinde lampa"], "the added wording is listed");

    let r = client
        .delete(format!("{base}/api/shortcuts?phrase=aprinde%20lampa"))
        .send()
        .await
        .expect("send");
    assert_eq!(r.status(), 200);
    let body: serde_json::Value =
        client.get(format!("{base}/api/tools")).send().await.expect("send").json().await.unwrap();
    assert!(phrases(&body).is_empty(), "and forgetting it removes it");

    // A wording with nothing to attach it to is refused rather than stored
    // against an empty phrase.
    let r = client
        .post(format!("{base}/api/shortcuts"))
        .header("content-type", "application/json")
        .body(r#"{"like":"turn on the hall lamp"}"#)
        .send()
        .await
        .expect("send");
    assert_eq!(r.status(), 400);
    // And so is a delete that names nothing.
    let r = client.delete(format!("{base}/api/shortcuts")).send().await.expect("send");
    assert_eq!(r.status(), 400);

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

    // Calibration is wired: the stub rejects, surfacing a 422 (not a 404),
    // proving `POST /api/speakers/{id}/calibrate` reaches the calibrate hook.
    let r = client
        .post(format!("{base}/api/speakers/1/calibrate"))
        .header("content-type", "application/json")
        .body(serde_json::json!({ "clips": [] }).to_string())
        .send()
        .await
        .expect("send");
    assert_eq!(r.status(), 422);

    // A non-numeric id on calibrate is a client error, not a 500.
    let r = client
        .post(format!("{base}/api/speakers/notanid/calibrate"))
        .header("content-type", "application/json")
        .body(serde_json::json!({ "clips": [] }).to_string())
        .send()
        .await
        .expect("send");
    assert_eq!(r.status(), 400);

    // Utterance list is wired: the stub returns an empty set (200), proving
    // `GET /api/speakers/{id}/utterances` reaches the list hook.
    let r = client.get(format!("{base}/api/speakers/1/utterances")).send().await.expect("send");
    assert_eq!(r.status(), 200);
    let body: serde_json::Value = r.json().await.expect("json");
    assert!(body.get("utterances").is_some(), "utterance list shape");

    // Utterance delete is wired: the stub accepts (200), proving
    // `DELETE /api/speakers/{id}/utterances/{uid}` reaches the delete hook.
    let r =
        client.delete(format!("{base}/api/speakers/1/utterances/2")).send().await.expect("send");
    assert_eq!(r.status(), 200);

    // A non-numeric utterance id is a client error, not a 500.
    let r = client
        .delete(format!("{base}/api/speakers/1/utterances/notanid"))
        .send()
        .await
        .expect("send");
    assert_eq!(r.status(), 400);

    handle.shutdown().await;
}

/// The history page must be able to browse both stores, see the detected
/// speaker, search, and delete — end to end over HTTP.
#[tokio::test]
async fn history_browse_search_and_delete_round_trip() {
    let handle = start(true).await;
    let base = format!("http://{}", handle.local_addr());
    let client = reqwest::Client::new();

    // Dictation listing carries the verified speaker when there was one.
    let r = client.get(format!("{base}/api/history/dictation")).send().await.expect("send");
    assert_eq!(r.status(), 200);
    let body: serde_json::Value = r.json().await.expect("json");
    assert_eq!(body["entries"].as_array().unwrap().len(), 2);
    assert_eq!(body["entries"][0]["speaker"], "Radu");
    assert!(body["entries"][1]["speaker"].is_null(), "unattributed entries stay null");

    // Search narrows the list; `limit` is honoured.
    let r = client
        .get(format!("{base}/api/history/dictation?q=second&limit=10"))
        .send()
        .await
        .expect("send");
    let body: serde_json::Value = r.json().await.expect("json");
    assert_eq!(body["entries"].as_array().unwrap().len(), 1);
    assert_eq!(body["entries"][0]["id"], 2);

    // Conversation threads list with their participants.
    let r = client.get(format!("{base}/api/history/conversations")).send().await.expect("send");
    assert_eq!(r.status(), 200);
    let body: serde_json::Value = r.json().await.expect("json");
    assert_eq!(body["threads"][0]["id"], 7);
    assert_eq!(body["threads"][0]["speakers"][0], "Radu");

    // Opening a thread returns its turns in order, speaker included.
    let r = client.get(format!("{base}/api/history/conversations/7")).send().await.expect("send");
    assert_eq!(r.status(), 200);
    let body: serde_json::Value = r.json().await.expect("json");
    assert_eq!(body["turns"][0]["role"], "user");
    assert_eq!(body["turns"][0]["speaker"], "Radu");
    assert_eq!(body["turns"][1]["role"], "assistant");

    // An unknown thread is a 404, not a 500.
    let r = client.get(format!("{base}/api/history/conversations/99")).send().await.expect("send");
    assert_eq!(r.status(), 404);

    // A non-numeric id is a client error.
    let r = client.get(format!("{base}/api/history/conversations/abc")).send().await.expect("send");
    assert_eq!(r.status(), 400);

    // Deleting one entry removes exactly that entry.
    let r = client.delete(format!("{base}/api/history/dictation/1")).send().await.expect("send");
    assert_eq!(r.status(), 200);
    let body: serde_json::Value = r.json().await.expect("json");
    assert_eq!(body["deleted"], 1);

    let r = client.get(format!("{base}/api/history/dictation")).send().await.expect("send");
    let body: serde_json::Value = r.json().await.expect("json");
    assert_eq!(body["entries"].as_array().unwrap().len(), 1);

    // "Clear all" wipes the rest.
    let r = client.delete(format!("{base}/api/history/dictation")).send().await.expect("send");
    let body: serde_json::Value = r.json().await.expect("json");
    assert_eq!(body["deleted"], 1);

    // An unknown history kind is a 404, not a silent success.
    let r = client.delete(format!("{base}/api/history/nonsense")).send().await.expect("send");
    assert_eq!(r.status(), 404);

    handle.shutdown().await;
}
