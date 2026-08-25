//! The READ half of git-plus-journal persistence: run identity that
//! outlives a process, and the boot-time FOLD of that run's journal.
//!
//! `record` (`Tidepool.Journal`) is write-only on the authored surface. Locating
//! a run's journal, loading it, folding it, and injecting the result is the
//! DRIVER's job — this module is the driver's half of it. Nothing here appends,
//! rewrites, truncates, or compacts a journal: the only file this module ever
//! WRITES is the run lease.
//!
//! # Segments — one journal file per PROCESS, all retained, all folded
//!
//! A run id owns an ORDERED SET of journal segments, one per process that run
//! survives, rather than one file appended to by every process in turn. A
//! resumed process never appends to a segment a prior process left — it opens
//! a fresh one. This is what closes the torn-tail hazard the single-file
//! design had: a crash mid-append leaves its segment's final line torn, and
//! that segment is then SEALED forever — no later process ever writes into it
//! — so `load_journal`'s "a torn FINAL line is the crash point, skip it with a
//! warning" contract is unconditionally true per segment, rather than true
//! only until the next process's first append lands on the same physical
//! line and merges with the torn bytes.
//!
//! **Naming**: [`segment_path`] — `journal-<runId>.<seg>.jsonl`, `seg` a plain
//! decimal ordinal, unpadded. Ordering across segments is by the PARSED
//! ordinal ([`list_segments`]), never by string/lexicographic comparison — a
//! run's 10th segment must sort after its 9th, not between its 1st and 2nd.
//!
//! **Allocation**: [`acquire_lease`] picks the next unused segment for THIS
//! process — one past the highest segment index already on disk for the run
//! id, or `0` when the run id owns none yet — via [`allocate_segment`],
//! which claims that candidate EXCLUSIVELY (`create_new`, an OS-enforced
//! atomic file create) rather than merely returning a computed path: two
//! processes racing the SAME directory listing at boot would otherwise both
//! compute the same "next" ordinal and both write into it. A collision
//! (`AlreadyExists`) retries at the next ordinal — bounded, since each
//! retry strictly advances past a real file, so the loop terminates in at
//! most (number of racing allocators) steps. One consequence: once a
//! segment is allocated its file exists on disk, empty or not — so a
//! predecessor that crashed before ever appending still counts as CLAIMED,
//! and the next resume advances past it rather than reusing it (unlike the
//! pre-exclusivity design, where an unwritten segment was indistinguishable
//! from an unallocated one). The allocated path is carried on
//! [`AcquiredLease`], not on [`RunLease`] — see "The run lease" below for
//! why that split is deliberate.
//!
//! **The boot fold** ([`crate::selfharness::driver::SelfHarnessDriver::open_run_journal`])
//! loads every segment for the run id, in segment order, and concatenates
//! their entries — each segment's own entries already in that segment's
//! append order ([`tidepool_handlers::load_journal`]'s contract, unchanged).
//! That concatenation is the run's TRUE PHYSICAL WRITE ORDER, the exact
//! sequence of bytes a crash leaves behind, and it is what
//! [`tidepool_handlers::last_by_kind_key`] folds on — see that function's doc
//! for why the winner is POSITION in this order, not `seq`. `seq` is run-
//! GLOBALLY UNIQUE by construction: a resumed segment's handler is seeded at
//! [`AcquiredLease::segment_ordinal`] — THIS process's own exclusively-claimed
//! segment ordinal, composed into `seq`'s high bits
//! ([`tidepool_handlers::compose_journal_seq`]) — never at a count continued
//! from the fold. Two processes resuming the SAME extant lease at once (a
//! deliberate TAKEOVER — see "The run lease" below; a live-owned lease is a
//! hard refusal by default now, never a silent join) fold the identical
//! prior state and would seed an
//! identical counter under a fold-derived scheme; seeding from each one's own
//! DISTINCT segment ordinal instead means their `seq` ranges can never
//! collide, with no coordination between them beyond the segment claim
//! itself. `seq` is written purely as provenance and is never what decides a
//! fold; for a well-behaved sequential run it also stays ascending in
//! physical order, but that ordering is not what makes it safe to write.
//!
//! **Retention**: every segment is kept forever. Nothing here deletes,
//! merges, truncates, or compacts one — the crashed segment with its genuine
//! torn tail stays on disk exactly as `load_journal` describes it, beside
//! every segment written after it.
//!
//! # The run lease — which run a process resumes, and which segment it owns
//!
//! A run must be identifiable BEFORE its first `record` (a crash in cycle 1
//! leaves no checkpoint, so the checkpoint cannot carry the run id) and must
//! outlive the process (a resumed run is a different process, so per-process
//! naming would fold nothing and orphan the prior file). The lease is one
//! file, `<log_dir>/run-current.json`, written at boot before any handler is
//! wired:
//!
//! ```json
//! {"runId": "20260817-101112-48213", "pid": 48213, "startedAt": "…"}
//! ```
//!
//! Deliberately absent from that file: which segment the writing process
//! owns. [`RunLease`] is a statement about the RUN — the identity a crash
//! must survive — and a segment index is a fact about one PROCESS within
//! that run. Recording it in the persisted lease would make the lease a
//! statement about whichever process last wrote it, which is exactly the
//! property that did NOT survive the crash in the single-file design (the
//! lease named a file; the next process inherited that name and appended
//! into it). Instead, [`acquire_lease`] ENUMERATES the segments already on
//! disk for the run id every time — fresh boot or resume alike — and hands
//! the freshly allocated path back on [`AcquiredLease::segment`], which is
//! never itself persisted. So the durable lease answers only "which run",
//! and the directory listing answers "which segments" — two questions, two
//! places, neither able to go stale relative to the other because the second
//! is never cached.
//!
//! | boot condition | behaviour |
//! |---|---|
//! | no lease | mint a `runId`, claim the lease file EXCLUSIVELY (an OS-enforced atomic create — never a blind overwrite); on collision with another racer's simultaneous fresh claim, restart from `load_lease` and take the RESUME row below instead of orphaning a second run; the winner allocates segment 0, empty fold |
//! | a lease naming a DEAD pid, or THIS process's own pid | RESUME: same `runId`, allocate the next unused segment, fold every existing segment |
//! | a lease naming a DIFFERENT, LIVE pid, no takeover | HARD REFUSAL: `Err(PersistenceError::LiveLeaseHeld)`, naming the pid — nothing on disk is touched |
//! | a lease naming a DIFFERENT, LIVE pid, [`LEASE_TAKEOVER_ENV_VAR`]`=1` | TAKEOVER: archive the prior lease record (see [`archive_stale_lease`]), then RESUME as above |
//! | `run_loop` returns normally | [`retire_lease`]: rename to `run-<runId>.json`, RETAINED — so the next boot mints a fresh run |
//! | the process crashes | the lease survives → the next boot resumes |
//!
//! Every segment a run ever writes is retained under that run's id, appended
//! to by exactly one process each. Nothing is ever rewritten and nothing is
//! ever deleted — a retired lease is renamed, not removed.
//!
//! # The fold
//!
//! [`ResumeFold`] keys on the `(kind, key)` PAIR, not the key alone: a harness
//! records several kinds of fact about one branch (dev-tree writes a `"split"`
//! and an `"outcome"` under the same branch name), and keying on the key alone
//! would collapse them. The winner is the entry LAST in physical write order
//! ([`tidepool_handlers::last_by_kind_key`]) — see that function's doc for why
//! this is positional rather than `seq`-based, and for what "physical order"
//! means once a run spans several segments.
//!
//! The fold is GENERIC over `(kind, key, payload)`. Payloads are opaque
//! [`serde_json::Value`]s end to end — dev-tree's
//! `split`/`outcome`/`replan`/`rebase`/`escalation` vocabulary is the authoring
//! harness's schema and never appears in this crate.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use tidepool_handlers::{last_by_kind_key, JournalEntry, SegmentPath};

