//! Exhaustive equivalence gate for `emit::free_vars_index::FreeVarsIndex`.
//!
//! `FreeVarsIndex::compute` replaces eight `emit/expr.rs` call sites that used
//! to call `tree.extract_subtree(idx)` (a full subtree copy) followed by
//! `free_vars` (a walk over that copy) — see `plans/self-iterating-harness/
//! 11-jit-codegen-latency-receipt.md`. A wrongly-computed free-variable set is
//! a MISCOMPILE (a closure captures the wrong set of variables), not merely a
//! perf regression, so this test is the license for converting any call
//! site: for every node index of every tree in a real + generated corpus, it
//! asserts `FreeVarsIndex::free_vars_at(idx)` is byte-for-byte identical to
//! the reference `tidepool_repr::free_vars::free_vars(&tree.extract_subtree(idx))`.
//! It must be green BEFORE any call site is converted, and stays green after.

use proptest::test_runner::{Config, TestRunner};
use std::path::PathBuf;
use tidepool_codegen::emit::free_vars_index::FreeVarsIndex;
use tidepool_repr::free_vars::free_vars;
use tidepool_repr::serial::read::read_cbor;
use tidepool_repr::CoreExpr;
use tidepool_testing::gen::{arb_core_expr_depth, arb_core_expr_shadowing};

/// Corpora reachable from this crate via plain relative paths (same set
/// `datacon_never_used_as_value.rs` uses). Skipped (not failed) if absent.
fn corpora() -> Vec<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    vec![
        root.join("../haskell/test/corpus_cbor"),
        root.join("tests/fixtures/sibling_alt_dict"),
        root.join("../tidepool-runtime/tests/captured_core"),
    ]
}

fn fixture_trees(dir: &std::path::Path) -> Vec<(String, CoreExpr)> {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries: Vec<PathBuf> = read_dir
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "cbor").unwrap_or(false))
        .filter(|p| p.file_stem().and_then(|s| s.to_str()) != Some("meta"))
        .collect();
    entries.sort();
    entries
        .into_iter()
        .filter_map(|p| {
            let name = p.file_stem()?.to_str()?.to_string();
            let bytes = std::fs::read(&p).ok()?;
            let tree: CoreExpr = read_cbor(&bytes).ok()?;
            Some((name, tree))
        })
        .collect()
}

/// Check every node index of `tree`: `FreeVarsIndex::free_vars_at(idx)` must
/// equal `free_vars(&tree.extract_subtree(idx))`. Returns the number of node
/// indices checked, or panics with a precise mismatch location.
fn check_tree_exhaustively(label: &str, tree: &CoreExpr) -> usize {
    let idx = FreeVarsIndex::compute(tree);
    for i in 0..tree.nodes.len() {
        let got = idx.free_vars_at(i);
        let want = free_vars(&tree.extract_subtree(i));
        assert_eq!(
            got, want,
            "{label}: free_vars_at({i}) diverged from the reference free_vars(extract_subtree({i}))"
        );
    }
    tree.nodes.len()
}

#[test]
fn equivalent_on_real_corpora() {
    let mut corpora_scanned = 0usize;
    let mut fixtures_scanned = 0usize;
    let mut nodes_checked = 0usize;

    for dir in corpora() {
        let trees = fixture_trees(&dir);
        if trees.is_empty() {
            eprintln!(
                "SKIP {} (no fixtures — partial checkout, not a failure)",
                dir.display()
            );
            continue;
        }
        corpora_scanned += 1;
        for (name, tree) in trees {
            fixtures_scanned += 1;
            nodes_checked += check_tree_exhaustively(&format!("{}/{name}", dir.display()), &tree);
        }
    }

    eprintln!(
        "[free_vars_index_equivalence::real_corpora] corpora_scanned={corpora_scanned} \
         fixtures_scanned={fixtures_scanned} nodes_checked={nodes_checked}"
    );
    assert!(
        corpora_scanned > 0,
        "no corpus directory was reachable — this run proved nothing; check the checkout"
    );
    assert!(
        nodes_checked > 0,
        "no nodes were checked across any fixture"
    );
}

#[test]
fn equivalent_on_generated_trees() {
    // Run inside a spawned thread with headroom: generated trees at depth 4-5
    // can nest deeply enough that the reference `free_vars`'s per-tree memo
    // plus this test's own recursion needs more than the default 2MiB stack
    // (the same discipline `proptest_varid_defense.rs` and `strategy.rs`'s own
    // proptest module use for `arb_core_expr`).
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            // `TestRunner::run` requires `Fn`, not `FnMut`, so counters are
            // accumulated through a `Cell` rather than captured by mutable
            // reference (this closure runs single-threaded, so `Cell` — not
            // an atomic — is the minimal fit).
            let nodes_checked = std::cell::Cell::new(0usize);
            let trees_checked = std::cell::Cell::new(0usize);

            let mut runner = TestRunner::new(Config {
                cases: 300,
                ..Config::default()
            });
            runner
                .run(&arb_core_expr_depth(4), |tree| {
                    nodes_checked
                        .set(nodes_checked.get() + check_tree_exhaustively("generated", &tree));
                    trees_checked.set(trees_checked.get() + 1);
                    Ok(())
                })
                .unwrap();

            // Deliberate variable shadowing (a Lam/Let/Case/Join binder reusing
            // an in-scope VarId) is the sharpest edge case for a binder-removal
            // bug: `remove_binders` must remove exactly the shadowing binder's
            // occurrences at THIS scope, not accidentally short-circuit because
            // the same VarId is free elsewhere in a sibling subtree.
            let mut shadow_runner = TestRunner::new(Config {
                cases: 300,
                ..Config::default()
            });
            shadow_runner
                .run(&arb_core_expr_shadowing(4, 40), |tree| {
                    nodes_checked.set(
                        nodes_checked.get() + check_tree_exhaustively("generated-shadowing", &tree),
                    );
                    trees_checked.set(trees_checked.get() + 1);
                    Ok(())
                })
                .unwrap();

            let nodes_checked = nodes_checked.get();
            eprintln!(
                "[free_vars_index_equivalence::generated] trees_checked={} \
                 nodes_checked={nodes_checked}",
                trees_checked.get()
            );
            assert!(
                nodes_checked > 0,
                "no nodes were checked across any generated tree"
            );
        })
        .unwrap()
        .join()
        .unwrap();
}
