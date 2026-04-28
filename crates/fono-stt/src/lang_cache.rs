// SPDX-License-Identifier: GPL-3.0-only
//! In-memory per-backend language cache.
//!
//! Plan v3 (`plans/2026-04-28-multi-language-stt-no-primary-v3.md`)
//! task 2. The cache records the most recently observed
//! correctly-detected language code per cloud STT backend and is
//! consulted **only as a rerun target** when post-validation fires.
//! No persistence, no file I/O — daemon restarts rebuild within one
//! or two utterances. OS-locale bootstrap (task 3) seeds the cache at
//! daemon start when the detected locale is in the configured
//! allow-list.
//!
//! Keyed by `&'static str` (the backend's `name()`). One
//! `Arc<LanguageCache>` lives in the daemon and is cloned into each
//! backend; tests construct their own via `LanguageCache::new()`.
//!
//! Order of `general.languages` is **not** consulted here. The cache
//! reflects what the user actually spoke last, not config order.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

#[derive(Debug, Default)]
pub struct LanguageCache {
    inner: RwLock<HashMap<&'static str, String>>,
}

impl LanguageCache {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
        }
    }

    /// Process-wide singleton. Cloud STT factories use this so batch
    /// + streaming variants of the same provider share state.
    pub fn global() -> Arc<Self> {
        static GLOBAL: OnceLock<Arc<LanguageCache>> = OnceLock::new();
        Arc::clone(GLOBAL.get_or_init(|| Arc::new(Self::new())))
    }

    /// Most recent correctly-detected code for `backend`, or `None`
    /// when the cache is empty for that key.
    #[must_use]
    pub fn get(&self, backend: &'static str) -> Option<String> {
        self.inner.read().ok()?.get(backend).cloned()
    }

    /// Record an observed in-allow-list detection. Codes are
    /// normalised (trim + lowercase). Empty strings are ignored.
    pub fn record(&self, backend: &'static str, code: impl Into<String>) {
        let code = code.into();
        let normalised = code.trim().to_ascii_lowercase();
        if normalised.is_empty() {
            return;
        }
        if let Ok(mut g) = self.inner.write() {
            g.insert(backend, normalised);
        }
    }

    /// Bootstrap: set the cache value only if no entry exists yet for
    /// `backend`. Used by the OS-locale seeding path at daemon start.
    pub fn seed_if_empty(&self, backend: &'static str, code: impl Into<String>) {
        let code = code.into();
        let normalised = code.trim().to_ascii_lowercase();
        if normalised.is_empty() {
            return;
        }
        if let Ok(mut g) = self.inner.write() {
            g.entry(backend).or_insert(normalised);
        }
    }

    /// Drop every entry. Wired to the tray "Clear language memory"
    /// item.
    pub fn clear(&self) {
        if let Ok(mut g) = self.inner.write() {
            g.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_returns_none_when_empty() {
        let c = LanguageCache::new();
        assert_eq!(c.get("groq"), None);
    }

    #[test]
    fn record_then_get_round_trip_normalises() {
        let c = LanguageCache::new();
        c.record("groq", "  EN  ");
        assert_eq!(c.get("groq"), Some("en".into()));
    }

    #[test]
    fn record_overwrites_existing() {
        let c = LanguageCache::new();
        c.record("groq", "en");
        c.record("groq", "ro");
        assert_eq!(c.get("groq"), Some("ro".into()));
    }

    #[test]
    fn seed_if_empty_is_a_noop_when_populated() {
        let c = LanguageCache::new();
        c.record("groq", "en");
        c.seed_if_empty("groq", "ro");
        assert_eq!(c.get("groq"), Some("en".into()));
    }

    #[test]
    fn seed_if_empty_writes_into_empty_slot() {
        let c = LanguageCache::new();
        c.seed_if_empty("groq", "ro");
        assert_eq!(c.get("groq"), Some("ro".into()));
    }

    #[test]
    fn clear_drops_everything() {
        let c = LanguageCache::new();
        c.record("groq", "en");
        c.record("openai", "ro");
        c.clear();
        assert_eq!(c.get("groq"), None);
        assert_eq!(c.get("openai"), None);
    }

    #[test]
    fn empty_codes_are_ignored() {
        let c = LanguageCache::new();
        c.record("groq", "");
        c.record("groq", "   ");
        assert_eq!(c.get("groq"), None);
    }

    #[test]
    fn keys_are_per_backend() {
        let c = LanguageCache::new();
        c.record("groq", "en");
        c.record("openai", "ro");
        assert_eq!(c.get("groq"), Some("en".into()));
        assert_eq!(c.get("openai"), Some("ro".into()));
    }
}
