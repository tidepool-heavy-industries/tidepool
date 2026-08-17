//! The READ half of git-plus-journal persistence (PRD 20 S1-L5,
//! `plans/self-iterating-harness/20-s1-l5-resume.md`): run identity that
//! outlives a process, and the boot-time FOLD of that run's journal.
//!
//! `record` (`Tidepool.Journal`) is write-only on the authored surface. Locating
//! a run's journal, loading it, folding it, and injecting the result is the
//! DRIVER's job — this module is the driver's half of it. Nothing here appends,
//! rewrites, truncates, or compacts a journal: the only file this module ever
//! WRITES is the run lease.
//!
//! # The run lease — which journal a resumed run folds, and appends to
//!
//! A run must be identifiable BEFORE its first `record` (a crash in cycle 1
//! leaves no checkpoint, so the checkpoint cannot carry the run id) and must
//! outlive the process (a resumed run is a different process, so per-process
//! naming would fold nothing and orphan the prior file). The lease is one file,
//! `<log_dir>/run-current.json`, written at boot before any handler is wired:
//!
//! ```json
//! {"runId": "20260817-101112-48213",
//!  "journal": "…/journal-20260817-101112-48213.jsonl",
//!  "pid": 48213, "startedAt": "…"}
//! ```
//!
//! | boot condition | behaviour |
//! |---|---|
//! | no lease | mint a `runId`, write the lease, fresh journal file, empty fold |
//! | a lease, journal present | RESUME: same `runId`, append to the same file, fold it |
//! | a lease, journal missing | resume with an EMPTY fold, keep the `runId` (the file appears on first append) |
//! | `run_loop` returns normally | [`retire_lease`]: rename to `run-<runId>.json`, RETAINED — so the next boot mints a fresh run |
//! | the process crashes | the lease survives → the next boot resumes |
//!
//! One journal file per run id, appended across however many processes that run
//! takes. Nothing is ever rewritten and nothing is ever deleted — a retired
//! lease is renamed, not removed.
//!
//! # The fold
//!
//! [`ResumeFold`] keys on the `(kind, key)` PAIR, not the key alone: a harness
//! records several kinds of fact about one branch (dev-tree writes a `"split"`
//! and an `"outcome"` under the same branch name), and keying on the key alone
//! would collapse them. The winner is MAX `seq`
//! ([`tidepool_handlers::last_by_kind_key`]), which makes the fold idempotent
//! and order-insensitive rather than merely last-line-wins.
//!
//! The fold is GENERIC over `(kind, key, payload)`. Payloads are opaque
//! [`serde_json::Value`]s end to end — dev-tree's
//! `split`/`outcome`/`replan`/`rebase`/`escalation` vocabulary is the authoring
//! harness's schema and never appears in this crate.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use tidepool_handlers::{last_by_kind_key, JournalEntry};

use super::persistence::PersistenceError;

/// Everything the driver folded out of one run's journal at boot: the last
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
}

impl ResumeFold {
    /// Fold `entries` (as [`tidepool_handlers::load_journal`] returned them)
    /// down to the last record per `(kind, key)`.
    ///
    /// IDEMPOTENT and ORDER-INSENSITIVE by construction: the result is a pure
    /// function of the SET of entries, because the winner is max `seq` rather
    /// than file position. Folding the same file twice, folding it again after
    /// a resumed process appended to it, or folding an interleaved append order
    /// all yield the same map (plus whatever is genuinely new).
    pub fn fold(run_id: impl Into<String>, entries: &[JournalEntry]) -> Self {
        ResumeFold {
            run_id: run_id.into(),
            entries: last_by_kind_key(entries).into_iter().collect(),
        }
    }

