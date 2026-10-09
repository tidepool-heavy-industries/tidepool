//! Journal effect handler: a durable append-only run journal.
//!
//! One JSON line per `record` call — `{ts, seq, kind, key, payload}` — appended
//! and `fsync`ed before the call returns. No rewrite or compaction code path
//! exists.

use std::fmt;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use tidepool_effect::dispatch::EffectContext;
use tidepool_effect::error::EffectError;
use tidepool_mcp::CapturedOutput;
use tidepool_repr::jsonl::{self, SyncPolicy};

use super::journal_version;

// JournalReq, DescribeEffect and the EffectHandler dispatch are GENERATED from
// the `tidepool-protocol` schema — re-exported here so the
// public path (`tidepool_handlers::JournalReq`) is unchanged. Only the handler
// struct and the per-verb method body below are hand-written.
pub use crate::generated::journal::JournalReq;

// ============================================================================
// SegmentPath — a segment path mintable only by exclusively claiming it
// ============================================================================

/// A journal segment path, mintable by [`SegmentPath::create_exclusive`] for
/// a fresh segment or [`SegmentPath::open_existing`] for a serialized resume —
/// never by wrapping an arbitrary `PathBuf`. A fresh path proves an exclusive
/// claim; a resumed path proves the existing file was opened for append.
/// [`JournalHandler::new`]/[`JournalHandler::resuming`] take this instead of a
/// bare `PathBuf` so callers cannot accidentally point a handler at a path
/// without establishing the corresponding file condition first.
///
/// The host owns segment naming (ordinal picking and retry-on-collision).
/// This type owns the claim primitive only, not the naming scheme.
///
/// Cheaply `Clone` — cloning an already-claimed path is harmless aliasing,
/// not a new claim; what's walled off is MINTING one from a bare `PathBuf`.
/// `Deref<Target = Path>` + `AsRef<Path>` so ordinary path operations
/// (`.exists()`, `.display()`, and passing to `std::fs::write`)
/// work exactly as they would on the underlying path.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SegmentPath(PathBuf);

impl SegmentPath {
    /// Exclusively claim `path`: an OS-enforced atomic "this file did not
    /// exist and now it does, and I'm the one who made it so"
    /// (`OpenOptions::create_new`). An `Err` with `.kind() ==
    /// ErrorKind::AlreadyExists` is the collision case a racing allocator's
    /// retry loop matches on to try the next candidate; any other error is a
    /// genuine I/O failure (e.g. a missing parent directory).
    pub fn create_exclusive(path: PathBuf) -> std::io::Result<Self> {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| {
                std::io::Error::new(
                    error.kind(),
                    format!(
                        "exclusively create journal segment {}: {error}",
                        path.display()
                    ),
                )
            })?;
        Ok(SegmentPath(path))
    }

    /// Open a previously created segment without truncating it. The caller
    /// must serialize resumes for the journal; Exomonad's host-incarnation
    /// lease provides that process-level exclusion.
    pub fn open_existing(path: PathBuf) -> std::io::Result<Self> {
        OpenOptions::new()
            .append(true)
            .open(&path)
            .map_err(|error| {
                std::io::Error::new(
                    error.kind(),
                    format!("open existing journal segment {}: {error}", path.display()),
                )
            })?;
        Ok(SegmentPath(path))
    }

    /// Wrap a path WITHOUT claiming it — no `create_new`, no OS interaction.
    /// Escape hatch for this crate's OWN unit tests below, which
    /// deliberately construct several `JournalHandler`s over the SAME path
    /// (`new` then `resuming`, simulating one process appending across
    /// instances) or over a path whose parent doesn't exist yet (append's
    /// own mkdir-p is under test) — both patterns `create_exclusive` cannot
    /// serve. `pub(crate)` AND `#[cfg(test)]`: never reachable from another
    /// crate (not even that crate's own test builds — `cfg(test)` gates on
    /// THIS crate being under test, not the caller), so this is not a second
    /// public construction path.
    #[cfg(test)]
    pub(crate) fn for_test(path: PathBuf) -> Self {
        SegmentPath(path)
    }
}

