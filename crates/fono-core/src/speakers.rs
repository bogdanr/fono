// SPDX-License-Identifier: GPL-3.0-only
//! Enrolled-speaker store for Fono's local voice-biometrics feature
//! (Slice 2 of `plans/2026-07-17-speaker-verification-v1.md`).
//!
//! Holds, per enrolled speaker, a name and a set of voice-print **embeddings**
//! (the fixed-width `f32` vectors produced by the speaker-ID model) tagged
//! with the capture source they came from, plus optional calibration stats
//! from the "test my voice" flow.
//!
//! Security model — embeddings are **biometric data**, so this store follows
//! the same discipline as [`crate::api_keys::ApiKeyStore`]:
//! - a dedicated `speakers.sqlite`, never `config.toml`;
//! - the DB file is clamped to owner-only `0600` on Unix;
//! - deleting a speaker cascades to wipe every stored embedding.
//!
//! Embeddings are ~1 KB each (256 × f32), stored as little-endian `f32` BLOBs
//! in the same DB — no sidecar files.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use crate::error::{Error, Result};

fn now_unix() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Calibration stats for an enrolled speaker, produced by the "test my voice"
/// flow: the mean and standard deviation of the speaker's own
/// (genuine-trial) AS-Norm scores, and how many trials fed them. Used to
/// resolve `threshold = "auto"` against the shipped impostor cohort.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Calibration {
    pub genuine_mean: f32,
    pub genuine_std: f32,
    pub trials: i64,
}

/// Intrinsic capture-time audio-quality metrics for one enrollment utterance.
/// Computed client-side during capture and persisted *once* — the audio is
/// discarded, so these can never be recomputed (capture-now-or-never). All
/// fields are optional: utterances enrolled before the metrics existed, or via
/// paths that do not measure them, carry `None`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct UtteranceQuality {
    /// Clip length in seconds.
    pub duration_secs: Option<f32>,
    /// RMS level in dBFS (negative; ~0 is full-scale, very negative is quiet).
    pub loudness_dbfs: Option<f32>,
    /// Rough signal-to-noise ratio in dB (speech vs noise-floor frames).
    pub snr_db: Option<f32>,
}

/// Metadata view of an enrolled speaker. Never carries raw embeddings.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SpeakerView {
    pub id: i64,
    pub name: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// Number of enrolled utterances (embeddings) for this speaker.
    pub utterance_count: i64,
    /// Calibration stats, once the speaker has run "test my voice".
    pub calibration: Option<Calibration>,
    /// Total enrolled speech in seconds (sum of per-utterance durations);
    /// `None` when no utterance carries a duration metric.
    pub total_secs: Option<f32>,
    /// Number of distinct capture sources (microphones/channels) enrolled.
    pub source_count: i64,
}

/// One stored enrollment utterance: its embedding, where it was captured, and
/// when.
#[derive(Debug, Clone)]
pub struct Utterance {
    pub id: i64,
    pub embedding: Vec<f32>,
    /// Free-form capture-source tag (e.g. `"browser"`, `"daemon-mic"`,
    /// `"wav-upload"`) so a channel mismatch can be warned about later.
    pub capture_source: String,
    pub created_at: i64,
    /// Intrinsic capture-time quality metrics (may be all-`None`).
    pub quality: UtteranceQuality,
}

/// Coverage-floor constants for [`suggest_prune`]. Pruning never drops the set
/// below this many clips or this many seconds of total speech, and never
/// removes the last remaining clip from any capture source — so a suggested
/// prune can only ever tighten quality, never leave a speaker under-enrolled.
pub const PRUNE_MIN_CLIPS: usize = 3;
/// See [`PRUNE_MIN_CLIPS`].
pub const PRUNE_MIN_SECS: f32 = 15.0;

/// Weakness thresholds: a clip is a prune *candidate* only if it trips one of
/// these — an outlier vs its peers, too quiet, clipping, too noisy, or too
/// short. Clips that look fine are never suggested for removal. A `None`
/// metric (older clip, or a capture path that did not measure it) never trips
/// its signal.
const WEAK_CONSISTENCY: f32 = 0.55;
const WEAK_SNR_DB: f32 = 8.0;
const WEAK_QUIET_DBFS: f32 = -45.0;
const WEAK_CLIP_DBFS: f32 = -1.0;
const WEAK_SHORT_SECS: f32 = 1.2;

