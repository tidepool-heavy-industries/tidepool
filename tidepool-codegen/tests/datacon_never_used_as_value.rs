//! GHC Core has a saturation invariant on data constructors: every
//! constructor application in Core is saturated. An unsaturated use in
//! source (`map Just xs`, `foldr (:) []`) is eta-expanded by GHC into a
//! lambda wrapping a saturated application (`Lam(...Con...)`), never a bare
//! `Var` referencing the constructor. Consequently a translated fragment's
//! tree never contains `Var(VarId(dc.id.0))` for a table constructor `dc` —
//! every reference is a `Con` frame — so `datacon_env::wrap_with_datacon_env`'s
//! synthesized wrapper bindings are always dead code on real input.
//!
//! If this test ever fires, the translator has started emitting constructor
//! references as values, the per-fragment wrapper/closure emission path
//! (`emit_lam` on a `LetNonRec` binding a curried constructor lambda) is live
//! on real input again, and a session-lifetime constructor-closure cache
//! becomes worth building.
//!
//! # What the corpus scan below does NOT cover, and the checks that close it
//!
//! The scan reads each fixture tree straight off disk and checks it against
//! that fixture's OWN `meta.cbor` table. Two axes of the real compile path are
//! absent from that, and both are axes the session re-entry path lives on:
//!
//! - **Table breadth.** A session fragment is not compiled against its own
//!   table. It is compiled against the ACCUMULATED table
//!   (`PersistentSession::merge_table` unions every turn's constructors), which
//!   is a strict superset of what the fragment mentions. A per-fixture scan can
//!   only ever ask about that fixture's own constructors.
//!   `accumulated_corpora_never_reference_a_constructor_as_a_free_variable`
//!   asks the superset question.
//! - **Tree stage.** The invariant is consumed after `normalize`, not on the
//!   deserialized tree — `compile_inner` and `add_function` both shape with
//!   `normalize` before anything reads free variables.
//!   `post_normalize_corpora_never_reference_a_constructor_as_a_free_variable`
//!   asks it on the tree the compile path actually sees.
//!
//! A third check, `accumulated_corpora_keep_one_id_per_qualified_name`, guards
//! a different table-accumulation hazard on the same path: `DataConTable`
//! resolves the freer `Val`/`E`/`Union`/`Leaf`/`Node` by module-qualified name,
//! `insert` writes `by_qualified_name` last-writer-wins, and `merge_table`
//! feeds it from a randomized `HashMap` iteration — so two ids under one
//! qualified name make constructor identity order-dependent per process.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use tidepool_repr::datacon::DataCon;
use tidepool_repr::free_vars::free_vars;
use tidepool_repr::serial::read::{read_cbor, read_metadata};
use tidepool_repr::types::{DataConId, Literal, VarId};
use tidepool_repr::{CoreExpr, CoreFrame, DataConTable, TreeBuilder};

/// Corpora reachable from this crate via plain relative paths, no new
/// dev-dependency: `tidepool_repr::serial::read` and `free_vars` are already
/// used elsewhere in this crate's test suite (`real_core_corpus.rs`), and the
/// other two directories are read as plain files. Skipped (not failed) if
/// absent, for a partial checkout.
fn corpora() -> Vec<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    vec![
        root.join("../haskell/test/corpus_cbor"),
        root.join("tests/fixtures/sibling_alt_dict"),
        root.join("../tidepool-runtime/tests/captured_core"),
    ]
}

fn table_var_ids(table: &DataConTable) -> BTreeSet<VarId> {
    table.iter().map(|dc| VarId(dc.id.0)).collect()
}

/// `dc.id.0` for every `Con` frame's tag in `tree` — the id space a saturated
/// constructor reference actually appears in.
fn con_tags_in_tree(tree: &CoreExpr) -> BTreeSet<DataConId> {
    tree.nodes
        .iter()
        .filter_map(|n| match n {
            CoreFrame::Con { tag, .. } => Some(*tag),
            _ => None,
        })
        .collect()
}

/// One fixture's `.cbor` trees against `meta.cbor`'s table, skipping the
/// table entry itself and any per-binding split the corpus reserves.
fn fixture_trees(dir: &Path) -> Vec<(String, CoreExpr)> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
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

