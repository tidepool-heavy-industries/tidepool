//! Pins `DataConTable::extend_checked` (the batched, sort-once ingestion
//! path) as structurally equivalent to folding `insert_checked` over the
//! same sequence, one constructor at a time — including on error.
//!
//! `extend_checked` exists purely to make a long-lived session's per-turn
//! `merge_table` cheap (skip already-accumulated constructors, batch the
//! `by_type_name` bucket sort). It must never observably differ from the
//! sequential path: same final table on success, same error (and same
//! partially-applied table) on a genuine collision.
//!
//! One axis is NOT well-defined against production behavior: `insert_checked`
//! guards collisions on the `id` axis only, so two DISTINCT ids that happen to
//! share one `qualified_name` are never rejected, and `by_qualified_name` is a
//! bare last-write-wins map — whichever id is processed LAST wins the
//! mapping. In production that "last" is decided by a per-process-random
//! `HashMap` iteration order (`DataConTable::iter`), so there is no single
//! canonical outcome to pin against. Every comparison in this file therefore
//! drives `extend_checked` and the `insert_checked` fold from the SAME
//! explicit, already-ordered `Vec`/`Vec<Vec<_>>` — never from `.iter()` on a
//! table — so both paths see identical input order and the comparison is
//! well-defined. `distinct_ids_sharing_a_qualified_name_last_writer_wins_matches_input_order`
//! pins that shared behavior explicitly (documented, not fixed — the
//! collision hole is a known pre-existing defect, out of scope here).

use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};
use tidepool_repr::datacon_table::DataConCollision;
use tidepool_repr::{DataCon, DataConId, DataConTable};

/// Fold `insert_checked` over `dcs`, one at a time, stopping at the first
/// collision (subsequent entries are never applied — matching what a `for dc
/// in dcs { table.insert_checked(dc)? }` loop leaves behind).
fn fold_sequential(dcs: &[DataCon]) -> (DataConTable, Result<(), DataConCollision>) {
    let mut table = DataConTable::new();
    for dc in dcs {
        if let Err(e) = table.insert_checked(dc.clone()) {
            return (table, Err(e));
        }
    }
    (table, Ok(()))
}

/// Same sequence through the batched API.
fn via_extend_checked(dcs: &[DataCon]) -> (DataConTable, Result<(), DataConCollision>) {
    let mut table = DataConTable::new();
    let result = table.extend_checked(dcs.iter().cloned());
    (table, result)
}

fn assert_equivalent(dcs: &[DataCon]) {
    let sequential = fold_sequential(dcs);
    let batched = via_extend_checked(dcs);
    assert_eq!(
        sequential, batched,
        "extend_checked diverged from folding insert_checked over {dcs:?}"
    );
}

fn dc(
    id: u64,
    name: &str,
    tag: u32,
    rep_arity: u32,
    qualified_name: Option<&str>,
    type_name: &str,
) -> DataCon {
    DataCon {
        id: DataConId(id),
        name: name.to_string(),
        tag,
        rep_arity,
        field_bangs: vec![],
        qualified_name: qualified_name.map(str::to_string),
        type_name: type_name.to_string(),
    }
}

// ---- Fixed hard-case sequences -------------------------------------------

#[test]
fn reinsert_identical_constructor_is_noop() {
    let first = dc(1, "Just", 2, 1, Some("GHC.Maybe.Just"), "Maybe");
    assert_equivalent(&[first.clone(), first]);
}

#[test]
fn overwrite_with_changed_name() {
    // dc_identity falls back to unqualified `name` only when qualified_name
    // is absent, so a genuine "changed name, same constructor" overwrite
    // needs an unchanged qualified_name to keep identity agreeing — the bare
    // `name` field is otherwise the identity, and changing it would collide.
    let first = dc(2, "OldName", 1, 0, Some("Mod.X"), "T");
    let second = dc(2, "NewName", 1, 0, Some("Mod.X"), "T");
    assert_equivalent(&[first, second]);
}

#[test]
fn overwrite_with_changed_type_name() {
    let first = dc(3, "Con", 1, 0, Some("Mod.Con"), "TypeA");
    let second = dc(3, "Con", 1, 0, Some("Mod.Con"), "TypeB");
    assert_equivalent(&[first, second]);
}