/// Is this utterance a weak-quality prune candidate? `consistency` is its
/// cosine to the centroid of the *other* utterances (computed on demand — it
/// is relational, so it is never stored).
fn is_weak(u: &Utterance, consistency: f32) -> bool {
    let q = &u.quality;
    consistency < WEAK_CONSISTENCY
        || q.snr_db.is_some_and(|s| s < WEAK_SNR_DB)
        || q.loudness_dbfs.is_some_and(|l| !(WEAK_QUIET_DBFS..=WEAK_CLIP_DBFS).contains(&l))
        || q.duration_secs.is_some_and(|d| d < WEAK_SHORT_SECS)
}

/// Suggest which utterances to prune, weakest first, while preserving the
/// coverage floor ([`PRUNE_MIN_CLIPS`] clips, [`PRUNE_MIN_SECS`] seconds, and
/// at least one clip per capture source). Only genuinely weak clips
/// ([`is_weak`]) are ever suggested — this never proposes dropping good audio
/// just to hit a target. The result is advisory: the daemon returns it for the
/// user to confirm, and nothing is deleted without an explicit request.
///
/// `consistency[i]` pairs with `utterances[i]` (same order); a short or empty
/// `consistency` slice treats the missing entries as perfectly consistent.
#[must_use]
pub fn suggest_prune(utterances: &[Utterance], consistency: &[f32]) -> Vec<i64> {
    // Nothing to do if we are already at or below the clip floor.
    if utterances.len() <= PRUNE_MIN_CLIPS {
        return Vec::new();
    }

    // Running trackers for the remaining set as we tentatively remove clips.
    let mut remaining_clips = utterances.len();
    let mut remaining_secs: f32 = utterances.iter().filter_map(|u| u.quality.duration_secs).sum();
    let mut per_source: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for u in utterances {
        *per_source.entry(u.capture_source.as_str()).or_insert(0) += 1;
    }

    // Candidates, weakest (lowest consistency) first.
    let mut order: Vec<usize> = (0..utterances.len()).collect();
    let cons = |i: usize| consistency.get(i).copied().unwrap_or(1.0);
    order.sort_by(|&a, &b| cons(a).total_cmp(&cons(b)));

    let mut remove = Vec::new();
    for i in order {
        let u = &utterances[i];
        if !is_weak(u, cons(i)) {
            continue;
        }
        let dur = u.quality.duration_secs.unwrap_or(0.0);
        let src = u.capture_source.as_str();
        let keeps_clip_floor = remaining_clips > PRUNE_MIN_CLIPS;
        let keeps_secs_floor = remaining_secs - dur >= PRUNE_MIN_SECS;
        let keeps_source = per_source.get(src).copied().unwrap_or(0) > 1;
        if keeps_clip_floor && keeps_secs_floor && keeps_source {
            remaining_clips -= 1;
            remaining_secs -= dur;
            *per_source.get_mut(src).unwrap() -= 1;
            remove.push(u.id);
        }
    }
    remove
}

/// SQLite-backed store of enrolled speakers and their voice-print embeddings.
pub struct SpeakerStore {
    conn: Connection,
}

