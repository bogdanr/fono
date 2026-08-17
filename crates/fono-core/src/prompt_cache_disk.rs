// SPDX-License-Identifier: GPL-3.0-only
//! Second tier of the prompt-state cache: checkpoints kept on disk behind the
//! bounded in-memory one.
//!
//! Measured on a coding client driving six conversations through the
//! OpenAI-compatible endpoint, returning to a conversation whose checkpoint had
//! been dropped cost 70–114 seconds of re-reading, against 4.3 seconds for one
//! still held; 32,435 prefix tokens were read a second time because a
//! checkpoint had been dropped, against 5 for every other cause combined. A
//! restart threw all of them away. Reading a checkpoint back takes 170–218 ms.
//!
//! The design is deliberately narrow, and the narrowness is the point — see
//! `docs/decisions/0042-prompt-checkpoints-on-disk.md`:
//!
//! - **One self-describing file per checkpoint, named by its content.** A second
//!   write of the same checkpoint is a no-op, and there is no index to fall out
//!   of step with the directory after a crash. Lookup reads headers only.
//! - **Publish by rename, and never `fsync`.** A rename already survives a
//!   process abort, a clean exit and a reboot; the only extra failure `fsync`
//!   covers is a power cut, whose cost is one cold prefill. A truncated file is
//!   *detected* below rather than mistaken for good data.
//! - **Validate every field on read, and delete on any doubt.** This is the one
//!   place care is spent: a state that did not match its claimed token count is
//!   a bug this project has already shipped once.
//! - **Retention is least-recently-used against a byte budget.** Checkpoints
//!   cost a near-constant ~17 KB per token, so their sizes vary by about a
//!   tenth while their usefulness varies by orders of magnitude; a value-per-byte
//!   score would reduce to this sort with a decay constant bolted on.

use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::prompt_cache::{PromptStateCacheKey, PromptStateCacheLayer};

/// Marks a file as one of ours before anything else is believed about it.
const MAGIC: &[u8; 4] = b"FKVC";

/// Layout and payload version, bumped by hand whenever either the header below
/// or the meaning of the stored state changes. A file whose version is not this
/// one is deleted rather than interpreted: the payload is llama.cpp's own
/// serialization and is not promised to be stable across its versions.
const FORMAT_VERSION: u32 = 1;

/// Bytes before the variable-length key, tokens and payload.
const HEADER_BYTES: u64 = 32;

/// Offset of the last-used timestamp, rewritten in place on a hit so retention
/// can sort by it without rewriting a multi-hundred-megabyte payload.
const LAST_USED_OFFSET: u64 = 8;

/// Extension every checkpoint file carries. Anything else in the directory is
/// left alone, so a stray file can never be mistaken for a checkpoint or
/// deleted by the sweep.
const EXTENSION: &str = "kv";

/// Headroom left above a checkpoint before it is written.
///
/// Without it a checkpoint exactly the size of the budget is written and then
/// deleted by the retention pass that follows, spending the whole write for
/// nothing.
const HEADROOM_PERCENT: u64 = 1;

/// How long a checkpoint survives without being used.
///
/// The byte budget already bounds the directory, so this is not about space. It
/// is about not keeping a copy of a conversation nobody has returned to in 14
/// days: the state is the prompt in another form, and holding it longer
/// than it is useful is a cost with no benefit. A hit rewrites the timestamp, so
/// anything still in use never ages out however old the file is.
const MAX_IDLE_MILLIS: u64 = 14 * 24 * 60 * 60 * 1000;

/// A checkpoint read back from disk.
#[derive(Debug, Clone)]
pub struct StoredCheckpoint {
    /// The key it was stored under, reconstructed exactly, so the caller can
    /// admit it to the in-memory cache without re-deriving anything.
    pub key: PromptStateCacheKey,
    pub prefix_tokens: Vec<i32>,
    pub state: Vec<u8>,
}

