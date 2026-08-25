//! The old-corpus replay test: every fixture under `tests/fixtures/persistence-corpus/<kind>/` is a
//! FROZEN, NEVER-REGENERATED artifact standing in for a real operator's
//! on-disk state at some past version — the inverse of `tidepool-repr`'s
//! `golden_wire_contract.rs` corpus, which IS regenerated on every breaking
//! bump because a `.cbor` fixture is a reproducible build artifact and a
//! `Checkpoint`/journal fixture is not.
//!
//! Each kind's directory currently holds exactly one fixture, `v0.*` — the
//! UNSTAMPED shape every one of the six persistence-versioning kinds had
//! before this design landed (no `"version"` key at all, reading as version
//! `0` by the unstamped-file convention — see
//! `tidepool_repr::version_ladder`'s module doc). Landing the very first
//! real, non-identity migration on any kind adds exactly one more fixture
//! to that kind's directory: the shape immediately BEFORE that migration
//! lands, captured the same way this one was (see each fixture's sibling
//! `.txt` note, or this file's own history, for how). This test walks each
//! kind's directory generically — a new fixture needs no new test
//! function — decodes every file through that kind's real versioned
//! reader, and asserts specific known-good fields land at their expected
//! value, not merely that decoding succeeded (a migration that silently
//! dropped a field would still parse cleanly into the current struct's
//! defaults).

use std::fs;
use std::path::PathBuf;

fn corpus_dir(kind: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/persistence-corpus")
        .join(kind)
}

fn fixtures(kind: &str) -> Vec<PathBuf> {
    let dir = corpus_dir(kind);
    let mut paths: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read corpus dir {dir:?}: {e}"))
        .map(|entry| entry.expect("dir entry").path())
        .filter(|p| p.is_file())
        .collect();
    paths.sort();
    assert!(
        !paths.is_empty(),
        "corpus dir {dir:?} has no fixtures — every kind must carry at least the v0 \
         (unstamped) fixture"
    );
    paths
}

/// Kind 1: `Checkpoint`'s envelope + `state` blob (two independent
/// counters — see `selfharness::persistence::{ENVELOPE_CURRENT,STATE_CURRENT}`).
#[test]
fn checkpoint_corpus_migrates_to_current() {
    use tidepool_harness::selfharness::persistence::{
        load_checkpoint, ENVELOPE_CURRENT, STATE_CURRENT,
    };
    for path in fixtures("checkpoint") {
        let checkpoint = load_checkpoint(&path)
            .unwrap_or_else(|e| panic!("{path:?}: must migrate and load, got {e}"))
            .unwrap_or_else(|| panic!("{path:?}: file exists, must decode to Some"));
        assert_eq!(
            checkpoint.version, ENVELOPE_CURRENT,
            "{path:?}: envelope must land at the current version"
        );
        assert_eq!(
            checkpoint.state_version, STATE_CURRENT,
            "{path:?}: state must land at the current version"
        );
        // Known-good field, not just "it parsed": the legacy fixture's
        // `state.mode` survives the migration byte-for-byte.
        assert_eq!(
            checkpoint.state.get("mode").and_then(|v| v.as_str()),
            Some("Deciding"),
            "{path:?}: state.mode must survive migration"
        );
        assert_eq!(checkpoint.generation().get(), 1, "{path:?}: generation");
    }
}

/// Kind 3: the harness log `Event`/`LogHeader` — see `log::version`.
#[test]
fn harness_log_corpus_migrates_to_current() {
    use tidepool_harness::log::{Event, LogReader};
    for path in fixtures("harness-log") {
        let (header, iter) =
            LogReader::open(&path).unwrap_or_else(|e| panic!("{path:?}: must load, got {e}"));
        assert_eq!(header.prelude_hash, "prelude-legacy", "{path:?}: header");
        let records: Vec<_> = iter
            .collect::<Result<_, _>>()
            .unwrap_or_else(|e| panic!("{path:?}: every event must decode, got {e}"));
        assert!(
            !records.is_empty(),
            "{path:?}: must carry at least one event"
        );
        let node_done = records
            .iter()
            .find_map(|r| match &r.event {
                Event::NodeDone {
                    result_rendered, ..
                } => Some(result_rendered.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{path:?}: must carry a NodeDone event"));
        assert_eq!(node_done, "42", "{path:?}: NodeDone.result_rendered");
    }
}

/// Kind 4: the worktree event journal — see `tidepool_worktree::journal_version`
/// (`tidepool-worktree` module, private but exercised here through the
/// public `EventJournal::open`).
#[test]
fn worktree_journal_corpus_migrates_to_current() {
    use tidepool_worktree::{EventJournal, RepositoryEvent};
    for path in fixtures("worktree-journal") {
        let journal =
            EventJournal::open(&path).unwrap_or_else(|e| panic!("{path:?}: must load, got {e:?}"));
        let entries = journal.since(0);
        assert!(
            !entries.is_empty(),
            "{path:?}: must carry at least one entry"
        );
        match &entries[0].event {
            RepositoryEvent::HeadChanged(r) => {
                assert_eq!(r.worktree.as_str(), "wt-legacy", "{path:?}: worktree id");
            }
            other => panic!("{path:?}: expected HeadChanged, got {other:?}"),
        }
    }
}

/// Kind 5: the handlers per-segment journal — see
/// `tidepool_handlers::handlers::journal_version` (private, exercised
/// through the public `load_journal`).
#[test]
fn handlers_journal_corpus_migrates_to_current() {
    use tidepool_handlers::load_journal;
    for path in fixtures("handlers-journal") {
        let entries =
            load_journal(&path).unwrap_or_else(|e| panic!("{path:?}: must load, got {e}"));
        assert!(
            !entries.is_empty(),
            "{path:?}: must carry at least one entry"
        );
        assert_eq!(entries[0].kind, "split", "{path:?}: entries[0].kind");
        assert_eq!(entries[0].key, "branch/legacy", "{path:?}: entries[0].key");
        assert_eq!(
            entries[0].payload.get("note").and_then(|v| v.as_str()),
            Some("pre-versioning segment"),
            "{path:?}: entries[0].payload.note"
        );
    }
}

/// Kind 6: the selfharness transcript — the lowest-risk of the four JSONL
/// consumers (`persistence-versioning-design.md` §2): no first-party reader
/// exists yet, so there is nothing to GATE, only a version to detect. This
/// pins that [`tidepool_harness::selfharness::persistence::read_transcript_header`]
/// correctly reads the legacy (unstamped) fixture as version `0`.
#[test]
fn selfharness_transcript_corpus_reads_as_unstamped() {
    use tidepool_harness::selfharness::persistence::read_transcript_header;
    for path in fixtures("selfharness-transcript") {
        let version = read_transcript_header(&path)
            .unwrap_or_else(|e| panic!("{path:?}: must read header, got {e}"));
        assert_eq!(
            version, 0,
            "{path:?}: legacy fixture must read as version 0"
        );
    }
}

/// Every kind's corpus directory actually exists and holds the floor (v0,
/// unstamped) fixture — a sanity check that the walker above isn't
/// vacuously passing because a directory went missing.
#[test]
fn every_kind_has_a_v0_fixture() {
    for kind in [
        "checkpoint",
        "harness-log",
        "worktree-journal",
        "handlers-journal",
        "selfharness-transcript",
    ] {
        let v0 = fixtures(kind);
        assert!(
            v0.iter().any(|p| p
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s == "v0")
                .unwrap_or(false)),
            "kind {kind:?} is missing its v0 (unstamped) fixture"
        );
    }
}