#[test]
fn overwrite_with_changed_tag_same_type_name() {
    let first = dc(4, "Con", 1, 0, Some("Mod.Con"), "TypeA");
    let second = dc(4, "Con", 2, 0, Some("Mod.Con"), "TypeA");
    // A sibling in the same bucket, to make the resort observable.
    let sibling = dc(5, "Other", 1, 0, Some("Mod.Other"), "TypeA");
    assert_equivalent(&[sibling.clone(), first, second]);
    // Order independence: sibling inserted after, too.
    let first2 = dc(4, "Con", 1, 0, Some("Mod.Con"), "TypeA");
    let second2 = dc(4, "Con", 2, 0, Some("Mod.Con"), "TypeA");
    assert_equivalent(&[first2, sibling, second2]);
}

#[test]
fn overwrite_with_changed_qualified_name() {
    let first = dc(6, "Con", 1, 0, Some("Mod.A.Con"), "T");
    // Same unqualified name+tag+arity, only the qualified_name moves — this
    // changes dc_identity, so it is in fact a genuine collision under
    // insert_checked's identity rule (module-qualified identity differs).
    // Pin that: both paths must agree it errors.
    let second = dc(6, "Con", 1, 0, Some("Mod.B.Con"), "T");
    assert_equivalent(&[first, second]);
}

#[test]
fn genuine_collision_errors_on_both_paths() {
    let first = dc(
        7,
        "Union",
        1,
        1,
        Some("Data.OpenUnion.Internal.Union"),
        "Union",
    );
    let second = dc(
        7,
        "DynFlags",
        1,
        1,
        Some("GHC.Driver.Session.DynFlags"),
        "DynFlags",
    );
    let (seq_table, seq_result) = fold_sequential(&[first.clone(), second.clone()]);
    let (batch_table, batch_result) = via_extend_checked(&[first, second]);
    assert!(seq_result.is_err());
    assert_eq!(seq_result, batch_result);
    assert_eq!(seq_table, batch_table);
    // The survivor (first) is unchanged, the collider never got applied.
    assert_eq!(seq_table.len(), 1);
}

#[test]
fn collision_midway_leaves_matching_partial_table() {
    // Two clean inserts, then a collision, then a constructor that would have
    // been clean had it been reached — extend_checked must stop at the same
    // point the sequential fold does, not "apply what it can" around it.
    let a = dc(10, "A", 1, 0, None, "T1");
    let b = dc(11, "B", 1, 0, None, "T2");
    let collide_first = dc(12, "First", 1, 0, None, "T3");
    let collide_second = dc(12, "Second", 1, 0, None, "T3");
    let never_reached = dc(13, "Never", 1, 0, None, "T4");
    let seq = [a, b, collide_first, collide_second, never_reached];
    let (seq_table, seq_result) = fold_sequential(&seq);
    let (batch_table, batch_result) = via_extend_checked(&seq);
    assert!(seq_result.is_err());
    assert_eq!(seq_result, batch_result);
    assert_eq!(seq_table, batch_table);
    assert_eq!(seq_table.len(), 3); // a, b, collide_first — not `never_reached`
}

// ---- Repeated overlapping merges (the real steady-state shape) -----------
//
// `merge_table` doesn't call `extend_checked` once — it calls it once per
// TURN, filtering out entries the accumulated table already holds first, and
// each call sees whatever the previous turn left behind. A batching
// optimization that is correct for a single call but wrong once a SECOND
// call reads an already-populated table is exactly the regression class that
// bit `wrap_with_datacon_env` (reverted upstream after a second fragment
// compiled against an accumulated table panicked on an out-of-range
// constructor tag). These helpers and tests pin that multi-call shape
// directly, not just the single-call API.

/// Mirrors `PersistentSession::merge_table`: filter a turn's constructors
/// down to those NOT already present with identical metadata, then
/// `extend_checked` the remainder into the accumulated table.
fn merge_one_turn(
    accumulated: &mut DataConTable,
    turn: &[DataCon],
) -> Result<(), DataConCollision> {
    let incoming: Vec<DataCon> = turn
        .iter()
        .filter(|&dc| accumulated.get(dc.id) != Some(dc))
        .cloned()
        .collect();
    accumulated.extend_checked(incoming)
}