/// What one file says about itself, read without touching its payload.
#[derive(Debug, Clone)]
struct Index {
    path: PathBuf,
    key: PromptStateCacheKey,
    prefix_tokens: Vec<i32>,
    payload_len: u64,
    file_len: u64,
    last_used: u64,
}

/// Checkpoints on disk, bounded by a byte budget.
#[derive(Debug, Clone)]
pub struct CheckpointStore {
    dir: PathBuf,
    max_bytes: u64,
}

impl CheckpointStore {
    /// Open (creating if needed) a store in `dir` with a `max_bytes` budget.
    ///
    /// Returns `None` when the tier must not run: a zero budget (the
    /// configured opt-out), a directory that cannot be created, or a directory
    /// backed by memory. The last of those matters because a "disk" tier on
    /// `tmpfs` spends the very memory the in-memory budget was shrunk to
    /// release, and would report a saving it did not make.
    ///
    /// Nothing is created when the answer is `None`, so the absence of the
    /// directory is proof the tier is off.
    pub fn open(dir: PathBuf, max_bytes: u64) -> Option<Self> {
        if max_bytes == 0 || memory_backed(&dir) {
            return None;
        }
        fs::create_dir_all(&dir).ok()?;
        restrict(&dir);
        Some(Self { dir, max_bytes })
    }

