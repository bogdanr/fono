// SPDX-License-Identifier: GPL-3.0-only
//! Daemon side of the first-run welcome screen (`/api/onboarding/*` in the
//! web settings server).
//!
//! The page offers every free cloud provider, the local model, and
//! paste-anywhere for a key the user already has. This module does the
//! work behind it: tests saved and pasted keys, writes the key and the
//! config, runs OpenRouter's OAuth PKCE exchange, and watches the
//! clipboard for a new key while the user is on a provider's key page.
//!
//! Privacy rules for the clipboard watcher (ADR 0043): it runs only after
//! the user clicks Connect on a provider without OAuth, stops after
//! [`WATCH_FOR`] or when the page says so, accepts only a key with that
//! one provider's prefix, and never logs or keeps anything else it reads.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use fono_core::config::{LlmBackend, SttBackend};
use fono_core::onboarding::{self as onb, Pkce};
use fono_core::provider_catalog::{CloudProvider, SignIn};
use fono_core::{Config, Paths, Secrets};
use fono_net::web_settings::{OnboardingFn, OnboardingRequest};
use serde_json::{json, Value};
use tracing::{info, warn};

use crate::key_probe::{probe_key, KeyReachability};
use crate::session::SessionOrchestrator;
use crate::wake::WakeHandle;

/// How long an OAuth sign-in may take before its `state` is refused.
/// Matches the provider's own lifetime for an authorization code.
const OAUTH_TTL: Duration = Duration::from_secs(600);

/// How long the clipboard watcher runs after a Connect click.
const WATCH_FOR: Duration = Duration::from_secs(120);

/// Clipboard polling interval. Fast enough that the page turns green
/// as the user switches back to it.
const WATCH_EVERY: Duration = Duration::from_millis(500);

/// Set when the daemon should open the welcome screen once its settings
/// server is up: a first start from the app menu, where no terminal could
/// run the wizard.
static WELCOME_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Ask the daemon to open the welcome screen when it starts.
pub fn request_welcome() {
    WELCOME_REQUESTED.store(true, Ordering::SeqCst);
}

/// Whether the welcome screen was requested; clears the request.
pub fn take_welcome_request() -> bool {
    WELCOME_REQUESTED.swap(false, Ordering::SeqCst)
}

/// A sign-in started on the provider's consent page, waiting for its
/// callback.
struct OAuthSession {
    provider: &'static CloudProvider,
    verifier: String,
    started: Instant,
}

/// What the page shows under the card being connected.
#[derive(Clone)]
struct Progress {
    provider: &'static str,
    /// `waiting`, `testing`, `connected`, `failed` or `timeout`.
    phase: &'static str,
    message: String,
}

/// State of the local-model download started from the welcome screen.
#[derive(Clone, Copy)]
enum LocalPhase {
    Downloading,
    Ready,
    Failed,
}

#[derive(Default)]
struct Inner {
    oauth: HashMap<String, OAuthSession>,
    /// Bumped to stop the running watcher; each watcher exits once the
    /// value no longer matches the one it started with.
    watch_generation: u64,
    progress: Option<Progress>,
    /// Last test result per key, keyed by `(key_env, hash of the key)`, so
    /// the page can poll without hitting providers every half second.
    probes: HashMap<(&'static str, u64), KeyReachability>,
    local: Option<LocalPhase>,
}

/// Shared handle behind the onboarding hook.
#[derive(Clone)]
struct Welcome {
    paths: Paths,
    orchestrator: Option<Arc<SessionOrchestrator>>,
    wake: Option<WakeHandle>,
    inner: Arc<Mutex<Inner>>,
}

/// Build the `/api/onboarding/*` hook for the web settings server.
pub fn hook(
    paths: &Paths,
    orchestrator: Option<&Arc<SessionOrchestrator>>,
    wake: &WakeHandle,
) -> OnboardingFn {
    let welcome = Welcome {
        paths: paths.clone(),
        orchestrator: orchestrator.map(Arc::clone),
        wake: Some(wake.clone()),
        inner: Arc::default(),
    };
    Arc::new(move |request| {
        let welcome = welcome.clone();
        Box::pin(async move { welcome.handle(request).await })
    })
}

/// Every provider the screen can show: the free row, then the rest.
fn offered() -> Vec<&'static CloudProvider> {
    let mut all = onb::free_dictation_providers();
    all.extend(onb::other_providers());
    all
}

fn key_hash(key: &str) -> u64 {
    let mut h = DefaultHasher::new();
    key.hash(&mut h);
    h.finish()
}