impl SpeakerStore {
    /// Open (or create) the store at `path` and apply migrations. The DB file
    /// is clamped to owner-only `0600` on Unix.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|source| Error::Io { path: dir.to_path_buf(), source })?;
        }
        let conn = Connection::open(path)?;
        restrict_to_owner(path);
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

            CREATE TABLE IF NOT EXISTS speakers(
                id             INTEGER PRIMARY KEY,
                name           TEXT NOT NULL UNIQUE,
                created_at     INTEGER NOT NULL,
                updated_at     INTEGER NOT NULL,
                cal_mean       REAL,
                cal_std        REAL,
                cal_trials     INTEGER
            );

            CREATE TABLE IF NOT EXISTS speaker_utterances(
                id             INTEGER PRIMARY KEY,
                speaker_id     INTEGER NOT NULL,
                embedding      BLOB NOT NULL,
                capture_source TEXT NOT NULL,
                created_at     INTEGER NOT NULL,
                FOREIGN KEY (speaker_id) REFERENCES speakers(id) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_utterances_speaker
                ON speaker_utterances(speaker_id);
            ",
        )?;
        // Additive columns for intrinsic capture-quality metrics. SQLite has
        // no `ADD COLUMN IF NOT EXISTS`, so guard each with `column_exists`
        // for idempotent upgrades of an existing DB.
        for col in ["duration_secs", "loudness_dbfs", "snr_db"] {
            if !self.column_exists("speaker_utterances", col)? {
                self.conn.execute_batch(&format!(
                    "ALTER TABLE speaker_utterances ADD COLUMN {col} REAL"
                ))?;
            }
        }
        Ok(())
    }

    /// Whether `table` has a column named `column`.
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

    /// Enroll a new (initially utterance-less) speaker. Fails if the name
    /// collides.
    pub fn add_speaker(&self, name: &str) -> Result<SpeakerView> {
        let name = name.trim();
        if name.is_empty() {
            return Err(Error::Other("speaker name must not be empty".into()));
        }
        let now = now_unix();
        self.conn
            .execute(
                "INSERT INTO speakers (name, created_at, updated_at) VALUES (?1, ?2, ?2)",
                params![name, now],
            )
            .map_err(map_unique(name))?;
        let id = self.conn.last_insert_rowid();
        self.get_speaker(id)?.ok_or_else(|| Error::Other("row vanished after insert".into()))
    }

    /// All speakers, newest first. Metadata only (no embeddings).
    pub fn list_speakers(&self) -> Result<Vec<SpeakerView>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, created_at, updated_at, cal_mean, cal_std, cal_trials
             FROM speakers ORDER BY created_at DESC, id DESC",
        )?;
        let rows = stmt
            .query_map([], row_to_speaker_parts)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut out = Vec::with_capacity(rows.len());
        for (id, name, created_at, updated_at, cal) in rows {
            out.push(self.assemble_view(id, name, created_at, updated_at, cal)?);
        }
        Ok(out)
    }

    /// Build a [`SpeakerView`] from its base row, filling the derived
    /// utterance count and enrollment aggregates.
    fn assemble_view(
        &self,
        id: i64,
        name: String,
        created_at: i64,
        updated_at: i64,
        calibration: Option<Calibration>,
    ) -> Result<SpeakerView> {
        let (total_secs, source_count) = self.enrollment_summary(id)?;
        Ok(SpeakerView {
            id,
            name,
            created_at,
            updated_at,
            utterance_count: self.utterance_count(id)?,
            calibration,
            total_secs,
            source_count,
        })
    }

    /// Enrollment aggregates for a speaker: total enrolled seconds (sum of
    /// per-utterance durations, `None` if none recorded) and the number of
    /// distinct capture sources. Feeds the profile-strength indicator.
    pub fn enrollment_summary(&self, speaker_id: i64) -> Result<(Option<f32>, i64)> {
        let total_secs: Option<f32> = self.conn.query_row(
            "SELECT SUM(duration_secs) FROM speaker_utterances WHERE speaker_id = ?1",
            params![speaker_id],
            |r| r.get(0),
        )?;
        let source_count: i64 = self.conn.query_row(
            "SELECT COUNT(DISTINCT capture_source) FROM speaker_utterances WHERE speaker_id = ?1",
            params![speaker_id],
            |r| r.get(0),
        )?;
        Ok((total_secs, source_count))
    }

    /// Fetch one speaker by id.
    pub fn get_speaker(&self, id: i64) -> Result<Option<SpeakerView>> {
        let row = self
            .conn
            .query_row(
                "SELECT id, name, created_at, updated_at, cal_mean, cal_std, cal_trials
                 FROM speakers WHERE id = ?1",
                params![id],
                row_to_speaker_parts,
            )
            .optional()?;
        let Some((id, name, created_at, updated_at, cal)) = row else {
            return Ok(None);
        };
        Ok(Some(self.assemble_view(id, name, created_at, updated_at, cal)?))
    }

    /// Look a speaker up by (exact, trimmed) name.
    pub fn get_speaker_by_name(&self, name: &str) -> Result<Option<SpeakerView>> {
        let id = self
            .conn
            .query_row("SELECT id FROM speakers WHERE name = ?1", params![name.trim()], |r| {
                r.get::<_, i64>(0)
            })
            .optional()?;
        id.map_or_else(|| Ok(None), |id| self.get_speaker(id))
    }

    /// Rename a speaker. Fails if the new name collides.
    pub fn rename(&self, id: i64, new_name: &str) -> Result<()> {
        let new_name = new_name.trim();
        if new_name.is_empty() {
            return Err(Error::Other("speaker name must not be empty".into()));
        }
        let n = self
            .conn
            .execute(
                "UPDATE speakers SET name = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, new_name, now_unix()],
            )
            .map_err(map_unique(new_name))?;
        if n == 0 {
            return Err(Error::Other(format!("no speaker with id {id}")));
        }
        Ok(())
    }

    /// Delete a speaker and (via cascade) every enrolled embedding.
    pub fn remove(&self, id: i64) -> Result<()> {
        let n = self.conn.execute("DELETE FROM speakers WHERE id = ?1", params![id])?;
        if n == 0 {
            return Err(Error::Other(format!("no speaker with id {id}")));
        }
        Ok(())
    }

    /// Append one enrollment utterance for a speaker and touch its
    /// `updated_at`. Returns the new utterance id.
    pub fn add_utterance(
        &self,
        speaker_id: i64,
        embedding: &[f32],
        capture_source: &str,
    ) -> Result<i64> {
        self.add_utterance_with_quality(
            speaker_id,
            embedding,
            capture_source,
            UtteranceQuality::default(),
        )
    }

    /// Append one enrollment utterance together with its intrinsic
    /// capture-quality metrics, and touch the speaker's `updated_at`.
    pub fn add_utterance_with_quality(
        &self,
        speaker_id: i64,
        embedding: &[f32],
        capture_source: &str,
        quality: UtteranceQuality,
    ) -> Result<i64> {
        if embedding.is_empty() {
            return Err(Error::Other("embedding must not be empty".into()));
        }
        let now = now_unix();
        let blob = encode_embedding(embedding);
        let touched = self.conn.execute(
            "UPDATE speakers SET updated_at = ?2 WHERE id = ?1",
            params![speaker_id, now],
        )?;
        if touched == 0 {
            return Err(Error::Other(format!("no speaker with id {speaker_id}")));
        }
        self.conn.execute(
            "INSERT INTO speaker_utterances
                 (speaker_id, embedding, capture_source, created_at,
                  duration_secs, loudness_dbfs, snr_db)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                speaker_id,
                blob,
                capture_source,
                now,
                quality.duration_secs,
                quality.loudness_dbfs,
                quality.snr_db,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Every enrolled utterance for a speaker, oldest first.
    pub fn utterances(&self, speaker_id: i64) -> Result<Vec<Utterance>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, embedding, capture_source, created_at,
                    duration_secs, loudness_dbfs, snr_db
             FROM speaker_utterances WHERE speaker_id = ?1 ORDER BY created_at ASC, id ASC",
        )?;
        let rows = stmt
            .query_map(params![speaker_id], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Vec<u8>>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    UtteranceQuality {
                        duration_secs: r.get::<_, Option<f32>>(4)?,
                        loudness_dbfs: r.get::<_, Option<f32>>(5)?,
                        snr_db: r.get::<_, Option<f32>>(6)?,
                    },
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows
            .into_iter()
            .map(|(id, blob, capture_source, created_at, quality)| Utterance {
                id,
                embedding: decode_embedding(&blob),
                capture_source,
                created_at,
                quality,
            })
            .collect())
    }

    /// Number of enrolled utterances for a speaker.
    pub fn utterance_count(&self, speaker_id: i64) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM speaker_utterances WHERE speaker_id = ?1",
            params![speaker_id],
            |r| r.get::<_, i64>(0),
        )?)
    }

    /// Delete a single enrolled utterance (re-enroll / prune a bad capture).
    pub fn remove_utterance(&self, utterance_id: i64) -> Result<()> {
        let n = self
            .conn
            .execute("DELETE FROM speaker_utterances WHERE id = ?1", params![utterance_id])?;
        if n == 0 {
            return Err(Error::Other(format!("no utterance with id {utterance_id}")));
        }
        Ok(())
    }

    /// Record (or clear, with `None`) a speaker's calibration stats.
    pub fn set_calibration(&self, id: i64, cal: Option<Calibration>) -> Result<()> {
        let (mean, std, trials) = cal.map_or((None, None, None), |c| {
            (Some(c.genuine_mean), Some(c.genuine_std), Some(c.trials))
        });
        let n = self.conn.execute(
            "UPDATE speakers SET cal_mean = ?2, cal_std = ?3, cal_trials = ?4, updated_at = ?5
             WHERE id = ?1",
            params![id, mean, std, trials, now_unix()],
        )?;
        if n == 0 {
            return Err(Error::Other(format!("no speaker with id {id}")));
        }
        Ok(())
    }

    /// Total number of enrolled speakers (for doctor / diagnostics).
    pub fn speaker_count(&self) -> Result<i64> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM speakers", [], |r| r.get::<_, i64>(0))?)
    }
}

