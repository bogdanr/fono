// SPDX-License-Identifier: GPL-3.0-only
//! Inbound API-key store for Fono's HTTP servers (the OpenAI/Ollama LLM
//! surface plus the `/v1/audio/transcriptions` STT and `/v1/audio/speech`
//! TTS routes served by `fono-net`'s `llm_server`, and the web settings
//! UI).
//!
//! This is deliberately **separate** from [`crate::secrets::Secrets`]
//! (which holds *outbound* provider keys such as `GROQ_API_KEY`). Here we
//! store the keys clients present *to* Fono, as named entries in a
//! dedicated `api_keys.sqlite` — never in `config.toml` and never in
//! `secrets.toml`.
//!
//! Security model:
//! - Secrets are hashed at rest with SHA-256; the plaintext is shown
//!   exactly **once** at creation and is otherwise unrecoverable.
//! - Verification uses a constant-time digest comparison.
//! - The DB file is clamped to owner-only `0600`, like `history.sqlite`.
//!
//! Usage tracking is **bounded**: instead of one row per request (an
//! unbounded access log), we keep pre-aggregated per-interval counters
//! (`day` and `month` buckets) plus a single debounced `last_used_at`
//! per key. A retention prune keeps the bucket count independent of
//! request volume.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// Prefix on every Fono-issued inbound key, self-identifying so it is
/// never confused with an outbound provider key (e.g. `gsk_…` / `sk-…`).
pub const TOKEN_PREFIX: &str = "fono_sk_";

/// Base62 alphabet for the random body of a token.
const BASE62: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// Length of the random base62 body (≈ 256 bits of entropy).
const BODY_LEN: usize = 43;

/// Only write a fresh `last_used_at` if the previous one is at least this
/// many seconds stale. Keeps the auth hot path from rewriting the row on
/// every single request.
const LAST_USED_DEBOUNCE_SECS: i64 = 60;

/// Retention windows for the bounded usage buckets.
const DAY_BUCKETS_KEPT: i64 = 62;
const MONTH_BUCKETS_KEPT: i64 = 13;

fn now_unix() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Metadata view of a stored key. **Never** carries the secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApiKeyView {
    pub id: i64,
    pub name: String,
    /// Display form: `fono_sk_…<last4>`.
    pub masked: String,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub last_used_at: Option<i64>,
    pub revoked: bool,
    /// Requests counted in the current UTC day bucket.
    pub usage_day: i64,
    /// Requests counted in the current UTC month bucket.
    pub usage_month: i64,
}

/// SQLite-backed store of inbound API keys and their bounded usage counters.
pub struct ApiKeyStore {
    conn: Connection,
}