/// Whether a test result means the key can be used. A rate limit or a
/// server error says nothing against the key itself.
fn usable(r: &KeyReachability) -> bool {
    matches!(r, KeyReachability::Valid | KeyReachability::Unexpected(_) | KeyReachability::NoProbe)
}

/// How the page labels a saved key.
fn key_status(r: &KeyReachability) -> &'static str {
    match r {
        KeyReachability::Valid => "works",
        KeyReachability::Rejected(_) => "rejected",
        _ => "unverified",
    }
}

/// The cloud provider dictation currently runs on, if any: the speech
/// provider, else the cleanup provider.
fn active_provider(config: &Config) -> Option<String> {
    if config.stt.backend != SttBackend::Local {
        if let Some(c) = &config.stt.cloud {
            return Some(c.provider.clone());
        }
    }
    let polish = config.polish.backend;
    (config.polish.enabled && polish.is_cloud())
        .then(|| fono_core::providers::llm_backend_str(&polish).to_string())
}

impl Welcome {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    async fn handle(&self, request: OnboardingRequest) -> Result<Value, String> {
        match request {
            OnboardingRequest::State => self.state().await,
            OnboardingRequest::Key(value) => self.paste(&value).await,
            OnboardingRequest::Connect { provider, callback_url } => {
                self.connect(&provider, &callback_url).await
            }
            OnboardingRequest::StopWatch | OnboardingRequest::Finish => {
                self.stop_watch();
                Ok(json!({}))
            }
            OnboardingRequest::UseLocal => self.use_local(),
            OnboardingRequest::OAuthCallback { code, state } => {
                self.oauth_callback(&code, &state).await
            }
        }
    }

    fn secrets(&self) -> Secrets {
        Secrets::load(&self.paths.secrets_file()).unwrap_or_default()
    }

    /// Test a key, reusing the last result for the same key.
    async fn probe(&self, key_env: &'static str, key: &str) -> KeyReachability {
        let slot = (key_env, key_hash(key));
        if let Some(r) = self.lock().probes.get(&slot) {
            return r.clone();
        }
        let r = probe_key(key_env, key).await;
        // Don't remember "couldn't reach it": the network may be back on
        // the next poll.
        if !matches!(r, KeyReachability::Unreachable(_)) {
            self.lock().probes.insert(slot, r.clone());
        }
        r
    }

    /// `GET /api/onboarding/state`.
    async fn state(&self) -> Result<Value, String> {
        let config = Config::load(&self.paths.config_file()).map_err(|e| format!("{e:#}"))?;
        let secrets = self.secrets();
        let providers = offered();
        let tests = futures::future::join_all(providers.iter().map(|p| {
            let found = secrets.resolve(p.key_env);
            async move {
                match found {
                    Some(k) => Some(self.probe(p.key_env, &k).await),
                    None => None,
                }
            }
        }))
        .await;
        let active = active_provider(&config);
        let free_ids: Vec<&str> = onb::free_dictation_providers().iter().map(|p| p.id).collect();
        let mut connected = None;
        let cards: Vec<Value> = providers
            .iter()
            .zip(&tests)
            .map(|(p, test)| {
                let is_active = active.as_deref() == Some(p.id);
                if is_active && test.as_ref().is_some_and(usable) {
                    connected = Some(p.id);
                }
                json!({
                    "id": p.id,
                    "name": p.display_name,
                    "line": p.onboarding.card_line,
                    "free": free_ids.contains(&p.id),
                    "oauth": matches!(p.onboarding.sign_in, SignIn::OAuthPkce { .. }),
                    "key": test.as_ref().map(key_status),
                    "key_in_env_only": test.is_some() && !secrets.has_in_file(p.key_env),
                    "active": is_active,
                })
            })
            .collect();
        let local_model = &config.stt.local.model;
        let local_mb = fono_stt::registry::ModelRegistry::get(local_model).map(|m| m.approx_mb);
        let (progress, local_phase) = {
            let inner = self.lock();
            let progress = inner
                .progress
                .as_ref()
                .map(|p| json!({ "provider": p.provider, "phase": p.phase, "message": p.message }));
            let local = inner.local;
            drop(inner);
            let local_phase = local.map(|l| match l {
                LocalPhase::Downloading => "downloading",
                LocalPhase::Ready => "ready",
                LocalPhase::Failed => "failed",
            });
            (progress, local_phase)
        };
        Ok(json!({
            "providers": cards,
            "recommended": onb::recommended_free_provider().map(|p| p.id),
            "connected": connected,
            "local": {
                "active": config.stt.backend == SttBackend::Local,
                "size_mb": local_mb,
                "phase": local_phase,
            },
            "progress": progress,
            "hotkey": config.hotkeys.dictation,
        }))
    }

