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
use tidepool_repr::serial::{read_cbor, write_cbor};
use tidepool_repr::tree::{MapLayer, RecursiveTree};
use tidepool_repr::varid_check::{check_toplevel_varids, toplevel_binders, BindingSite};
use tidepool_repr::{CoreExpr, VarId};

use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};
use serial_test::serial;
use tidepool_repr::frame::CoreFrame;
use tidepool_repr::types::Literal;
use tidepool_testing::gen::arb_core_expr;

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
            out.push((
                path.file_name().unwrap().to_string_lossy().into_owned(),
                expr,
            ));
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

// --- Property tests for duplicate-VarId detection ---

const SPINE_TAG: u64 = 0xFE00_0000_0000_0000;

#[derive(Debug, Clone)]
enum SpineFrame {
    NonRec(VarId),
    Rec(Vec<VarId>),
}

/// Build a synthetic spine over K RHS subtrees and a final body.
fn build_spine_from_frames(
    frames: &[SpineFrame],
    rhs_subtrees: &[CoreExpr],
    body_tree: &CoreExpr,
) -> CoreExpr {
    let mut nodes = Vec::new();
    let mut rhs_roots = Vec::new();

    // 1. Append RHS subtrees. Each subtree's root is its LAST node.
    for tree in rhs_subtrees {
        let offset = nodes.len();
        for node in &tree.nodes {
            nodes.push(node.clone().map_layer(|r| r + offset));
        }
        rhs_roots.push(nodes.len() - 1);
    }

    // 2. Append body tree.
    let body_offset = nodes.len();
    for node in &body_tree.nodes {
        nodes.push(node.clone().map_layer(|r| r + body_offset));
    }
    let mut current_body = nodes.len() - 1;

    // 3. Append Let frames INNERMOST-FIRST so the outermost Let is the root (last node).
    let mut next_rhs_idx = rhs_roots.len();
    for frame in frames.iter().rev() {
        match frame {
            SpineFrame::NonRec(vid) => {
                next_rhs_idx -= 1;
                let rhs = rhs_roots[next_rhs_idx];
                let let_node = CoreFrame::LetNonRec {
                    binder: *vid,
                    rhs,
                    body: current_body,
                };
                nodes.push(let_node);
                current_body = nodes.len() - 1;
            }
            SpineFrame::Rec(vids) => {
                let mut bindings = Vec::new();
                for vid in vids {
                    next_rhs_idx -= 1;
                    bindings.push((*vid, rhs_roots[next_rhs_idx]));
                }
                let let_node = CoreFrame::LetRec {
                    bindings,
                    body: current_body,
                };
                nodes.push(let_node);
                current_body = nodes.len() - 1;
            }
        }
    }

    RecursiveTree { nodes }
}

/// Strategy for a sequence of spine frames containing a total of 1..max_binders.
fn arb_spine_frames(max_binders: usize) -> impl Strategy<Value = Vec<SpineFrame>> {
    prop::collection::vec(
        prop_oneof![
            Just((false, 1)),         // NonRec
            (Just(true), 1..=3usize), // Rec group
        ],
        1..max_binders,
    )
    .prop_map(move |specs| {
        let mut frames = Vec::new();
        let mut total_binders = 0;
        let mut i = 0;
        for (is_rec, count) in specs {
            if total_binders + count > max_binders {
                break;
            }
            if is_rec {
                let mut vids = Vec::new();
                for _ in 0..count {
                    vids.push(VarId(SPINE_TAG | i));
                    i += 1;
                }
                frames.push(SpineFrame::Rec(vids));
            } else {
                frames.push(SpineFrame::NonRec(VarId(SPINE_TAG | i)));
                i += 1;
            }
            total_binders += count;
        }
        frames
    })
    .prop_filter("non-empty spine", |f| !f.is_empty())
}

/// Combined strategy: (frames, RHS subtrees).
fn arb_spine_and_rhss() -> impl Strategy<Value = (Vec<SpineFrame>, Vec<CoreExpr>)> {
    arb_spine_frames(20).prop_flat_map(|frames| {
        let binder_count: usize = frames
            .iter()
            .map(|f| match f {
                SpineFrame::NonRec(_) => 1,
                SpineFrame::Rec(v) => v.len(),
            })
            .sum();
        (
            Just(frames),
            prop::collection::vec(arb_core_expr(), binder_count),
        )
    })
}

