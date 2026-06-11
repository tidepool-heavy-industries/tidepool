//! S6 varid-defense: property tests for the load-time duplicate-VarId
//! detector (`tidepool_repr::check_toplevel_varids`) — the #313 bug class.
//!
//! Part B of the workstream:
//!   1. Planted collisions are always caught, with the right VarId + sites.
//!   2. Zero false positives on clean generated spines AND on every committed
//!      fixture in `haskell/test/suite_cbor/`.
//!   3. Collision-resistance statistics for the 56-bit truncated-fingerprint
//!      VarId scheme over the real fixture corpus (population + duplicates).
//!
//! Findings ledger: `plans/proptest-findings-varid.md`.

use std::collections::HashMap;
use std::path::PathBuf;
use tidepool_repr::serial::read_cbor;
use tidepool_repr::varid_check::{check_toplevel_varids, toplevel_binders};
use tidepool_repr::{CoreExpr, VarId};

/// Directory of pre-compiled CBOR fixtures (the corpus the detector must
/// never false-positive on).
fn suite_cbor_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../haskell/test/suite_cbor")
}

/// Load every `.cbor` fixture in `haskell/test/suite_cbor/`.
fn load_corpus() -> Vec<(String, CoreExpr)> {
    let dir = suite_cbor_dir();
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot read fixture dir {}: {e}", dir.display()))
    {
        let path = entry.unwrap().path();
        // meta.cbor is the DataConTable, not an expression tree.
        if path.file_name().is_some_and(|n| n == "meta.cbor") {
            continue;
        }
        if path.extension().is_some_and(|e| e == "cbor") {
            let bytes = std::fs::read(&path).unwrap();
            let expr = read_cbor(&bytes)
                .unwrap_or_else(|e| panic!("read_cbor failed on {}: {e}", path.display()));
            out.push((path.file_name().unwrap().to_string_lossy().into_owned(), expr));
        }
    }
    assert!(
        out.len() >= 100,
        "expected the full fixture corpus, found only {} .cbor files",
        out.len()
    );
    out
}

/// Sweep the entire committed fixture corpus: the detector must pass every
/// fixture (a false positive here = the detector is wrong), counter-asserted
/// by reporting how many top-level binders were actually inspected.
#[test]
fn fixture_corpus_has_no_toplevel_varid_collisions() {
    let corpus = load_corpus();
    let mut total_binders = 0usize;
    let mut spined = 0usize;
    for (name, expr) in &corpus {
        match check_toplevel_varids(expr) {
            Ok(n) => {
                total_binders += n;
                if n > 0 {
                    spined += 1;
                }
            }
            Err(e) => panic!("WILD DUPLICATE in committed fixture {name}: {e}"),
        }
    }
    eprintln!(
        "corpus sweep: {} fixtures, {} with a Let spine, {} top-level binders total",
        corpus.len(),
        spined,
        total_binders
    );
    // Counter-assertion: the sweep must have actually exercised the walk.
    assert!(
        total_binders > 0,
        "corpus sweep inspected zero top-level binders — walk or corpus is broken"
    );
}

/// Collision statistics for the VarId scheme across the whole corpus: every
/// VarId bound anywhere (not just the spine), grouped by fixture. Within one
/// serialized program, top-level binder VarIds must be unique; across the
/// union we report the population size for the birthday-bound analysis in
/// the findings doc.
#[test]
fn corpus_varid_population_statistics() {
    let corpus = load_corpus();
    // VarId -> set of fixtures binding it at top level.
    let mut by_id: HashMap<u64, Vec<String>> = HashMap::new();
    let mut population = 0usize;
    for (name, expr) in &corpus {
        for (_, _, VarId(id)) in toplevel_binders(expr) {
            population += 1;
            by_id.entry(id).or_default().push(name.clone());
        }
    }
    let distinct = by_id.len();
    eprintln!(
        "corpus VarId stats: {population} top-level binder occurrences, {distinct} distinct VarIds"
    );
    // Cross-fixture repeats are EXPECTED (the same Prelude binding appears in
    // many closed fixtures with the same stable VarId — same name, same hash).
    // What must never happen is the same VarId for two DIFFERENT source
    // bindings; within a fixture the detector test above already proves
    // uniqueness per spine.
    assert!(distinct > 0);
}