impl ApiKeyStore {
    /// Open (or create) the store at `path` and apply migrations. The DB
    /// file is clamped to owner-only `0600` on Unix.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|source| Error::Io { path: dir.to_path_buf(), source })?;
        }
        let conn = Connection::open(path)?;
        restrict_to_owner(path);
        // Tolerate brief cross-connection contention (CLI + daemon) under WAL.
        let _ = conn.busy_timeout(std::time::Duration::from_secs(5));
        let db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    /// Open an in-memory store (tests).
    pub fn open_in_memory() -> Result<Self> {
        let db = Self { conn: Connection::open_in_memory()? };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            "
            PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;

            CREATE TABLE IF NOT EXISTS api_keys(
                id           INTEGER PRIMARY KEY,
                name         TEXT NOT NULL UNIQUE,
                hash         BLOB NOT NULL,
                prefix       TEXT NOT NULL,
                last4        TEXT NOT NULL,
                created_at   INTEGER NOT NULL,
                expires_at   INTEGER,
                last_used_at INTEGER,
                revoked      INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS api_key_usage(
                key_id       INTEGER NOT NULL,
                bucket_kind  TEXT NOT NULL,
                bucket_start INTEGER NOT NULL,
                count        INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (key_id, bucket_kind, bucket_start),
                FOREIGN KEY (key_id) REFERENCES api_keys(id) ON DELETE CASCADE
            );
            ",
        )?;
        Ok(())
    }

    /// Create a new named key. Returns the metadata view **and** the
    /// plaintext token, which the caller must surface exactly once — it
    /// cannot be recovered afterwards.
    pub fn create(&self, name: &str, expires_at: Option<i64>) -> Result<(ApiKeyView, String)> {
        let name = name.trim();
        if name.is_empty() {
            return Err(Error::Other("API key name must not be empty".into()));
        }
        let token = generate_token();
        let hash = sha256(&token);
        let last4: String =
            token.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
        let created_at = now_unix();
        self.conn
            .execute(
                "INSERT INTO api_keys (name, hash, prefix, last4, created_at, expires_at, revoked)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)",
                params![name, hash, TOKEN_PREFIX, last4, created_at, expires_at],
            )
            .map_err(map_unique(name))?;
        let id = self.conn.last_insert_rowid();
        let view = self.get(id)?.ok_or_else(|| Error::Other("row vanished after insert".into()))?;
        Ok((view, token))
    }

    /// All keys, newest first. Metadata only.
    pub fn list(&self) -> Result<Vec<ApiKeyView>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, prefix, last4, created_at, expires_at, last_used_at, revoked
             FROM api_keys ORDER BY created_at DESC, id DESC",
        )?;
        let ids_meta = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, Option<i64>>(5)?,
                    r.get::<_, Option<i64>>(6)?,
                    r.get::<_, i64>(7)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let now = now_unix();
        let mut out = Vec::with_capacity(ids_meta.len());
        for (id, name, prefix, last4, created_at, expires_at, last_used_at, revoked) in ids_meta {
            let (usage_day, usage_month) = self.usage_at(id, now)?;
            out.push(ApiKeyView {
                id,
                name,
                masked: mask(&prefix, &last4),
                created_at,
                expires_at,
                last_used_at,
                revoked: revoked != 0,
                usage_day,
                usage_month,
            });
        }
        Ok(out)
    }

    /// Fetch one key by id.
    pub fn get(&self, id: i64) -> Result<Option<ApiKeyView>> {
        let row = self
            .conn
            .query_row(
                "SELECT id, name, prefix, last4, created_at, expires_at, last_used_at, revoked
                 FROM api_keys WHERE id = ?1",
                params![id],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, Option<i64>>(5)?,
                        r.get::<_, Option<i64>>(6)?,
                        r.get::<_, i64>(7)?,
                    ))
                },
            )
            .optional()?;
        let Some((id, name, prefix, last4, created_at, expires_at, last_used_at, revoked)) = row
        else {
            return Ok(None);
        };
        let (usage_day, usage_month) = self.usage_at(id, now_unix())?;
        Ok(Some(ApiKeyView {
            id,
            name,
            masked: mask(&prefix, &last4),
            created_at,
            expires_at,
            last_used_at,
            revoked: revoked != 0,
            usage_day,
            usage_month,
        }))
    }

    /// Rename a key. Fails if the new name collides.
    pub fn rename(&self, id: i64, new_name: &str) -> Result<()> {
        let new_name = new_name.trim();
        if new_name.is_empty() {
            return Err(Error::Other("API key name must not be empty".into()));
        }
        let n = self
            .conn
            .execute("UPDATE api_keys SET name = ?2 WHERE id = ?1", params![id, new_name])
            .map_err(map_unique(new_name))?;
        if n == 0 {
            return Err(Error::Other(format!("no API key with id {id}")));
        }
        Ok(())
    }

    /// Set (or clear, with `None`) the expiry timestamp.
    pub fn set_expiry(&self, id: i64, expires_at: Option<i64>) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE api_keys SET expires_at = ?2 WHERE id = ?1",
            params![id, expires_at],
        )?;
        if n == 0 {
            return Err(Error::Other(format!("no API key with id {id}")));
        }
        Ok(())
    }

    /// Soft-revoke: the key stays in the table (so its usage history and
    /// masked form remain visible) but [`Self::verify`] rejects it.
    pub fn revoke(&self, id: i64) -> Result<()> {
        let n = self.conn.execute("UPDATE api_keys SET revoked = 1 WHERE id = ?1", params![id])?;
        if n == 0 {
            return Err(Error::Other(format!("no API key with id {id}")));
        }
        Ok(())
    }

    /// Reverse a soft-revoke so the key authenticates again. (An expired
    /// key stays rejected until its expiry is also changed.)
    pub fn unrevoke(&self, id: i64) -> Result<()> {
        let n = self.conn.execute("UPDATE api_keys SET revoked = 0 WHERE id = ?1", params![id])?;
        if n == 0 {
            return Err(Error::Other(format!("no API key with id {id}")));
        }
        Ok(())
    }

    /// Permanently delete a key and its usage buckets.
    pub fn delete(&self, id: i64) -> Result<()> {
        // ON DELETE CASCADE clears api_key_usage.
        let n = self.conn.execute("DELETE FROM api_keys WHERE id = ?1", params![id])?;
        if n == 0 {
            return Err(Error::Other(format!("no API key with id {id}")));
        }
        Ok(())
    }

    /// Number of keys that are neither revoked nor expired at `now`.
    pub fn active_count(&self) -> Result<i64> {
        let now = now_unix();
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM api_keys
             WHERE revoked = 0 AND (expires_at IS NULL OR expires_at > ?1)",
            params![now],
            |r| r.get::<_, i64>(0),
        )?)
    }

    /// Verify a presented bearer token. Returns the matching key id if the
    /// token is valid and the key is neither revoked nor expired.
    ///
    /// The digest comparison is constant-time; we fold over every
    /// candidate so timing does not reveal which (if any) key matched.
    pub fn verify(&self, presented: &str) -> Result<Option<i64>> {
        let presented_hash = sha256(presented);
        let now = now_unix();
        let mut stmt = self.conn.prepare("SELECT id, hash, expires_at, revoked FROM api_keys")?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Vec<u8>>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut matched: Option<i64> = None;
        for (id, hash, expires_at, revoked) in rows {
            let digest_ok = constant_time_eq(&hash, &presented_hash);
            let usable = revoked == 0 && expires_at.map(|e| e > now).unwrap_or(true);
            if digest_ok && usable {
                matched = Some(id);
            }
        }
        Ok(matched)
    }

    /// Record one authenticated request against `key_id`: bump the current
    /// day and month buckets and, if sufficiently stale, refresh
    /// `last_used_at`. Bounded — never inserts a per-request row.
    pub fn record_hit(&self, key_id: i64, now: i64) -> Result<()> {
        let day_start = day_bucket(now);
        let month_start = month_bucket(now);
        for (kind, start) in [("day", day_start), ("month", month_start)] {
            self.conn.execute(
                "INSERT INTO api_key_usage (key_id, bucket_kind, bucket_start, count)
                 VALUES (?1, ?2, ?3, 1)
                 ON CONFLICT(key_id, bucket_kind, bucket_start)
                 DO UPDATE SET count = count + 1",
                params![key_id, kind, start],
            )?;
        }
        // Debounced last_used update: only write when meaningfully stale.
        self.conn.execute(
            "UPDATE api_keys SET last_used_at = ?2
             WHERE id = ?1 AND (last_used_at IS NULL OR last_used_at < ?2 - ?3)",
            params![key_id, now, LAST_USED_DEBOUNCE_SECS],
        )?;
        Ok(())
    }

    /// Trim usage buckets older than the retention windows. Idempotent;
    /// call periodically. Keeps total rows ≤ keys × (62 + 13).
    pub fn prune(&self, now: i64) -> Result<usize> {
        let day_cutoff = day_bucket(now) - DAY_BUCKETS_KEPT * 86_400;
        let month_cutoff = month_bucket(sub_months(now, MONTH_BUCKETS_KEPT));
        let mut n = self.conn.execute(
            "DELETE FROM api_key_usage WHERE bucket_kind = 'day' AND bucket_start < ?1",
            params![day_cutoff],
        )?;
        n += self.conn.execute(
            "DELETE FROM api_key_usage WHERE bucket_kind = 'month' AND bucket_start < ?1",
            params![month_cutoff],
        )?;
        Ok(n)
    }

    /// Current (day, month) usage counts for a key at `now`.
    pub fn usage(&self, key_id: i64) -> Result<(i64, i64)> {
        self.usage_at(key_id, now_unix())
    }

    fn usage_at(&self, key_id: i64, now: i64) -> Result<(i64, i64)> {
        let day = self.bucket_count(key_id, "day", day_bucket(now))?;
        let month = self.bucket_count(key_id, "month", month_bucket(now))?;
        Ok((day, month))
    }

    fn bucket_count(&self, key_id: i64, kind: &str, start: i64) -> Result<i64> {
        Ok(self
            .conn
            .query_row(
                "SELECT count FROM api_key_usage
                 WHERE key_id = ?1 AND bucket_kind = ?2 AND bucket_start = ?3",
                params![key_id, kind, start],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0))
    }

    /// Total number of usage-bucket rows (for tests / diagnostics).
    pub fn usage_row_count(&self) -> Result<i64> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM api_key_usage", [], |r| r.get::<_, i64>(0))?)
    }
}