    /// Why [`Self::open`] would refuse, in words a user can act on, or `None`
    /// when it would not. Reported by `fono doctor` rather than left silent:
    /// a cache directory living in RAM is a fact worth knowing.
    pub fn refusal(dir: &Path, max_bytes: u64) -> Option<String> {
        if max_bytes == 0 {
            return Some("turned off in the configuration".to_string());
        }
        if memory_backed(dir) {
            return Some(format!(
                "{} is in memory, not on disk, so storing conversations there would \
                 spend the memory this is meant to free",
                dir.display()
            ));
        }
        None
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    /// The deepest stored checkpoint whose tokens are a proper prefix of
    /// `tokens`, under the same runtime and one of `layers`.
    ///
    /// Same rule as the in-memory tier, so a caller can consult this when
    /// memory has nothing deeper and treat the answer identically. Any file
    /// that fails validation is deleted on the way past.
    pub fn lookup(
        &self,
        runtime: &str,
        layers: &[PromptStateCacheLayer],
        tokens: &[i32],
    ) -> Option<StoredCheckpoint> {
        let best = self
            .index()
            .into_iter()
            .filter(|i| {
                i.key.runtime_sha256() == runtime
                    && layers.contains(i.key.layer())
                    && i.prefix_tokens.len() < tokens.len()
                    && tokens.starts_with(&i.prefix_tokens)
            })
            .max_by_key(|i| i.prefix_tokens.len())?;
        if let Some(state) = read_payload(&best) {
            touch(&best.path);
            Some(StoredCheckpoint { key: best.key, prefix_tokens: best.prefix_tokens, state })
        } else {
            discard(&best.path);
            None
        }
    }

    /// Write a checkpoint, unless one of the same content is already there.
    ///
    /// Returns whether a file was written. A checkpoint that cannot fit the
    /// budget with headroom is declined rather than written and immediately
    /// swept; a checkpoint with no recorded tokens is declined because nothing
    /// could ever match it.
    pub fn store(
        &self,
        key: &PromptStateCacheKey,
        prefix_tokens: &[i32],
        state: &[u8],
    ) -> std::io::Result<bool> {
        if prefix_tokens.is_empty() || state.is_empty() {
            return Ok(false);
        }
        // The key carries its own token count and the restore relies on it to
        // place the state at the right position, so a disagreement here is the
        // bug class that failed whole requests once already. Refuse now rather
        // than write a file whose validation will reject it on the way back.
        if key.token_count() != prefix_tokens.len() {
            return Ok(false);
        }
        let encoded = encode_key(key);
        let name = file_name(&encoded, prefix_tokens);
        let path = self.dir.join(&name);
        if path.exists() {
            touch(&path);
            return Ok(false);
        }

        let file_len = HEADER_BYTES
            + encoded.len() as u64
            + (prefix_tokens.len() as u64) * 4
            + state.len() as u64;
        if file_len > self.max_bytes.saturating_sub(self.max_bytes * HEADROOM_PERCENT / 100) {
            return Ok(false);
        }
        // Make room before writing, not after, so the budget is never exceeded
        // even transiently and the write is never wasted on a file the sweep
        // would take straight back.
        self.reclaim(file_len);

        let temp = self.dir.join(format!("{name}.part-{}", std::process::id()));
        let written = write_temp(&temp, &encoded, prefix_tokens, state);
        if let Err(err) = written {
            let _ = fs::remove_file(&temp);
            return Err(err);
        }
        // Publish atomically. No `fsync`: see the module docs.
        if let Err(err) = fs::rename(&temp, &path) {
            let _ = fs::remove_file(&temp);
            return Err(err);
        }
        restrict(&path);
        Ok(true)
    }

    /// Drop everything stored under a runtime key that is no longer current or
    /// unused for 14 days, then bring the total inside the budget. Run at
    /// startup, where a model or setting having changed since last time is most
    /// likely.
    pub fn sweep(&self, current_runtime: &str) -> usize {
        let mut dropped = 0;
        let now = now_millis();
        for entry in self.index() {
            let stale = entry.key.runtime_sha256() != current_runtime;
            let idle = now.saturating_sub(entry.last_used) > MAX_IDLE_MILLIS;
            if stale || idle {
                discard(&entry.path);
                dropped += 1;
            }
        }
        dropped + self.reclaim(0)
    }

    /// Delete every checkpoint. The explicit control a user needs to clear the
    /// tier without knowing where it lives.
    pub fn clear(&self) -> usize {
        let entries = self.index();
        let dropped = entries.len();
        for entry in entries {
            discard(&entry.path);
        }
        dropped
    }

    /// Total bytes stored, and how many checkpoints that is.
    pub fn usage(&self) -> (u64, usize) {
        let entries = self.index();
        (entries.iter().map(|e| e.file_len).sum(), entries.len())
    }

    /// Delete least-recently-used checkpoints until `extra` more bytes fit
    /// inside the budget. Returns how many were deleted.
    fn reclaim(&self, extra: u64) -> usize {
        let mut entries = self.index();
        let mut total: u64 = entries.iter().map(|e| e.file_len).sum();
        if total + extra <= self.max_bytes {
            return 0;
        }
        entries.sort_by_key(|e| e.last_used);
        let mut dropped = 0;
        for entry in entries {
            if total + extra <= self.max_bytes {
                break;
            }
            discard(&entry.path);
            total = total.saturating_sub(entry.file_len);
            dropped += 1;
        }
        dropped
    }

    /// Every valid checkpoint in the directory, payloads left on disk. Files
    /// that fail validation are deleted as they are met, which is what keeps
    /// the directory from accumulating casualties of a power cut.
    fn index(&self) -> Vec<Index> {
        let Ok(dir) = fs::read_dir(&self.dir) else { return Vec::new() };
        let mut out = Vec::new();
        for entry in dir.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some(EXTENSION) {
                continue;
            }
            match read_index(&path) {
                Some(index) => out.push(index),
                None => discard(&path),
            }
        }
        out
    }
}

