//! Journal effect handler: a durable append-only run journal.
//!
//! One JSON line per `record` call — `{seq, kind, key, payload}` — appended
//! and `fsync`ed before the call returns. No rewrite or compaction code path
//! exists. The fold API below (`load_journal`/`last_by_key`/`last_by_kind_key`)
//! is for the swarm driver's boot-time resume; nothing here wires it in
//! (`tidepool_harness::selfharness::resume` is what does).

use std::collections::HashMap;
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
use tidepool_repr::version_ladder::{self, LadderError};

use super::journal_version::{self, MIGRATIONS};

// JournalReq, DescribeEffect and the EffectHandler dispatch are GENERATED from
// the `tidepool-protocol` schema — re-exported here so the
// public path (`tidepool_handlers::JournalReq`) is unchanged. Only the handler
// struct and the per-verb method body below are hand-written.
pub use crate::generated::journal::JournalReq;

// ============================================================================
// Entries + the fold API (for the swarm driver's boot-time resume — not
// wired to anything here)
// ============================================================================

/// One durable journal entry, as it round-trips to/from a JSON line.
#[derive(Debug, Clone, PartialEq)]
pub struct JournalEntry {
    pub seq: u64,
    pub kind: String,
    pub key: String,
    pub payload: serde_json::Value,
}

impl JournalEntry {
    /// The entry's wire shape — the SAME object a journal line carries and
    /// the same one a boot-time fold ships to the authored side
    /// (`Tidepool.Resume`'s `ResumeEntry` decodes exactly these four names).
    /// Public so the fold's encoder reuses this one spelling instead of
    /// re-deriving it in another crate, where the two could drift apart.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "seq": self.seq,
            "kind": self.kind,
            "key": self.key,
            "payload": self.payload,
        })
    }

    fn from_json(v: &serde_json::Value) -> Result<Self, JournalParseError> {
        let seq =
            v.get("seq")
                .and_then(serde_json::Value::as_u64)
                .ok_or(JournalParseError::Field {
                    field: "seq",
                    reason: "missing or non-integer",
                })?;
        let kind = v
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .ok_or(JournalParseError::Field {
                field: "kind",
                reason: "missing or non-string",
            })?
            .to_string();
        let key = v
            .get("key")
            .and_then(serde_json::Value::as_str)
            .ok_or(JournalParseError::Field {
                field: "key",
                reason: "missing or non-string",
            })?
            .to_string();
        let payload = v.get("payload").cloned().ok_or(JournalParseError::Field {
            field: "payload",
            reason: "missing",
        })?;
        Ok(JournalEntry {
            seq,
            kind,
            key,
            payload,
        })
    }
}

/// Why one journal line failed to parse into a [`JournalEntry`] — the
/// line/field context [`JournalLoadError::TornMidFile`] carries, and the same
/// detail a torn FINAL line's `tracing::warn!` reports (that one is never an
/// error — see [`load_journal`]).
#[derive(Debug)]
pub enum JournalParseError {
    /// The line was not valid JSON at all.
    NotJson(serde_json::Error),
    /// The line parsed as JSON but a required field was missing or had the
    /// wrong type.
    Field {
        field: &'static str,
        reason: &'static str,
    },
}

impl fmt::Display for JournalParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JournalParseError::NotJson(e) => write!(f, "{e}"),
            JournalParseError::Field { field, reason } => write!(f, "{reason} \"{field}\""),
        }
    }
}

impl std::error::Error for JournalParseError {}

/// Why [`load_journal`] refused to load a journal file. A torn FINAL line
/// (the crash-mid-append case) is not one of these — it is skipped with a
/// `tracing::warn!` and left out of the returned entries, never an error.
#[derive(Debug)]
pub enum JournalLoadError {
    /// Opening or reading the file itself failed. Never a missing file (that
    /// is `Ok(vec![])`, per this function's doc) — a real I/O failure:
    /// permissions, a bad fd, disk trouble.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// A line before the last one failed to parse. The journal is
    /// append-only and every write but the last is complete by
    /// construction, so this means real corruption — never silently
    /// absorbed the way a torn final line is.
    TornMidFile {
        path: PathBuf,
        /// ONE-based file line number (the first line is `1`), matching what
        /// an operator sees in a text editor or `sed -n '<n>p'` — not the
        /// zero-based array index [`load_journal`] iterates with.
        line_no: usize,
        /// [`JournalParseError`]'s `Display` text — a `String` rather than
        /// the typed error itself, since the shared
        /// [`tidepool_repr::jsonl::read_tail`] this now runs
        /// through is schema-agnostic and only carries `parse`'s `Err` as
        /// text.
        detail: String,
    },
    /// This segment's version is below the floor this build still carries a
    /// migration path from — never a silent reset.
    BelowFloor {
        path: PathBuf,
        found: u32,
        floor: u32,
    },
    /// This segment's version is newer than this build knows how to read.
    FutureVersion {
        path: PathBuf,
        found: u32,
        current: u32,
    },
}