impl std::ops::Deref for SegmentPath {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for SegmentPath {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl PartialEq<PathBuf> for SegmentPath {
    fn eq(&self, other: &PathBuf) -> bool {
        &self.0 == other
    }
}

impl PartialEq<SegmentPath> for PathBuf {
    fn eq(&self, other: &SegmentPath) -> bool {
        self == &other.0
    }
}

// ============================================================================
// The handler
// ============================================================================

/// Bits of a composed `seq` ([`compose_journal_seq`]) reserved for the
/// segment-LOCAL counter; the remaining high bits are the segment ordinal.
/// 32 bits of local counter is far past any run's real per-segment append
/// count (one entry per completed step, never per token — see
/// [`JournalHandler::append`]'s doc), so this is not a practical ceiling.
const LOCAL_SEQ_BITS: u32 = 32;

/// Compose a journal entry's full `seq` from the segment it was written into
/// and its position within that segment: the segment ordinal in the high
/// bits, a per-segment-local counter (starting at `0`) in the low bits.
///
/// This makes `seq` unique across writers without cross-process coordination:
/// each writer gets a distinct segment ordinal from an exclusive path claim,
/// and each local counter starts at zero.
///
/// When segment ordinals increase over time, `seq` also increases in physical
/// write order across segments.
pub fn compose_journal_seq(segment_ordinal: u64, local_seq: u64) -> u64 {
    debug_assert!(
        local_seq < (1u64 << LOCAL_SEQ_BITS),
        "local_seq overflowed into the segment-ordinal bits of a composed journal seq"
    );
    (segment_ordinal << LOCAL_SEQ_BITS) | local_seq
}

#[derive(Clone, Debug)]
pub struct JournalHandler {
    path: PathBuf,
    // The segment ordinal every seq this handler writes is composed against
    // (see [`compose_journal_seq`]) — fixed for the handler's lifetime, one
    // per process's own exclusively-claimed segment.
    segment_ordinal: u64,
    // The segment-LOCAL counter: monotonic for the lifetime of THIS handler
    // instance, always starting at `0` regardless of fresh or resumed —
    // uniqueness across handler instances comes from `segment_ordinal`
    // differing, not from where this counter starts. Allocated under the
    // SAME lock as the write itself (see [`Self::append`]), so seq order and
    // physical (on-disk) order coincide within one segment instead of racing.
    local_seq: Arc<AtomicU64>,
    // Serializes an entire append — seq allocation, open, write, fsync —
    // across every clone of this handler sharing `path`. See [`Self::append`]
    // for why durability rests on this rather than on syscall atomicity.
    lock: Arc<Mutex<()>>,
}

impl JournalHandler {
    /// One journal file over an already-claimed [`SegmentPath`] (typically
    /// one process's own SEGMENT of a run; the resume layer decides that
    /// path and claims it, never this type). Composes `seq` at segment
    /// ordinal `0` — right for a FRESH run's first process, whose segment
    /// always IS ordinal 0. A process continuing a run a prior process
    /// already wrote to must use [`Self::resuming`] instead, naming its OWN
    /// (necessarily different) segment ordinal.
    ///
    /// Stamps the segment's version header as its first line — see
    /// [`Self::stamp_header_if_fresh`]. Fallible for exactly that reason:
    /// unlike before, construction now does real I/O.
    pub fn new(path: SegmentPath) -> Result<Self, JournalAppendError> {
        Self::construct(path, 0)
    }

    /// Append to an existing journal, composing every `seq` against this
    /// writer's own `segment_ordinal` (see [`compose_journal_seq`]).
    /// `segment_ordinal` is exactly what
    /// the acquired lease carries — the ordinal
    /// [`SegmentPath::create_exclusive`] claimed for
    /// this process's segment, never computed here. Opening in append mode
    /// is unchanged; nothing here reads or rewrites the file.
    pub fn resuming(path: SegmentPath, segment_ordinal: u64) -> Result<Self, JournalAppendError> {
        Self::construct(path, segment_ordinal)
    }

    fn construct(path: SegmentPath, segment_ordinal: u64) -> Result<Self, JournalAppendError> {
        let handler = Self {
            path: path.0,
            segment_ordinal,
            local_seq: Arc::new(AtomicU64::new(0)),
            lock: Arc::new(Mutex::new(())),
        };
        handler.stamp_header_if_fresh()?;
        Ok(handler)
    }