/// Read and fully validate one file's header. `None` means the file must be
/// deleted: there is no failure mode worth a retry, and a checkpoint that
/// restores into a broken state is worse than one that does not restore at all.
fn read_index(path: &Path) -> Option<Index> {
    let file_len = fs::metadata(path).ok()?.len();
    if file_len < HEADER_BYTES {
        return None;
    }
    let mut file = fs::File::open(path).ok()?;
    let mut header = [0u8; HEADER_BYTES as usize];
    file.read_exact(&mut header).ok()?;
    if &header[0..4] != MAGIC || u32::from_le_bytes(header[4..8].try_into().ok()?) != FORMAT_VERSION
    {
        return None;
    }
    let last_used = u64::from_le_bytes(header[8..16].try_into().ok()?);
    let payload_len = u64::from_le_bytes(header[16..24].try_into().ok()?);
    let token_count = u32::from_le_bytes(header[24..28].try_into().ok()?) as u64;
    let key_len = u32::from_le_bytes(header[28..32].try_into().ok()?) as u64;

    // The length check is what catches a file truncated by a power cut, which
    // is the failure not fsyncing accepts.
    if file_len != HEADER_BYTES + key_len + token_count * 4 + payload_len {
        return None;
    }

    let mut key_bytes = vec![0u8; usize::try_from(key_len).ok()?];
    file.read_exact(&mut key_bytes).ok()?;
    let key = decode_key(std::str::from_utf8(&key_bytes).ok()?)?;

    let mut token_bytes = vec![0u8; usize::try_from(token_count * 4).ok()?];
    file.read_exact(&mut token_bytes).ok()?;
    let prefix_tokens: Vec<i32> =
        token_bytes.chunks_exact(4).map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();

    // The name is a hash of the key and the tokens, so re-deriving it proves
    // the header describes the checkpoint the filename claims — and makes a
    // second write of the same content a no-op rather than a duplicate.
    let expected = file_name(std::str::from_utf8(&key_bytes).ok()?, &prefix_tokens);
    if path.file_name().and_then(|n| n.to_str()) != Some(expected.as_str()) {
        return None;
    }
    if key.token_count() != prefix_tokens.len() {
        return None;
    }

    Some(Index { path: path.to_path_buf(), key, prefix_tokens, payload_len, file_len, last_used })
}

/// The payload of an already-validated file.
fn read_payload(index: &Index) -> Option<Vec<u8>> {
    let mut file = fs::File::open(&index.path).ok()?;
    let start = index.file_len - index.payload_len;
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut state = vec![0u8; usize::try_from(index.payload_len).ok()?];
    file.read_exact(&mut state).ok()?;
    Some(state)
}

fn write_temp(
    temp: &Path,
    encoded_key: &str,
    prefix_tokens: &[i32],
    state: &[u8],
) -> std::io::Result<()> {
    let mut file = fs::File::create(temp)?;
    restrict(temp);
    let mut header = [0u8; HEADER_BYTES as usize];
    header[0..4].copy_from_slice(MAGIC);
    header[4..8].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    header[8..16].copy_from_slice(&now_millis().to_le_bytes());
    header[16..24].copy_from_slice(&(state.len() as u64).to_le_bytes());
    header[24..28].copy_from_slice(&(prefix_tokens.len() as u32).to_le_bytes());
    header[28..32].copy_from_slice(&(encoded_key.len() as u32).to_le_bytes());
    file.write_all(&header)?;
    file.write_all(encoded_key.as_bytes())?;
    let mut tokens = Vec::with_capacity(prefix_tokens.len() * 4);
    for token in prefix_tokens {
        tokens.extend_from_slice(&token.to_le_bytes());
    }
    file.write_all(&tokens)?;
    file.write_all(state)?;
    Ok(())
}

/// Record that a checkpoint was just used, by rewriting eight bytes rather than
/// the payload beside them. Failure is ignored: a stale timestamp costs a
/// checkpoint its place in the retention order and nothing else.
fn touch(path: &Path) {
    let Ok(mut file) = fs::OpenOptions::new().write(true).open(path) else { return };
    if file.seek(SeekFrom::Start(LAST_USED_OFFSET)).is_ok() {
        let _ = file.write_all(&now_millis().to_le_bytes());
    }
}

fn discard(path: &Path) {
    let _ = fs::remove_file(path);
}

/// Milliseconds, not seconds: several checkpoints can easily be written inside
/// one second, and at second resolution retention could not tell them apart and
/// would drop an arbitrary one of them.
fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Name a checkpoint after its content, so storing the same one twice writes
/// nothing and two different ones can never collide.
fn file_name(encoded_key: &str, prefix_tokens: &[i32]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(FORMAT_VERSION.to_le_bytes());
    hasher.update((encoded_key.len() as u64).to_le_bytes());
    hasher.update(encoded_key.as_bytes());
    for token in prefix_tokens {
        hasher.update(token.to_le_bytes());
    }
    format!("{:x}.{EXTENSION}", hasher.finalize())
}

