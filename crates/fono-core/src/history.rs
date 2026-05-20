// SPDX-License-Identifier: GPL-3.0-only
//! SQLite-backed transcription history with FTS5 search, retention cleanup,
//! and optional secret redaction. Schema matches Phase 1 Task 1.4.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use once_cell::sync::Lazy;
use regex::Regex;
use rusqlite::{params, Connection, OptionalExtension};

use crate::error::Result;

/// 20+ char alphanumeric/underscore/dash blobs — typical API-key shape.
static SECRET_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"[A-Za-z0-9_-]{20,}").expect("static regex"));

/// A transcription record as written to `history.sqlite`.
#[derive(Debug, Clone)]
pub struct Transcription {
    pub id: Option<i64>,
    pub ts: i64,
    pub duration_ms: Option<i64>,
    pub raw: String,
    pub cleaned: Option<String>,
    pub app_class: Option<String>,
    pub app_title: Option<String>,
    pub stt_backend: Option<String>,
    pub polish_backend: Option<String>,
    pub language: Option<String>,
}

impl Transcription {
    #[must_use]
    pub fn new(raw: impl Into<String>) -> Self {
        Self {
            id: None,
            ts: now_unix(),
            duration_ms: None,
            raw: raw.into(),
            cleaned: None,
            app_class: None,
            app_title: None,
            stt_backend: None,
            polish_backend: None,
            language: None,
        }
    }
}

fn now_unix() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Thin wrapper around [`rusqlite::Connection`].
pub struct HistoryDb {
    conn: Connection,
}

