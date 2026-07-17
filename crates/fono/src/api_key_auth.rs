// SPDX-License-Identifier: GPL-3.0-only
//! Daemon-side glue between the [`fono_core::api_keys::ApiKeyStore`] and
//! the HTTP servers' injected auth closures ([`fono_net::AuthVerifier`],
//! [`fono_net::UsageSink`]) plus the web-settings management hooks.
//!
//! The store wraps a `rusqlite::Connection` (not `Sync`), so it lives
//! behind an `Arc<Mutex<…>>`. Verification and management lock it
//! briefly; usage recording UPSERT-increments bounded per-interval
//! counters and, at most hourly, prunes stale buckets so the DB never
//! grows into an access log.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use fono_core::api_keys::ApiKeyStore;
use fono_core::Paths;
use fono_net::web_settings::{CreateApiKeyFn, DeleteApiKeyFn, ListApiKeysFn, UpdateApiKeyFn};
use fono_net::{AuthVerifier, UsageSink};

/// Prune stale usage buckets no more than once per this many seconds.
const PRUNE_INTERVAL_SECS: i64 = 3_600;

fn now_unix() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Shared handle to the inbound API-key store, cheap to clone.
#[derive(Clone)]
pub struct ApiKeyAuth {
    store: Arc<Mutex<ApiKeyStore>>,
    last_prune: Arc<AtomicI64>,
}

impl ApiKeyAuth {
    /// Open (or create) the store at the canonical `api_keys.sqlite` path.
    pub fn open(paths: &Paths) -> anyhow::Result<Self> {
        let store = ApiKeyStore::open(&paths.api_keys_db())?;
        Ok(Self { store: Arc::new(Mutex::new(store)), last_prune: Arc::new(AtomicI64::new(0)) })
    }

    /// Number of keys that are neither revoked nor expired. Used by the
    /// daemon's exposure warnings and `fono doctor`.
    pub fn active_count(&self) -> i64 {
        self.store.lock().map(|s| s.active_count().unwrap_or(0)).unwrap_or(0)
    }

    /// Verifier closure for [`fono_net::LlmServer::with_auth`] /
    /// [`fono_net::web_settings::WebSettingsServer::with_auth`].
    pub fn verifier(&self) -> AuthVerifier {
        let store = Arc::clone(&self.store);
        Arc::new(move |presented: &str| {
            store.lock().ok().and_then(|s| s.verify(presented).ok().flatten())
        })
    }

    /// Usage sink closure: records one authenticated hit and, at most
    /// hourly, prunes stale buckets to keep the DB bounded.
    pub fn usage_sink(&self) -> UsageSink {
        let store = Arc::clone(&self.store);
        let last_prune = Arc::clone(&self.last_prune);
        Arc::new(move |key_id: i64| {
            let now = now_unix();
            if let Ok(s) = store.lock() {
                let _ = s.record_hit(key_id, now);
                let prev = last_prune.load(Ordering::Relaxed);
                if now - prev >= PRUNE_INTERVAL_SECS
                    && last_prune
                        .compare_exchange(prev, now, Ordering::Relaxed, Ordering::Relaxed)
                        .is_ok()
                {
                    let _ = s.prune(now);
                }
            }
        })
    }

    /// `GET /api/apikeys` hook.
    pub fn list_hook(&self) -> ListApiKeysFn {
        let store = Arc::clone(&self.store);
        Arc::new(move || {
            let keys = {
                let s = store.lock().map_err(|_| "api-key store lock poisoned".to_string())?;
                s.list().map_err(|e| e.to_string())?
            };
            Ok(serde_json::json!({ "keys": keys }))
        })
    }

    /// `POST /api/apikeys` hook — returns the plaintext secret exactly once.
    pub fn create_hook(&self) -> CreateApiKeyFn {
        let store = Arc::clone(&self.store);
        Arc::new(move |name: &str, expires_at: Option<i64>| {
            let (view, secret) = {
                let s = store.lock().map_err(|_| "api-key store lock poisoned".to_string())?;
                s.create(name, expires_at).map_err(|e| e.to_string())?
            };
            Ok(serde_json::json!({ "key": view, "secret": secret }))
        })
    }

