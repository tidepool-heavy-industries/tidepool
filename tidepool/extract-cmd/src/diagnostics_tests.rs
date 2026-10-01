use super::MACHINE_STDERR_PREFIXES;
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
                found.insert(format!("{} ", &tail[..space]));
            }
        }
        cursor = word_start;
    }
    found
}

/// The daemon's and the human-facing renderer's shared prefix list must
/// name exactly the machine-readable lines the compiler worker emits —
/// no more (a stale prefix that filters nothing) and no fewer (a new
/// emitter whose lines leak into a source diagnostic).
#[test]
fn machine_stderr_prefixes_match_the_haskell_emitters() {
    let timing = include_str!("../../../bridge/haskell/src/Tidepool/Timing.hs");
    let ghc_pipeline = include_str!("../../../bridge/haskell/src/Tidepool/GhcPipeline.hs");

    let mut emitted = emitted_prefixes(timing);
    emitted.extend(emitted_prefixes(ghc_pipeline));

    let declared: BTreeSet<String> = MACHINE_STDERR_PREFIXES
        .iter()
        .map(|prefix| prefix.to_string())
        .collect();

    assert_eq!(
        declared, emitted,
        "MACHINE_STDERR_PREFIXES has drifted from the tidepool-* literals \
         emitted by Timing.hs/GhcPipeline.hs"
    );
}