    fn ensure_parent_dir(&self) -> Result<(), JournalAppendError> {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|source| {
                    JournalAppendError::CreateDir {
                        path: parent.to_path_buf(),
                        source,
                    }
                })?;
            }
        }
        Ok(())
    }

    /// Write this segment's version-stamp header line — ONCE, at segment
    /// birth. `SegmentPath::create_exclusive` guarantees the file this
    /// handler owns is empty at construction time (an exclusive
    /// `create_new` claim), so "is the file empty" is an exact proxy for
    /// "is this handler the FIRST to ever construct over this path" — the
    /// one case that legitimately skips the header is this crate's own test
    /// escape hatch ([`SegmentPath::for_test`]) constructing a SECOND,
    /// independent handler over a path a prior handler already wrote to
    /// (never a real segment, which is always exclusively claimed).
    fn stamp_header_if_fresh(&self) -> Result<(), JournalAppendError> {
        self.ensure_parent_dir()?;
        let is_fresh = std::fs::metadata(&self.path)
            .map(|m| m.len() == 0)
            .unwrap_or(true);
        if !is_fresh {
            return Ok(());
        }
        let header = serde_json::json!({"version": journal_version::CURRENT}).to_string();
        jsonl::append_new_line(&self.path, &header, SyncPolicy::Data).map_err(|source| {
            JournalAppendError::Write {
                path: self.path.clone(),
                source,
            }
        })
    }

    /// Append one entry, DURABLY, before returning. Parent directories are
    /// created as needed. No rewrite, truncate, or compaction path exists —
    /// this always opens in append mode.
    ///
    /// The whole append — allocating `seq`, opening the file, writing the
    /// line, and forcing it to storage — runs under one lock shared by every
    /// clone of this handler. That lock, not syscall-level atomicity, is what
    /// makes a concurrent burst of appends never tear a line: `write_all`
    /// issues exactly one `write()` call only on a FULL write: on a short
    /// write it loops internally, and a later loop iteration's `write()` is
    /// no longer atomic against a concurrent `O_APPEND` writer. Mutual
    /// exclusion holds regardless of how many `write()` calls one append
    /// takes.
    ///
    /// `sync_data()`, not `flush()`, is what makes this durable: `flush()` on
    /// a `File` is a no-op (there is no userspace buffer to flush — the bytes
    /// already reached the kernel via `write_all`), so it only ever proved the
    /// write reached the page cache. Without an explicit fsync, a `record`
    /// call can return `Ok` and then the entry can still vanish on power
    /// loss — exactly the gap an acceptance test asserting "crash, resume,
    /// only the delta" is silently trusting not to exist. One entry per
    /// COMPLETED step (never per token), so paying one `sync_data()` per
    /// append is the right trade, and its failure is treated as a write
    /// failure: a journal write failure already aborts the run, and "the disk
    /// did not durably take it" is exactly that.
    fn append(
        &self,
        kind: String,
        key: String,
        payload: serde_json::Value,
    ) -> Result<(), JournalAppendError> {
        self.ensure_parent_dir()?;
        let _guard = self.lock.lock();
        let local = self.local_seq.fetch_add(1, Ordering::SeqCst);
        let seq = compose_journal_seq(self.segment_ordinal, local);
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let line = serde_json::to_string(&serde_json::json!({
            "ts": ts,
            "seq": seq,
            "kind": kind,
            "key": key,
            "payload": payload,
        }))
        .map_err(|source| JournalAppendError::Serialize {
            path: self.path.clone(),
            source,
        })?;
        // `append_new_line` folds open+write+fsync into one call; this
        // handler's own lock above already serializes the WHOLE operation
        // (seq allocation through fsync) across every clone sharing `path` —
        // see this method's doc for why that, not syscall atomicity, is what
        // makes a concurrent burst never tear a line. `SyncPolicy::Data`
        // preserves the original `sync_data` (not `sync_all`) choice.
        jsonl::append_new_line(&self.path, &line, SyncPolicy::Data).map_err(|source| {
            // The shared primitive doesn't distinguish open/write/sync
            // failures; classify a NotFound (a directory that vanished
            // between the mkdir-p above and this open) as Open, everything
            // else as Write — a coarser but still-legible split than before.
            if source.kind() == std::io::ErrorKind::NotFound {
                JournalAppendError::Open {
                    path: self.path.clone(),
                    source,
                }
            } else {
                JournalAppendError::Write {
                    path: self.path.clone(),
                    source,
                }
            }
        })?;
        Ok(())
    }

    // `pub(crate)` rather than private: the generated dispatch arm lives in a
    // sibling module (`crate::generated::journal`) now, not expanded inline
    // here.
    pub(crate) fn record_step(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
        kind: String,
        key: String,
        payload: crate::effect_glue::JsonArg,
    ) -> Result<tidepool_effect::Response, EffectError> {
        self.append(kind, key, payload.0)
            .map_err(|e| EffectError::Handler(e.to_string()))?;
        cx.respond(())
    }

    /// The run's sibling TRACE stream path, derived from this handler's own
    /// journal segment: same directory, `journal-` filename prefix swapped
    /// for `trace-` (fallback: a `.trace.jsonl` suffix on the same stem).
    /// Per-process by construction — the journal segment is exclusively
    /// claimed, so the derived trace path inherits single-writer safety
    /// without a second claim protocol.
    fn trace_path(&self) -> PathBuf {
        let name = self
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("journal.jsonl");
        let trace_name = if let Some(rest) = name.strip_prefix("journal-") {
            format!("trace-{rest}")
        } else {
            format!("{name}.trace.jsonl")
        };
        self.path.with_file_name(trace_name)
    }

    /// Append one observability entry to the trace stream — decision
    /// narration and telemetry. Envelope:
    /// a `{"version": 1}` header on a fresh file, then one
    /// `{ts, seq, stage, key, payload}` line per call — `ts` is
    /// milliseconds since the Unix epoch, stamped HERE so every consumer's
    /// lines merge into one timeline; `seq` reuses the journal's composed
    /// scheme for provenance. Payload shape is deliberately free to evolve;
    /// the envelope is the durable part. Same durability discipline as the
    /// journal (locked open+write+fsync): a trace the process lied about
    /// writing would be observability that vanishes exactly when it matters
    /// (a crash), which is when it is read.
    pub(crate) fn trace_step(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
        stage: String,
        key: String,
        payload: crate::effect_glue::JsonArg,
    ) -> Result<tidepool_effect::Response, EffectError> {
        // Observability must never stop the work it observes: unlike `record`,
        // a trace failure degrades to a warning.
        if let Err(e) = self.append_trace(stage, key, payload.0) {
            tracing::warn!("trace append failed (observability degraded, run continues): {e}");
        }
        cx.respond(())
    }

    fn append_trace(
        &self,
        stage: String,
        key: String,
        payload: serde_json::Value,
    ) -> Result<(), JournalAppendError> {
        self.ensure_parent_dir()?;
        let path = self.trace_path();
        let _guard = self.lock.lock();
        let is_fresh = std::fs::metadata(&path)
            .map(|m| m.len() == 0)
            .unwrap_or(true);
        if is_fresh {
            let header = serde_json::json!({"version": 1}).to_string();
            jsonl::append_new_line(&path, &header, SyncPolicy::Data).map_err(|source| {
                JournalAppendError::Write {
                    path: path.clone(),
                    source,
                }
            })?;
        }
        let local = self.local_seq.fetch_add(1, Ordering::SeqCst);
        let seq = compose_journal_seq(self.segment_ordinal, local);
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let line = serde_json::json!({
            "ts": ts,
            "seq": seq,
            "stage": stage,
            "key": key,
            "payload": payload,
        })
        .to_string();
        jsonl::append_new_line(&path, &line, SyncPolicy::Data)
            .map_err(|source| JournalAppendError::Write { path, source })
    }
}