    /// `PATCH /api/apikeys/{id}` hook — rename / set-expiry / revoke.
    pub fn update_hook(&self) -> UpdateApiKeyFn {
        let store = Arc::clone(&self.store);
        Arc::new(move |id: i64, body: serde_json::Value| {
            let view = {
                let s = store.lock().map_err(|_| "api-key store lock poisoned".to_string())?;
                update_key(&s, id, &body)?
            };
            Ok(serde_json::json!({ "key": view }))
        })
    }

    /// `DELETE /api/apikeys/{id}` hook.
    pub fn delete_hook(&self) -> DeleteApiKeyFn {
        let store = Arc::clone(&self.store);
        Arc::new(move |id: i64| {
            let s = store.lock().map_err(|_| "api-key store lock poisoned".to_string())?;
            s.delete(id).map_err(|e| e.to_string())
        })
    }

    /// Direct access to the store for the CLI (`fono server keys …`).
    pub fn store(&self) -> Arc<Mutex<ApiKeyStore>> {
        Arc::clone(&self.store)
    }
}

/// Apply a `PATCH /api/apikeys/{id}` body (any of rename / set-expiry /
/// revoke) and return the updated key view. Kept separate so the mutex
/// guard in [`ApiKeyAuth::update_hook`] is released before the JSON is
/// serialised.
fn update_key(
    s: &ApiKeyStore,
    id: i64,
    body: &serde_json::Value,
) -> std::result::Result<fono_core::api_keys::ApiKeyView, String> {
    if let Some(name) = body.get("name").and_then(|v| v.as_str()) {
        s.rename(id, name).map_err(|e| e.to_string())?;
    }
    // `expires_at` present as a number sets it; present as null clears.
    if let Some(v) = body.get("expires_at") {
        if v.is_null() {
            s.set_expiry(id, None).map_err(|e| e.to_string())?;
        } else if let Some(ts) = v.as_i64() {
            s.set_expiry(id, Some(ts)).map_err(|e| e.to_string())?;
        }
    }
    match body.get("revoked").and_then(serde_json::Value::as_bool) {
        Some(true) => s.revoke(id).map_err(|e| e.to_string())?,
        Some(false) => s.unrevoke(id).map_err(|e| e.to_string())?,
        None => {}
    }
    s.get(id).map_err(|e| e.to_string())?.ok_or_else(|| "no such API key".to_string())
}

/// Migrate a legacy `[server.*].auth_token_ref` into a named key so
/// pre-existing LAN/Home-Assistant clients keep working across the
/// upgrade. Seeds `migrated-<server>` with the resolved token value,
/// leaves `auth = true`, and returns `true` if anything was migrated (the
/// caller then clears the ref and persists the config).
///
/// Note: because keys are hashed at rest we cannot store an *arbitrary*
/// pre-existing token verbatim and hand it back; instead we mint a fresh
/// key. The migration therefore emits the new secret to the daemon log
/// once so the operator can update their clients. A missing/unresolvable
/// ref migrates nothing.
pub fn migrate_legacy_token(
    auth: &ApiKeyAuth,
    server_label: &str,
    legacy_ref: &str,
    resolved: Option<String>,
) -> bool {
    if legacy_ref.is_empty() {
        return false;
    }
    if resolved.is_none() {
        tracing::warn!(
            "[server.{server_label}].auth_token_ref = {legacy_ref:?} could not be resolved; \
             no migration performed. Create a key with `fono server keys create`."
        );
        return false;
    }
    let name = format!("migrated-{server_label}");
    let Ok(s) = auth.store.lock() else {
        return false;
    };
    // Skip if a prior run already migrated this server.
    if let Ok(keys) = s.list() {
        if keys.iter().any(|k| k.name == name) {
            return false;
        }
    }
    match s.create(&name, None) {
        Ok((_view, secret)) => {
            tracing::warn!(
                "Migrated legacy [server.{server_label}].auth_token_ref to a new inbound API \
                 key named {name:?}. Update your clients to use this new key: {secret}"
            );
            true
        }
        Err(e) => {
            tracing::warn!("failed to migrate legacy token for server.{server_label}: {e}");
            false
        }
    }
}