    /// The fold of a run that recorded nothing — what a fresh boot carries.
    pub fn empty(run_id: impl Into<String>) -> Self {
        ResumeFold {
            run_id: run_id.into(),
            entries: BTreeMap::new(),
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

    /// The highest `seq` in the fold, or `None` for an empty one. Equal to the
    /// highest `seq` across ALL loaded entries (the globally-newest entry is by
    /// definition the winner for its own `(kind, key)`), which is what makes
    /// `max_seq() + 1` the right seed for
    /// [`tidepool_handlers::JournalHandler::resuming`].
    pub fn max_seq(&self) -> Option<u64> {
        self.entries.values().map(|e| e.seq).max()
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunLease {
    /// Stable across every process this run survives — what the journal file is
    /// named after and what a [`ResumeFold`] reports.
    #[serde(rename = "runId")]
    pub run_id: String,
    /// The run's journal file. Held here rather than re-derived at each use so
    /// the id and the path cannot desync.
    pub journal: PathBuf,
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
/// a finished run's identity stays readable beside its journal.
pub fn retired_lease_path(log_dir: &Path, run_id: &str) -> PathBuf {
    log_dir.join(format!("run-{run_id}.json"))
}

/// The journal file a run id owns: `<log_dir>/journal-<runId>.jsonl`. One file
/// per RUN, not per process — that is the whole point of the lease.
pub fn journal_path_for(log_dir: &Path, run_id: &str) -> PathBuf {
    log_dir.join(format!("journal-{run_id}.jsonl"))
}

/// Mint a run id that no run in `log_dir` already owns.
///
/// The base is `{epoch-seconds}-{pid}` — both halves, not the timestamp alone,
/// since two processes launched within one wall-clock second would otherwise
/// collide. But that base is NOT sufficient on its own: one process retiring a
/// run and starting another inside the same second mints the same id twice, and
/// the "fresh" run would then adopt the finished run's journal — appending into
/// it and, on a later boot, folding its entries as though they were its own.
///
/// So the mint checks the artifacts rather than trusting the clock: a base
/// whose journal file or whose retired lease already exists gets a `-<n>`
/// suffix until neither does. That makes "a fresh run never adopts an existing
/// run's journal" a property of the directory, not of how fast the clock ticks.
pub fn mint_run_id_in(log_dir: &Path) -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let base = format!("{secs}-{}", std::process::id());
    let mut candidate = base.clone();
    let mut n = 1u32;
    while journal_path_for(log_dir, &candidate).exists()
        || retired_lease_path(log_dir, &candidate).exists()
    {
        candidate = format!("{base}-{n}");
        n += 1;
    }
    candidate
}

/// What [`acquire_lease`] found: the run this process is now part of, and
/// whether it INHERITED that identity from a prior process (a crash) or minted
/// it.
#[derive(Debug, Clone, PartialEq)]
pub struct AcquiredLease {
    pub lease: RunLease,
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

/// Write `lease` as the ACTIVE lease, creating `log_dir` if needed. Atomic (a
/// `.tmp` sibling, then a rename over the target) for the same reason
/// `save_checkpoint` is: a kill mid-write must never leave a torn lease for the
/// next boot to read.
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
    let tmp = PathBuf::from(format!("{}.tmp", path.display()));
    std::fs::write(&tmp, &bytes).map_err(|source| PersistenceError::Io {
        path: tmp.clone(),
        source,
    })?;
    std::fs::rename(&tmp, &path).map_err(|source| PersistenceError::Io { path, source })
}

/// The boot-time lease step: RESUME the run a prior process left behind, or
/// mint a fresh one. See this module's doc for the four cases — note that all
/// three non-retired ones end here with a `RunLease` in hand, and only the FOLD
/// differs between them (a missing journal file loads as an empty journal, so
/// "a lease, journal missing" needs no branch of its own).
///
/// Writing the lease on the resume path too is deliberate: it re-stamps `pid`
/// and `startedAt` with the process that now holds the run, which is what a
/// human reading the file wants, and it is the same atomic write either way.
pub fn acquire_lease(log_dir: &Path) -> Result<AcquiredLease, PersistenceError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        .to_string();
    let (mut lease, resumed) = match load_lease(log_dir)? {
        Some(lease) => (lease, true),
        None => {
            let run_id = mint_run_id_in(log_dir);
            let journal = journal_path_for(log_dir, &run_id);
            (
                RunLease {
                    run_id,
                    journal,
                    pid: std::process::id(),
                    started_at: now.clone(),
                },
                false,
            )
        }
    };
    lease.pid = std::process::id();
    lease.started_at = now;
    write_lease(log_dir, &lease)?;
    Ok(AcquiredLease { lease, resumed })
}

/// Retire the ACTIVE lease at a normal run completion: RENAME it to
/// `run-<runId>.json` so the next boot mints a fresh run instead of resuming a
/// finished one. Retained, never deleted (PRD 20's "retained like worktrees").
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
    /// that makes it safe to fold a file a resumed process has since appended
    /// to.
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

    /// Order-insensitivity is the reason the winner is max seq and not file
    /// position: an interleaved append order must fold identically, down to
    /// the emitted wire bytes.
    #[test]
    fn fold_is_order_insensitive_including_its_wire_bytes() {
        let canonical = vec![
            entry(0, "split", "a", 1),
            entry(1, "outcome", "a", 2),
            entry(2, "split", "b", 3),
            entry(3, "split", "a", 4),
        ];
        let expected = ResumeFold::fold("run-1", &canonical);
        assert_eq!(
            expected.max_seq(),
            Some(3),
            "the newest entry must survive the fold"
        );

        for shift in 1..canonical.len() {
            let mut shuffled = canonical[shift..].to_vec();
            shuffled.extend_from_slice(&canonical[..shift]);
            let folded = ResumeFold::fold("run-1", &shuffled);
            assert_eq!(folded, expected, "rotation by {shift} folded differently");
            assert_eq!(
                folded.to_json().to_string(),
                expected.to_json().to_string(),
                "rotation by {shift} emitted different wire bytes — the compile \
                 memo would miss on an equivalent fold"
            );
        }
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

    /// Mint → resume (same run id, same journal path) → retire (the lease is
    /// RENAMED, not deleted) → the next boot mints a FRESH run.
    #[test]
    fn lease_mint_resume_retire_round_trip() {
        let dir = temp_dir("lease");

        let first = acquire_lease(&dir).expect("mint");
        assert!(!first.resumed, "no lease on disk means a fresh run");
        assert_eq!(
            first.lease.journal,
            journal_path_for(&dir, &first.lease.run_id)
        );

        // A second process over the same log dir inherits the run.
        let second = acquire_lease(&dir).expect("resume");
        assert!(second.resumed, "an existing lease means this boot resumes");
        assert_eq!(second.lease.run_id, first.lease.run_id);
        assert_eq!(second.lease.journal, first.lease.journal);

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
        // repeats, and the "fresh" run would append into the finished run's
        // journal and later fold its entries as its own.
        let third = acquire_lease(&dir).expect("mint after retirement");
        assert!(!third.resumed);
        assert_ne!(
            third.lease.run_id, first.lease.run_id,
            "a retired run's id must not be minted again"
        );
        assert_ne!(
            third.lease.journal, first.lease.journal,
            "a fresh run must never adopt a retired run's journal file"
        );
    }

    /// "A lease, journal missing": keep the run id, fold nothing. The journal
    /// file appears on the first append — a run that crashed before recording
    /// anything must still be the SAME run.
    #[test]
    fn lease_present_but_journal_missing_resumes_with_an_empty_fold() {
        let dir = temp_dir("lease-no-journal");
        let first = acquire_lease(&dir).expect("mint");
        assert!(
            !first.lease.journal.exists(),
            "nothing has recorded yet, so no journal file exists"
        );

        let resumed = acquire_lease(&dir).expect("resume");
        assert!(resumed.resumed);
        assert_eq!(resumed.lease.run_id, first.lease.run_id);

        let entries = tidepool_handlers::load_journal(&resumed.lease.journal)
            .expect("a missing journal loads as empty, never an error");
        let fold = ResumeFold::fold(&resumed.lease.run_id, &entries);
        assert!(fold.is_empty());
        assert_eq!(fold.run_id(), first.lease.run_id);
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
}