/// Why a journal `append` failed to durably record an entry — the operation
/// that failed, plus the path it was operating on. Converted to
/// [`EffectError::Handler`] at the effect boundary (a journal write failure
/// already aborts the run, locked, and this only makes WHY it aborted
/// legible instead of a hand-formatted string).
#[derive(Debug)]
pub enum JournalAppendError {
    CreateDir {
        path: PathBuf,
        source: std::io::Error,
    },
    Serialize {
        path: PathBuf,
        source: serde_json::Error,
    },
    Open {
        path: PathBuf,
        source: std::io::Error,
    },
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    Sync {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl fmt::Display for JournalAppendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JournalAppendError::CreateDir { path, source } => {
                write!(f, "journal: failed to create dir {path:?}: {source}")
            }
            JournalAppendError::Serialize { path, source } => {
                write!(f, "journal: serialize failed: {source} (path {path:?})")
            }
            JournalAppendError::Open { path, source } => {
                write!(f, "journal: failed to open {path:?}: {source}")
            }
            JournalAppendError::Write { path, source } => {
                write!(f, "journal: write failed: {source} (path {path:?})")
            }
            JournalAppendError::Sync { path, source } => {
                write!(f, "journal: fsync failed: {source} (path {path:?})")
            }
        }
    }
}