/// Repeated `merge_table`-style calls across many turns into one growing
/// table.
fn merge_all_turns(turns: &[Vec<DataCon>]) -> (DataConTable, Result<(), DataConCollision>) {
    let mut table = DataConTable::new();
    for turn in turns {
        if let Err(e) = merge_one_turn(&mut table, turn) {
            return (table, Err(e));
        }
    }
    (table, Ok(()))
}

/// The ground truth for the same turn sequence: flatten every turn's
/// constructors in order (identical re-encounters included, unfiltered) and
/// fold `insert_checked` over the lot, one at a time.
fn flatten_and_fold_sequential(
    turns: &[Vec<DataCon>],
) -> (DataConTable, Result<(), DataConCollision>) {
    let flattened: Vec<DataCon> = turns.iter().flatten().cloned().collect();
    fold_sequential(&flattened)
}

fn assert_multiturn_equivalent(turns: &[Vec<DataCon>]) {
    let merged = merge_all_turns(turns);
    let sequential = flatten_and_fold_sequential(turns);
    assert_eq!(
        merged, sequential,
        "repeated merge_table-style turns diverged from the flattened sequential fold for {turns:?}"
    );
}

#[test]
fn steady_state_turn_reintroduces_nothing_new() {
    // Turn 1 introduces A, B. Turns 2 and 3 re-present the exact same A, B —
    // the "turn adds nothing" steady state merge_table's skip filter exists
    // for. Every turn must still land on the identical table.
    let a = dc(20, "A", 1, 0, Some("Mod.A"), "T1");
    let b = dc(21, "B", 1, 0, Some("Mod.B"), "T2");
    let turns = vec![
        vec![a.clone(), b.clone()],
        vec![a.clone(), b.clone()],
        vec![a, b],
    ];
    assert_multiturn_equivalent(&turns);
}

#[test]
fn later_turn_legitimately_overwrites_earlier_turns_metadata() {
    // Turn 1 introduces Con with type_name unresolved; turn 2 re-presents
    // every id from turn 1 UNCHANGED except Con, whose type_name has since
    // been resolved — a legitimate cross-turn overwrite that must resort
    // Con's (moved) bucket without disturbing the untouched entries.
    let untouched = dc(22, "Sibling", 1, 0, Some("Mod.Sibling"), "TypeA");
    let con_v1 = dc(23, "Con", 2, 0, Some("Mod.Con"), "TypeA");
    let con_v2 = dc(23, "Con", 2, 0, Some("Mod.Con"), "TypeB");
    let turns = vec![vec![untouched.clone(), con_v1], vec![untouched, con_v2]];
    assert_multiturn_equivalent(&turns);
}

#[test]
fn later_turn_collision_leaves_matching_partial_accumulated_table() {
    // Turn 1 is clean. Turn 2 re-presents turn 1's entries (all skipped as
    // identical) plus a genuine collider against one of turn 1's ids — the
    // accumulated table at the point of failure must match what folding
    // insert_checked over the flattened turns would leave.
    let a = dc(24, "A", 1, 0, None, "T1");
    let b = dc(25, "B", 1, 0, None, "T2");
    let collider = dc(24, "NotA", 1, 0, None, "T1");
    let turns = vec![vec![a.clone(), b.clone()], vec![a, b, collider]];
    let (merged_table, merged_result) = merge_all_turns(&turns);
    let (seq_table, seq_result) = flatten_and_fold_sequential(&turns);
    assert!(merged_result.is_err());
    assert_eq!(merged_result, seq_result);
    assert_eq!(merged_table, seq_table);
    assert_eq!(merged_table.len(), 2); // a, b survive; the collider never lands
}

#[test]
fn same_logical_constructor_across_generations_preserves_tiebreak_order() {
    // Simulates gen-versioned session vars: the "same" logical constructor
    // (same unqualified name, same arity) reappears under a FRESH DataConId
    // and generation-suffixed qualified_name in a later turn — a distinct
    // identity to insert_checked (no collision, since dc_identity differs),
    // but one that competes for the same `by_name` bucket and its
    // insertion-order tie-break (`get_by_name_arity`'s "last matching entry"
    // rule). A downstream classifier keyed on that tie-break must see the
    // same winner whether accumulation is batched or sequential.
    let gen1 = dc(30, "Foo", 1, 1, Some("Tidepool.Session.Val.G1.Foo"), "Foo");
    let gen2 = dc(31, "Foo", 1, 1, Some("Tidepool.Session.Val.G2.Foo"), "Foo");
    let turns = vec![
        vec![gen1.clone()],
        vec![gen1, gen2], // turn 2 re-presents gen1 (no-op) and introduces gen2
    ];
    assert_multiturn_equivalent(&turns);
    // Pin the actual tie-break outcome, not just cross-path agreement.
    let (table, result) = merge_all_turns(&turns);
    assert!(result.is_ok());
    assert_eq!(table.get_by_name_arity("Foo", 1), Some(DataConId(31)));
}