/// `fono_sk_…<last4>`.
fn mask(prefix: &str, last4: &str) -> String {
    format!("{prefix}\u{2026}{last4}")
}

fn map_unique(name: &str) -> impl Fn(rusqlite::Error) -> Error + '_ {
    move |e| match e {
        rusqlite::Error::SqliteFailure(err, _)
            if err.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            Error::Other(format!("an API key named '{name}' already exists"))
        }
        other => Error::from(other),
    }
}

/// Generate a fresh `fono_sk_<base62>` token using the OS CSPRNG.
fn generate_token() -> String {
    let mut raw = [0u8; BODY_LEN];
    // getrandom draws from the OS CSPRNG; failure here means the platform
    // RNG is unavailable, which is unrecoverable for secret generation.
    getrandom::getrandom(&mut raw).expect("OS CSPRNG unavailable");
    let mut body = String::with_capacity(BODY_LEN);
    for b in raw {
        body.push(BASE62[(b as usize) % BASE62.len()] as char);
    }
    format!("{TOKEN_PREFIX}{body}")
}

fn sha256(s: &str) -> Vec<u8> {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    h.finalize().to_vec()
}

/// Constant-time byte-slice equality (no early return on mismatch).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn day_bucket(now: i64) -> i64 {
    now.div_euclid(86_400) * 86_400
}