impl std::error::Error for JournalAppendError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn exclusive_segment_claim_refuses_collision_without_changing_existing_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("claimed.jsonl");
        SegmentPath::create_exclusive(path.clone()).unwrap();
        std::fs::write(&path, b"retained journal bytes\n").unwrap();
        let refusal = SegmentPath::create_exclusive(path.clone()).unwrap_err();
        assert_eq!(refusal.kind(), std::io::ErrorKind::AlreadyExists);
        assert!(refusal
            .to_string()
            .contains("exclusively create journal segment"));
        assert!(refusal.to_string().contains(&path.display().to_string()));
        assert_eq!(std::fs::read(&path).unwrap(), b"retained journal bytes\n");
        SegmentPath::open_existing(path.clone()).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"retained journal bytes\n");
    }

    #[test]
    fn opening_missing_segment_reports_path_and_does_not_create_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("absent.jsonl");
        let refusal = SegmentPath::open_existing(path.clone()).unwrap_err();
        assert_eq!(refusal.kind(), std::io::ErrorKind::NotFound);
        assert!(refusal
            .to_string()
            .contains("open existing journal segment"));
        assert!(refusal.to_string().contains(&path.display().to_string()));
        assert!(!path.exists());
    }

    fn tmp_file(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "tidepool_journal_{label}_{}.jsonl",
            std::process::id()
        ))
    }

    fn tmp_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "tidepool_journal_dir_{label}_{}",
            std::process::id()
        ))
    }

    fn rows(path: &Path) -> Vec<serde_json::Value> {
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .filter(|row: &serde_json::Value| row.get("kind").is_some())
            .collect()
    }

    #[test]
    fn append_preserves_the_entry_wire_shape_and_timestamp() {
        let path = tmp_file("append-wire");
        std::fs::remove_file(&path).ok();
        let handler = JournalHandler::new(SegmentPath::for_test(path.clone())).unwrap();
        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        handler
            .append(
                "split".into(),
                "branch/a".into(),
                serde_json::json!({"n": 1}),
            )
            .unwrap();
        let after = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let entries = rows(&path);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["seq"], 0);
        assert_eq!(entries[0]["kind"], "split");
        assert_eq!(entries[0]["key"], "branch/a");
        assert_eq!(entries[0]["payload"], serde_json::json!({"n": 1}));
        assert!((before..=after).contains(&entries[0]["ts"].as_u64().unwrap()));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn seq_is_monotonic_within_a_segment() {
        let path = tmp_file("seq");
        std::fs::remove_file(&path).ok();
        let handler = JournalHandler::new(SegmentPath::for_test(path.clone())).unwrap();
        for i in 0..5 {
            handler
                .append("step".into(), format!("key{i}"), serde_json::json!(i))
                .unwrap();
        }
        let seqs: Vec<u64> = rows(&path)
            .iter()
            .map(|entry| entry["seq"].as_u64().unwrap())
            .collect();
        assert_eq!(seqs, vec![0, 1, 2, 3, 4]);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn resuming_at_a_distinct_ordinal_keeps_seq_disjoint() {
        let path = tmp_file("resuming");
        std::fs::remove_file(&path).ok();
        let first =
            JournalHandler::new(SegmentPath::create_exclusive(path.clone()).unwrap()).unwrap();
        for i in 0..3 {
            first
                .append("step".into(), format!("k{i}"), serde_json::json!(i))
                .unwrap();
        }
        let second =
            JournalHandler::resuming(SegmentPath::open_existing(path.clone()).unwrap(), 1).unwrap();
        for i in 3..6 {
            second
                .append("step".into(), format!("k{i}"), serde_json::json!(i))
                .unwrap();
        }
        let seqs: Vec<u64> = rows(&path)
            .iter()
            .map(|entry| entry["seq"].as_u64().unwrap())
            .collect();
        assert_eq!(
            seqs,
            (0..3)
                .map(|n| compose_journal_seq(0, n))
                .chain((0..3).map(|n| compose_journal_seq(1, n)))
                .collect::<Vec<_>>()
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn concurrent_resumes_never_collide_on_seq() {
        let seg_a = tmp_file("concurrent-resume-a");
        let seg_b = tmp_file("concurrent-resume-b");
        std::fs::remove_file(&seg_a).ok();
        std::fs::remove_file(&seg_b).ok();
        let a = JournalHandler::resuming(SegmentPath::for_test(seg_a.clone()), 5).unwrap();
        let b = JournalHandler::resuming(SegmentPath::for_test(seg_b.clone()), 6).unwrap();
        for i in 0..4 {
            a.append("step".into(), format!("a{i}"), serde_json::json!(i))
                .unwrap();
            b.append("step".into(), format!("b{i}"), serde_json::json!(i))
                .unwrap();
        }
        let seqs_a: Vec<u64> = rows(&seg_a)
            .iter()
            .map(|row| row["seq"].as_u64().unwrap())
            .collect();
        let seqs_b: Vec<u64> = rows(&seg_b)
            .iter()
            .map(|row| row["seq"].as_u64().unwrap())
            .collect();
        assert!(seqs_a.iter().all(|seq| !seqs_b.contains(seq)));
        std::fs::remove_file(&seg_a).ok();
        std::fs::remove_file(&seg_b).ok();
    }

    #[test]
    fn parent_dir_creation_works() {
        let base = tmp_dir("parentdir");
        std::fs::remove_dir_all(&base).ok();
        let path = base.join("nested").join("run.jsonl");
        let handler = JournalHandler::new(SegmentPath::for_test(path.clone())).unwrap();
        handler
            .append("step".into(), "a".into(), serde_json::json!(1))
            .unwrap();
        assert_eq!(rows(&path).len(), 1);
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn create_dir_failure_is_a_structured_error() {
        let base = tmp_dir("createdir_conflict");
        std::fs::remove_dir_all(&base).ok();
        std::fs::create_dir_all(&base).unwrap();
        let blocker = base.join("blocker");
        std::fs::write(&blocker, b"not a directory").unwrap();
        let err = JournalHandler::new(SegmentPath::for_test(blocker.join("journal.jsonl")))
            .expect_err("a file in place of the parent directory must fail create_dir_all");
        assert!(matches!(err, JournalAppendError::CreateDir { path: ref p, .. } if p == &blocker));
        assert!(err.to_string().contains("failed to create dir"));
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn trace_lands_in_sibling_stream_with_ts_envelope() {
        let dir = tmp_dir("trace");
        std::fs::remove_dir_all(&dir).ok();
        let path = dir.join("journal-run1.0.jsonl");
        let handler = JournalHandler::new(SegmentPath::for_test(path)).unwrap();
        handler
            .append_trace(
                "park".into(),
                "loop".into(),
                serde_json::json!({"why": "completed"}),
            )
            .unwrap();
        let trace = std::fs::read_to_string(dir.join("trace-run1.0.jsonl")).unwrap();
        let rows: Vec<serde_json::Value> = trace
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(rows[0], serde_json::json!({"version": 1}));
        assert_eq!(rows[1]["stage"], "park");
        assert!(rows[1]["ts"].as_u64().unwrap() > 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn fresh_segment_gets_a_header_line() {
        let path = tmp_file("header");
        std::fs::remove_file(&path).ok();
        let _handler = JournalHandler::new(SegmentPath::for_test(path.clone())).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        let header: serde_json::Value =
            serde_json::from_str(contents.lines().next().unwrap()).unwrap();
        assert_eq!(
            header,
            serde_json::json!({"version": journal_version::CURRENT})
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn concurrent_burst_through_cloned_handlers_yields_complete_wire_rows() {
        let path = tmp_file("concurrent_burst");
        std::fs::remove_file(&path).ok();
        let handler = JournalHandler::new(SegmentPath::for_test(path.clone())).unwrap();
        const THREADS: usize = 8;
        const PER_THREAD: usize = 50;
        std::thread::scope(|scope| {
            for t in 0..THREADS {
                let handler = handler.clone();
                scope.spawn(move || {
                    for i in 0..PER_THREAD {
                        handler
                            .append(
                                "burst".into(),
                                format!("t{t}-{i}"),
                                serde_json::json!({"t": t, "i": i}),
                            )
                            .unwrap();
                    }
                });
            }
        });
        let entries = rows(&path);
        assert_eq!(entries.len(), THREADS * PER_THREAD);
        let mut seqs: Vec<u64> = entries
            .iter()
            .map(|entry| entry["seq"].as_u64().unwrap())
            .collect();
        seqs.sort_unstable();
        seqs.dedup();
        assert_eq!(seqs.len(), THREADS * PER_THREAD);
        std::fs::remove_file(&path).ok();
    }
}

impl tidepool_mcp::InstalledEffectSupport for JournalHandler {
    fn installed_effect_support(&self) -> Vec<exomonad_tool::ToolEffectKey> {
        vec![exomonad_tool::ActorEffectKey::Journal.into()]
    }
}