impl fmt::Display for JournalLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JournalLoadError::Io { path, source } => {
                write!(f, "journal I/O error on {path:?}: {source}")
            }
            JournalLoadError::TornMidFile {
                path,
                line_no,
                detail,
            } => write!(
                f,
                "journal {path:?} corrupted at line {line_no} (not the final line): {detail}"
            ),
            JournalLoadError::BelowFloor { path, found, floor } => write!(
                f,
                "journal segment {path:?} version {found} is below the floor this build still \
                 supports ({floor}) — archive or delete it and start a fresh run, or read it \
                 with an older tidepool build that still supports version {found}"
            ),
            JournalLoadError::FutureVersion {
                path,
                found,
                current,
            } => write!(
                f,
                "journal segment {path:?} version {found} is newer than this build supports \
                 (current {current}) — rebuild against a newer tidepool, or archive/delete the \
                 segment and start fresh"
            ),
        }
    }
}

fn ladder_err_to_load_err(e: LadderError, path: &Path) -> JournalLoadError {
    match e {
        LadderError::BelowFloor { found, floor } => JournalLoadError::BelowFloor {
            path: path.to_path_buf(),
            found,
            floor,
        },
        LadderError::UnsupportedVersion { found, current } => JournalLoadError::FutureVersion {
            path: path.to_path_buf(),
            found,
            current,
        },
        LadderError::Migration { from, source } => JournalLoadError::TornMidFile {
            path: path.to_path_buf(),
            line_no: 0,
            detail: format!("migration from version {from} failed: {}", source.0),
        },
    }
}

/// Distinguishes the segment's version-stamp header line (`{"version": N}`,
/// no `"kind"` key) from an ordinary [`JournalEntry`] row (always has
/// `"kind"`). Only ever checked against the FIRST raw line — see
/// [`load_journal`].
fn is_segment_header(v: &serde_json::Value) -> bool {
    v.get("kind").is_none() && v.get("version").is_some()
}

impl std::error::Error for JournalLoadError {}

/// Load a journal file into its entries, in append order. A MISSING file is
/// an empty journal (`Ok(vec![])`), not an error — a run that has not
/// recorded anything yet has no file on disk. A torn FINAL line (a crash
/// mid-append) is skipped with a `tracing::warn!`; a torn line anywhere else
/// is loud (`Err(JournalLoadError::TornMidFile)`) — the journal is
/// append-only, so only the very last write can ever be incomplete.
pub fn load_journal(path: &Path) -> Result<Vec<JournalEntry>, JournalLoadError> {
    // `TailPolicy::Observe`: a fold reads SEGMENT files it does not own (see
    // `tidepool_harness::selfharness::resume`), so a torn tail is reported
    // but never truncated — see `tidepool_repr::jsonl`'s module doc.
    //
    // Parsed as raw `Value` first, not directly into `JournalEntry`: a
    // segment written by a stamping `JournalHandler` has a header line
    // (`{"version": N}`) as line one that an OLDER segment (predating this
    // scheme) never had — every line in a pre-scheme segment is a plain
    // entry — and `read_tail`'s single `parse` closure has no way to know
    // in advance which shape a given line is. A shape-level (valid JSON,
    // wrong fields) failure on the true final line therefore no longer
    // benefits from `read_tail`'s own torn-tail forgiveness — only
    // JSON-syntax corruption does — the loop below restores that
    // forgiveness itself, so the net behavior for a torn write is
    // unchanged.
    let (raw_lines, torn) = jsonl::read_tail(
        path,
        |l| serde_json::from_str::<serde_json::Value>(l).map_err(|e| e.to_string()),
        jsonl::TailPolicy::Observe,
    )
    .map_err(|e| match e {
        jsonl::JsonlReadError::Io(source) => JournalLoadError::Io {
            path: path.to_path_buf(),
            source,
        },
        jsonl::JsonlReadError::TornMidFile { line_no, detail } => JournalLoadError::TornMidFile {
            path: path.to_path_buf(),
            line_no,
            detail,
        },
    })?;
    if let Some(torn) = &torn {
        tracing::warn!(
            "journal {:?}: torn final line skipped (crash mid-append?): {}",
            path,
            torn.reason
        );
    }

    let (found, header_lines, rest): (u32, usize, &[serde_json::Value]) =
        match raw_lines.split_first() {
            Some((first, rest)) if is_segment_header(first) => {
                (version_ladder::found_version(first), 1, rest)
            }
            // No header at all: a segment written before this scheme
            // existed, where every line is a plain entry — the whole file
            // reads as version 0.
            _ => (0, 0, &raw_lines[..]),
        };
    // The header's own bounds must be validated even when the segment has
    // zero entries after it yet.
    version_ladder::migrate_to_current(
        serde_json::Value::Null,
        found,
        journal_version::FLOOR,
        journal_version::CURRENT,
        MIGRATIONS,
    )
    .map_err(|e| ladder_err_to_load_err(e, path))?;

    let n = rest.len();
    let mut entries = Vec::with_capacity(n);
    for (i, raw) in rest.iter().enumerate() {
        let migrated = version_ladder::migrate_to_current(
            raw.clone(),
            found,
            journal_version::FLOOR,
            journal_version::CURRENT,
            MIGRATIONS,
        )
        .map_err(|e| ladder_err_to_load_err(e, path))?;
        match JournalEntry::from_json(&migrated) {
            Ok(entry) => entries.push(entry),
            Err(parse_err) => {
                let is_final_and_untorn = i == n - 1 && torn.is_none();
                if is_final_and_untorn {
                    tracing::warn!(
                        "journal {:?}: torn final line skipped (shape, crash mid-append?): {}",
                        path,
                        parse_err
                    );
                    break;
                }
                return Err(JournalLoadError::TornMidFile {
                    path: path.to_path_buf(),
                    line_no: header_lines + i + 1,
                    detail: parse_err.to_string(),
                });
            }
        }
    }
    Ok(entries)
}

