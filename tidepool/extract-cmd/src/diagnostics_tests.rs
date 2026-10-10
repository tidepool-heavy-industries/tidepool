use super::{is_machine_stderr_line, MACHINE_STDERR_PREFIXES};
use std::collections::BTreeSet;

/// Scans a Haskell source file for every `"tidepool-<word> "` literal —
/// a quoted string starting with `tidepool-` whose first token (up to
/// the next space, found strictly before the closing quote) is the
/// emitted line's prefix. This is a scan of the literal text the worker
/// actually writes, not a copy of this module's own list, so a renamed
/// or newly added emitter is picked up and a removed one is caught as a
/// stale entry here.
fn emitted_prefixes(source: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut cursor = 0usize;
    while let Some(rel) = source[cursor..].find("\"tidepool-") {
        let word_start = cursor + rel + 1; // past the opening quote
        let tail = &source[word_start..];
        let space = tail.find(' ');
        let quote = tail.find('"');
        if let (Some(space), Some(quote)) = (space, quote) {
            if space < quote {
                let prefix = &tail[..space];
                if prefix
                    .chars()
                    .all(|character| character.is_ascii_lowercase() || character == '-')
                {
                    found.insert(format!("{prefix} "));
                }
            }
        }
        cursor = word_start;
    }
    found
}

#[test]
fn prefix_scanner_does_not_admit_rendered_source_errors() {
    assert!(emitted_prefixes("\"tidepool-reuse-error: witness failed\"").is_empty());
    assert_eq!(
        emitted_prefixes("\"tidepool-validation {}\""),
        BTreeSet::from(["tidepool-validation ".to_string()])
    );
}

/// The daemon's and the human-facing renderer's shared prefix list must
/// name exactly the machine-readable lines the worker and frontend emit —
/// no more (a stale prefix that filters nothing) and no fewer (a new
/// emitter whose lines leak into a source diagnostic).
#[test]
fn machine_stderr_prefixes_match_the_emitters() {
    let timing = include_str!("../../../bridge/haskell/src/Tidepool/Timing.hs");
    let ghc_pipeline = include_str!("../../../bridge/haskell/src/Tidepool/GhcPipeline.hs");

    let mut emitted = emitted_prefixes(timing);
    emitted.extend(emitted_prefixes(ghc_pipeline));
    emitted.extend(emitted_prefixes(include_str!("daemon.rs")));

    let declared: BTreeSet<String> = MACHINE_STDERR_PREFIXES
        .iter()
        .map(|prefix| prefix.to_string())
        .collect();

    assert_eq!(
        declared, emitted,
        "MACHINE_STDERR_PREFIXES has drifted from the tidepool-* literals \
         emitted by Timing.hs/GhcPipeline.hs/daemon.rs"
    );
}

#[test]
fn meta_execution_lines_are_measurements_only_at_the_prefix_boundary() {
    for (line, machine) in [
        (
            "tidepool-meta-execution request=7 unit=\"main\" module=\"Original\"",
            true,
        ),
        (
            "  tidepool-meta-execution request=7 unit=\"main\" module=\"Original\"",
            true,
        ),
        ("tidepool-meta-execution-error: splice failed", false),
        (
            "Original.hs:1: error: tidepool-meta-execution is not in scope",
            false,
        ),
        ("tidepool-reuse {\"schema\":1}", true),
        ("  tidepool-reuse {\"schema\":1}", true),
        ("tidepool-reuse-error: witness failed", false),
        (
            "Original.hs:1: error: tidepool-reuse is not in scope",
            false,
        ),
        ("tidepool-checked-reused-source module=Support", true),
        ("  tidepool-checked-reused-source module=Support", true),
        (
            "tidepool-checked-reused-source-error: witness failed",
            false,
        ),
        (
            "Original.hs:1: error: tidepool-checked-reused-source is not in scope",
            false,
        ),
    ] {
        assert_eq!(is_machine_stderr_line(line), machine);
    }
}