/// The main invariant, over every reachable corpus: no table constructor's
/// `VarId` ever appears as a free variable in its fixture's tree.
#[test]
fn real_corpora_never_reference_a_constructor_as_a_free_variable() {
    let mut corpora_scanned = 0usize;
    let mut fixtures_scanned = 0usize;
    let mut constructors_checked = 0usize;
    let mut violations: Vec<String> = Vec::new();

    for dir in corpora() {
        let meta_path = dir.join("meta.cbor");
        if !meta_path.exists() {
            eprintln!(
                "SKIP {} (no meta.cbor — partial checkout, not a failure)",
                dir.display()
            );
            continue;
        }
        let meta = std::fs::read(&meta_path).expect("meta.cbor exists but is unreadable");
        let table: DataConTable = read_metadata(&meta).expect("meta.cbor parses").0;
        let ctor_ids = table_var_ids(&table);
        corpora_scanned += 1;
        constructors_checked += ctor_ids.len();

        for (name, tree) in fixture_trees(&dir) {
            fixtures_scanned += 1;
            let fvs: BTreeSet<VarId> = free_vars(&tree).into_iter().collect();
            let hits: Vec<VarId> = fvs.intersection(&ctor_ids).copied().collect();
            if !hits.is_empty() {
                violations.push(format!(
                    "{}/{name}: constructor(s) referenced as a free Var: {hits:?}",
                    dir.display()
                ));
            }
        }
    }

    eprintln!(
        "[datacon_never_used_as_value] corpora_scanned={corpora_scanned} \
         fixtures_scanned={fixtures_scanned} constructors_checked={constructors_checked}"
    );

    assert!(
        corpora_scanned > 0,
        "no corpus directory was reachable — every one was skipped, so this \
         run proved nothing; check the checkout"
    );
    assert!(
        violations.is_empty(),
        "constructor(s) referenced as a value in real Core — see this file's \
         doc comment for what that means:\n{}",
        violations.join("\n")
    );
}

/// Every corpus's constructors unioned into ONE table, the way
/// `PersistentSession::merge_table` accumulates a session table across turns,
/// paired with every fixture tree reachable. `insert_checked` is used (not
/// `insert`) so a genuine cross-corpus `stableVarId` collision is reported
/// rather than silently overwritten.
fn accumulated_corpora() -> (DataConTable, Vec<(String, CoreExpr)>, Vec<String>) {
    let mut table = DataConTable::new();
    let mut trees = Vec::new();
    let mut collisions = Vec::new();

    for dir in corpora() {
        let meta_path = dir.join("meta.cbor");
        if !meta_path.exists() {
            continue;
        }
        let meta = std::fs::read(&meta_path).expect("meta.cbor exists but is unreadable");
        let corpus_table: DataConTable = read_metadata(&meta).expect("meta.cbor parses").0;
        for dc in corpus_table.iter() {
            if let Err(e) = table.insert_checked(dc.clone()) {
                collisions.push(format!("{}: {e}", dir.display()));
            }
        }
        for (name, tree) in fixture_trees(&dir) {
            trees.push((format!("{}/{name}", dir.display()), tree));
        }
    }
    (table, trees, collisions)
}

/// The superset question the per-fixture scan cannot ask: a session fragment is
/// compiled against the ACCUMULATED table, so the referenced-constructor
/// decision faces every constructor every turn ever contributed — not just the
/// fragment's own. No fixture tree may reference any of them as a free `Var`.
#[test]
fn accumulated_corpora_never_reference_a_constructor_as_a_free_variable() {
    let (table, trees, collisions) = accumulated_corpora();
    assert!(
        !trees.is_empty(),
        "no corpus fixture was reachable — this run proved nothing; check the checkout"
    );
    assert!(
        collisions.is_empty(),
        "accumulating the corpora hit a stableVarId collision:\n{}",
        collisions.join("\n")
    );

    let ctor_ids = table_var_ids(&table);
    let mut violations: Vec<String> = Vec::new();
    for (name, tree) in &trees {
        let fvs: BTreeSet<VarId> = free_vars(tree).into_iter().collect();
        let hits: Vec<VarId> = fvs.intersection(&ctor_ids).copied().collect();
        if !hits.is_empty() {
            violations.push(format!(
                "{name}: constructor(s) referenced as a free Var: {hits:?}"
            ));
        }
    }

    eprintln!(
        "[datacon_never_used_as_value] accumulated: fixtures={} constructors={}",
        trees.len(),
        ctor_ids.len()
    );
    assert!(
        violations.is_empty(),
        "constructor(s) referenced as a value against the ACCUMULATED session \
         table — see this file's doc comment:\n{}",
        violations.join("\n")
    );
}

/// The same superset question on the tree the compile path actually reads.
/// `compile_inner` and `add_function` both run `normalize` before any
/// free-variable analysis, so an invariant checked only on the deserialized
/// tree is checked one stage too early.
#[test]
fn post_normalize_corpora_never_reference_a_constructor_as_a_free_variable() {
    let (table, trees, collisions) = accumulated_corpora();
    assert!(!trees.is_empty(), "no corpus fixture was reachable");
    assert!(collisions.is_empty(), "{}", collisions.join("\n"));

    let ctor_ids = table_var_ids(&table);
    let mut violations: Vec<String> = Vec::new();
    for (name, tree) in &trees {
        let normalized = tidepool_repr::normalize(tree, &table);
        let fvs: BTreeSet<VarId> = free_vars(&normalized).into_iter().collect();
        let hits: Vec<VarId> = fvs.intersection(&ctor_ids).copied().collect();
        if !hits.is_empty() {
            violations.push(format!(
                "{name}: constructor(s) free in the POST-NORMALIZE tree: {hits:?}"
            ));
        }
    }

    assert!(
        violations.is_empty(),
        "normalize produced a tree referencing a constructor as a value:\n{}",
        violations.join("\n")
    );
}