/// Fold entries down to the LAST record per `key` (later `seq` wins) — the
/// shape a boot-time resume wants: "what do I already know about this
/// branch/task". Not wired into anything here; the swarm driver injects this
/// at boot.
///
/// Answers a NARROWER question than [`last_by_kind_key`], and both are kept:
/// this one is "the latest thing recorded about `key`, whatever kind it was",
/// which is the right answer when a caller's keys carry one kind of fact each.
/// A caller recording SEVERAL kinds under one key (a `"split"` and an
/// `"outcome"` for the same branch) wants [`last_by_kind_key`] — this one
/// collapses them.
pub fn last_by_key(entries: &[JournalEntry]) -> HashMap<String, JournalEntry> {
    let mut out = HashMap::new();
    for entry in entries {
        out.insert(entry.key.clone(), entry.clone());
    }
    out
}

/// Fold entries down to the last record per `(kind, key)` PAIR — the shape
/// boot-time resume wants when one key carries several kinds of fact (dev-tree
/// records a `"split"`, an `"outcome"`, a `"replan"` and a `"rebase"` all under
/// the same branch name; [`last_by_key`] would collapse the split under the
/// outcome and lose the recorded plan).
///
/// The winner is the LAST entry in `entries` — POSITION, never `seq`. Correct
/// only when the caller supplies `entries` in the run's TRUE PHYSICAL WRITE
/// ORDER: for a segmented journal (`tidepool_harness::selfharness::resume`),
/// that means every segment's entries concatenated in segment order, each
/// segment's own entries already in this function's append order. That
/// physical order is exactly the durable byte sequence a crash leaves behind,
/// so folding on it is folding over the real evidence.
///
/// `seq` is written to every entry as PROVENANCE — composed from each
/// process's own exclusively-claimed segment ordinal ([`compose_journal_seq`]),
/// which makes it run-GLOBALLY UNIQUE by construction, with no cross-process
/// coordination beyond the segment claim itself (see that function's doc) —
/// but it is deliberately NOT what this fold sorts on. Folding on `seq`
/// instead would let a foreign, hand-edited, or mis-seeded segment carrying a
/// `seq` that contradicts physical order silently INVERT the result — not
/// merely lose an entry, but pick the wrong one as the winner. Position can't
/// be inverted that way: the caller's supplied order IS the order folded on.
pub fn last_by_kind_key(entries: &[JournalEntry]) -> HashMap<(String, String), JournalEntry> {
    let mut out: HashMap<(String, String), JournalEntry> = HashMap::new();
    for entry in entries {
        out.insert((entry.kind.clone(), entry.key.clone()), entry.clone());
    }
    out
}

// ============================================================================
// SegmentPath — a segment path mintable only by exclusively claiming it
// ============================================================================