/// Encode an embedding as little-endian `f32` bytes.
fn encode_embedding(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

/// Decode a little-endian `f32` BLOB back into an embedding. A trailing
/// partial value (blob length not a multiple of 4) is ignored defensively.
fn decode_embedding(blob: &[u8]) -> Vec<f32> {
    blob.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

type SpeakerParts = (i64, String, i64, i64, Option<Calibration>);

fn row_to_speaker_parts(r: &rusqlite::Row<'_>) -> rusqlite::Result<SpeakerParts> {
    let id = r.get::<_, i64>(0)?;
    let name = r.get::<_, String>(1)?;
    let created_at = r.get::<_, i64>(2)?;
    let updated_at = r.get::<_, i64>(3)?;
    let mean = r.get::<_, Option<f32>>(4)?;
    let std = r.get::<_, Option<f32>>(5)?;
    let trials = r.get::<_, Option<i64>>(6)?;
    let cal = match (mean, std, trials) {
        (Some(genuine_mean), Some(genuine_std), Some(trials)) => {
            Some(Calibration { genuine_mean, genuine_std, trials })
        }
        _ => None,
    };
    Ok((id, name, created_at, updated_at, cal))
}

fn map_unique(name: &str) -> impl Fn(rusqlite::Error) -> Error + '_ {
    move |e| match e {
        rusqlite::Error::SqliteFailure(err, _)
            if err.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            Error::Other(format!("a speaker named '{name}' already exists"))
        }
        other => Error::from(other),
    }
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

    /// Build an `Utterance` with the given id, capture source, and quality
    /// metrics (duration, loudness dBFS, SNR dB) for prune tests.
    fn mk_utt(id: i64, src: &str, dur: f32, loud: f32, snr: f32) -> Utterance {
        Utterance {
            id,
            embedding: vec![0.0; 4],
            capture_source: src.to_string(),
            created_at: 0,
            quality: UtteranceQuality {
                duration_secs: Some(dur),
                loudness_dbfs: Some(loud),
                snr_db: Some(snr),
            },
        }
    }

    #[test]
    fn suggest_prune_respects_clip_floor() {
        // Exactly the floor count, all weak → nothing to prune.
        let utts: Vec<Utterance> =
            (0..PRUNE_MIN_CLIPS as i64).map(|i| mk_utt(i, "mic", 6.0, -20.0, 3.0)).collect();
        let cons = vec![0.1; utts.len()];
        assert!(suggest_prune(&utts, &cons).is_empty());
    }

    #[test]
    fn suggest_prune_keeps_good_clips() {
        // Plenty of strong clips → never suggest dropping any.
        let utts: Vec<Utterance> = (0..6).map(|i| mk_utt(i, "mic", 6.0, -20.0, 25.0)).collect();
        let cons = vec![0.95; utts.len()];
        assert!(suggest_prune(&utts, &cons).is_empty());
    }

    #[test]
    fn suggest_prune_drops_weak_while_holding_floor() {
        // 4 strong + 2 weak (a noisy one and an outlier). Floor is 3 clips /
        // 15 s; strong clips alone give 4 clips / 24 s, so both weak drop.
        let mut utts: Vec<Utterance> = (0..4).map(|i| mk_utt(i, "mic", 6.0, -20.0, 25.0)).collect();
        utts.push(mk_utt(100, "mic", 6.0, -20.0, 2.0)); // noisy
        utts.push(mk_utt(101, "mic", 6.0, -20.0, 25.0)); // metric-clean but outlier
        let mut cons = vec![0.95; 4];
        cons.push(0.9); // noisy clip still flagged by SNR
        cons.push(0.2); // outlier flagged by consistency
        let remove = suggest_prune(&utts, &cons);
        assert!(remove.contains(&100) && remove.contains(&101), "both weak dropped: {remove:?}");
        assert_eq!(remove.len(), 2);
    }

    #[test]
    fn suggest_prune_preserves_device_diversity() {
        // Many good "mic" clips plus a single weak clip on a second device:
        // dropping it would lose that device, so it must be kept.
        let mut utts: Vec<Utterance> = (0..5).map(|i| mk_utt(i, "mic", 6.0, -20.0, 25.0)).collect();
        utts.push(mk_utt(200, "usb", 6.0, -20.0, 2.0)); // weak, only "usb" clip
        let mut cons = vec![0.95; 5];
        cons.push(0.9);
        assert!(!suggest_prune(&utts, &cons).contains(&200), "last clip of a device is kept");
    }

    #[test]
    fn suggest_prune_holds_seconds_floor() {
        // 4 short weak clips (3 s each = 12 s total). Removing any would fall
        // under both the seconds floor and, after one, the clip floor.
        let utts: Vec<Utterance> = (0..4).map(|i| mk_utt(i, "mic", 3.0, -20.0, 2.0)).collect();
        let cons = vec![0.2; utts.len()];
        assert!(suggest_prune(&utts, &cons).is_empty(), "seconds floor blocks pruning");
    }

    #[test]
    fn add_list_and_count() {
        let db = SpeakerStore::open_in_memory().unwrap();
        let alice = db.add_speaker("Alice").unwrap();
        assert_eq!(alice.name, "Alice");
        assert_eq!(alice.utterance_count, 0);
        assert!(alice.calibration.is_none());
        db.add_speaker("Bob").unwrap();
        assert_eq!(db.speaker_count().unwrap(), 2);
        assert_eq!(db.list_speakers().unwrap().len(), 2);
    }

    #[test]
    fn duplicate_name_rejected() {
        let db = SpeakerStore::open_in_memory().unwrap();
        db.add_speaker("dup").unwrap();
        assert!(db.add_speaker("dup").is_err());
        assert!(db.add_speaker("  dup  ").is_err(), "trimmed name collides too");
    }

    #[test]
    fn empty_name_rejected() {
        let db = SpeakerStore::open_in_memory().unwrap();
        assert!(db.add_speaker("   ").is_err());
    }

    #[test]
    fn lookup_by_name() {
        let db = SpeakerStore::open_in_memory().unwrap();
        let v = db.add_speaker("Carol").unwrap();
        assert_eq!(db.get_speaker_by_name("Carol").unwrap().unwrap().id, v.id);
        assert!(db.get_speaker_by_name("Nobody").unwrap().is_none());
    }

    #[test]
    fn embedding_roundtrips_through_blob() {
        let db = SpeakerStore::open_in_memory().unwrap();
        let v = db.add_speaker("Dave").unwrap();
        let emb = vec![0.1f32, -0.2, 0.3, 123.456, -789.0];
        db.add_utterance(v.id, &emb, "browser").unwrap();
        let utts = db.utterances(v.id).unwrap();
        assert_eq!(utts.len(), 1);
        assert_eq!(utts[0].embedding, emb);
        assert_eq!(utts[0].capture_source, "browser");
        assert_eq!(db.get_speaker(v.id).unwrap().unwrap().utterance_count, 1);
    }

    #[test]
    fn adding_utterance_touches_updated_at() {
        let db = SpeakerStore::open_in_memory().unwrap();
        let v = db.add_speaker("Erin").unwrap();
        // Force a distinct later timestamp by writing updated_at back in time.
        db.conn
            .execute(
                "UPDATE speakers SET updated_at = created_at - 100 WHERE id = ?1",
                params![v.id],
            )
            .unwrap();
        let before = db.get_speaker(v.id).unwrap().unwrap().updated_at;
        db.add_utterance(v.id, &[1.0, 2.0], "daemon-mic").unwrap();
        let after = db.get_speaker(v.id).unwrap().unwrap().updated_at;
        assert!(after > before);
    }

    #[test]
    fn utterance_for_missing_speaker_errors() {
        let db = SpeakerStore::open_in_memory().unwrap();
        assert!(db.add_utterance(999, &[1.0], "x").is_err());
    }

    #[test]
    fn empty_embedding_rejected() {
        let db = SpeakerStore::open_in_memory().unwrap();
        let v = db.add_speaker("F").unwrap();
        assert!(db.add_utterance(v.id, &[], "x").is_err());
    }

    #[test]
    fn remove_speaker_cascades_utterances() {
        let db = SpeakerStore::open_in_memory().unwrap();
        let v = db.add_speaker("Gina").unwrap();
        db.add_utterance(v.id, &[1.0, 2.0], "browser").unwrap();
        db.add_utterance(v.id, &[3.0, 4.0], "browser").unwrap();
        db.remove(v.id).unwrap();
        assert_eq!(db.speaker_count().unwrap(), 0);
        // Cascade wiped the embeddings.
        assert_eq!(db.utterance_count(v.id).unwrap(), 0);
    }

    #[test]
    fn remove_single_utterance() {
        let db = SpeakerStore::open_in_memory().unwrap();
        let v = db.add_speaker("Hank").unwrap();
        let uid = db.add_utterance(v.id, &[1.0, 2.0], "browser").unwrap();
        db.add_utterance(v.id, &[3.0, 4.0], "browser").unwrap();
        db.remove_utterance(uid).unwrap();
        assert_eq!(db.utterance_count(v.id).unwrap(), 1);
    }

    #[test]
    fn rename_and_calibration() {
        let db = SpeakerStore::open_in_memory().unwrap();
        let v = db.add_speaker("old").unwrap();
        db.rename(v.id, "new").unwrap();
        assert_eq!(db.get_speaker(v.id).unwrap().unwrap().name, "new");
        let cal = Calibration { genuine_mean: 2.5, genuine_std: 0.7, trials: 20 };
        db.set_calibration(v.id, Some(cal)).unwrap();
        assert_eq!(db.get_speaker(v.id).unwrap().unwrap().calibration, Some(cal));
        db.set_calibration(v.id, None).unwrap();
        assert!(db.get_speaker(v.id).unwrap().unwrap().calibration.is_none());
    }

    #[test]
    fn rename_collision_rejected() {
        let db = SpeakerStore::open_in_memory().unwrap();
        let a = db.add_speaker("a").unwrap();
        db.add_speaker("b").unwrap();
        assert!(db.rename(a.id, "b").is_err());
    }

    #[test]
    fn decode_ignores_trailing_partial() {
        // 5 bytes -> one full f32, trailing byte dropped.
        assert_eq!(decode_embedding(&[0, 0, 128, 63, 7]), vec![1.0f32]);
    }

    #[test]
    fn quality_metrics_persist_and_default_to_none() {
        let db = SpeakerStore::open_in_memory().unwrap();
        let v = db.add_speaker("Ivy").unwrap();
        // Plain add_utterance leaves the metrics NULL.
        db.add_utterance(v.id, &[1.0, 2.0], "browser").unwrap();
        // The quality-bearing variant records them.
        let q = UtteranceQuality {
            duration_secs: Some(3.5),
            loudness_dbfs: Some(-22.0),
            snr_db: Some(18.0),
        };
        db.add_utterance_with_quality(v.id, &[3.0, 4.0], "usb-mic", q).unwrap();
        let utts = db.utterances(v.id).unwrap();
        assert_eq!(utts.len(), 2);
        assert_eq!(utts[0].quality, UtteranceQuality::default());
        assert_eq!(utts[1].quality, q);
    }

    #[test]
    fn enrollment_summary_totals_and_distinct_sources() {
        let db = SpeakerStore::open_in_memory().unwrap();
        let v = db.add_speaker("Jill").unwrap();
        // No utterances yet: no duration, no sources.
        assert_eq!(db.enrollment_summary(v.id).unwrap(), (None, 0));
        let q = |d: f32| UtteranceQuality { duration_secs: Some(d), ..Default::default() };
        db.add_utterance_with_quality(v.id, &[1.0], "browser", q(4.0)).unwrap();
        db.add_utterance_with_quality(v.id, &[2.0], "browser", q(6.0)).unwrap();
        db.add_utterance_with_quality(v.id, &[3.0], "usb-mic", q(5.0)).unwrap();
        let (total, sources) = db.enrollment_summary(v.id).unwrap();
        assert!((total.unwrap() - 15.0).abs() < 1e-4, "durations sum");
        assert_eq!(sources, 2, "two distinct capture sources");
        // Surfaced on the view too.
        let view = db.get_speaker(v.id).unwrap().unwrap();
        assert!((view.total_secs.unwrap() - 15.0).abs() < 1e-4);
        assert_eq!(view.source_count, 2);
    }

    #[test]
    fn migration_is_idempotent() {
        // Opening twice over the same file must not fail re-adding columns.
        let dir = std::env::temp_dir().join(format!("fono-spk-mig-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("speakers.sqlite");
        {
            let db = SpeakerStore::open(&path).unwrap();
            db.add_speaker("K").unwrap();
        }
        // Second open re-runs migrate(); columns already exist.
        let db = SpeakerStore::open(&path).unwrap();
        assert_eq!(db.speaker_count().unwrap(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
