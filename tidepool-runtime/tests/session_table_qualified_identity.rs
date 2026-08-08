//! The precondition the JIT's frozen effect-result classifier rests on: across
//! a MULTI-TURN session, a module-qualified constructor name must denote ONE
//! `DataConId`.
//!
//! `JitEffectMachine.tags` (the `ConTags` that classify a run's result as `Val`
//! or `E` at the yield boundary) is resolved once, in `compile_inner`, from the
//! BOOTSTRAP turn's table. `add_function` refreshes `lit_wrappers`,
//! `json_con_ids` and `time_con_ids` from each later turn's table but never
//! re-resolves `tags`. Every later turn is therefore classified by the
//! bootstrap turn's `Val`/`E` ids.
//!
//! That is correct exactly while the accumulated session table maps each freer
//! qualified name to a single id. It is not otherwise, and the failure is
//! ORDER-DEPENDENT rather than deterministic: `DataConTable::insert` writes
//! `by_qualified_name` last-writer-wins, `insert_checked` guards only the
//! `by_id` axis, and `PersistentSession::merge_table` feeds inserts from
//! `iter()` — `by_id.values()` over a `std::collections::HashMap`, whose order
//! is randomized per process. Two ids under one qualified name would therefore
//! resolve to whichever the merge happened to insert last on that run.
//!
//! This test compiles two real turns through the production extract path,
//! accumulates their tables the way `merge_table` does, and asserts the
//! precondition on the result. It also dumps every qualified name carrying more
//! than one id, not only the freer five: per-turn id minting would not stop at
//! the effect constructors.
//!
//! GHC-heavy tier — needs `TIDEPOOL_EXTRACT` and `--ignore-default-filter`.

use std::collections::{BTreeMap, BTreeSet};

use tidepool_codegen::effect_machine::ConTags;
use tidepool_repr::types::DataConId;
use tidepool_repr::DataConTable;

use tidepool_testing::eval_harness::{self, mock, EvalHarness};

/// Every qualified name in `table`, with the set of ids recorded under it.
fn qualified_buckets(table: &DataConTable) -> BTreeMap<String, BTreeSet<DataConId>> {
    let mut buckets: BTreeMap<String, BTreeSet<DataConId>> = BTreeMap::new();
    for dc in table.iter() {
        if let Some(qn) = &dc.qualified_name {
            buckets.entry(qn.clone()).or_default().insert(dc.id);
        }
    }
    buckets
}

/// Accumulate `turn` onto `session` exactly as `PersistentSession::merge_table`
/// does — `insert_checked` per constructor, loud on a `by_id` collision.
fn merge_table(session: &mut DataConTable, turn: &DataConTable) {
    for dc in turn.iter() {
        session
            .insert_checked(dc.clone())
            .expect("session DataConTable collision");
    }
}

fn compile_turn(harness: &EvalHarness, body: &str) -> DataConTable {
    let source = mock::mcp_module(body);
    harness
        .compile(&source, "result")
        .unwrap_or_else(|e| panic!("compile failed for turn body {body:?}: {e}"))
        .table
}

/// Two real turns, accumulated: no qualified name may denote two ids, and the
/// bootstrap turn's `ConTags` must still classify the accumulated table.
#[test]
fn accumulated_session_table_keeps_one_id_per_qualified_name() {
    if !eval_harness::extract_available() {
        eprintln!("Skipping: tidepool-extract toolchain not available (run inside `nix develop`)");
        return;
    }
    let harness = EvalHarness::new().with_stdlib();

    // Turn 1 is the bootstrap turn — its table is what `ConTags` freezes on.
    let t1 = compile_turn(&harness, "result :: M Int\nresult = pure (0 :: Int)");
    // Turn 2 reaches for different machinery (KV + an ask), so it pulls in
    // constructors turn 1 never mentioned — the accumulation this guards.
    let t2 = compile_turn(
        &harness,
        "result :: M Int\nresult = do\n  \
           send (KvSet \"k\" (toJSON (1 :: Int)))\n  \
           n <- send (Ask \"pick a number\")\n  \
           send (KvSet \"answer\" n)\n  \
           pure (1 :: Int)",
    );

    let boot_tags = ConTags::from_table(&t1).expect("turn 1's table resolves the freer five");

    let mut session = t1.clone();
    merge_table(&mut session, &t2);

    let buckets = qualified_buckets(&session);
    let collisions: Vec<(&String, &BTreeSet<DataConId>)> =
        buckets.iter().filter(|(_, ids)| ids.len() > 1).collect();

    eprintln!(
        "[session_table_qualified_identity] t1_cons={} t2_cons={} accumulated_cons={} \
         qualified_names={} colliding_names={}",
        t1.iter().count(),
        t2.iter().count(),
        session.iter().count(),
        buckets.len(),
        collisions.len()
    );
    for (qn, ids) in &collisions {
        eprintln!("  COLLISION {qn} -> {ids:?}");
    }

    assert!(
        collisions.is_empty(),
        "a qualified constructor name denotes more than one DataConId in the \
         accumulated session table — `by_qualified_name` is last-writer-wins and \
         `merge_table` feeds it from a randomized HashMap iteration, so which id \
         wins varies per process:\n{collisions:#?}"
    );

    // The frozen classifier's precondition, stated directly: re-resolving
    // against the accumulated table must agree with what the bootstrap froze.
    let accumulated_tags =
        ConTags::from_table(&session).expect("the accumulated table resolves the freer five");
    assert_eq!(
        (
            boot_tags.val,
            boot_tags.e,
            boot_tags.union,
            boot_tags.leaf,
            boot_tags.node
        ),
        (
            accumulated_tags.val,
            accumulated_tags.e,
            accumulated_tags.union,
            accumulated_tags.leaf,
            accumulated_tags.node
        ),
        "the bootstrap turn's ConTags no longer classify the accumulated session \
         table — every post-bootstrap turn is classified by the frozen bootstrap \
         ids (jit_machine.rs: `tags` is set only in compile_inner), so a result \
         built with the accumulated table's ids would fail as \
         YieldError::UnexpectedConTag"
    );
}
