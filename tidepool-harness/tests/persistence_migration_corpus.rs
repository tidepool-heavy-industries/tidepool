//! Replay fixtures for persistence formats that still support their original
//! unstamped representation. Each fixture is decoded through the production
//! reader and checked for preserved data, not merely successful parsing.
//!
//! Formats whose compatibility floor has advanced beyond v0 do not belong in
//! this corpus. Their below-floor behavior is covered by the reader's own
//! boundary tests.

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

/// Every format covered here has an unstamped fixture.
#[test]
fn every_kind_has_a_v0_fixture() {
    for kind in [
        "checkpoint",
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