#[test]
fn distinct_ids_sharing_a_qualified_name_last_writer_wins_matches_input_order() {
    // KNOWN DEFECT, preserved not fixed: insert_checked's collision guard is
    // keyed on `id` only. Two DISTINCT ids that share one qualified_name both
    // insert cleanly, and `by_qualified_name` (a bare HashMap) silently
    // records whichever was processed LAST. This test does not close that
    // hole — it pins that extend_checked reproduces the identical
    // last-writer-wins outcome as folding insert_checked, for the SAME
    // explicit input order, in BOTH directions — proving the outcome tracks
    // input order (not some canonical resolution) and that batching never
    // perturbs it.
    let first = dc(40, "A", 1, 0, Some("Shared.Qualified.Name"), "T1");
    let second = dc(41, "B", 1, 0, Some("Shared.Qualified.Name"), "T2");

    let fwd = [first.clone(), second.clone()];
    let (table_fwd, result_fwd) = via_extend_checked(&fwd);
    assert!(result_fwd.is_ok());
    assert_eq!(
        table_fwd.get_by_qualified_name("Shared.Qualified.Name"),
        Some(DataConId(41)),
        "second-processed id must win the qualified_name mapping"
    );
    assert_eq!(fold_sequential(&fwd), (table_fwd, result_fwd));

    let rev = [second, first];
    let (table_rev, result_rev) = via_extend_checked(&rev);
    assert!(result_rev.is_ok());
    assert_eq!(
        table_rev.get_by_qualified_name("Shared.Qualified.Name"),
        Some(DataConId(40)),
        "reversing input order must flip the winner — it tracks order, not identity"
    );
    assert_eq!(fold_sequential(&rev), (table_rev, result_rev));
}

#[test]
fn merge_table_skip_filter_reclaims_qualified_name_ownership_across_turns() {
    // A SEPARATE, more specific finding than the single-call last-writer-wins
    // pin above: `merge_table`'s skip-identical pre-filter (STEP 1) decides
    // whether to skip an unchanged entry using a filter pass that does not
    // see intra-batch effects. When two distinct ids share a qualified_name
    // (the pre-existing hole — confirmed absent from real extracted sessions
    // as of this writing, so currently unexercised in production) and a
    // LATER turn re-presents an unchanged id whose job is to reclaim that
    // qualified_name from a different id introduced earlier in the SAME
    // turn, the skip filter omits it — because at filter time it still
    // "owned" the mapping — and never learns that a same-turn predecessor
    // just took it over. Sequential per-item reprocessing (today's
    // production code, and this crate's `insert_checked` fold) always
    // reclaims it, because it never skips.
    //
    // This is NOT the qualified-name collision hole itself (this file must
    // not touch that) — it is a documented consequence of the skip-identical
    // optimization interacting with that pre-existing hole. Pinned here so
    // it is visible and intentional rather than a surprise; not fixed, per
    // explicit direction to leave the qualified-name axis alone this pass.
    let id1 = dc(50, "A", 1, 0, Some("Shared.Turn.Name"), "T1");
    let id2 = dc(51, "B", 1, 0, Some("Shared.Turn.Name"), "T2");
    let turns = vec![vec![id1.clone()], vec![id2, id1]];

    let (merged_table, merged_result) = merge_all_turns(&turns);
    let (sequential_table, sequential_result) = flatten_and_fold_sequential(&turns);
    assert!(merged_result.is_ok());
    assert!(sequential_result.is_ok());

    // Sequential (today's per-turn `for dc in table.iter() { insert_checked
    // }`, folded across turns) ALWAYS reclaims — id 1's redundant reprocess
    // is never skipped, so its unconditional qualified_name write wins last.
    assert_eq!(
        sequential_table.get_by_qualified_name("Shared.Turn.Name"),
        Some(DataConId(50)),
        "sequential reprocessing always reclaims the qualified_name"
    );
    // The batched skip-filter path (`merge_table`'s actual, unmodified
    // behavior) DOES NOT reclaim it here: turn 2's filter pass decides id 1
    // is skippable using the table state as of the START of turn 2 (where id
    // 1 still owned the mapping), never seeing that id 2 — its own
    // predecessor WITHIN that same turn — takes the mapping first. This IS
    // the confirmed divergence: extend_checked and its merge_table wiring
    // are only equivalence-tested against the sequential fold within a
    // SINGLE call (see `extend_checked_matches_sequential_insert_checked_property`,
    // which is unaffected — the skip filter lives in `merge_table`, not
    // `extend_checked`). Left unfixed per explicit direction not to touch
    // the qualified-name axis this pass.
    assert_eq!(
        merged_table.get_by_qualified_name("Shared.Turn.Name"),
        Some(DataConId(51)),
        "merge_table's skip filter does NOT reclaim — this is the pinned divergence"
    );
    assert_ne!(
        merged_table.get_by_qualified_name("Shared.Turn.Name"),
        sequential_table.get_by_qualified_name("Shared.Turn.Name"),
        "documents that this specific shape diverges; not asserting full table equality here"
    );
}