    /// `POST /api/onboarding/key`: a key pasted anywhere on the page.
    async fn paste(&self, value: &str) -> Result<Value, String> {
        let Some(entry) = onb::detect_provider(value) else {
            return Err("This doesn't look like an API key from a service Fono knows. \
                        Copy the whole key and paste it again."
                .to_string());
        };
        let key = onb::key_candidate(value).unwrap_or_default().to_string();
        self.accept_key(entry, &key).await
    }

    /// Test `key`, then save it and switch to `entry`. The reply never
    /// carries the key.
    async fn accept_key(&self, entry: &'static CloudProvider, key: &str) -> Result<Value, String> {
        self.set_progress(entry.id, "testing", "Testing the key…");
        match self.probe(entry.key_env, key).await {
            KeyReachability::Rejected(_) => {
                let msg = format!(
                    "{} didn't accept this key. Create a new one and copy it again.",
                    entry.display_name
                );
                self.set_progress(entry.id, "failed", &msg);
                return Err(msg);
            }
            KeyReachability::Unreachable(_) => {
                let msg = format!(
                    "Couldn't reach {}. Check the internet connection and try again.",
                    entry.display_name
                );
                self.set_progress(entry.id, "failed", &msg);
                return Err(msg);
            }
            _ => {}
        }
        let mut secrets = self.secrets();
        secrets.insert(entry.key_env, key);
        secrets.save(&self.paths.secrets_file()).map_err(|e| format!("save key: {e:#}"))?;
        self.use_provider(entry).await?;
        info!("welcome: connected {} with a new key", entry.id);
        Ok(json!({ "provider": entry.id, "name": entry.display_name }))
    }

    /// Point the config at `entry` and reload the pipeline.
    async fn use_provider(&self, entry: &'static CloudProvider) -> Result<(), String> {
        let path = self.paths.config_file();
        let mut config = Config::load(&path).map_err(|e| format!("{e:#}"))?;
        crate::wizard::connect_provider(&mut config, entry);
        config.save(&path).map_err(|e| format!("save config: {e:#}"))?;
        self.stop_watch();
        let msg = format!("{} is connected.", entry.display_name);
        self.set_progress(entry.id, "connected", &msg);
        self.reload().await;
        Ok(())
    }

    async fn reload(&self) {
        if let Some(o) = &self.orchestrator {
            match o.reload().await {
                Ok(_) => self.wake.iter().for_each(WakeHandle::reload),
                Err(e) => warn!("welcome: reload after setup change failed: {e:#}"),
            }
        }
    }

    fn set_progress(&self, provider: &'static str, phase: &'static str, message: &str) {
        self.lock().progress = Some(Progress { provider, phase, message: message.to_string() });
    }

    /// `POST /api/onboarding/connect/{provider}`.
    async fn connect(&self, provider: &str, callback_url: &str) -> Result<Value, String> {
        let entry = offered()
            .into_iter()
            .find(|p| p.id == provider)
            .ok_or_else(|| "Fono doesn't know that service.".to_string())?;
        // A key that already works is reused, never replaced: reinstalling
        // must not leave a trail of new keys in the provider's console.
        if let Some(key) = self.secrets().resolve(entry.key_env) {
            if usable(&self.probe(entry.key_env, &key).await) {
                self.use_provider(entry).await?;
                return Ok(json!({ "mode": "connected", "provider": entry.id }));
            }
        }
        match entry.onboarding.sign_in {
            SignIn::OAuthPkce { auth_url, .. } => {
                let url = self.start_oauth(entry, auth_url, callback_url)?;
                Ok(json!({ "mode": "oauth", "open_url": url }))
            }
            SignIn::KeyPage => {
                self.start_watch(entry);
                Ok(json!({ "mode": "key_page", "open_url": entry.console_url }))
            }
        }
    }

    fn start_oauth(
        &self,
        entry: &'static CloudProvider,
        auth_url: &str,
        callback_url: &str,
    ) -> Result<String, String> {
        let pkce = Pkce::generate().map_err(|e| format!("no random source: {e}"))?;
        let state = onb::random_token().map_err(|e| format!("no random source: {e}"))?;
        let label = onb::key_label(&onb::host_name());
        let url = reqwest::Url::parse_with_params(
            auth_url,
            [
                ("callback_url", callback_url),
                ("code_challenge", &pkce.challenge),
                ("code_challenge_method", "S256"),
                ("state", &state),
                ("key_label", &label),
            ],
        )
        .map_err(|e| format!("sign-in address: {e}"))?;
        let mut inner = self.lock();
        inner.oauth.retain(|_, s| s.started.elapsed() < OAUTH_TTL);
        inner.oauth.insert(
            state,
            OAuthSession { provider: entry, verifier: pkce.verifier, started: Instant::now() },
        );
        inner.progress = Some(Progress {
            provider: entry.id,
            phase: "waiting",
            message: format!("Sign in to {} and click Authorize.", entry.display_name),
        });
        Ok(url.into())
    }