use super::persistence::{PersistenceError, LEASE_TAKEOVER_ENV_VAR};

/// Everything the driver folded out of one run's segments at boot: the last
/// entry recorded under each `(kind, key)` pair, plus the run id those entries
/// came from.
///
/// A [`BTreeMap`], not a `HashMap`, on purpose: [`Self::to_json`] emits entries
/// in `(kind, key)` order, so the spliced wire text is byte-deterministic for a
/// given set of entries and the compile memo hits rather than missing on map
/// iteration order.
#[derive(Debug, Clone, PartialEq)]
pub struct ResumeFold {
    run_id: String,
    entries: BTreeMap<(String, String), JournalEntry>,
    /// The highest `seq` across every entry FOLDED FROM (not just the
    /// entries that survived the fold) — see [`Self::max_seq`]'s doc for why
    /// this must be tracked separately from the surviving entries rather than
    /// recomputed from them now that the winner is positional, not max-seq.
    max_seq: Option<u64>,
}

impl ResumeFold {
    /// Fold `entries` — already concatenated in the run's TRUE PHYSICAL WRITE
    /// ORDER (segment order, then each segment's own append order; see this
    /// module's doc) — down to the last record per `(kind, key)`.
    ///
    /// IDEMPOTENT: folding an already-folded set (at most one entry per
    /// `(kind, key)`, so there is nothing left for position to disambiguate)
    /// is a no-op, regardless of what order that set is handed back in.
    /// Folding the identical byte sequence twice — the operation a resumed
    /// boot performs on a run whose segments have not changed since the last
    /// boot — always yields the same map: the fold is a pure function of the
    /// ORDERED sequence of entries, not of a set (see [`last_by_kind_key`]'s
    /// doc for why order now matters).
    pub fn fold(run_id: impl Into<String>, entries: &[JournalEntry]) -> Self {
        ResumeFold {
            run_id: run_id.into(),
            entries: last_by_kind_key(entries).into_iter().collect(),
            max_seq: entries.iter().map(|e| e.seq).max(),
        }
    }

    /// The fold of a run that recorded nothing — what a fresh boot carries.
    pub fn empty(run_id: impl Into<String>) -> Self {
        ResumeFold {
            run_id: run_id.into(),
            entries: BTreeMap::new(),
            max_seq: None,
        }
    }

    /// The run these entries came from — the same id across every process the
    /// run survived.
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Whether this fold carries anything worth skipping. `true` for a fresh
    /// run, and the condition the driver's entry selection turns on: an empty
    /// fold compiles the ordinary `loop` entry, unchanged.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// How many `(kind, key)` pairs survived the fold — for the boot log line,
    /// not for control flow.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// The highest `seq` written across the WHOLE folded input, or `None` for
    /// an empty one.
    ///
    /// Computed from ALL entries [`Self::fold`] was given, not from the
    /// surviving fold entries: the fold's winner is now POSITIONAL (see
    /// [`last_by_kind_key`]), so an overwritten entry can in principle carry
    /// a higher `seq` than the entry that overwrote it (a foreign, hand-
    /// edited, or mis-seeded segment). Deriving `max_seq` from the survivors
    /// alone would then under-count, and a resumed segment seeded from that
    /// undercount could allocate a `seq` some earlier, dropped entry already
    /// used. Scanning every input entry keeps this correct regardless of
    /// what the fold kept.
    pub fn max_seq(&self) -> Option<u64> {
        self.max_seq
    }

    /// The seq a RESUMED run's first append must carry: `max_seq() + 1`, or `0`
    /// when nothing was folded.
    pub fn next_seq(&self) -> u64 {
        self.max_seq().map_or(0, |m| m + 1)
    }

    /// The wire shape `state_cross::resume_in` splices and
    /// `Tidepool.Resume`'s hand-written `FromJSON` decodes:
    ///
    /// ```json
    /// { "runId": "…",
    ///   "entries": [ {"seq": 7, "kind": "split", "key": "…", "payload": {…}} ] }
    /// ```
    ///
    /// A LIST, not an object-of-objects: a `(kind, key)` pair is not a JSON key,
    /// and a flat list decodes through the stdlib's hand-written instances with
    /// no `Map` instance question. Entries emit in `(kind, key)` order (the
    /// [`BTreeMap`]'s own), so the splice is byte-deterministic. Each entry's
    /// object is [`JournalEntry::to_json`] — the same spelling a journal LINE
    /// carries, single-sourced so the two cannot drift.
    pub fn to_json(&self) -> Json {
        serde_json::json!({
            "runId": self.run_id,
            "entries": self.entries.values().map(JournalEntry::to_json).collect::<Vec<_>>(),
        })
    }
}

// ============================================================================
// The run lease
// ============================================================================

/// The lease file's basename under the log dir — the ACTIVE run's identity.
const CURRENT_LEASE: &str = "run-current.json";

/// One run's identity, durable across the processes the run takes. Written at
/// boot, before any handler is wired; renamed (never deleted) at a normal
/// `run_loop` return.
///
/// Carries no segment path — see this module's doc for why that is a
/// deliberate omission, not a gap: a segment is a fact about one process,
/// this file is a statement about the run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunLease {
    /// Stable across every process this run survives — what every segment
    /// filename is derived from and what a [`ResumeFold`] reports.
    #[serde(rename = "runId")]
    pub run_id: String,
    /// The process that most recently took the lease — diagnostic only; a stale
    /// pid is exactly the crash case this file exists to survive, so nothing
    /// checks it.
    pub pid: u32,
    /// When that process took it, ISO-8601-ish (`seconds since epoch` rendered
    /// as text is enough for a human reading the file; nothing parses it).
    #[serde(rename = "startedAt")]
    pub started_at: String,
}

/// Where the ACTIVE lease lives.
pub fn lease_path(log_dir: &Path) -> PathBuf {
    log_dir.join(CURRENT_LEASE)
}

/// Where a RETIRED lease is kept — retained like a worktree, never deleted, so
/// a finished run's identity stays readable beside its segments.
pub fn retired_lease_path(log_dir: &Path, run_id: &str) -> PathBuf {
    log_dir.join(format!("run-{run_id}.json"))
}

/// The basename prefix every one of `run_id`'s segments shares —
/// `journal-<runId>.`, so a segment's own suffix (`<seg>.jsonl`) is
/// unambiguous to strip back off in [`list_segments`].
fn segment_prefix(run_id: &str) -> String {
    format!("journal-{run_id}.")
}

/// One segment's path: `<log_dir>/journal-<runId>.<seg>.jsonl`. `seg` is a
/// plain decimal ordinal, deliberately unpadded — nothing here or in
/// [`list_segments`] ever compares segment order as strings, only as the
/// parsed integer, so an unpadded name can never silently mis-sort (padding
/// would only paper over a comparison bug, not prevent one).
pub fn segment_path(log_dir: &Path, run_id: &str, seg: u64) -> PathBuf {
    log_dir.join(format!("{}{seg}.jsonl", segment_prefix(run_id)))
}