impl HistoryDb {
    /// Open (or create) the DB at `path` and apply migrations.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|source| crate::error::Error::Io { path: dir.to_path_buf(), source })?;
        }
        let conn = Connection::open(path)?;
        let db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    /// Open an in-memory DB (tests).
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<()> {
        // Pre-release: any pre-existing `transcriptions` table without the
        // current `polish_backend` column is treated as an incompatible
        // schema and dropped. No data preservation across schema breaks.
        let needs_rebuild = self.table_exists("transcriptions")?
            && !self.column_exists("transcriptions", "polish_backend")?;
        if needs_rebuild {
            self.conn.execute_batch(
                "DROP TABLE IF EXISTS transcriptions_fts;
                 DROP TABLE IF EXISTS transcriptions;",
            )?;
        }
        self.conn.execute_batch(
            "
            PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;

            CREATE TABLE IF NOT EXISTS transcriptions(
                id            INTEGER PRIMARY KEY,
                ts            INTEGER NOT NULL,
                duration_ms   INTEGER,
                raw           TEXT NOT NULL,
                cleaned       TEXT,
                app_class     TEXT,
                app_title     TEXT,
                stt_backend   TEXT,
                polish_backend   TEXT,
                language      TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_transcriptions_ts
                ON transcriptions(ts);

            CREATE VIRTUAL TABLE IF NOT EXISTS transcriptions_fts
                USING fts5(raw, cleaned, content='transcriptions', content_rowid='id');

            CREATE TRIGGER IF NOT EXISTS transcriptions_ai
                AFTER INSERT ON transcriptions BEGIN
                  INSERT INTO transcriptions_fts(rowid, raw, cleaned)
                    VALUES (new.id, new.raw, new.cleaned);
                END;

            CREATE TRIGGER IF NOT EXISTS transcriptions_ad
                AFTER DELETE ON transcriptions BEGIN
                  INSERT INTO transcriptions_fts(transcriptions_fts, rowid, raw, cleaned)
                    VALUES ('delete', old.id, old.raw, old.cleaned);
                END;

            CREATE TRIGGER IF NOT EXISTS transcriptions_au
                AFTER UPDATE ON transcriptions BEGIN
                  INSERT INTO transcriptions_fts(transcriptions_fts, rowid, raw, cleaned)
                    VALUES ('delete', old.id, old.raw, old.cleaned);
                  INSERT INTO transcriptions_fts(rowid, raw, cleaned)
                    VALUES (new.id, new.raw, new.cleaned);
                END;
            ",
        )?;
        Ok(())
    }

    fn table_exists(&self, table: &str) -> Result<bool> {
        let mut stmt =
            self.conn.prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1")?;
        Ok(stmt.exists([table])?)
    }

    fn column_exists(&self, table: &str, column: &str) -> Result<bool> {
        let mut stmt = self.conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let name: String = row.get(1)?;
            if name == column {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Insert a transcription, applying `redact_secrets` if requested. Returns
    /// the new row id.
    pub fn insert(&self, t: &Transcription, redact_secrets: bool) -> Result<i64> {
        let (raw, cleaned) = if redact_secrets {
            (redact(&t.raw), t.cleaned.as_deref().map(redact))
        } else {
            (t.raw.clone(), t.cleaned.clone())
        };
        self.conn.execute(
            "INSERT INTO transcriptions
             (ts, duration_ms, raw, cleaned, app_class, app_title, stt_backend, polish_backend, language)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                t.ts,
                t.duration_ms,
                raw,
                cleaned,
                t.app_class,
                t.app_title,
                t.stt_backend,
                t.polish_backend,
                t.language,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Delete any rows older than `retention_days`. Returns the number deleted.
    pub fn purge_older_than(&self, retention_days: u32) -> Result<usize> {
        if retention_days == 0 {
            return Ok(0);
        }
        let cutoff = now_unix() - i64::from(retention_days) * 86_400;
        let n = self.conn.execute("DELETE FROM transcriptions WHERE ts < ?1", params![cutoff])?;
        Ok(n)
    }

    /// Return the most recent cleaned (or raw if cleaned is null) transcription.
    pub fn last_text(&self) -> Result<Option<String>> {
        let res = self
            .conn
            .query_row(
                "SELECT COALESCE(cleaned, raw) FROM transcriptions ORDER BY ts DESC LIMIT 1",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        Ok(res)
    }

    /// Search via FTS5. `query` is passed straight to FTS5 MATCH syntax.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<Transcription>> {
        let mut stmt = self.conn.prepare(
            "SELECT t.id, t.ts, t.duration_ms, t.raw, t.cleaned, t.app_class, t.app_title,
                    t.stt_backend, t.polish_backend, t.language
             FROM transcriptions t
             JOIN transcriptions_fts fts ON fts.rowid = t.id
             WHERE transcriptions_fts MATCH ?1
             ORDER BY t.ts DESC
             LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![query, limit as i64], row_to_transcription)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Most recent `limit` entries ordered newest-first.
    pub fn recent(&self, limit: usize) -> Result<Vec<Transcription>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, ts, duration_ms, raw, cleaned, app_class, app_title,
                    stt_backend, polish_backend, language
             FROM transcriptions ORDER BY ts DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![limit as i64], row_to_transcription)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn count(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM transcriptions", [], |r| r.get::<_, i64>(0))?)
    }
}

fn row_to_transcription(r: &rusqlite::Row<'_>) -> rusqlite::Result<Transcription> {
    Ok(Transcription {
        id: Some(r.get(0)?),
        ts: r.get(1)?,
        duration_ms: r.get(2)?,
        raw: r.get(3)?,
        cleaned: r.get(4)?,
        app_class: r.get(5)?,
        app_title: r.get(6)?,
        stt_backend: r.get(7)?,
        polish_backend: r.get(8)?,
        language: r.get(9)?,
    })
}

/// Replace anything matching [`SECRET_RE`] with `[REDACTED]`.
#[must_use]
pub fn redact(text: &str) -> String {
    SECRET_RE.replace_all(text, "[REDACTED]").into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_search() {
        let db = HistoryDb::open_in_memory().unwrap();
        let mut t = Transcription::new("hello world from fono");
        t.cleaned = Some("Hello, world from Fono.".into());
        let id = db.insert(&t, false).unwrap();
        assert!(id > 0);
        let hits = db.search("fono", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(db.last_text().unwrap().as_deref(), Some("Hello, world from Fono."));
    }

    #[test]
    fn redaction_masks_keys() {
        let db = HistoryDb::open_in_memory().unwrap();
        let t = Transcription::new("my key is sk-abcdefghijklmnopqrstuv thanks");
        db.insert(&t, true).unwrap();
        let rec = &db.recent(1).unwrap()[0];
        assert!(rec.raw.contains("[REDACTED]"));
        assert!(!rec.raw.contains("sk-abcdefghijklmnopqrstuv"));
    }

    #[test]
    fn retention_cleanup() {
        let db = HistoryDb::open_in_memory().unwrap();
        let mut old = Transcription::new("ancient");
        old.ts = now_unix() - 100 * 86_400;
        db.insert(&old, false).unwrap();
        let fresh = Transcription::new("fresh");
        db.insert(&fresh, false).unwrap();
        let n = db.purge_older_than(30).unwrap();
        assert_eq!(n, 1);
        assert_eq!(db.count().unwrap(), 1);
    }

    #[test]
    fn drops_legacy_schema_without_polish_backend() {
        // Simulate a pre-rename DB: build the schema with the old column
        // name and a row, then open it through HistoryDb. The legacy
        // table is treated as incompatible and dropped; new inserts work.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE transcriptions(
                id            INTEGER PRIMARY KEY,
                ts            INTEGER NOT NULL,
                duration_ms   INTEGER,
                raw           TEXT NOT NULL,
                cleaned       TEXT,
                app_class     TEXT,
                app_title     TEXT,
                stt_backend   TEXT,
                llm_backend   TEXT,
                language      TEXT
             );
             INSERT INTO transcriptions(ts, raw, llm_backend) VALUES (1, 'legacy', 'groq');",
        )
        .unwrap();
        let db = HistoryDb { conn };
        db.migrate().unwrap();
        assert_eq!(db.count().unwrap(), 0, "legacy rows must be wiped");
        let mut fresh = Transcription::new("after-rebuild");
        fresh.polish_backend = Some("local".into());
        db.insert(&fresh, false).unwrap();
        assert_eq!(db.count().unwrap(), 1);
    }
}