/// When a property test wants Vec-shaped freedom for generation convenience,
/// either constrain the generator to the production invariant, or assert
/// only what holds in the wider domain — asserting a narrower-domain property
/// over Vec-shaped freedom is how a generator outruns what production can
/// actually feed it. Here: production's `merge_table` takes a
/// `&DataConTable`, whose `iter()` is `by_id.values()`, so a real turn holds
/// AT MOST ONE entry per id. `Vec<DataCon>` has no such constraint, so a
/// generator drawing turns as plain `Vec<DataCon>` can produce two entries
/// for the same id within one turn — a shape `merge_one_turn`'s
/// skip-identical filter (which decides per-entry against the table as it
/// stood BEFORE the turn) was never required to agree with the flattened
/// sequential fold on, and doesn't
/// (`duplicate_ids_within_one_turn_diverge_but_cannot_occur_in_production`
/// pins the divergence explicitly). Restore the invariant here rather than
/// widen the property.
fn dedup_turn_by_id_keep_last(turn: Vec<DataCon>) -> Vec<DataCon> {
    let mut last_index_for_id: std::collections::HashMap<DataConId, usize> =
        std::collections::HashMap::new();
    for (i, dc) in turn.iter().enumerate() {
        last_index_for_id.insert(dc.id, i);
    }
    turn.into_iter()
        .enumerate()
        .filter(|(i, dc)| last_index_for_id.get(&dc.id) == Some(i))
        .map(|(_, dc)| dc)
        .collect()
}

#[test]
fn duplicate_ids_within_one_turn_diverge_but_cannot_occur_in_production() {
    // Documents a known NON-property, matching the pattern in
    // `merge_table_skip_filter_reclaims_qualified_name_ownership_across_turns`
    // above: unreachable in production, so pinned explicitly rather than
    // fixed. Production's `merge_table` takes a `&DataConTable`, whose
    // `iter()` is `by_id.values()` (`tidepool-repr/src/datacon_table.rs`) —
    // a real turn holds AT MOST ONE entry per id. This test feeds
    // `merge_all_turns`/`flatten_and_fold_sequential` a turn with TWO
    // entries for the same id, a shape only reachable through this test
    // file's `Vec<DataCon>`-typed turn representation, never through
    // production's `DataConTable`-typed one. Do not read the `assert_ne!`
    // below as a lurking bug in the skip-identical filter and do not "fix"
    // it there — see `dedup_turn_by_id_keep_last`, which excludes this shape
    // from the property fuzzer for exactly this reason.
    let turns = vec![
        vec![dc(4, "Bar", 1, 2, Some("Mod.4"), "TyA")],
        vec![
            dc(4, "Foo", 1, 2, Some("Mod.4"), "TyA"),
            dc(4, "Bar", 1, 2, Some("Mod.4"), "TyA"),
        ],
    ];
    let (merged_table, merged_result) = merge_all_turns(&turns);
    let (sequential_table, sequential_result) = flatten_and_fold_sequential(&turns);
    assert!(merged_result.is_ok());
    assert!(sequential_result.is_ok());
    // Sequential (flatten + fold insert_checked one at a time) applies "Foo"
    // then "Bar" in order — "Bar" is last and wins.
    assert_eq!(
        sequential_table.get(DataConId(4)).map(|d| d.name.as_str()),
        Some("Bar")
    );
    // merge_all_turns's skip-identical filter compares each of turn 2's
    // entries against the table as it stood BEFORE turn 2 independently:
    // "Bar" matches what's already accumulated (from turn 1) and is
    // filtered out; only "Foo" survives to be applied. That inverts the
    // within-turn order.
    assert_eq!(
        merged_table.get(DataConId(4)).map(|d| d.name.as_str()),
        Some("Foo")
    );
    assert_ne!(
        merged_table, sequential_table,
        "documents the divergence; see comment above for why it's unreachable in production"
    );
}