/// Parse a directory entry's filename back to its segment ordinal, if it is
/// one of `run_id`'s segments. Anything else in the log dir — the lease,
/// retired leases, a checkpoint, another run's segments — simply does not
/// match and is skipped by [`list_segments`], never an error.
fn parse_segment_ordinal(run_id: &str, file_name: &str) -> Option<u64> {
    file_name
        .strip_prefix(&segment_prefix(run_id))?
        .strip_suffix(".jsonl")?
        .parse::<u64>()
        .ok()
}

/// Every segment `run_id` owns, on disk right now, in NUMERIC segment order —
/// PARSED-integer order, never lexicographic, so a run's 10th segment sorts
/// after its 9th rather than between its 1st and 2nd.
///
/// A `log_dir` that does not exist yet owns no segments (`Ok(vec![])`), the
/// same "nothing recorded yet" reading [`load_journal`] gives a missing file
/// — a run that has not been booted in this directory before has nothing to
/// enumerate, which is not an error.
pub fn list_segments(log_dir: &Path, run_id: &str) -> Result<Vec<PathBuf>, PersistenceError> {
    let read_dir = match std::fs::read_dir(log_dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(PersistenceError::Io {
                path: log_dir.to_path_buf(),
                source,
            })
        }
    };
    let mut segments = Vec::new();
    for entry in read_dir {
        let entry = entry.map_err(|source| PersistenceError::Io {
            path: log_dir.to_path_buf(),
            source,
        })?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if let Some(seg) = parse_segment_ordinal(run_id, &name) {
            segments.push((seg, entry.path()));
        }
    }
    segments.sort_by_key(|(seg, _)| *seg);
    Ok(segments.into_iter().map(|(_, path)| path).collect())
}

/// The segment index the NEXT process to boot in `log_dir` under `run_id`
/// SHOULD allocate: one past the highest segment already on disk, or `0`
/// when `run_id` owns none yet. A pure computation over a directory
/// listing, nothing more — it does not itself claim anything; see
/// [`allocate_segment`], its only caller, for why "should" and "does" are
/// two different steps.
fn next_segment_ordinal(log_dir: &Path, run_id: &str) -> Result<u64, PersistenceError> {
    let existing = list_segments(log_dir, run_id)?;
    Ok(existing
        .iter()
        .filter_map(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| parse_segment_ordinal(run_id, n))
        })
        .max()
        .map_or(0, |m| m + 1))
}