/// A module-qualified constructor name must denote ONE id across an accumulated
/// session table. `freer_names::resolve` — which `ConTags` uses to find `Val`,
/// `E`, `Union`, `Leaf` and `Node` — consults `by_qualified_name` first, and
/// that map is written last-writer-wins by `DataConTable::insert` with no
/// collision check (`insert_checked` guards only the `by_id` axis). Because
/// `merge_table` drives those inserts from `DataConTable::iter()` —
/// `by_id.values()` over a `std::collections::HashMap` — the winner is selected
/// by an iteration order randomized per process, making constructor identity
/// vary between otherwise identical runs.
#[test]
fn accumulated_corpora_keep_one_id_per_qualified_name() {
    let (table, trees, _) = accumulated_corpora();
    assert!(!trees.is_empty(), "no corpus fixture was reachable");

    let mut buckets: std::collections::BTreeMap<&str, BTreeSet<DataConId>> = Default::default();
    for dc in table.iter() {
        if let Some(qn) = &dc.qualified_name {
            buckets.entry(qn.as_str()).or_default().insert(dc.id);
        }
    }
    let collisions: Vec<String> = buckets
        .iter()
        .filter(|(_, ids)| ids.len() > 1)
        .map(|(qn, ids)| format!("{qn} -> {ids:?}"))
        .collect();

    eprintln!(
        "[datacon_never_used_as_value] qualified_names={} colliding={}",
        buckets.len(),
        collisions.len()
    );
    assert!(
        collisions.is_empty(),
        "a qualified constructor name denotes more than one DataConId, so which \
         one resolves depends on HashMap iteration order:\n{}",
        collisions.join("\n")
    );
}

/// Positive control (a): a hand-built fragment that DOES reference a
/// constructor as a value (`App(App(Var(ctorId), 3), 4)`, not a saturated
/// `Con`) must be reported as a hit. Proves the detector fires — a zero
/// result above is not a scanner bug that can never trigger.
#[test]
fn positive_control_detects_constructor_referenced_as_a_value() {
    let ctor = DataConId(50);
    let mut table = DataConTable::new();
    table.insert(DataCon {
        id: ctor,
        name: "Pair".to_string(),
        tag: 1,
        rep_arity: 2,
        field_bangs: vec![],
        qualified_name: None,
        type_name: String::new(),
    });

    let mut b = TreeBuilder::new();
    let ctor_var = b.push(CoreFrame::Var(VarId(ctor.0)));
    let arg1 = b.push(CoreFrame::Lit(Literal::LitInt(3)));
    let app1 = b.push(CoreFrame::App {
        fun: ctor_var,
        arg: arg1,
    });
    let arg2 = b.push(CoreFrame::Lit(Literal::LitInt(4)));
    b.push(CoreFrame::App {
        fun: app1,
        arg: arg2,
    });
    let tree = b.build();

    let ctor_ids = table_var_ids(&table);
    let fvs: BTreeSet<VarId> = free_vars(&tree).into_iter().collect();
    let hits: Vec<VarId> = fvs.intersection(&ctor_ids).copied().collect();

    assert_eq!(
        hits,
        vec![VarId(ctor.0)],
        "the detector must report a hit for a constructor referenced as a value"
    );
}

/// Positive control (b): on one real fixture, the table's constructor ids
/// actually intersect the tags appearing in the tree's `Con` frames. Proves
/// the tree and table are in the same id space — the main test's zero-hit
/// result isn't tree and table silently scanning past each other.
#[test]
fn positive_control_tree_and_table_share_id_space() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../haskell/test/corpus_cbor");
    let meta_path = dir.join("meta.cbor");
    assert!(
        meta_path.exists(),
        "no meta.cbor at {} — this positive control needs the committed \
         corpus_cbor fixtures; check the checkout",
        meta_path.display()
    );
    let meta = std::fs::read(&meta_path).unwrap();
    let table: DataConTable = read_metadata(&meta).unwrap().0;
    let table_ids: BTreeSet<DataConId> = table.iter().map(|dc| dc.id).collect();

    let fixtures = fixture_trees(&dir);
    assert!(!fixtures.is_empty(), "corpus_cbor has no fixtures to check");

    let shared: usize = fixtures
        .iter()
        .filter(|(_, tree)| {
            !con_tags_in_tree(tree)
                .intersection(&table_ids)
                .collect::<BTreeSet<_>>()
                .is_empty()
        })
        .count();

    assert!(
        shared > 0,
        "no fixture's Con-frame tags intersect the table's constructor ids — \
         tree and table are in different id spaces, so the main test's \
         zero-hit result is void"
    );
}