    /// `GET /oauth/callback`. The session is removed before anything else,
    /// so a `state` works once whatever the outcome.
    async fn oauth_callback(&self, code: &str, state: &str) -> Result<Value, String> {
        let session = self.lock().oauth.remove(state);
        let Some(session) = session.filter(|s| s.started.elapsed() < OAUTH_TTL) else {
            return Err("This sign-in link is old or was already used. \
                        Go back to Fono and click Connect again."
                .to_string());
        };
        let entry = session.provider;
        let SignIn::OAuthPkce { exchange_url, .. } = entry.onboarding.sign_in else {
            return Err("This service doesn't sign in this way.".to_string());
        };
        let key = exchange_code(exchange_url, code, &session.verifier).await.inspect_err(|e| {
            self.set_progress(entry.id, "failed", e);
        })?;
        self.accept_key(entry, &key).await
    }

    /// Watch the clipboard for `entry`'s key, replacing any running watch.
    fn start_watch(&self, entry: &'static CloudProvider) {
        let generation = {
            let mut inner = self.lock();
            inner.watch_generation += 1;
            inner.progress = Some(Progress {
                provider: entry.id,
                phase: "waiting",
                message: "Waiting for the key…".to_string(),
            });
            inner.watch_generation
        };
        let welcome = self.clone();
        tokio::spawn(async move {
            let started = Instant::now();
            while started.elapsed() < WATCH_FOR {
                tokio::time::sleep(WATCH_EVERY).await;
                if welcome.lock().watch_generation != generation {
                    return;
                }
                let text = tokio::task::spawn_blocking(fono_inject::read_clipboard_text)
                    .await
                    .ok()
                    .flatten();
                // Anything that isn't this provider's key is dropped here
                // unread and unlogged.
                let Some(key) = text.filter(|t| onb::is_key_for(t, entry)) else { continue };
                let key = onb::key_candidate(&key).unwrap_or_default().to_string();
                // A rejected key leaves the watch running: the user may
                // copy a fresh one.
                if welcome.accept_key(entry, &key).await.is_ok() {
                    return;
                }
            }
            let mut inner = welcome.lock();
            if inner.watch_generation == generation {
                inner.progress = Some(Progress {
                    provider: entry.id,
                    phase: "timeout",
                    message: "Stopped watching the clipboard. Paste the key on this page instead."
                        .to_string(),
                });
            }
        });
    }

    fn stop_watch(&self) {
        self.lock().watch_generation += 1;
    }

    /// `POST /api/onboarding/local`: run dictation on this computer. Keys
    /// stay saved, so switching back to the cloud later needs no new key.
    fn use_local(&self) -> Result<Value, String> {
        let path = self.paths.config_file();
        let mut config = Config::load(&path).map_err(|e| format!("{e:#}"))?;
        config.stt.backend = SttBackend::Local;
        if config.polish.enabled {
            config.polish.backend = LlmBackend::Local;
        }
        config.save(&path).map_err(|e| format!("save config: {e:#}"))?;
        self.stop_watch();
        self.lock().local = Some(LocalPhase::Downloading);
        let welcome = self.clone();
        tokio::spawn(async move {
            let ok = crate::models::ensure_models(&welcome.paths, &config).await.is_ok();
            welcome.lock().local = Some(if ok { LocalPhase::Ready } else { LocalPhase::Failed });
            if ok {
                welcome.reload().await;
            }
        });
        info!("welcome: switched dictation to the local model");
        Ok(json!({ "mode": "local" }))
    }
}