/// Claim THIS process's own segment for `run_id`, EXCLUSIVELY. The fast path
/// is [`next_segment_ordinal`]'s listing-based guess; the claim itself is
/// [`SegmentPath::create_exclusive`] — an OS-enforced atomic "this file did
/// not exist and now it does, and I'm the one who made it so" — so a second
/// allocator racing the same listing can never silently share the winner's
/// path. On `AlreadyExists` (another process's claim landed first, or beat
/// us to a still-empty ordinal a crashed process only reserved) the
/// candidate ordinal is bumped and retried; nothing here is a substantive
/// fallback — under real contention the loser of a single collision lands
/// exactly where an uncontended call would have put it anyway, one ordinal
/// later. This is the crate's ONE legitimate non-test caller of
/// `create_exclusive` — see that method's doc.
///
/// Returns the claimed path together with the ordinal it landed on — the
/// caller (only [`acquire_lease`]) needs the ordinal itself to seed
/// [`tidepool_handlers::JournalHandler::resuming`]'s structurally-unique
/// `seq` composition; see [`AcquiredLease::segment_ordinal`].
fn allocate_segment(log_dir: &Path, run_id: &str) -> Result<(SegmentPath, u64), PersistenceError> {
    let mut seg = next_segment_ordinal(log_dir, run_id)?;
    loop {
        let path = segment_path(log_dir, run_id, seg);
        match SegmentPath::create_exclusive(path.clone()) {
            Ok(claimed) => return Ok((claimed, seg)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => seg += 1,
            Err(source) => return Err(PersistenceError::Io { path, source }),
        }
    }
}

/// Best-effort liveness check via `/proc/<pid>` (Linux only — the same idiom
/// `Harness::sweep_stale_run_dirs` uses for its own stale-dir sweep).
/// [`acquire_lease`] now gates a hard refusal on this for any pid other than
/// its own — `false` (a platform without `/proc`, or `/proc` itself
/// unreadable) means a genuinely live OTHER process goes undetected and its
/// lease reclaims as if dead, the same best-effort ceiling this check always
/// had, now load-bearing rather than advisory.
fn pid_is_alive(pid: u32) -> bool {
    Path::new("/proc").join(pid.to_string()).exists()
}

/// Load and fold every segment `run_id` owns in `log_dir`, in true physical
/// write order (segment order, then each segment's own append order) — the
/// order [`ResumeFold::fold`]/[`last_by_kind_key`] fold on. `Ok(empty fold)`
/// when the run owns no segments yet.
pub fn fold_run_journal(log_dir: &Path, run_id: &str) -> Result<ResumeFold, RunJournalError> {
    let segments = list_segments(log_dir, run_id)?;
    let mut entries = Vec::new();
    for segment in &segments {
        entries.extend(tidepool_handlers::load_journal(segment)?);
    }
    Ok(ResumeFold::fold(run_id, &entries))
}

/// Everything that can go wrong loading a run's journal: enumerating its
/// segments ([`PersistenceError`], an I/O failure against `log_dir` itself),
/// or loading one of them ([`tidepool_handlers::JournalLoadError`] — a torn
/// line anywhere but a segment's own final line, which is real corruption
/// per [`tidepool_handlers::load_journal`]'s unchanged contract).
#[derive(Debug, thiserror::Error)]
pub enum RunJournalError {
    #[error(transparent)]
    Enumerate(#[from] PersistenceError),
    #[error(transparent)]
    Load(#[from] tidepool_handlers::JournalLoadError),
}

/// What [`acquire_lease`] found: the run this process is now part of, the
/// segment THIS process owns (freshly allocated every boot — never read back
/// off the lease; see this module's doc), and whether the run was INHERITED
/// from a prior process (a crash) or minted.
#[derive(Debug, Clone, PartialEq)]
pub struct AcquiredLease {
    pub lease: RunLease,
    /// This process's own segment — allocated (and EXCLUSIVELY claimed, see
    /// [`allocate_segment`]) at acquisition time. A [`SegmentPath`], not a
    /// bare `PathBuf`: holding one is proof the file was exclusively
    /// created, not merely a computed name — no process ever appends into a
    /// segment another process owns, and now that can't even be constructed
    /// by accident.
    pub segment: SegmentPath,
    /// The ordinal [`Self::segment`] landed on. Exclusively claimed per
    /// process ([`SegmentPath::create_exclusive`]), so two processes —
    /// including two racing to RESUME the same lease at once — can never
    /// share one. This is what [`tidepool_handlers::compose_journal_seq`]
    /// mixes into the high bits of every `seq` this process's
    /// [`tidepool_handlers::JournalHandler`] writes, making `seq`
    /// structurally unique across a run without any cross-process
    /// coordination beyond the segment claim itself.
    pub segment_ordinal: u64,
    /// `true` when a lease was already on disk — this boot continues a run a
    /// prior process started, whether or not that run had journaled anything
    /// yet.
    pub resumed: bool,
}

/// Read the ACTIVE lease, if one is there. `Ok(None)` for the ordinary
/// fresh-boot case (no file); a file that exists but does not parse is a typed
/// error, never a silent reset — a lease we cannot read is a run we would
/// silently redo.
pub fn load_lease(log_dir: &Path) -> Result<Option<RunLease>, PersistenceError> {
    let path = lease_path(log_dir);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(PersistenceError::Io { path, source }),
    };
    let lease =
        serde_json::from_slice(&bytes).map_err(|source| PersistenceError::Json { path, source })?;
    Ok(Some(lease))
}

/// Write `lease` as the ACTIVE lease, creating `log_dir` if needed. Written
/// via the shared durable atomic-write helper (a uniquely-named temp
/// sibling, fsynced, then renamed over the target) for the same reason
/// `save_checkpoint` is: a kill mid-write must never leave a torn lease for
/// the next boot to read. The helper's per-call unique temp name is what
/// keeps this safe under [`acquire_lease`]'s RESUME row, which can
/// legitimately be entered by several processes/threads at once (every racer
/// that lost the fresh claim lands here together) — a shared tmp name would
/// let one caller's rename consume a sibling ITS write never produced.
/// Overwriting the ACTIVE lease itself is still the intended end state
/// either way: last writer wins, same as before.
pub fn write_lease(log_dir: &Path, lease: &RunLease) -> Result<(), PersistenceError> {
    std::fs::create_dir_all(log_dir).map_err(|source| PersistenceError::Io {
        path: log_dir.to_path_buf(),
        source,
    })?;
    let path = lease_path(log_dir);
    let bytes = serde_json::to_vec_pretty(lease).map_err(|source| PersistenceError::Json {
        path: path.clone(),
        source,
    })?;
    tidepool_atomic_write::write_durable(&path, &bytes).map_err(|e| PersistenceError::Io {
        path: e.path,
        source: e.source,
    })
}

/// A per-CALL unique `.tmp` sibling of `path` — see [`write_lease`]'s doc for
/// why a fixed name is unsafe under concurrent callers. Shares
/// [`CLAIM_ATTEMPT_ID`] with [`try_claim_lease_exclusive`]'s tmp naming
/// (same disambiguation need, same counter — no reason for two).
fn lease_tmp_path(path: &Path) -> PathBuf {
    let attempt = CLAIM_ATTEMPT_ID.fetch_add(1, Ordering::Relaxed);
    PathBuf::from(format!(
        "{}.write-{}-{attempt}.tmp",
        path.display(),
        std::process::id()
    ))
}

/// Disambiguates concurrent [`lease_tmp_path`] callers' temp filenames within
/// one process (`std::process::id()` alone is shared by every racing thread
/// in a test) — never itself the source of exclusivity, which for
/// [`try_claim_lease_exclusive`] is [`std::fs::hard_link`]'s atomic "the
/// target did not exist and now it does".
static CLAIM_ATTEMPT_ID: AtomicU64 = AtomicU64::new(0);

/// Claim the ACTIVE lease slot EXCLUSIVELY for a FRESH run: write `lease`
/// fully to a private temp file — so the claim itself can never land torn —
/// then [`std::fs::hard_link`] it into place, the same "did not exist and now
/// it does, and I'm the one who made it so" idiom [`allocate_segment`] uses
/// via `create_new` for segments (a hard link is used here, rather than
/// `create_new` directly, so the full serialized content is already durable
/// on disk before the exclusive claim step — a `create_new`-then-`write_all`
/// sequence would let a crash between those two steps leave a lease that
/// EXISTS but is torn, unlike every other lease write in this module).
///
/// `Ok(false)` — never an `Err` — on collision: another racer's fresh claim
/// landed first. That is a normal branch for [`acquire_lease`], not a fault:
/// the caller's next step is `load_lease`, which now finds the winner's
/// lease and takes the RESUME row instead.
fn try_claim_lease_exclusive(log_dir: &Path, lease: &RunLease) -> Result<bool, PersistenceError> {
    std::fs::create_dir_all(log_dir).map_err(|source| PersistenceError::Io {
        path: log_dir.to_path_buf(),
        source,
    })?;
    let path = lease_path(log_dir);
    let bytes = serde_json::to_vec_pretty(lease).map_err(|source| PersistenceError::Json {
        path: path.clone(),
        source,
    })?;
    let tmp = lease_tmp_path(&path);
    std::fs::write(&tmp, &bytes).map_err(|source| PersistenceError::Io {
        path: tmp.clone(),
        source,
    })?;
    let claimed = match std::fs::hard_link(&tmp, &path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(source) => Err(PersistenceError::Io {
            path: path.clone(),
            source,
        }),
    };
    let _ = std::fs::remove_file(&tmp);
    claimed
}

/// Mint a run id that no run in `log_dir` already owns: neither a retired
/// lease nor any segment file names it. `{epoch-seconds}-{pid}` is the base
/// (both halves, not the timestamp alone, since two processes launched
/// within one wall-clock second would otherwise collide), suffixed with
/// `-<n>` until neither artifact exists for the candidate — a property of
/// the directory, not of how fast the clock ticks.
fn mint_run_id_in(log_dir: &Path) -> Result<String, PersistenceError> {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let base = format!("{secs}-{}", std::process::id());
    let mut candidate = base.clone();
    let mut n = 1u32;
    loop {
        let taken = retired_lease_path(log_dir, &candidate).exists()
            || !list_segments(log_dir, &candidate)?.is_empty();
        if !taken {
            return Ok(candidate);
        }
        candidate = format!("{base}-{n}");
        n += 1;
    }
}

fn now_secs_string() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        .to_string()
}

/// Archive the PRIOR lease record before a TAKEOVER overwrites it — called
/// only from [`acquire_lease`]'s takeover branch, so the fact that a live
/// process's lease was forcibly reclaimed (never an ordinary dead-pid or
/// self-pid resume) survives on disk under its own name. Deliberately named
/// apart from [`retired_lease_path`] (reserved for a run that finished
/// normally — reusing that name here could be mistaken for a clean finish,
/// or collide with one written later). Best-effort audit trail, not part of
/// the lease protocol itself: nothing here or elsewhere ever reads this file
/// back.
fn archive_stale_lease(log_dir: &Path, lease: &RunLease) -> Result<(), PersistenceError> {
    let path = log_dir.join(format!(
        "run-{}.takeover-from-pid-{}-at-{}.json",
        lease.run_id,
        lease.pid,
        now_secs_string()
    ));
    let bytes = serde_json::to_vec_pretty(lease).map_err(|source| PersistenceError::Json {
        path: path.clone(),
        source,
    })?;
    tidepool_atomic_write::write_durable(&path, &bytes).map_err(|e| PersistenceError::Io {
        path: e.path,
        source: e.source,
    })
}

/// The boot-time lease step: RESUME the run a prior process left behind, or
/// mint a fresh one, and ALLOCATE the segment this process will append to —
/// the next unused ordinal for that run id, every time, fresh boot or resume
/// alike (see this module's doc for why that allocation is never read back
/// off the persisted lease).
///
/// The no-lease (fresh) row loops: [`try_claim_lease_exclusive`] either wins —
/// this process IS the run's first process — or loses to a racer whose claim
/// landed first, in which case this reloads the lease and falls into the
/// SAME iteration's `Some` arm, taking the resume row for the winner's run.
/// Two processes booting into an empty `log_dir` at once can therefore never
/// both mint: exactly one becomes the fresh run, and every other one resumes
/// it — never a silently orphaned second run each believing itself fresh.
///
/// # Live-PID refusal
///
/// A lease naming a DIFFERENT pid that is still alive (checked via
/// [`pid_is_alive`]) is a HARD REFUSAL — `Err(PersistenceError::LiveLeaseHeld)`,
/// naming the pid and the takeover remedy — not merely a warning: segments
/// keep the JOURNAL file-safe under concurrent writers, but they do nothing
/// to stop two processes from independently repeating the same external
/// effects (writes, commits, model calls) or racing the shared checkpoint
/// with last-writer-wins. Nothing on disk is touched on this path — the
/// stale-but-existing lease is left exactly as found, so a retry (after
/// confirming the other process is really gone, or setting
/// [`LEASE_TAKEOVER_ENV_VAR`]) sees the same state.
///
/// Set [`LEASE_TAKEOVER_ENV_VAR`]`=1` to force the join anyway — for a
/// verified-stale record (the pid was reused by something unrelated, or the
/// box rebooted and `/proc` just hasn't caught up) or an operator-approved
/// takeover. The prior lease is archived first ([`archive_stale_lease`]) so
/// the forced claim leaves an audit trail, then the resume proceeds exactly
/// as the dead-pid case below.
///
/// A lease naming THIS process's own pid is exempt from both the refusal and
/// the takeover machinery — it is definitionally not a second process, so
/// there is no dual-ownership hazard to refuse. In production
/// `acquire_lease` runs exactly once per process boot, so this case is a
/// test-only artifact (this crate's own resume tests simulate "a later
/// process resumes" by calling this function again within one test process);
/// it is handled here rather than special-cased in every such test.
///
/// A lease naming a DEAD pid resumes exactly as before this fix — reclaim
/// behavior for that case is unchanged.
///
/// Writing the lease on the resume path is deliberate: it re-stamps `pid` and
/// `startedAt` with the process that now holds the run, which is what a human
/// reading the file wants. That path keeps the ordinary replace-rename
/// (`write_lease`) — overwriting an EXISTING lease is the intent there, unlike
/// the fresh row, which must never blindly overwrite a lease that turns out
/// to already exist.
pub fn acquire_lease(log_dir: &Path) -> Result<AcquiredLease, PersistenceError> {
    loop {
        if let Some(mut lease) = load_lease(log_dir)? {
            let my_pid = std::process::id();
            if lease.pid != my_pid && pid_is_alive(lease.pid) {
                if std::env::var(LEASE_TAKEOVER_ENV_VAR).as_deref() != Ok("1") {
                    return Err(PersistenceError::LiveLeaseHeld {
                        run_id: lease.run_id,
                        pid: lease.pid,
                    });
                }
                tracing::warn!(
                    run_id = %lease.run_id,
                    prior_pid = lease.pid,
                    resuming_pid = my_pid,
                    "TAKEOVER ({LEASE_TAKEOVER_ENV_VAR}=1): forcibly claiming a run whose \
                     lease still names a LIVE prior process — segments keep this file-safe, \
                     but if that process is still genuinely working the run, its effects and \
                     this process's will now interleave"
                );
                archive_stale_lease(log_dir, &lease)?;
            }
            lease.pid = my_pid;
            lease.started_at = now_secs_string();
            write_lease(log_dir, &lease)?;
            let (segment, segment_ordinal) = allocate_segment(log_dir, &lease.run_id)?;
            return Ok(AcquiredLease {
                lease,
                segment,
                segment_ordinal,
                resumed: true,
            });
        }

        // No lease on disk (yet). Mint a candidate and try to claim the slot
        // EXCLUSIVELY — losing the race (`Ok(false)`) means some other
        // process's fresh claim landed between our `load_lease` above and
        // here, so we loop back and resume THEIR run rather than overwrite
        // it with ours.
        let candidate = RunLease {
            run_id: mint_run_id_in(log_dir)?,
            pid: std::process::id(),
            started_at: now_secs_string(),
        };
        if try_claim_lease_exclusive(log_dir, &candidate)? {
            let (segment, segment_ordinal) = allocate_segment(log_dir, &candidate.run_id)?;
            return Ok(AcquiredLease {
                lease: candidate,
                segment,
                segment_ordinal,
                resumed: false,
            });
        }
    }
}

/// Retire the ACTIVE lease at a normal run completion: RENAME it to
/// `run-<runId>.json` so the next boot mints a fresh run instead of resuming a
/// finished one. Retained, never deleted, like worktrees.
///
/// `Ok(None)` when there was no active lease — retiring twice, or retiring a
/// run that never took one, is a no-op rather than an error.
pub fn retire_lease(log_dir: &Path) -> Result<Option<PathBuf>, PersistenceError> {
    let Some(lease) = load_lease(log_dir)? else {
        return Ok(None);
    };
    let retired = retired_lease_path(log_dir, &lease.run_id);
    std::fs::rename(lease_path(log_dir), &retired).map_err(|source| PersistenceError::Io {
        path: retired.clone(),
        source,
    })?;
    Ok(Some(retired))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(seq: u64, kind: &str, key: &str, payload: i64) -> JournalEntry {
        JournalEntry {
            seq,
            kind: kind.to_string(),
            key: key.to_string(),
            payload: serde_json::json!(payload),
        }
    }

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "selfharness-resume-{label}-{}-{}",
            std::process::id(),
            NEXT_TEST_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp test dir");
        dir
    }

    static NEXT_TEST_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    /// Folding a fold's own entries again yields the same map — the property
    /// that makes it safe to fold a run whose segments have not changed since
    /// the last boot.
    #[test]
    fn fold_is_idempotent() {
        let entries = vec![
            entry(0, "split", "a", 1),
            entry(1, "outcome", "a", 2),
            entry(2, "split", "b", 3),
        ];
        let once = ResumeFold::fold("run-1", &entries);
        let twice = ResumeFold::fold("run-1", &once.entries.values().cloned().collect::<Vec<_>>());
        assert_eq!(once, twice);
        assert_eq!(once.to_json(), twice.to_json());
    }

    /// The winner is POSITION in the given order, not `seq` — a later entry in
    /// the slice always wins its `(kind, key)` even when an earlier entry
    /// carries a higher `seq`. This is the property that makes a fold over the
    /// real physical byte order correct even against a foreign, hand-edited,
    /// or mis-seeded segment (a wrong `seq` can no longer invert the result).
    #[test]
    fn fold_winner_is_last_in_physical_order_regardless_of_seq() {
        let entries = vec![
            entry(99, "split", "a", 1), // high seq, but written FIRST
            entry(0, "split", "a", 2),  // low seq, but written LAST
        ];
        let folded = ResumeFold::fold("run-1", &entries);
        assert_eq!(
            folded.to_json()["entries"][0]["payload"],
            serde_json::json!(2),
            "the physically-last entry must win even though its seq is lower"
        );
    }

    /// `max_seq`/`next_seq` scan every folded-from entry, not just the
    /// survivors — an overwritten entry's `seq` still has to be accounted for
    /// so a resumed segment's handler never allocates a `seq` an earlier,
    /// dropped entry already used.
    #[test]
    fn max_seq_accounts_for_overwritten_entries_too() {
        let entries = vec![
            entry(99, "split", "a", 1), // overwritten below, but still the max seq
            entry(0, "split", "a", 2),
        ];
        let folded = ResumeFold::fold("run-1", &entries);
        assert_eq!(folded.len(), 1, "only one (kind, key) survives");
        assert_eq!(
            folded.max_seq(),
            Some(99),
            "max_seq must be the true max over everything folded, not just what survived"
        );
        assert_eq!(folded.next_seq(), 100);
    }

    #[test]
    fn empty_journal_folds_to_an_empty_fold() {
        let fold = ResumeFold::fold("run-1", &[]);
        assert!(fold.is_empty());
        assert_eq!(fold.len(), 0);
        assert_eq!(fold.max_seq(), None);
        assert_eq!(
            fold.next_seq(),
            0,
            "a run that journaled nothing starts its appends at 0, like a fresh one"
        );
        assert_eq!(
            fold.to_json(),
            serde_json::json!({"runId": "run-1", "entries": []})
        );
    }

    /// The wire shape `Tidepool.Resume`'s hand-written `FromJSON` is written
    /// against: `runId` plus a LIST of `{seq, kind, key, payload}`, sorted by
    /// `(kind, key)`.
    #[test]
    fn wire_shape_is_sorted_by_kind_then_key() {
        let fold = ResumeFold::fold(
            "run-7",
            &[
                entry(2, "split", "b", 3),
                entry(0, "split", "a", 1),
                entry(1, "outcome", "a", 2),
            ],
        );
        let json = fold.to_json();
        assert_eq!(json["runId"], serde_json::json!("run-7"));
        let entries = json["entries"].as_array().expect("entries is a list");
        let pairs: Vec<(&str, &str)> = entries
            .iter()
            .map(|e| {
                (
                    e["kind"].as_str().expect("kind"),
                    e["key"].as_str().expect("key"),
                )
            })
            .collect();
        assert_eq!(
            pairs,
            vec![("outcome", "a"), ("split", "a"), ("split", "b")]
        );
        assert_eq!(entries[0]["seq"], serde_json::json!(1));
        assert_eq!(entries[0]["payload"], serde_json::json!(2));
    }

    /// Segment naming/ordering: numeric, not lexicographic — segment 10 must
    /// sort after segment 9, never between segment 1 and segment 2.
    #[test]
    fn list_segments_orders_numerically_past_nine() {
        let dir = temp_dir("segment-order");
        // Written out of numeric order, and in a range where lexicographic
        // comparison ("10" < "2") would misorder them.
        for seg in [2u64, 10, 1, 9, 0] {
            std::fs::write(segment_path(&dir, "run-x", seg), "").expect("plant segment");
        }
        // A different run id's segment must never be picked up.
        std::fs::write(segment_path(&dir, "run-y", 5), "").expect("plant other run's segment");

        let listed = list_segments(&dir, "run-x").expect("list segments");
        let ordinals: Vec<u64> = listed
            .iter()
            .map(|p| {
                parse_segment_ordinal("run-x", p.file_name().unwrap().to_str().unwrap()).unwrap()
            })
            .collect();
        assert_eq!(ordinals, vec![0, 1, 2, 9, 10]);
    }

    /// A fresh run allocates segment 0.
    #[test]
    fn fresh_run_allocates_segment_zero() {
        let dir = temp_dir("fresh-segment-zero");
        let acquired = acquire_lease(&dir).expect("mint");
        assert!(!acquired.resumed);
        assert_eq!(
            acquired.segment,
            segment_path(&dir, &acquired.lease.run_id, 0)
        );
    }

    /// Every process that resumes a run allocates the NEXT unused segment —
    /// never the same one a prior process owned, so a crash mid-append can
    /// never be written into by a later process.
    #[test]
    fn each_resume_allocates_a_fresh_unused_segment() {
        let dir = temp_dir("resume-fresh-segment");

        let first = acquire_lease(&dir).expect("mint");
        assert_eq!(first.segment, segment_path(&dir, &first.lease.run_id, 0));
        std::fs::write(&first.segment, "").expect("simulate process 1 writing its segment");

        let second = acquire_lease(&dir).expect("resume 1");
        assert!(second.resumed);
        assert_eq!(second.lease.run_id, first.lease.run_id);
        assert_eq!(
            second.segment,
            segment_path(&dir, &first.lease.run_id, 1),
            "process 2 must never adopt process 1's segment"
        );
        assert_ne!(second.segment, first.segment);
        std::fs::write(&second.segment, "").expect("simulate process 2 writing its segment");

        let third = acquire_lease(&dir).expect("resume 2");
        assert_eq!(third.segment, segment_path(&dir, &first.lease.run_id, 2));

        // Every prior segment is still there — nothing here ever removes one.
        assert!(first.segment.exists());
        assert!(second.segment.exists());
    }

    /// A resumed process whose predecessor crashed before ever appending
    /// still advances to its OWN, later segment rather than reusing the
    /// predecessor's — allocation (exclusive `create_new`, [`allocate_segment`])
    /// is what CLAIMS an ordinal now, not the act of later writing to it, so
    /// an empty, never-appended segment is exactly as claimed as one with a
    /// torn tail.
    #[test]
    fn resume_after_a_predecessor_that_never_wrote_still_advances() {
        let dir = temp_dir("resume-no-write");
        let first = acquire_lease(&dir).expect("mint");
        assert!(
            first.segment.exists(),
            "allocation itself claims the segment file, empty, via create_new"
        );

        let second = acquire_lease(&dir).expect("resume");
        assert_eq!(second.lease.run_id, first.lease.run_id);
        assert_ne!(
            second.segment, first.segment,
            "the predecessor's segment is already claimed (its file exists, even \
             empty) — a resume must never reuse it"
        );
        assert_eq!(second.segment, segment_path(&dir, &first.lease.run_id, 1));
    }

    /// The race [`allocate_segment`] exists to close: several allocators
    /// contending for the SAME run id's next segment, simultaneously, via a
    /// barrier so every thread's `create_new` genuinely contends rather than
    /// serializing by scheduling luck. Every one must land on a DISTINCT
    /// path — under the pre-exclusivity design (a bare directory listing,
    /// no `create_new`) every thread would compute ordinal 0 from the same
    /// empty listing and return the SAME path.
    #[test]
    fn concurrent_allocators_never_collide_on_the_same_segment() {
        use std::sync::{Arc, Barrier};

        let dir = Arc::new(temp_dir("concurrent-allocate"));
        const N: usize = 8;
        let barrier = Arc::new(Barrier::new(N));
        let handles: Vec<_> = (0..N)
            .map(|_| {
                let dir = Arc::clone(&dir);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    allocate_segment(&dir, "run-concurrent").expect("allocate")
                })
            })
            .collect();
        let mut paths: Vec<SegmentPath> = handles
            .into_iter()
            .map(|h| h.join().expect("allocator thread must not panic").0)
            .collect();
        paths.sort();
        paths.dedup();
        assert_eq!(
            paths.len(),
            N,
            "every concurrently racing allocator must land on a distinct segment path, \
             and each SegmentPath must be a genuinely distinct claim, not just a distinct string"
        );
    }

    /// The race [`try_claim_lease_exclusive`] exists to close: several
    /// processes booting into the SAME EMPTY `log_dir` simultaneously, via a
    /// barrier so every thread's claim genuinely contends. Exactly ONE must
    /// win the fresh claim (`resumed == false`); every other racer must land
    /// on the winner's RESUME path (`resumed == true`, same `run_id`) rather
    /// than each minting its own — the orphaned-journal hazard this whole fix
    /// closes. Every racer, winner or loser, still gets its own distinct
    /// segment (unaffected by this fix — [`allocate_segment`]'s own
    /// exclusivity), and the lease left on disk afterward is intact, never a
    /// torn write from the contended claim.
    #[test]
    fn racing_fresh_acquirers_produce_one_winner_and_resumed_losers() {
        use std::sync::{Arc, Barrier};

        let dir = Arc::new(temp_dir("racing-fresh-acquire"));
        const N: usize = 8;
        let barrier = Arc::new(Barrier::new(N));
        let handles: Vec<_> = (0..N)
            .map(|_| {
                let dir = Arc::clone(&dir);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    acquire_lease(&dir).expect("acquire")
                })
            })
            .collect();
        let results: Vec<AcquiredLease> = handles
            .into_iter()
            .map(|h| h.join().expect("racing acquirer thread must not panic"))
            .collect();

        let winners: Vec<&AcquiredLease> = results.iter().filter(|r| !r.resumed).collect();
        assert_eq!(
            winners.len(),
            1,
            "exactly one racer must win the fresh claim, every other racer must \
             land on ITS resume path; got winners {winners:?}"
        );
        let winner_run_id = winners[0].lease.run_id.clone();

        for r in &results {
            assert_eq!(
                r.lease.run_id, winner_run_id,
                "every racer — winner or loser — must end up on the SAME run; a \
                 loser minting and keeping its own run id is exactly the \
                 orphaned-journal hazard this test guards against"
            );
        }

        // Every racer, even a loser, still claims its own distinct segment —
        // segment exclusivity was never the broken half of this race.
        let mut segments: Vec<SegmentPath> = results.iter().map(|r| r.segment.clone()).collect();
        segments.sort();
        segments.dedup();
        assert_eq!(
            segments.len(),
            N,
            "every racer must still land on a distinct segment"
        );

        // The lease left on disk is genuinely readable — no torn write
        // survived the contended claim.
        let on_disk = load_lease(&dir)
            .expect("load lease")
            .expect("a lease must exist after the race settles");
        assert_eq!(on_disk.run_id, winner_run_id);
    }

    /// A lease naming THIS process's own (necessarily alive) pid is exempt
    /// from the live-pid refusal — there is no second process here, so
    /// nothing to refuse. This is the case every OTHER resume test in this
    /// file relies on implicitly (they simulate "a later process resumes" by
    /// calling `acquire_lease` again within one test process), so it is
    /// pinned directly, once, here.
    #[test]
    fn self_owned_lease_resumes_without_a_live_pid_refusal() {
        let dir = temp_dir("lease-self-pid-exempt");
        let my_pid = std::process::id();
        write_lease(
            &dir,
            &RunLease {
                run_id: "run-self".to_string(),
                pid: my_pid,
                started_at: "0".to_string(),
            },
        )
        .expect("plant a lease naming this (guaranteed-alive) test process");

        let acquired = acquire_lease(&dir)
            .expect("a lease naming this process's own pid must resume, never refuse");
        assert!(acquired.resumed, "a lease was on disk — this boot resumes");
        assert_eq!(
            acquired.lease.pid, my_pid,
            "re-stamped to this process, as always"
        );
    }

    /// THE fix this lane exists for: a lease naming a DIFFERENT, genuinely
    /// LIVE process (a real child, not a simulated pid) is a hard refusal —
    /// `PersistenceError::LiveLeaseHeld`, naming the pid — and nothing on
    /// disk is touched (the lease still names the live child afterward, no
    /// segment was allocated).
    #[test]
    fn live_pid_lease_is_a_hard_refusal_without_takeover() {
        std::env::remove_var(LEASE_TAKEOVER_ENV_VAR);
        let dir = temp_dir("lease-live-pid-refusal");

        let mut child = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("spawn a real, genuinely-alive foreign process");
        let child_pid = child.id();

        write_lease(
            &dir,
            &RunLease {
                run_id: "run-live".to_string(),
                pid: child_pid,
                started_at: "0".to_string(),
            },
        )
        .expect("plant a lease naming the live child");

        let err = acquire_lease(&dir)
            .expect_err("a lease naming a different LIVE pid must refuse, never join");
        match &err {
            PersistenceError::LiveLeaseHeld { run_id, pid } => {
                assert_eq!(run_id, "run-live");
                assert_eq!(*pid, child_pid);
            }
            other => panic!("expected LiveLeaseHeld, got {other:?}"),
        }
        assert!(
            err.to_string().contains(LEASE_TAKEOVER_ENV_VAR),
            "the refusal must name the takeover remedy: {err}"
        );

        // Nothing was mutated: the lease still names the live child, and no
        // segment was allocated for this (refused) process.
        let still = load_lease(&dir).expect("load lease").expect("lease kept");
        assert_eq!(still.pid, child_pid, "the lease is untouched by a refusal");
        assert!(
            list_segments(&dir, "run-live")
                .expect("list segments")
                .is_empty(),
            "a refused acquisition must never allocate a segment"
        );

        let _ = child.kill();
        let _ = child.wait();
    }

    /// [`LEASE_TAKEOVER_ENV_VAR`]`=1` forces the join anyway: the resume
    /// succeeds, the prior lease record is archived (a
    /// `run-<id>.takeover-from-pid-<pid>-at-<ts>.json` sibling, distinct from
    /// [`retired_lease_path`]'s normal-completion naming) before the active
    /// lease is overwritten with this process's own pid.
    #[test]
    fn takeover_env_var_forcibly_claims_a_live_lease_and_archives_the_prior_record() {
        let dir = temp_dir("lease-takeover");

        let mut child = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("spawn a real, genuinely-alive foreign process");
        let child_pid = child.id();

        write_lease(
            &dir,
            &RunLease {
                run_id: "run-takeover".to_string(),
                pid: child_pid,
                started_at: "0".to_string(),
            },
        )
        .expect("plant a lease naming the live child");

        // SAFETY (env mutation in a test): this crate's suite runs one test
        // per OS process under the project's mandated `cargo-nextest` runner
        // (root CLAUDE.md), so no other test observes this process's env.
        std::env::set_var(LEASE_TAKEOVER_ENV_VAR, "1");
        let acquired = acquire_lease(&dir);
        std::env::remove_var(LEASE_TAKEOVER_ENV_VAR);
        let acquired = acquired.expect("the takeover env var must force the join");

        assert!(acquired.resumed);
        assert_eq!(acquired.lease.run_id, "run-takeover");
        assert_eq!(
            acquired.lease.pid,
            std::process::id(),
            "the active lease is re-stamped to the taking-over process"
        );

        let archived: Vec<_> = std::fs::read_dir(&dir)
            .expect("read dir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("takeover-from-pid"))
            .collect();
        assert_eq!(
            archived.len(),
            1,
            "exactly one archived record of the forced takeover, got {archived:?}"
        );
        assert!(
            archived[0].contains(&child_pid.to_string()),
            "the archived record must name the prior (taken-over) pid: {archived:?}"
        );

        let _ = child.kill();
        let _ = child.wait();
    }

    /// A lease naming a DEAD pid reclaims exactly as before this fix —
    /// unaffected by the live-pid refusal or the takeover machinery, no env
    /// var needed, and no takeover-archive record is written (this is an
    /// ordinary crash resume, not a forced claim from a live owner).
    #[test]
    fn dead_pid_lease_reclaims_without_a_hard_refusal_or_takeover_var() {
        std::env::remove_var(LEASE_TAKEOVER_ENV_VAR);
        let dir = temp_dir("lease-dead-pid-reclaim");

        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("spawn a process that exits immediately");
        let dead_pid = child.id();
        child.wait().expect("reap it — now genuinely dead");

        write_lease(
            &dir,
            &RunLease {
                run_id: "run-dead".to_string(),
                pid: dead_pid,
                started_at: "0".to_string(),
            },
        )
        .expect("plant a lease naming the now-dead child");

        let acquired =
            acquire_lease(&dir).expect("a lease naming a dead pid must reclaim, never refuse");
        assert!(acquired.resumed);
        assert_eq!(acquired.lease.run_id, "run-dead");
        assert_eq!(acquired.lease.pid, std::process::id());

        let archived: Vec<_> = std::fs::read_dir(&dir)
            .expect("read dir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("takeover-from-pid"))
            .collect();
        assert!(
            archived.is_empty(),
            "an ordinary dead-pid reclaim must never write a takeover-archive record: {archived:?}"
        );
    }

    /// Mint → resume (same run id, a NEW segment) → retire (the lease is
    /// RENAMED, not deleted) → the next boot mints a FRESH run.
    #[test]
    fn lease_mint_resume_retire_round_trip() {
        let dir = temp_dir("lease");

        let first = acquire_lease(&dir).expect("mint");
        assert!(!first.resumed, "no lease on disk means a fresh run");

        // A second process over the same log dir inherits the run.
        let second = acquire_lease(&dir).expect("resume");
        assert!(second.resumed, "an existing lease means this boot resumes");
        assert_eq!(second.lease.run_id, first.lease.run_id);

        let retired = retire_lease(&dir)
            .expect("retire")
            .expect("a lease to retire");
        assert_eq!(retired, retired_lease_path(&dir, &first.lease.run_id));
        assert!(retired.exists(), "a retired lease is kept, never deleted");
        assert!(
            !lease_path(&dir).exists(),
            "the active lease must be gone so the next boot mints"
        );
        assert_eq!(retire_lease(&dir).expect("retiring twice is a no-op"), None);

        // Now the next boot is a FRESH run again — and NOT the retired one
        // wearing its name. Minting inside the same wall-clock second as the
        // retirement is the case that catches this: `{secs}-{pid}` alone
        // repeats, and the "fresh" run would otherwise be handed a run id a
        // finished run already owns.
        let third = acquire_lease(&dir).expect("mint after retirement");
        assert!(!third.resumed);
        assert_ne!(
            third.lease.run_id, first.lease.run_id,
            "a retired run's id must not be minted again"
        );
    }

    /// A lease that exists but does not parse is loud, never a silent reset to
    /// "fresh run" — that would silently redo a run's finished work.
    #[test]
    fn malformed_lease_is_a_typed_error() {
        let dir = temp_dir("lease-malformed");
        std::fs::write(lease_path(&dir), b"{not json").expect("write a torn lease");
        let err = load_lease(&dir).expect_err("a malformed lease must not read as absent");
        assert!(matches!(err, PersistenceError::Json { .. }), "got {err:?}");
    }

    /// The regression this whole lane exists for: a segment with a torn tail
    /// (a crash mid-append) folds correctly on the boot that finds it, and —
    /// unlike the single-file design — every FURTHER boot stays parseable,
    /// because each one opens its own fresh segment rather than appending
    /// into the torn one. Walks several boots past the torn tail, not just
    /// one, since "one boot deep" was exactly the old design's limit.
    #[test]
    fn a_segment_with_a_torn_tail_never_poisons_a_later_boot() {
        let dir = temp_dir("torn-tail-segments");
        let run_id = "run-torn";

        // Segment 0: two complete lines, then a torn (no-trailing-newline)
        // final line — exactly what a kill mid-`write_all` leaves.
        let complete = format!(
            "{}\n{}\n",
            serde_json::json!({"seq": 0, "kind": "step", "key": "alpha", "payload": 1}),
            serde_json::json!({"seq": 1, "kind": "step", "key": "beta", "payload": 2}),
        );
        let torn_full =
            serde_json::json!({"seq": 2, "kind": "step", "key": "gamma", "payload": 3}).to_string();
        let torn_prefix = &torn_full[..torn_full.len() / 2];
        std::fs::write(
            segment_path(&dir, run_id, 0),
            format!("{complete}{torn_prefix}"),
        )
        .expect("plant a torn-tail segment 0");

        // Boot 2 folds segment 0 (torn tail skipped) and writes its OWN
        // segment 1 — never touching segment 0's bytes.
        let fold1 = fold_run_journal(&dir, run_id).expect("fold across segment 0 alone");
        assert_eq!(
            fold1.len(),
            2,
            "alpha and beta survive; the torn gamma does not"
        );
        assert_eq!(fold1.next_seq(), 2, "gamma's seq was never durably claimed");
        std::fs::write(
            segment_path(&dir, run_id, 1),
            format!(
                "{}\n",
                serde_json::json!({"seq": 2, "kind": "step", "key": "gamma", "payload": 3})
            ),
        )
        .expect("boot 2 redoes gamma into its OWN segment");

        // Boot 3, 4, 5: every further boot still folds cleanly. Under the old
        // single-file design this is exactly where a second append onto the
        // merged torn line would start failing loudly.
        for boot in [2u64, 3, 4] {
            let seg = segment_path(&dir, run_id, boot);
            std::fs::write(&seg, "").expect("a later boot's own (empty) segment");
            let folded = fold_run_journal(&dir, run_id)
                .unwrap_or_else(|e| panic!("boot past the torn tail must stay parseable: {e}"));
            assert_eq!(
                folded.len(),
                3,
                "alpha, beta, and the redone gamma must all still be present at boot {boot}"
            );
        }

        // Segment 0's torn tail is still there, byte for byte — nothing ever
        // rewrites, truncates, or deletes it.
        let seg0 = std::fs::read_to_string(segment_path(&dir, run_id, 0)).expect("segment 0 read");
        assert!(
            seg0.ends_with(torn_prefix),
            "segment 0's torn tail is untouched"
        );
    }

    /// Every segment survives a resume on disk — retention is a property of
    /// the whole run, not just the segments folded from.
    #[test]
    fn every_segment_is_retained_after_a_resume() {
        let dir = temp_dir("retain-all-segments");
        let first = acquire_lease(&dir).expect("mint");
        std::fs::write(&first.segment, "").expect("process 1 writes");
        let second = acquire_lease(&dir).expect("resume");
        std::fs::write(&second.segment, "").expect("process 2 writes");
        let third = acquire_lease(&dir).expect("resume again");
        std::fs::write(&third.segment, "").expect("process 3 writes");

        let listed = list_segments(&dir, &first.lease.run_id).expect("list");
        assert_eq!(listed, vec![first.segment, second.segment, third.segment]);
    }
}