#[test]
#[serial]
fn clean_spines_never_flag() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(Config::with_cases(500));
            runner
                .run(&arb_spine_and_rhss(), |(frames, rhss)| {
                    let body = RecursiveTree {
                        nodes: vec![CoreFrame::Lit(Literal::LitInt(0))],
                    };
                    let expr = build_spine_from_frames(&frames, &rhss, &body);
                    let binder_count = rhss.len();

                    let res = check_toplevel_varids(&expr);
                    prop_assert_eq!(res, Ok(binder_count));

                    let binders = toplevel_binders(&expr);
                    prop_assert_eq!(binders.len(), binder_count);

                    let mut expected_vids = Vec::new();
                    for f in &frames {
                        match f {
                            SpineFrame::NonRec(v) => expected_vids.push(*v),
                            SpineFrame::Rec(vs) => expected_vids.extend(vs),
                        }
                    }

                    for (i, (_, _, vid)) in binders.iter().enumerate() {
                        prop_assert_eq!(*vid, expected_vids[i]);
                    }

                    Ok(())
                })
                .unwrap();
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
#[serial]
fn planted_collision_always_caught() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(Config::with_cases(500));
            runner
                .run(
                    &(arb_spine_and_rhss(), 0..1000usize, 0..1000usize),
                    |((frames, rhss), i_raw, j_raw)| {
                        let binder_count = rhss.len();
                        if binder_count < 2 {
                            return Ok(());
                        }

                        let i = i_raw % binder_count;
                        let j = j_raw % binder_count;
                        if i == j {
                            return Ok(());
                        }
                        let (first_idx, second_idx) = if i < j { (i, j) } else { (j, i) };

                        let body = RecursiveTree {
                            nodes: vec![CoreFrame::Lit(Literal::LitInt(0))],
                        };
                        let mut expr = build_spine_from_frames(&frames, &rhss, &body);
                        let binders = toplevel_binders(&expr);

                        let (first_node, first_pos, first_vid) = binders[first_idx];
                        let (second_node, second_pos, _) = binders[second_idx];

                        // Overwrite second binder with first binder's ID.
                        match &mut expr.nodes[second_node] {
                            CoreFrame::LetNonRec { binder, .. } => *binder = first_vid,
                            CoreFrame::LetRec { bindings, .. } => {
                                bindings[second_pos].0 = first_vid
                            }
                            _ => unreachable!(),
                        }

                        let res = check_toplevel_varids(&expr);
                        match res {
                            Err(e) => {
                                prop_assert_eq!(e.var_id, first_vid);
                                prop_assert_eq!(
                                    e.first,
                                    BindingSite {
                                        node: first_node,
                                        position: first_pos
                                    }
                                );
                                prop_assert_eq!(
                                    e.second,
                                    BindingSite {
                                        node: second_node,
                                        position: second_pos
                                    }
                                );
                            }
                            Ok(_) => prop_assert!(false, "Expected collision error, got Ok"),
                        }

                        Ok(())
                    },
                )
                .unwrap();
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
#[serial]
fn nested_shadowing_never_flags() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(Config::with_cases(500));
            runner
                .run(
                    &(arb_spine_and_rhss(), 0..20usize),
                    |((frames, mut rhss), rhs_idx_raw)| {
                        let binder_count = rhss.len();
                        let rhs_idx = rhs_idx_raw % binder_count;

                        // Pick a spine binder ID to shadow (outermost).
                        let shadowed_vid = VarId(SPINE_TAG);

                        // Wrap the chosen RHS in a Lam that uses shadowed_vid.
                        let root = rhss[rhs_idx].nodes.len() - 1;
                        rhss[rhs_idx].nodes.push(CoreFrame::Lam {
                            binder: shadowed_vid,
                            body: root,
                        });

                        let body = RecursiveTree {
                            nodes: vec![CoreFrame::Lit(Literal::LitInt(0))],
                        };
                        let expr = build_spine_from_frames(&frames, &rhss, &body);
                        let res = check_toplevel_varids(&expr);
                        // Still Ok because the shadowed_vid is nested inside an RHS.
                        prop_assert_eq!(res, Ok(binder_count));

                        Ok(())
                    },
                )
                .unwrap();
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
#[serial]
fn detector_is_deserialization_stable() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(Config::with_cases(500));
            runner
                .run(&arb_spine_and_rhss(), |(frames, rhss)| {
                    let binder_count = rhss.len();
                    let body = RecursiveTree {
                        nodes: vec![CoreFrame::Lit(Literal::LitInt(0))],
                    };
                    let mut expr = build_spine_from_frames(&frames, &rhss, &body);

                    // Plant a collision in half the cases to test error stability.
                    if binder_count >= 2 {
                        let binders = toplevel_binders(&expr);
                        let (_, _, first_vid) = binders[0];
                        let (second_node, second_pos, _) = binders[1];
                        match &mut expr.nodes[second_node] {
                            CoreFrame::LetNonRec { binder, .. } => *binder = first_vid,
                            CoreFrame::LetRec { bindings, .. } => {
                                bindings[second_pos].0 = first_vid
                            }
                            _ => unreachable!(),
                        }
                    }

                    let res_before = check_toplevel_varids(&expr);

                    let bytes = write_cbor(&expr).expect("write_cbor failed");
                    let recovered = read_cbor(&bytes).expect("read_cbor failed");

                    let res_after = check_toplevel_varids(&recovered);

                    prop_assert_eq!(res_before, res_after);

                    Ok(())
                })
                .unwrap();
        })
        .unwrap()
        .join()
        .unwrap();
}