/// Trade an OAuth authorization code for an API key.
async fn exchange_code(exchange_url: &str, code: &str, verifier: &str) -> Result<String, String> {
    let fail = "The sign-in didn't finish. Go back to Fono and click Connect again.";
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("{fail} ({e})"))?;
    let resp = client
        .post(exchange_url)
        .json(&json!({
            "code": code,
            "code_verifier": verifier,
            "code_challenge_method": "S256",
        }))
        .send()
        .await
        .map_err(|e| format!("{fail} ({e})"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("{fail} (HTTP {})", status.as_u16()));
    }
    let body: Value = resp.json().await.map_err(|e| format!("{fail} ({e})"))?;
    body.get("key")
        .and_then(Value::as_str)
        .filter(|k| !k.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| fail.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fono_core::provider_catalog;

    fn welcome() -> (tempfile::TempDir, Welcome) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        let w = Welcome { paths, orchestrator: None, wake: None, inner: Arc::default() };
        (dir, w)
    }

    fn openrouter() -> &'static CloudProvider {
        provider_catalog::find("openrouter").unwrap()
    }

    #[test]
    fn oauth_url_carries_pkce_state_and_label() {
        let (_d, w) = welcome();
        let SignIn::OAuthPkce { auth_url, .. } = openrouter().onboarding.sign_in else {
            panic!("openrouter signs in with OAuth");
        };
        let url =
            w.start_oauth(openrouter(), auth_url, "http://localhost:7/oauth/callback").unwrap();
        let url = reqwest::Url::parse(&url).unwrap();
        let q: HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(q["callback_url"], "http://localhost:7/oauth/callback");
        assert_eq!(q["code_challenge_method"], "S256");
        assert!(q["key_label"].starts_with("fono"));
        let verifier = w.lock().oauth[&q["state"]].verifier.clone();
        assert_eq!(Pkce::from_verifier(verifier).challenge, q["code_challenge"]);
    }

    #[tokio::test]
    async fn unknown_state_is_refused() {
        let (_d, w) = welcome();
        assert!(w.oauth_callback("code", "made-up").await.is_err());
    }

    #[tokio::test]
    async fn state_is_single_use_and_expires() {
        let (_d, w) = welcome();
        let SignIn::OAuthPkce { auth_url, .. } = openrouter().onboarding.sign_in else {
            unreachable!()
        };
        w.start_oauth(openrouter(), auth_url, "http://localhost:7/oauth/callback").unwrap();
        let state = w.lock().oauth.keys().next().unwrap().clone();
        // Age the session past its lifetime: refused, and gone afterwards.
        // Windows counts `Instant` from boot, so a runner up for less than
        // the lifetime can't go that far back; there is nothing to test then.
        let Some(aged) = Instant::now().checked_sub(OAUTH_TTL) else { return };
        w.lock().oauth.get_mut(&state).unwrap().started = aged;
        assert!(w.oauth_callback("code", &state).await.is_err());
        assert!(w.lock().oauth.is_empty(), "a used state must not be accepted again");
    }

    #[tokio::test]
    async fn pasted_garbage_is_refused_without_saving() {
        let (_d, w) = welcome();
        let err = w.paste("hello world").await.unwrap_err();
        assert!(err.contains("doesn't look like an API key"));
        assert!(!w.paths.secrets_file().exists());
    }

    #[test]
    fn active_provider_prefers_speech() {
        let mut c = Config::default();
        c.polish.enabled = true;
        c.polish.backend = LlmBackend::Local;
        c.stt.backend = SttBackend::Local;
        assert_eq!(active_provider(&c), None);
        crate::wizard::connect_provider(&mut c, provider_catalog::find("groq").unwrap());
        assert_eq!(active_provider(&c).as_deref(), Some("groq"));
    }

    #[test]
    fn connecting_keeps_what_the_provider_does_not_cover() {
        let mut c = Config::default();
        c.stt.backend = SttBackend::Local;
        c.stt.local.model = "small".into();
        let assistant_before = c.assistant.enabled;
        crate::wizard::connect_provider(&mut c, provider_catalog::find("cerebras").unwrap());
        assert_eq!(c.stt.backend, SttBackend::Local, "Cerebras has no speech-to-text");
        assert_eq!(c.stt.local.model, "small");
        assert_eq!(c.assistant.enabled, assistant_before, "the assistant stays opt-in");
    }

    #[test]
    fn a_conversation_agent_server_takes_the_first_key_for_its_assistant() {
        let groq = provider_catalog::find("groq").unwrap();
        let mut c = Config::default();
        c.server.wyoming.enabled = true;
        crate::wizard::connect_provider(&mut c, groq);
        assert!(c.assistant.enabled);
        assert_eq!(c.assistant.backend, LlmBackend::Groq);

        // A second key does not take over an assistant already set up.
        crate::wizard::connect_provider(&mut c, provider_catalog::find("cerebras").unwrap());
        assert_eq!(c.assistant.backend, LlmBackend::Groq);

        // Without the conversation service, the assistant stays opt-in.
        let mut c = Config::default();
        c.server.wyoming.enabled = true;
        c.server.wyoming.services = vec![fono_core::config::WyomingService::Asr];
        crate::wizard::connect_provider(&mut c, groq);
        assert!(!c.assistant.enabled);
    }
}