/// Turns drawn from the same small overlapping pool as `arb_datacon`, so
/// consecutive turns mostly re-present what's already accumulated (subset,
/// the documented steady state) with occasional new-or-changed entries.
/// `arb_datacon`'s `qualified_name` is keyed to `id` (see there) so the
/// fuzzer stays within the domain real sessions actually exercise — the
/// pre-existing qualified-name collision hole is covered by dedicated
/// explicit tests above instead, per the standing instruction not to widen
/// or "fix" that axis in this pass. Each generated turn is deduped by id
/// (see `dedup_turn_by_id_keep_last`) to keep it within the one-entry-per-id
/// domain production can actually produce.
fn arb_turns() -> impl Strategy<Value = Vec<Vec<DataCon>>> {
    prop::collection::vec(
        prop::collection::vec(arb_datacon(), 0..6).prop_map(dedup_turn_by_id_keep_last),
        2..8,
    )
}

#[test]
fn multiturn_merge_matches_flattened_sequential_property() {
    let mut runner = TestRunner::new(Config::with_cases(2000));
    runner
        .run(&arb_turns(), |turns| {
            let merged = merge_all_turns(&turns);
            let sequential = flatten_and_fold_sequential(&turns);
            prop_assert_eq!(merged, sequential);
            Ok(())
        })
        .unwrap();
}

// ---- Property test over generated sequences -------------------------------

/// Small pools so ids/names collide with each other often — re-encounters,
/// overwrites, and genuine collisions all need to show up with reasonable
/// probability for the property to be worth anything.
///
/// `qualified_name` is keyed to `id` (never independently drawn): two
/// DISTINCT ids sharing one qualified_name is the pre-existing collision
/// hole documented at the top of this file — confirmed absent from real
/// extracted sessions, and deliberately out of scope to touch or widen this
/// pass. Keeping it out of the general fuzzer avoids a non-deterministic
/// flake on a shape that is already pinned explicitly above
/// (`distinct_ids_sharing_a_qualified_name_last_writer_wins_matches_input_order`,
/// `merge_table_skip_filter_reclaims_qualified_name_ownership_across_turns`).
fn arb_datacon() -> impl Strategy<Value = DataCon> {
    (0u64..5).prop_flat_map(|id| {
        (
            Just(id),
            prop_oneof![
                Just("Foo".to_string()),
                Just("Bar".to_string()),
                Just("Baz".to_string())
            ],
            prop::option::of(Just(format!("Mod.{id}"))),
            prop_oneof![Just("TyA".to_string()), Just("TyB".to_string())],
            1u32..3,
            0u32..3,
        )
            .prop_map(
                |(id, name, qualified_name, type_name, tag, rep_arity)| DataCon {
                    id: DataConId(id),
                    name,
                    tag,
                    rep_arity,
                    field_bangs: vec![],
                    qualified_name,
                    type_name,
                },
            )
    })
}

fn arb_datacon_seq() -> impl Strategy<Value = Vec<DataCon>> {
    prop::collection::vec(arb_datacon(), 1..12)
}

#[test]
fn extend_checked_matches_sequential_insert_checked_property() {
    let mut runner = TestRunner::new(Config::with_cases(2000));
    runner
        .run(&arb_datacon_seq(), |seq| {
            let sequential = fold_sequential(&seq);
            let batched = via_extend_checked(&seq);
            prop_assert_eq!(sequential, batched);
            Ok(())
        })
        .unwrap();
}