/// The key as one line of text. The three hashes are hex and the layer name has
/// no separator in it, so `|` cannot occur in any field.
fn encode_key(key: &PromptStateCacheKey) -> String {
    key.stable_id()
}

fn decode_key(encoded: &str) -> Option<PromptStateCacheKey> {
    PromptStateCacheKey::parse_stable_id(encoded)
}

/// `0700` on a directory, `0600` on a file. A checkpoint restores a
/// conversation's context as faithfully as replaying its transcript, so it
/// carries the same posture as the transcript (ADR 0040).
#[cfg(unix)]
fn restrict(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mode = if path.is_dir() { 0o700 } else { 0o600 };
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
}

#[cfg(not(unix))]
fn restrict(_path: &Path) {}

/// Whether `path` lives on a filesystem held in RAM. Callers deciding whether
/// to offer a disk tier at all want this before they size a budget: storing
/// checkpoints in RAM to free RAM is worse than not storing them.
pub fn path_is_memory_backed(path: &Path) -> bool {
    memory_backed(path)
}

/// Whether `path` (or the nearest existing ancestor, since it may not exist
/// yet) lives on a filesystem held in RAM.
///
/// Reads `/proc/mounts` and takes the longest mount point that is a prefix of
/// the path, which is the filesystem the path is on. `statfs` would answer in
/// one syscall, but it means hand-declaring a struct whose layout differs
/// between libcs — this needs no FFI at all, and the answer is asked for once
/// per model load.
#[cfg(target_os = "linux")]
fn memory_backed(path: &Path) -> bool {
    let mut existing = path;
    while !existing.exists() {
        match existing.parent() {
            Some(parent) => existing = parent,
            None => return false,
        }
    }
    let Ok(path) = existing.canonicalize() else { return false };
    let Ok(mounts) = fs::read_to_string("/proc/mounts") else { return false };
    let mut best: Option<(usize, bool)> = None;
    for line in mounts.lines() {
        let mut fields = line.split(' ').skip(1);
        let (Some(point), Some(fstype)) = (fields.next(), fields.next()) else { continue };
        let point = Path::new(point);
        if !path.starts_with(point) {
            continue;
        }
        let depth = point.components().count();
        if best.is_none_or(|(deepest, _)| depth >= deepest) {
            // `ramfs` is the same idea without a swap backing.
            best = Some((depth, matches!(fstype, "tmpfs" | "ramfs")));
        }
    }
    best.is_some_and(|(_, in_ram)| in_ram)
}