/// Start-of-month (UTC) unix timestamp for `now`.
fn month_bucket(now: i64) -> i64 {
    let days = now.div_euclid(86_400);
    let (y, m, _d) = civil_from_days(days);
    days_from_civil(y, m, 1) * 86_400
}

/// Roughly subtract `months` from `now`, landing on the same-ish day. Used
/// only to compute a prune cutoff, so exact day-of-month is irrelevant.
fn sub_months(now: i64, months: i64) -> i64 {
    let days = now.div_euclid(86_400);
    let (mut y, mut m, d) = civil_from_days(days);
    let total = (y * 12 + (m as i64 - 1)) - months;
    y = total.div_euclid(12);
    m = (total.rem_euclid(12) + 1) as u32;
    days_from_civil(y, m, d.min(28)) * 86_400
}

// Howard Hinnant's public-domain civil<->days algorithms (days since the
// Unix epoch, 1970-01-01). Dependency-free calendar math for the month
// bucket boundaries.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) as i64 + 2) / 5 + (d as i64 - 1);
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Best-effort clamp to owner-only `0600` (main DB + WAL/SHM sidecars).
fn restrict_to_owner(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::Permissions::from_mode(0o600);
        let _ = std::fs::set_permissions(path, mode.clone());
        for suffix in ["-wal", "-shm"] {
            let mut os = path.as_os_str().to_owned();
            os.push(suffix);
            let sidecar = std::path::PathBuf::from(os);
            if sidecar.exists() {
                let _ = std::fs::set_permissions(&sidecar, mode.clone());
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_list_and_mask() {
        let db = ApiKeyStore::open_in_memory().unwrap();
        let (view, token) = db.create("HomeAssistant", None).unwrap();
        assert!(token.starts_with(TOKEN_PREFIX));
        assert_eq!(token.len(), TOKEN_PREFIX.len() + BODY_LEN);
        assert!(view.masked.starts_with(TOKEN_PREFIX));
        assert!(view.masked.ends_with(&token[token.len() - 4..]));
        let all = db.list().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "HomeAssistant");
    }

    #[test]
    fn duplicate_name_rejected() {
        let db = ApiKeyStore::open_in_memory().unwrap();
        db.create("dup", None).unwrap();
        assert!(db.create("dup", None).is_err());
    }

    #[test]
    fn verify_matches_only_correct_token() {
        let db = ApiKeyStore::open_in_memory().unwrap();
        let (view, token) = db.create("k", None).unwrap();
        assert_eq!(db.verify(&token).unwrap(), Some(view.id));
        assert_eq!(db.verify("fono_sk_wrongwrongwrong").unwrap(), None);
    }

    #[test]
    fn hash_stored_not_plaintext() {
        let db = ApiKeyStore::open_in_memory().unwrap();
        let (_v, token) = db.create("k", None).unwrap();
        let stored: Vec<u8> =
            db.conn.query_row("SELECT hash FROM api_keys", [], |r| r.get(0)).unwrap();
        assert_ne!(stored, token.as_bytes());
        assert_eq!(stored.len(), 32, "SHA-256 digest is 32 bytes");
        // The plaintext must not appear anywhere in the row.
        let name: String =
            db.conn.query_row("SELECT name FROM api_keys", [], |r| r.get(0)).unwrap();
        assert!(!token.contains(&name) || name == "k");
    }

    #[test]
    fn revoked_key_rejected() {
        let db = ApiKeyStore::open_in_memory().unwrap();
        let (view, token) = db.create("k", None).unwrap();
        db.revoke(view.id).unwrap();
        assert_eq!(db.verify(&token).unwrap(), None);
        assert_eq!(db.active_count().unwrap(), 0);
    }

    #[test]
    fn unrevoke_restores_key() {
        let db = ApiKeyStore::open_in_memory().unwrap();
        let (view, token) = db.create("k", None).unwrap();
        db.revoke(view.id).unwrap();
        assert_eq!(db.verify(&token).unwrap(), None);
        db.unrevoke(view.id).unwrap();
        assert_eq!(db.verify(&token).unwrap(), Some(view.id));
        assert_eq!(db.active_count().unwrap(), 1);
    }

    #[test]
    fn expired_key_rejected() {
        let db = ApiKeyStore::open_in_memory().unwrap();
        let past = now_unix() - 10;
        let (view, token) = db.create("k", Some(past)).unwrap();
        assert_eq!(db.verify(&token).unwrap(), None);
        // Not counted as active.
        assert_eq!(db.active_count().unwrap(), 0);
        // But still listed (with its metadata).
        assert_eq!(db.get(view.id).unwrap().unwrap().expires_at, Some(past));
    }

    #[test]
    fn rename_and_set_expiry() {
        let db = ApiKeyStore::open_in_memory().unwrap();
        let (view, _t) = db.create("old", None).unwrap();
        db.rename(view.id, "new").unwrap();
        db.set_expiry(view.id, Some(9_999_999_999)).unwrap();
        let v = db.get(view.id).unwrap().unwrap();
        assert_eq!(v.name, "new");
        assert_eq!(v.expires_at, Some(9_999_999_999));
    }

    #[test]
    fn delete_removes_key_and_usage() {
        let db = ApiKeyStore::open_in_memory().unwrap();
        let (view, _t) = db.create("k", None).unwrap();
        db.record_hit(view.id, now_unix()).unwrap();
        assert!(db.usage_row_count().unwrap() > 0);
        db.delete(view.id).unwrap();
        assert_eq!(db.list().unwrap().len(), 0);
        assert_eq!(db.usage_row_count().unwrap(), 0, "cascade clears usage");
    }

    #[test]
    fn usage_counters_increment() {
        let db = ApiKeyStore::open_in_memory().unwrap();
        let (view, _t) = db.create("k", None).unwrap();
        let now = now_unix();
        for _ in 0..5 {
            db.record_hit(view.id, now).unwrap();
        }
        let (day, month) = db.usage(view.id).unwrap();
        assert_eq!(day, 5);
        assert_eq!(month, 5);
    }

    #[test]
    fn usage_stays_bounded_across_many_days() {
        let db = ApiKeyStore::open_in_memory().unwrap();
        let (view, _t) = db.create("k", None).unwrap();
        // Simulate 400 days, 50 requests each — a large request volume.
        let base = day_bucket(now_unix());
        for day in 0..400i64 {
            let ts = base - day * 86_400;
            for _ in 0..50 {
                db.record_hit(view.id, ts).unwrap();
            }
            db.prune(base).unwrap();
        }
        let rows = db.usage_row_count().unwrap();
        // Bound: (62 day + 13 month) buckets per key, generously.
        assert!(
            rows <= (DAY_BUCKETS_KEPT + MONTH_BUCKETS_KEPT) + 5,
            "rows={rows} must stay bounded"
        );
    }

    #[test]
    fn last_used_debounced() {
        let db = ApiKeyStore::open_in_memory().unwrap();
        let (view, _t) = db.create("k", None).unwrap();
        let now = now_unix();
        db.record_hit(view.id, now).unwrap();
        let first = db.get(view.id).unwrap().unwrap().last_used_at.unwrap();
        // A second hit one second later must not move last_used (debounced).
        db.record_hit(view.id, now + 1).unwrap();
        let second = db.get(view.id).unwrap().unwrap().last_used_at.unwrap();
        assert_eq!(first, second, "debounce should suppress the update");
        // A hit well past the debounce window does move it.
        db.record_hit(view.id, now + LAST_USED_DEBOUNCE_SECS + 5).unwrap();
        let third = db.get(view.id).unwrap().unwrap().last_used_at.unwrap();
        assert!(third > second);
    }

    #[test]
    fn constant_time_eq_basic() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }

    #[test]
    fn month_bucket_is_first_of_month() {
        // 2026-07-17 -> bucket should be 2026-07-01 00:00:00 UTC.
        let ts = days_from_civil(2026, 7, 17) * 86_400 + 12 * 3600;
        let b = month_bucket(ts);
        assert_eq!(b, days_from_civil(2026, 7, 1) * 86_400);
        let (y, m, d) = civil_from_days(b / 86_400);
        assert_eq!((y, m, d), (2026, 7, 1));
    }
}