/// A journal segment path, mintable ONLY by [`SegmentPath::create_exclusive`]
/// — never by wrapping an arbitrary `PathBuf`. The invariant this buys:
/// holding a `SegmentPath` is proof the underlying file was exclusively
/// claimed (`OpenOptions::create_new`), not merely a promise that some
/// caller meant to claim it first. [`JournalHandler::new`]/[`JournalHandler::resuming`]
/// take this instead of a bare `PathBuf` so "a handler pointed at a segment
/// nobody allocated" is a type error to construct, not a runtime hazard to
/// remember to avoid.
///
/// `tidepool_harness::selfharness::resume::allocate_segment` is the one
/// legitimate non-test caller: it owns the segment NAMING scheme (ordinal
/// picking, retry-on-collision) and calls [`Self::create_exclusive`] on each
/// candidate path in turn. This type owns the CLAIM primitive only, not the
/// naming scheme — `tidepool-handlers` sits below `tidepool-harness` in the
/// crate graph (see this module's other doc comments on why this crate
/// never decides where a segment lives), so the naming scheme cannot live
/// here.
///
/// Cheaply `Clone` — cloning an already-claimed path is harmless aliasing,
/// not a new claim; what's walled off is MINTING one from a bare `PathBuf`.
/// `Deref<Target = Path>` + `AsRef<Path>` so ordinary path operations
/// (`.exists()`, `.display()`, passing to `std::fs::write`/`load_journal`)
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
            .open(&path)?;
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
/// This is what makes `seq` run-GLOBALLY UNIQUE by construction, with NO
/// cross-process coordination beyond the segment claim itself: two processes
/// racing to resume the same lease at once (`tidepool_harness::selfharness
/// ::resume::acquire_lease`'s warn-never-refuse alive-pid policy — a wedged
/// pid must never block a resume, so this cannot lean on refusing the race)
/// always land on DISTINCT segment ordinals, because
/// [`SegmentPath::create_exclusive`] is what claims one — so their composed
/// `seq` ranges can never collide even though both folded the identical prior
/// state and would otherwise seed an identical local counter. This replaces
/// continuing a resumed handler's counter from a folded `max_seq`, which is
/// exactly the mechanism that collided under a concurrent resume: two
/// handlers seeded from the same fold started their own local counters at
/// the same value.
///
/// For a WELL-BEHAVED sequential run (one process at a time, never two
/// resuming at once), segment ordinals are allocated strictly increasing
/// across time, so the composed `seq` is still strictly ascending in the
/// run's true physical write order (segment order, then each segment's own
/// append order) — see `resuming_across_two_segments_keeps_seq_ascending_in_
/// physical_order` for the pinned property, and this module's `seq` docs on
/// [`last_by_kind_key`] for why the fold never depends on that ordering
/// anyway.
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
    /// one process's own SEGMENT of a run; see
    /// `tidepool_harness::selfharness::resume`, which is what decides that
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

    /// The RESUMED-run constructor: append to an existing journal, composing
    /// every `seq` against THIS process's own `segment_ordinal` (see
    /// [`compose_journal_seq`]) rather than continuing a counter seeded from
    /// a folded `max_seq` — the latter is what let two concurrent resumes,
    /// which fold the identical prior state, seed an identical counter and
    /// collide. `segment_ordinal` is exactly what
    /// `tidepool_harness::selfharness::resume::AcquiredLease::segment_ordinal`
    /// carries — the ordinal [`SegmentPath::create_exclusive`] claimed for
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
        let entry = JournalEntry {
            seq,
            kind,
            key,
            payload,
        };
        let line = serde_json::to_string(&entry.to_json()).map_err(|source| {
            JournalAppendError::Serialize {
                path: self.path.clone(),
                source,
            }
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
    /// narration and telemetry, never read back by resume. Envelope:
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
        self.append_trace(stage, key, payload.0)
            .map_err(|e| EffectError::Handler(e.to_string()))?;
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
        let is_fresh = std::fs::metadata(&path).map(|m| m.len() == 0).unwrap_or(true);
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
        jsonl::append_new_line(&path, &line, SyncPolicy::Data).map_err(|source| {
            JournalAppendError::Write { path, source }
        })
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

    fn tmp_file(label: &str) -> PathBuf {
        let pid = std::process::id();
        std::env::temp_dir().join(format!("tidepool_journal_{label}_{pid}.jsonl"))
    }

    fn tmp_dir(label: &str) -> PathBuf {
        let pid = std::process::id();
        std::env::temp_dir().join(format!("tidepool_journal_dir_{label}_{pid}"))
    }

    #[test]
    fn append_then_fold_roundtrips() {
        let path = tmp_file("roundtrip");
        let _ = std::fs::remove_file(&path);
        let h = JournalHandler::new(SegmentPath::for_test(path.clone()))
            .expect("fresh segment header stamp succeeds");

        h.append(
            "split".into(),
            "branch/a".into(),
            serde_json::json!({"n": 1}),
        )
        .unwrap();
        h.append(
            "outcome".into(),
            "branch/b".into(),
            serde_json::json!({"ok": true}),
        )
        .unwrap();

        let entries = load_journal(&path).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].kind, "split");
        assert_eq!(entries[0].key, "branch/a");
        assert_eq!(entries[0].payload, serde_json::json!({"n": 1}));
        assert_eq!(entries[1].kind, "outcome");
        assert_eq!(entries[1].key, "branch/b");
        assert_eq!(entries[1].payload, serde_json::json!({"ok": true}));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn seq_is_monotonic() {
        let path = tmp_file("seq");
        let _ = std::fs::remove_file(&path);
        let h = JournalHandler::new(SegmentPath::for_test(path.clone()))
            .expect("fresh segment header stamp succeeds");

        for i in 0..5 {
            h.append("k".into(), format!("key{i}"), serde_json::json!(i))
                .unwrap();
        }

        let entries = load_journal(&path).unwrap();
        let seqs: Vec<u64> = entries.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![0, 1, 2, 3, 4]);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn torn_last_line_skipped_with_warning() {
        let path = tmp_file("torn");
        let _ = std::fs::remove_file(&path);
        let h = JournalHandler::new(SegmentPath::for_test(path.clone()))
            .expect("fresh segment header stamp succeeds");
        h.append("split".into(), "a".into(), serde_json::json!(1))
            .unwrap();
        h.append("split".into(), "b".into(), serde_json::json!(2))
            .unwrap();

        // Simulate a crash mid-append: keep the well-formed header + first
        // entry line, but truncate the second entry partway through, as a
        // torn write would leave it.
        let contents = std::fs::read_to_string(&path).unwrap();
        let second_newline = contents.match_indices('\n').nth(1).unwrap().0;
        let torn = format!(
            "{}\n{}",
            &contents[..second_newline],
            &contents[second_newline + 1..second_newline + 5]
        );
        std::fs::write(&path, torn).unwrap();

        let entries = load_journal(&path).unwrap();
        assert_eq!(
            entries.len(),
            1,
            "the torn final line must be skipped, not fatal"
        );
        assert_eq!(entries[0].key, "a");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn torn_mid_file_line_is_loud_not_absorbed() {
        let path = tmp_file("torn_mid");
        let _ = std::fs::remove_file(&path);
        // A well-formed final line preceded by a corrupted first line can
        // never happen from a real append-only crash, so it must be
        // reported, not silently dropped the way a torn final line is.
        std::fs::write(
            &path,
            "{not json\n{\"seq\":0,\"kind\":\"k\",\"key\":\"a\",\"payload\":1}\n",
        )
        .unwrap();

        let result = load_journal(&path);
        assert!(
            matches!(
                result,
                Err(JournalLoadError::TornMidFile { line_no: 1, .. })
            ),
            "expected TornMidFile at ONE-based line 1 (the first line in the \
             file, an operator's \"line 1\"), got {:?}",
            result
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn by_key_helper_returns_last_record_per_key() {
        let path = tmp_file("bykey");
        let _ = std::fs::remove_file(&path);
        let h = JournalHandler::new(SegmentPath::for_test(path.clone()))
            .expect("fresh segment header stamp succeeds");
        h.append("split".into(), "branch/a".into(), serde_json::json!(1))
            .unwrap();
        h.append("outcome".into(), "branch/a".into(), serde_json::json!(2))
            .unwrap();
        h.append("split".into(), "branch/b".into(), serde_json::json!(3))
            .unwrap();

        let entries = load_journal(&path).unwrap();
        let by_key = last_by_key(&entries);

        assert_eq!(by_key.len(), 2);
        assert_eq!(by_key["branch/a"].kind, "outcome");
        assert_eq!(by_key["branch/a"].payload, serde_json::json!(2));
        assert_eq!(by_key["branch/b"].kind, "split");

        let _ = std::fs::remove_file(&path);
    }

    fn entry(seq: u64, kind: &str, key: &str, payload: i64) -> JournalEntry {
        JournalEntry {
            seq,
            kind: kind.to_string(),
            key: key.to_string(),
            payload: serde_json::json!(payload),
        }
    }

    /// The whole reason `last_by_kind_key` exists next to `last_by_key`: one
    /// branch carrying BOTH a recorded split and a recorded outcome keeps
    /// both facts, where keying on the branch name alone loses the split.
    #[test]
    fn by_kind_key_keeps_both_kinds_recorded_under_one_key() {
        let entries = vec![
            entry(0, "split", "branch/a", 1),
            entry(1, "outcome", "branch/a", 2),
            entry(2, "split", "branch/b", 3),
        ];

        let folded = last_by_kind_key(&entries);
        assert_eq!(folded.len(), 3, "got {folded:?}");
        assert_eq!(
            folded[&("split".into(), "branch/a".into())].payload,
            serde_json::json!(1),
            "the split must survive the outcome recorded under the same key"
        );
        assert_eq!(
            folded[&("outcome".into(), "branch/a".into())].payload,
            serde_json::json!(2)
        );

        // The narrower fold is still the honest answer to its own question —
        // and demonstrably collapses what the pair-keyed one keeps.
        let by_key = last_by_key(&entries);
        assert_eq!(by_key.len(), 2);
        assert_eq!(by_key["branch/a"].kind, "outcome");
    }

    /// POSITION wins, never `seq`: the entry LAST in `entries` always wins its
    /// `(kind, key)`, even when an earlier entry carries a strictly higher
    /// `seq`. A foreign, hand-edited, or mis-seeded segment can therefore
    /// never invert the result the way a max-seq fold could.
    #[test]
    fn by_kind_key_takes_the_last_entry_regardless_of_seq() {
        let entries = vec![
            entry(5, "split", "a", 50), // highest seq, but NOT last in order
            entry(3, "split", "a", 30),
            entry(0, "split", "a", 10), // lowest seq, but LAST in order — wins
            entry(2, "outcome", "a", 20),
        ];
        let folded = last_by_kind_key(&entries);
        assert_eq!(
            folded[&("split".into(), "a".into())].payload,
            serde_json::json!(10),
            "the physically-last entry must win even though its seq (0) is the lowest"
        );

        // A rotation is a DIFFERENT physical order and is expected to fold
        // differently now — order is exactly what this fold answers over.
        let mut rotated = entries[1..].to_vec();
        rotated.push(entries[0].clone());
        let rotated_folded = last_by_kind_key(&rotated);
        assert_eq!(
            rotated_folded[&("split".into(), "a".into())].payload,
            serde_json::json!(50),
            "with entry(5,...) now last, IT must win"
        );
    }

    /// Two entries at the same `(kind, key)` fold to whichever is LAST in
    /// `entries` — plain position, no tie-breaking logic needed, since `seq`
    /// never enters the decision.
    #[test]
    fn by_kind_key_takes_the_last_entry_at_a_repeated_key() {
        let entries = vec![entry(7, "split", "a", 1), entry(7, "split", "a", 2)];
        let folded = last_by_kind_key(&entries);
        assert_eq!(
            folded[&("split".into(), "a".into())].payload,
            serde_json::json!(2)
        );
    }

    /// A resumed run's appends must be distinguishable from what a prior
    /// process left on disk. Two handler instances over one file, the second
    /// `resuming` at segment ordinal `1` (a DIFFERENT ordinal from the
    /// first's implicit `0` — exactly what two distinct, exclusively-claimed
    /// segments give two real processes): every seq in the file is distinct,
    /// and every one of the second handler's seqs sorts strictly after every
    /// one of the first's, so the fold can tell the two processes' entries
    /// apart. (A second `new`, or `resuming` at the SAME ordinal, would
    /// collide with the first handler's seqs — see
    /// `concurrent_resumes_never_collide_on_seq` for exactly that hazard,
    /// closed by two DIFFERENT ordinals rather than by continuing a count.)
    #[test]
    fn resuming_at_a_distinct_ordinal_keeps_seq_disjoint_from_the_prior_handler() {
        let path = tmp_file("resuming");
        let _ = std::fs::remove_file(&path);

        let first = JournalHandler::new(SegmentPath::for_test(path.clone()))
            .expect("fresh segment header stamp succeeds");
        for i in 0..3 {
            first
                .append("split".into(), format!("k{i}"), serde_json::json!(i))
                .unwrap();
        }

        let second = JournalHandler::resuming(SegmentPath::for_test(path.clone()), 1)
            .expect("fresh segment header stamp succeeds");
        for i in 3..6 {
            second
                .append("split".into(), format!("k{i}"), serde_json::json!(i))
                .unwrap();
        }

        let entries = load_journal(&path).unwrap();
        let seqs: Vec<u64> = entries.iter().map(|e| e.seq).collect();
        assert_eq!(
            seqs,
            vec![
                compose_journal_seq(0, 0),
                compose_journal_seq(0, 1),
                compose_journal_seq(0, 2),
                compose_journal_seq(1, 0),
                compose_journal_seq(1, 1),
                compose_journal_seq(1, 2),
            ],
            "every seq must be distinct, and the resumed handler's must all sort \
             after the prior handler's"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// `seq` is run-GLOBALLY UNIQUE by construction (segment ordinal composed
    /// into the high bits — see [`compose_journal_seq`]'s doc), but the fold
    /// sorts on PHYSICAL order, never on `seq` (see `last_by_kind_key`'s
    /// doc). This pins that the two orders still AGREE for a WELL-BEHAVED
    /// sequential run: two segments (two `JournalHandler`s, the second
    /// `resuming` at the NEXT ordinal, exactly as `selfharness::resume`
    /// allocates a resumed process's segment), concatenated in segment
    /// order, have `seq` strictly ascending — the same order the
    /// concatenation itself is already in. A future change that breaks that
    /// agreement must fail here, not surface later as a mysterious fold
    /// result.
    #[test]
    fn resuming_across_two_segments_keeps_seq_ascending_in_physical_order() {
        let seg0 = tmp_file("seq-order-seg0");
        let seg1 = tmp_file("seq-order-seg1");
        let _ = std::fs::remove_file(&seg0);
        let _ = std::fs::remove_file(&seg1);

        let first = JournalHandler::new(SegmentPath::for_test(seg0.clone()))
            .expect("fresh segment header stamp succeeds");
        for i in 0..3 {
            first
                .append("step".into(), format!("k{i}"), serde_json::json!(i))
                .unwrap();
        }
        let seg0_entries = load_journal(&seg0).unwrap();

        let second = JournalHandler::resuming(SegmentPath::for_test(seg1.clone()), 1)
            .expect("fresh segment header stamp succeeds");
        for i in 3..6 {
            second
                .append("step".into(), format!("k{i}"), serde_json::json!(i))
                .unwrap();
        }
        let seg1_entries = load_journal(&seg1).unwrap();

        // The concatenation IS the physical order — segment order, then each
        // segment's own append order.
        let physical_order: Vec<u64> = seg0_entries
            .iter()
            .chain(seg1_entries.iter())
            .map(|e| e.seq)
            .collect();
        assert_eq!(
            physical_order,
            vec![
                compose_journal_seq(0, 0),
                compose_journal_seq(0, 1),
                compose_journal_seq(0, 2),
                compose_journal_seq(1, 0),
                compose_journal_seq(1, 1),
                compose_journal_seq(1, 2),
            ]
        );

        let mut by_seq = physical_order.clone();
        by_seq.sort_unstable();
        assert_eq!(
            physical_order, by_seq,
            "seq order and physical order must agree on a well-behaved sequential run"
        );

        let _ = std::fs::remove_file(&seg0);
        let _ = std::fs::remove_file(&seg1);
    }

    /// The concurrent-resume hazard this whole scheme exists to close: two
    /// handlers RESUMING THE SAME PRIOR STATE at once (both folded the
    /// identical entries, both would compute the identical `max_seq + 1`
    /// under the old seeding) must still never collide, because each is
    /// `resuming` at its OWN, DISTINCT segment ordinal — exactly what two
    /// real processes get from `SegmentPath::create_exclusive` racing the
    /// same lease. No coordination between the two handlers is needed or
    /// used here; disjointness is structural.
    #[test]
    fn concurrent_resumes_never_collide_on_seq() {
        let seg_a = tmp_file("concurrent-resume-a");
        let seg_b = tmp_file("concurrent-resume-b");
        let _ = std::fs::remove_file(&seg_a);
        let _ = std::fs::remove_file(&seg_b);

        // Both handlers resume from the SAME prior fold — the exact
        // condition (two resumes of one extant lease) the bug reproduced
        // under, seeded here by each simply starting its own local counter
        // at 0, which `resuming` always does regardless of what came before.
        let handler_a = JournalHandler::resuming(SegmentPath::for_test(seg_a.clone()), 5)
            .expect("fresh segment header stamp succeeds");
        let handler_b = JournalHandler::resuming(SegmentPath::for_test(seg_b.clone()), 6)
            .expect("fresh segment header stamp succeeds");

        for i in 0..4 {
            handler_a
                .append("step".into(), format!("a{i}"), serde_json::json!(i))
                .unwrap();
            handler_b
                .append("step".into(), format!("b{i}"), serde_json::json!(i))
                .unwrap();
        }

        let entries_a = load_journal(&seg_a).unwrap();
        let entries_b = load_journal(&seg_b).unwrap();

        let mut seqs_a: Vec<u64> = entries_a.iter().map(|e| e.seq).collect();
        let seqs_b: Vec<u64> = entries_b.iter().map(|e| e.seq).collect();
        let collisions: Vec<u64> = seqs_a
            .iter()
            .filter(|s| seqs_b.contains(s))
            .copied()
            .collect();
        assert!(
            collisions.is_empty(),
            "two concurrent resumes of one extant lease must never produce a \
             shared seq value, got collisions {collisions:?} (a: {seqs_a:?}, \
             b: {seqs_b:?})"
        );

        seqs_a.extend(seqs_b);
        let mut all = seqs_a;
        let before = all.len();
        all.sort_unstable();
        all.dedup();
        assert_eq!(
            all.len(),
            before,
            "every seq across both concurrently-resumed handlers must be distinct"
        );

        let _ = std::fs::remove_file(&seg_a);
        let _ = std::fs::remove_file(&seg_b);
    }

    #[test]
    fn parent_dir_creation_works() {
        let base = tmp_dir("parentdir");
        let _ = std::fs::remove_dir_all(&base);
        let path = base.join("nested").join("run.jsonl");
        let h = JournalHandler::new(SegmentPath::for_test(path.clone()))
            .expect("fresh segment header stamp succeeds");

        h.append("split".into(), "a".into(), serde_json::json!(1))
            .unwrap();

        assert!(path.exists());
        let entries = load_journal(&path).unwrap();
        assert_eq!(entries.len(), 1);

        let _ = std::fs::remove_dir_all(&base);
    }

    /// A parent-directory creation failure surfaces as a structured
    /// [`JournalAppendError::CreateDir`] naming the path, not a bare string —
    /// `append`'s type is now the append boundary's contract.
    #[test]
    fn create_dir_failure_is_a_structured_error() {
        let base = tmp_dir("createdir_conflict");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        // A regular FILE where the journal's parent directory needs to be —
        // `create_dir_all` must fail on it, since it exists but is not a
        // directory.
        let blocker = base.join("blocker");
        std::fs::write(&blocker, b"not a directory").unwrap();
        let path = blocker.join("journal.jsonl");
        // Construction itself now does the mkdir-p (to stamp the segment
        // header), so the failure surfaces here, not at a later `append`.
        let err = JournalHandler::new(SegmentPath::for_test(path))
            .expect_err("a file in place of the parent directory must fail create_dir_all");

        assert!(
            matches!(err, JournalAppendError::CreateDir { path: ref p, .. } if p == &blocker),
            "expected CreateDir naming {blocker:?}, got {err:?}"
        );
        assert!(
            err.to_string().contains("failed to create dir"),
            "message content must survive: {err}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// The trace stream: sibling file derived from the segment name, version
    /// header first, then ts-stamped envelope lines whose payload is free.
    #[test]
    fn trace_lands_in_sibling_stream_with_ts_envelope() {
        let dir = tmp_dir("trace");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("journal-run1.0.jsonl");
        let h = JournalHandler::new(SegmentPath::for_test(path.clone()))
            .expect("fresh segment header stamp succeeds");
        h.append_trace(
            "resume-verdict".into(),
            "branch/a".into(),
            serde_json::json!({"verdict": "skip-done"}),
        )
        .unwrap();
        h.append_trace("park".into(), "loop".into(), serde_json::json!({"why": "completed"}))
            .unwrap();

        let trace_path = dir.join("trace-run1.0.jsonl");
        assert!(trace_path.exists(), "trace derives journal- -> trace- name");
        let contents = std::fs::read_to_string(&trace_path).unwrap();
        let lines: Vec<serde_json::Value> = contents
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines[0], serde_json::json!({"version": 1}));
        assert_eq!(lines.len(), 3);
        for entry in &lines[1..] {
            assert!(entry["ts"].as_u64().unwrap() > 0, "every line is ts-stamped");
            assert!(entry.get("seq").is_some());
            assert!(entry.get("stage").is_some());
        }
        assert_eq!(lines[1]["stage"], "resume-verdict");
        assert_eq!(lines[1]["payload"]["verdict"], "skip-done");
        assert_eq!(lines[2]["key"], "loop");

        // The journal segment itself holds only its own header — trace never
        // leaks into the resume substrate.
        let journal_entries = load_journal(&path).unwrap();
        assert!(journal_entries.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_loads_as_empty() {
        let path = tmp_file("missing");
        let _ = std::fs::remove_file(&path);
        assert_eq!(load_journal(&path).unwrap(), vec![]);
    }

    /// A fresh segment's first line is a version-stamped header.
    #[test]
    fn fresh_segment_gets_a_header_line() {
        let path = tmp_file("header");
        let _ = std::fs::remove_file(&path);
        let h = JournalHandler::new(SegmentPath::for_test(path.clone()))
            .expect("fresh segment header stamp succeeds");
        h.append("split".into(), "a".into(), serde_json::json!(1))
            .unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        let first_line = contents.lines().next().unwrap();
        let header: serde_json::Value = serde_json::from_str(first_line).unwrap();
        assert_eq!(
            header,
            serde_json::json!({"version": journal_version::CURRENT})
        );
        // The header doesn't count as an entry.
        assert_eq!(load_journal(&path).unwrap().len(), 1);
        let _ = std::fs::remove_file(&path);
    }

    /// A legacy segment (written before this scheme existed — no header
    /// line at all, every line a plain entry) must still load, reading as
    /// version 0 and migrating through the identity step.
    #[test]
    fn unstamped_legacy_segment_still_loads() {
        let path = tmp_file("legacy_segment");
        let _ = std::fs::remove_file(&path);
        let entry = JournalEntry {
            seq: 0,
            kind: "split".into(),
            key: "a".into(),
            payload: serde_json::json!(1),
        };
        std::fs::write(
            &path,
            format!("{}\n", serde_json::to_string(&entry.to_json()).unwrap()),
        )
        .unwrap();

        let entries = load_journal(&path).expect("legacy unstamped segment must load");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, "a");
        let _ = std::fs::remove_file(&path);
    }

    /// A version newer than this build supports is a loud, typed refusal.
    #[test]
    fn future_segment_version_is_a_typed_rejection() {
        let path = tmp_file("future_version");
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, "{\"version\":9999}\n").unwrap();

        let err = load_journal(&path).expect_err("a future version must be refused");
        assert!(
            matches!(err, JournalLoadError::FutureVersion { found: 9999, .. }),
            "expected FutureVersion, got {err:?}"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// A multi-threaded burst of records through CLONED handlers (sharing the
    /// same seq counter, the same append lock, and the same path) must never
    /// tear a line. This does NOT rest on `write_all`/`O_APPEND` syscall
    /// atomicity (a short write makes `write_all` loop into several `write()`
    /// calls, and a later one is not atomic against a concurrent writer) —
    /// it rests on `JournalHandler::append`'s lock serializing the whole
    /// open+write+fsync across every clone, so no two appends are ever
    /// in flight at once regardless of how many `write()` calls either takes.
    #[test]
    fn concurrent_burst_through_cloned_handlers_yields_no_torn_lines() {
        let path = tmp_file("concurrent_burst");
        let _ = std::fs::remove_file(&path);
        let handler = JournalHandler::new(SegmentPath::for_test(path.clone()))
            .expect("fresh segment header stamp succeeds");

        const THREADS: usize = 8;
        const PER_THREAD: usize = 50;
        std::thread::scope(|scope| {
            for t in 0..THREADS {
                let h = handler.clone();
                scope.spawn(move || {
                    for i in 0..PER_THREAD {
                        h.append(
                            "burst".into(),
                            format!("t{t}-{i}"),
                            serde_json::json!({"t": t, "i": i}),
                        )
                        .unwrap();
                    }
                });
            }
        });

        let entries =
            load_journal(&path).expect("a torn line must never happen, so this must never error");
        assert_eq!(
            entries.len(),
            THREADS * PER_THREAD,
            "every append from every thread must survive as a complete, parseable line"
        );

        let mut seqs: Vec<u64> = entries.iter().map(|e| e.seq).collect();
        seqs.sort_unstable();
        seqs.dedup();
        assert_eq!(
            seqs.len(),
            THREADS * PER_THREAD,
            "the shared seq counter must not be raced past — no seq reused across threads"
        );

        let _ = std::fs::remove_file(&path);
    }
}