#[cfg(not(target_os = "linux"))]
fn memory_backed(_path: &Path) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(tokens: usize) -> PromptStateCacheKey {
        PromptStateCacheKey::new(
            PromptStateCacheLayer::HistoryPrefix,
            "runtime-abc",
            "prompt-def",
            "token-123",
            tokens,
        )
    }

    fn store(dir: &Path, budget: u64) -> CheckpointStore {
        CheckpointStore::open(dir.to_path_buf(), budget).expect("store opens")
    }

    /// A scratch directory under `target/`, deliberately **not** under
    /// `/tmp`: that is `tmpfs` on plenty of systems (including the one this was
    /// written on), and [`CheckpointStore::open`] correctly refuses a directory
    /// held in memory — so a test using it would test the refusal instead of
    /// whatever it meant to.
    fn temp_dir(name: &str) -> PathBuf {
        let dir = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/ckpt-tests"))
            .join(format!("{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_stored_checkpoint_comes_back_byte_for_byte() {
        let dir = temp_dir("roundtrip");
        let store = store(&dir, 10 * 1024 * 1024);
        let tokens = vec![1, 2, 3, 4, 5];
        let state = vec![7u8; 4096];
        assert!(store.store(&key(tokens.len()), &tokens, &state).unwrap());

        let found = store
            .lookup("runtime-abc", &[PromptStateCacheLayer::HistoryPrefix], &[1, 2, 3, 4, 5, 6])
            .expect("prefix matches");
        assert_eq!(found.prefix_tokens, tokens);
        assert_eq!(found.state, state);
        assert_eq!(found.key, key(tokens.len()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_deepest_matching_prefix_wins() {
        let dir = temp_dir("deepest");
        let store = store(&dir, 10 * 1024 * 1024);
        store.store(&key(2), &[1, 2], &[1u8; 64]).unwrap();
        store.store(&key(4), &[1, 2, 3, 4], &[2u8; 64]).unwrap();

        let found = store
            .lookup("runtime-abc", &[PromptStateCacheLayer::HistoryPrefix], &[1, 2, 3, 4, 5])
            .expect("matches");
        assert_eq!(found.prefix_tokens, vec![1, 2, 3, 4]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_prompt_that_diverges_matches_nothing() {
        let dir = temp_dir("diverge");
        let store = store(&dir, 10 * 1024 * 1024);
        store.store(&key(3), &[1, 2, 3], &[1u8; 64]).unwrap();
        assert!(store
            .lookup("runtime-abc", &[PromptStateCacheLayer::HistoryPrefix], &[1, 9, 3, 4])
            .is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn another_runtime_never_reads_this_ones_checkpoints() {
        let dir = temp_dir("runtime");
        let store = store(&dir, 10 * 1024 * 1024);
        store.store(&key(2), &[1, 2], &[1u8; 64]).unwrap();
        assert!(store
            .lookup("runtime-other", &[PromptStateCacheLayer::HistoryPrefix], &[1, 2, 3])
            .is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn storing_the_same_checkpoint_twice_writes_once() {
        let dir = temp_dir("idempotent");
        let store = store(&dir, 10 * 1024 * 1024);
        assert!(store.store(&key(2), &[1, 2], &[1u8; 64]).unwrap());
        assert!(!store.store(&key(2), &[1, 2], &[1u8; 64]).unwrap());
        assert_eq!(store.usage().1, 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_truncated_file_is_deleted_rather_than_restored() {
        let dir = temp_dir("truncated");
        let store = store(&dir, 10 * 1024 * 1024);
        store.store(&key(2), &[1, 2], &[1u8; 4096]).unwrap();
        let path = fs::read_dir(&dir).unwrap().flatten().next().unwrap().path();
        let bytes = fs::read(&path).unwrap();
        fs::write(&path, &bytes[..bytes.len() - 100]).unwrap();

        assert!(store
            .lookup("runtime-abc", &[PromptStateCacheLayer::HistoryPrefix], &[1, 2, 3])
            .is_none());
        assert!(!path.exists(), "a short file is deleted, not left to be tried again");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_from_another_format_version_is_deleted() {
        let dir = temp_dir("version");
        let store = store(&dir, 10 * 1024 * 1024);
        store.store(&key(2), &[1, 2], &[1u8; 256]).unwrap();
        let path = fs::read_dir(&dir).unwrap().flatten().next().unwrap().path();
        let mut bytes = fs::read(&path).unwrap();
        bytes[4..8].copy_from_slice(&(FORMAT_VERSION + 1).to_le_bytes());
        fs::write(&path, &bytes).unwrap();

        assert_eq!(store.usage().1, 0);
        assert!(!path.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_checkpoint_too_large_for_the_budget_is_never_written() {
        let dir = temp_dir("oversize");
        let store = store(&dir, 4096);
        assert!(!store.store(&key(2), &[1, 2], &[1u8; 8192]).unwrap());
        assert_eq!(store.usage(), (0, 0));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_budget_drops_the_least_recently_used_checkpoint() {
        let dir = temp_dir("budget");
        // Room for two 4 KiB payloads and their headers, not three.
        let store = store(&dir, 9 * 1024);
        let pause = || std::thread::sleep(std::time::Duration::from_millis(3));
        store.store(&key(2), &[1, 2], &[1u8; 4096]).unwrap();
        pause();
        store.store(&key(4), &[3, 4, 5, 6], &[2u8; 4096]).unwrap();
        pause();
        store.store(&key(6), &[7, 8, 9, 10, 11, 12], &[3u8; 4096]).unwrap();

        let (bytes, count) = store.usage();
        assert!(bytes <= store.max_bytes(), "{bytes} over {}", store.max_bytes());
        assert_eq!(count, 2);
        assert!(
            store
                .lookup("runtime-abc", &[PromptStateCacheLayer::HistoryPrefix], &[1, 2, 3])
                .is_none(),
            "the first stored checkpoint is the one dropped"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_sweep_drops_everything_the_runtime_key_no_longer_matches() {
        let dir = temp_dir("sweep");
        let store = store(&dir, 10 * 1024 * 1024);
        store.store(&key(2), &[1, 2], &[1u8; 256]).unwrap();
        assert_eq!(store.sweep("runtime-abc"), 0, "the current runtime is left alone");
        assert_eq!(store.sweep("runtime-new"), 1);
        assert_eq!(store.usage(), (0, 0));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_sweep_drops_a_checkpoint_nobody_has_returned_to() {
        let dir = temp_dir("idle");
        let store = store(&dir, 10 * 1024 * 1024);
        store.store(&key(2), &[1, 2], &[1u8; 256]).unwrap();
        store.store(&key(4), &[3, 4, 5, 6], &[2u8; 256]).unwrap();
        assert_eq!(store.sweep("runtime-abc"), 0, "both were just written");

        // Backdate one past the idle limit, exactly as 14 days of not being
        // looked at would.
        let stale = fs::read_dir(&dir).unwrap().flatten().next().unwrap().path();
        let mut file = fs::OpenOptions::new().write(true).open(&stale).unwrap();
        file.seek(SeekFrom::Start(LAST_USED_OFFSET)).unwrap();
        file.write_all(&(now_millis() - MAX_IDLE_MILLIS - 1).to_le_bytes()).unwrap();
        drop(file);

        assert_eq!(store.sweep("runtime-abc"), 1);
        assert_eq!(store.usage().1, 1, "the one still in use survives");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_zero_budget_creates_no_directory_at_all() {
        let dir = temp_dir("optout");
        assert!(CheckpointStore::open(dir.clone(), 0).is_none());
        assert!(!dir.exists(), "the absence of the directory is the proof the tier is off");
        assert!(CheckpointStore::refusal(&dir, 0).is_some());
    }

    #[test]
    fn a_memory_backed_directory_is_refused() {
        // `/dev/shm` is tmpfs on any Linux worth testing on; elsewhere the
        // check is a no-op and there is nothing to assert.
        if !cfg!(target_os = "linux") || !Path::new("/dev/shm").exists() {
            return;
        }
        let dir = Path::new("/dev/shm").join(format!("fono-ckpt-{}", std::process::id()));
        assert!(CheckpointStore::open(dir.clone(), 1024 * 1024).is_none());
        assert!(CheckpointStore::refusal(&dir, 1024 * 1024).is_some());
        assert!(!dir.exists());
    }

    #[test]
    fn clearing_leaves_nothing_behind() {
        let dir = temp_dir("clear");
        let store = store(&dir, 10 * 1024 * 1024);
        store.store(&key(2), &[1, 2], &[1u8; 256]).unwrap();
        store.store(&key(4), &[3, 4, 5, 6], &[2u8; 256]).unwrap();
        assert_eq!(store.clear(), 2);
        assert_eq!(store.usage(), (0, 0));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stray_file_is_neither_read_nor_deleted() {
        let dir = temp_dir("stray");
        let store = store(&dir, 10 * 1024 * 1024);
        let stray = dir.join("notes.txt");
        fs::write(&stray, b"not a checkpoint").unwrap();
        assert_eq!(store.usage(), (0, 0));
        assert!(stray.exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
