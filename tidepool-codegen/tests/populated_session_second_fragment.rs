//! The populated-session second-fragment path: a fragment compiled against an
//! ACCUMULATED session table, run as a child while the parent is suspended.
//!
//! This is the shape a mid-loop compaction turn takes, and it is the shape the
//! datacon/table gates were structurally blind to. The differential replays are
//! single-expression, the quick tier had no resident-session multi-fragment
//! flow, `datacon_env`'s own unit tests build a fresh table per case, and
//! `datacon_never_used_as_value.rs` scans each fixture tree against its OWN
//! corpus table. None of them compiles a second fragment against a table that
//! is a strict superset of what that fragment mentions, and none of them RUNS
//! one.
//!
//! The compile entry on this path is `add_function` (it never calls
//! `lower_jump_crosses_lam`); the run entry is `run_child_fragment`. Both are
//! driven here.
//!
//! # What this constrains, established by mutation rather than assertion
//!
//! Two breaks were induced on the axes this file claims to cover, to find out
//! which claims it can actually hold.
//!
//! **The RUN's yield-boundary classification: CONSTRAINED.** `ConTags` (the
//! `Val`/`E` discrimination in `effect_machine`) is re-resolved on every
//! `add_function` call against THAT fragment's table (Err -> Ok heals;
//! Ok -> Ok re-installs; Ok -> Err never clobbers — see the GLOBAL-ID
//! INVARIANT comment in `add_function`), not frozen at bootstrap. Because
//! `tags` always reflects the MOST RECENTLY compiled fragment's table, a
//! fragment compiled earlier against a table that disagrees with a later one
//! on `Val`'s id has its baked constructor id go stale once a later
//! `add_function` call re-resolves `tags` from a different table. Minting
//! `Val` in the bootstrap table under a different id than the fragment emits
//! turns all four tests in this file red, the child run failing as
//! `Yield(UnexpectedConTag(10))`.
//!
//! **Emission resolving constructors against the accumulated table: NOT
//! constrained, and it cannot be.** Compiling the second fragment against the
//! BOOTSTRAP table instead of the accumulated one — the mistake this file was
//! written to catch — leaves all four tests green. Emission bakes each
//! `DataConId` straight out of the `Con`/`Case` frame; it does not consult the
//! table to resolve constructor references. `add_function`'s `table` argument
//! feeds `normalize`, `wrap_with_datacon_env`, `lit_wrappers` and the
//! primop constructor-id bundles, none of which these synthetic fragments
//! exercise. So "the fragment is compiled against the accumulated table" is
//! setup here, not an assertion — a wrong table on this path is invisible to
//! this file, and catching it needs a fragment that reaches one of those four
//! consumers.
//!
//! `wrap_with_datacon_env_binds_only_referenced_constructors_from_populated_table`
//! below closes that gap for exactly one of the four: `wrap_with_datacon_env`
//! itself. It calls the function directly against an accumulated (bootstrap +
//! turn-2) table and asserts on the prune's own observable — the exact set of
//! bound constructor wrappers — rather than on an end-to-end run result. That
//! distinction matters specifically here: before the constructor-wrapper
//! prune, the wrap set was always the full table regardless of what the
//! fragment referenced, so a run-result assertion alone cannot tell an
//! over-inclusive wrap set from a correct one. `normalize`, `lit_wrappers` and
//! the primop id bundles remain open — this file does not give them the same
//! direct treatment.

use crate::support;
use support::LinearMachine;
use tidepool_codegen::datacon_env::wrap_with_datacon_env;
use tidepool_codegen::effect_machine::ConTags;

use tidepool_codegen::emit::ExternalEnv;
use tidepool_codegen::jit_machine::JitEffectMachine;
use tidepool_codegen::suspension::{ResumeInput, SuspendableOutcome};
use tidepool_effect::dispatch::{DispatchEffect, EffectContext};
use tidepool_effect::error::EffectError;
use tidepool_effect::Response;
use tidepool_eval::value::Value;
use tidepool_repr::datacon::DataCon;
use tidepool_repr::datacon_table::DataConTable;
use tidepool_repr::frame::CoreFrame;
use tidepool_repr::types::{Alt, AltCon, DataConId, Literal, VarId};
use tidepool_repr::{CoreExpr, TreeBuilder};

use serial_test::serial;

// ─── the bootstrap ("turn 1") constructor set ────────────────────────────────
const C1: DataConId = DataConId(1);
const PAIR_ID: DataConId = DataConId(2);
const VAL_ID: DataConId = DataConId(10);
const E_ID: DataConId = DataConId(11);
const UNION_ID: DataConId = DataConId(12);
const LEAF_ID: DataConId = DataConId(13);
const NODE_ID: DataConId = DataConId(14);

// ─── constructors "turn 2" introduces — absent from the bootstrap table ──────
const BOX2: DataConId = DataConId(21);
const WRAP2: DataConId = DataConId(22);

/// The `Ask` union tag the suspend driver intercepts.
const ASK_TAG: u64 = 0;

fn datacon(id: DataConId, name: &str, qualified: Option<&str>, rep_arity: u32) -> DataCon {
    DataCon {
        id,
        name: name.to_string(),
        tag: 0,
        rep_arity,
        field_bangs: vec![],
        qualified_name: qualified.map(str::to_string),
        type_name: String::new(),
    }
}

/// Turn 1's table: a payload constructor, a pair, and the freer five.
fn bootstrap_table() -> DataConTable {
    let mut table = DataConTable::new();
    table.insert(datacon(C1, "C1", None, 1));
    table.insert(datacon(PAIR_ID, "Pair", None, 2));
    for (id, name, qual, arity) in [
        (VAL_ID, "Val", "Control.Monad.Freer.Val", 1u32),
        (E_ID, "E", "Control.Monad.Freer.E", 2),
        (UNION_ID, "Union", "Data.OpenUnion.Union", 2),
        (LEAF_ID, "Leaf", "Data.FTCQueue.Leaf", 1),
        (NODE_ID, "Node", "Data.FTCQueue.Node", 2),
    ] {
        table.insert(datacon(id, name, Some(qual), arity));
    }
    table
}

/// Turn 2's own table: the freer five it also needs, plus two constructors the
/// bootstrap turn never saw.
fn turn2_table() -> DataConTable {
    let mut table = DataConTable::new();
    table.insert(datacon(BOX2, "Box2", None, 1));
    table.insert(datacon(WRAP2, "Wrap2", None, 1));
    for (id, name, qual, arity) in [
        (VAL_ID, "Val", "Control.Monad.Freer.Val", 1u32),
        (E_ID, "E", "Control.Monad.Freer.E", 2),
        (UNION_ID, "Union", "Data.OpenUnion.Union", 2),
        (LEAF_ID, "Leaf", "Data.FTCQueue.Leaf", 1),
        (NODE_ID, "Node", "Data.FTCQueue.Node", 2),
    ] {
        table.insert(datacon(id, name, Some(qual), arity));
    }
    table
}

/// Accumulate `turn` onto `session`. `PersistentSession::merge_table` filters
/// out already-identical entries and batches the rest through
/// `DataConTable::extend_checked`; this test helper collision-checks each
/// constructor individually via `insert_checked` instead, which rejects the
/// same collisions.
fn merge_table(session: &mut DataConTable, turn: &DataConTable) {
    for dc in turn.iter() {
        session
            .insert_checked(dc.clone())
            .expect("session DataConTable collision");
    }
}

/// The suspending parent:
///
/// ```text
/// let captured = C1 n in
///   E (Union (W# ASK_TAG) (I# req)) (Leaf (\v -> Val (Pair captured (C1 v))))
/// ```
fn build_suspending_parent(captured_n: i64, req: i64) -> CoreExpr {
    let mut b = TreeBuilder::new();
    let cap_lit = b.push(CoreFrame::Lit(Literal::LitInt(captured_n)));
    let captured = b.push(CoreFrame::Con {
        tag: C1,
        fields: vec![cap_lit],
    });
    let var_v = b.push(CoreFrame::Var(VarId(0)));
    let c1_v = b.push(CoreFrame::Con {
        tag: C1,
        fields: vec![var_v],
    });
    let var_captured = b.push(CoreFrame::Var(VarId(1)));
    let pair = b.push(CoreFrame::Con {
        tag: PAIR_ID,
        fields: vec![var_captured, c1_v],
    });
    let val = b.push(CoreFrame::Con {
        tag: VAL_ID,
        fields: vec![pair],
    });
    let lam = b.push(CoreFrame::Lam {
        binder: VarId(0),
        body: val,
    });
    let leaf = b.push(CoreFrame::Con {
        tag: LEAF_ID,
        fields: vec![lam],
    });
    let tag_word = b.push(CoreFrame::Lit(Literal::LitWord(ASK_TAG)));
    let request = b.push(CoreFrame::Lit(Literal::LitInt(req)));
    let union = b.push(CoreFrame::Con {
        tag: UNION_ID,
        fields: vec![tag_word, request],
    });
    let e = b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![union, leaf],
    });
    b.push(CoreFrame::LetNonRec {
        binder: VarId(1),
        rhs: captured,
        body: e,
    });
    b.build()
}

/// Turn 2's fragment, in the effectful shape a real turn returns:
///
/// ```text
/// Val (case Box2 n of Box2 m -> Wrap2 m)
/// ```
///
/// Both `Box2` and `Wrap2` exist ONLY in the accumulated table — a fragment
/// emitted against the bootstrap table alone could not build or match them.
/// The `Val` wrapper forces the result through the yield-boundary
/// classification that `ConTags` performs.
fn build_turn2_fragment(n: i64) -> CoreExpr {
    let mut b = TreeBuilder::new();
    let lit = b.push(CoreFrame::Lit(Literal::LitInt(n)));
    let boxed = b.push(CoreFrame::Con {
        tag: BOX2,
        fields: vec![lit],
    });
    let field = b.push(CoreFrame::Var(VarId(31)));
    let wrapped = b.push(CoreFrame::Con {
        tag: WRAP2,
        fields: vec![field],
    });
    let cased = b.push(CoreFrame::Case {
        scrutinee: boxed,
        binder: VarId(30),
        alts: vec![Alt {
            con: AltCon::DataAlt(BOX2),
            binders: vec![VarId(31)],
            body: wrapped,
        }],
    });
    b.push(CoreFrame::Con {
        tag: VAL_ID,
        fields: vec![cased],
    });
    b.build()
}

/// The drive loop needs a `DispatchEffect`; nothing here reaches dispatch.
struct NoDispatch;
impl DispatchEffect<()> for NoDispatch {
    fn dispatch(
        &mut self,
        _request: &Value,
        _cx: &EffectContext<'_, ()>,
    ) -> Result<Option<Response>, EffectError> {
        Ok(None)
    }
}

fn expect_int(v: &Value) -> i64 {
    match v {
        Value::Lit(Literal::LitInt(n)) => *n,
        Value::Con(_, fields) if fields.len() == 1 => expect_int(&fields[0]),
        other => panic!("expected an Int result, got {other:?}"),
    }
}

/// Bootstrap a session machine on `table` and drive it to its suspension.
fn suspend_parent(table: &DataConTable, captured_n: i64, req: i64) -> LinearMachine {
    let entry = build_suspending_parent(captured_n, req);
    let mut machine = LinearMachine::new(
        JitEffectMachine::compile_session(&entry, table, 1 << 16).expect("compile_session parent"),
    );
    let outcome = machine
        .run_suspendable(table, &mut NoDispatch, &())
        .expect("parent run_suspendable");
    match outcome {
        SuspendableOutcome::Suspended { request, .. } => {
            assert_eq!(
                expect_int(&request),
                req,
                "suspension carries the ask payload"
            );
        }
        SuspendableOutcome::Completed(_) => panic!("parent should suspend at the ask"),
    }
    machine
}

// ───────────────────────────────────────────────────────────────────────────
// THE GATE: compile a second fragment against the ACCUMULATED table and RUN it
// ───────────────────────────────────────────────────────────────────────────

/// The full populated-session path. Turn 1 bootstraps and suspends; turn 2's
/// table is merged onto the session table; turn 2's fragment is `add_function`ed
/// against the ACCUMULATED table and run through `run_child_fragment` with the
/// parent's continuation stowed; then the parent resumes intact.
///
/// The fragment mentions two constructors the bootstrap table does not contain
/// and returns through `Val`, so this fails if either half of the path regresses:
/// emission resolving against the wrong table, or the result failing to classify
/// at the yield boundary.
#[test]
#[serial]
fn second_fragment_on_accumulated_table_compiles_runs_and_classifies() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let boot = bootstrap_table();
            let mut machine = suspend_parent(&boot, 4242, 7);

            let mut session = boot.clone();
            merge_table(&mut session, &turn2_table());
            assert!(
                session.get(BOX2).is_some() && boot.get(BOX2).is_none(),
                "the accumulated table must be a strict superset of the bootstrap one"
            );

            let func_id = machine
                .add_function(
                    "turn2_1",
                    &build_turn2_fragment(99),
                    &session,
                    &ExternalEnv::new(),
                )
                .expect("second fragment compiles against the accumulated session table");

            let result = machine
                .run_child_fragment(func_id, &session, &mut NoDispatch, &())
                .expect(
                    "second fragment runs as a child of the suspended parent and its result \
                     classifies at the yield boundary",
                );

            match &result {
                Value::Con(id, fields) if id.0 == WRAP2.0 && fields.len() == 1 => {
                    assert_eq!(
                        expect_int(&fields[0]),
                        99,
                        "the second fragment's case must project the Box2 field"
                    );
                }
                other => panic!(
                    "expected Wrap2 99 from the accumulated-table fragment, got {other:?} — \
                     emission resolved the turn-2 constructors against the wrong table"
                ),
            }

            assert!(
                machine.is_suspended(),
                "the parent stays suspended across the child fragment"
            );

            let out = machine
                .resume_suspended(
                    &session,
                    &mut NoDispatch,
                    &(),
                    ResumeInput::Answer(Value::Lit(Literal::LitInt(7))),
                )
                .expect("parent resumes after the second fragment");
            match out {
                SuspendableOutcome::Completed(v) => match &v {
                    Value::Con(id, fields) if id.0 == PAIR_ID.0 && fields.len() == 2 => {
                        assert_eq!(expect_int(&fields[0]), 4242, "captured survives the child");
                        assert_eq!(expect_int(&fields[1]), 7, "the answer threads through");
                    }
                    other => panic!("expected Pair(C1 captured, C1 answer), got {other:?}"),
                },
                SuspendableOutcome::Suspended { .. } => panic!("resume should complete"),
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

/// Several turns accreted in sequence, each against the table as it stood: the
/// accumulation itself, not just one second fragment. Each turn's fragment
/// mentions the constructor its own turn introduced.
#[test]
#[serial]
fn successive_fragments_track_the_growing_session_table() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let boot = bootstrap_table();
            let mut machine = suspend_parent(&boot, 11, 3);
            let mut session = boot.clone();

            for turn in 0..4i64 {
                merge_table(&mut session, &turn2_table());
                let func_id = machine
                    .add_function(
                        &format!("accrete_{turn}"),
                        &build_turn2_fragment(500 + turn),
                        &session,
                        &ExternalEnv::new(),
                    )
                    .expect("fragment compiles against the grown table");
                let result = machine
                    .run_child_fragment(func_id, &session, &mut NoDispatch, &())
                    .expect("fragment runs and classifies");
                assert_eq!(
                    expect_int(&result),
                    500 + turn,
                    "each turn computes its own value against the accumulated table"
                );
            }

            let out = machine
                .resume_suspended(
                    &session,
                    &mut NoDispatch,
                    &(),
                    ResumeInput::Answer(Value::Lit(Literal::LitInt(3))),
                )
                .expect("parent resumes after four accreted fragments");
            assert!(matches!(out, SuspendableOutcome::Completed(_)));
        })
        .unwrap()
        .join()
        .unwrap();
}

// ───────────────────────────────────────────────────────────────────────────
// The constructor-wrapper prune's own observable, against a populated table
// ───────────────────────────────────────────────────────────────────────────

/// Build a fragment free in exactly `vars`, in order: `Var(vars[0])`, or an
/// `App` chain over all of `vars` when there is more than one.
fn fragment_referencing(vars: &[VarId]) -> CoreExpr {
    let mut b = TreeBuilder::new();
    let node_indices: Vec<usize> = vars.iter().map(|v| b.push(CoreFrame::Var(*v))).collect();
    let mut indices = node_indices.into_iter();
    let mut acc = indices.next().expect("fragment_referencing needs >=1 var");
    for n in indices {
        acc = b.push(CoreFrame::App { fun: acc, arg: n });
    }
    b.build()
}

/// Walk the top-level `LetNonRec` chain a `wrap_with_datacon_env` result
/// begins with, collecting each minted binding's `VarId` (which is
/// `VarId(dc.id.0)` for the constructor it binds). Minted nodes are always
/// pushed AFTER the original fragment's own nodes (`wrap_with_datacon_env`
/// drains the fragment into a fresh builder first), so `original_len` — the
/// pre-wrap fragment's node count — is the boundary: the walk stops as soon
/// as it steps into a node index below it, i.e. into the original fragment's
/// own tree (which could itself start with an unrelated `LetNonRec`).
fn top_level_wrapped_binder_ids(
    expr: &CoreExpr,
    original_len: usize,
) -> std::collections::BTreeSet<u64> {
    let mut out = std::collections::BTreeSet::new();
    if expr.nodes.is_empty() {
        return out;
    }
    let mut idx = expr.nodes.len() - 1;
    while idx >= original_len {
        let CoreFrame::LetNonRec { binder, body, .. } = &expr.nodes[idx] else {
            break;
        };
        out.insert(binder.0);
        idx = *body;
    }
    out
}

/// The prune's own observable, isolated from the run machinery: which
/// constructor wrappers `wrap_with_datacon_env` binds for a fragment compiled
/// against a POPULATED / accumulated table, not a fresh one.
///
/// This is the axis every other gate in this area was blind to. The
/// differential replays, the quick tier, and `datacon_env`'s own unit tests
/// all exercise a single fragment against a FRESH table — never a table that
/// is a strict superset of what the fragment mentions. And the run-result
/// assertions elsewhere in this file cannot distinguish an over-inclusive
/// wrap set from a correctly-pruned one: before the prune, `wrap_with_datacon_env`
/// always bound the entire table, and the run still classified correctly
/// because the extra wrappers were simply unreferenced dead code. Only
/// inspecting the bound set itself catches a wrong referenced-set computation.
#[test]
fn wrap_with_datacon_env_binds_only_referenced_constructors_from_populated_table() {
    let boot = bootstrap_table();
    let mut session = boot.clone();
    merge_table(&mut session, &turn2_table());
    assert_eq!(
        session.iter().count(),
        9,
        "sanity: accumulated table has all 9 constructors (7 bootstrap + 2 turn-2-only)"
    );

    // Reference one bootstrap constructor (C1) and both turn-2-only
    // constructors (BOX2, WRAP2). Leave PAIR_ID/VAL_ID/E_ID/UNION_ID/LEAF_ID/
    // NODE_ID unreferenced — they must NOT be bound.
    let referenced = [VarId(C1.0), VarId(BOX2.0), VarId(WRAP2.0)];
    let fragment = fragment_referencing(&referenced);
    let original_len = fragment.nodes.len();

    let wrapped = wrap_with_datacon_env(fragment, &session);

    let actual = top_level_wrapped_binder_ids(&wrapped, original_len);
    let expected: std::collections::BTreeSet<u64> = [C1.0, BOX2.0, WRAP2.0].into_iter().collect();

    assert_eq!(
        actual, expected,
        "wrap_with_datacon_env must bind exactly the constructors the fragment \
         references from the ACCUMULATED table — any set difference here is a \
         referenced-set regression that only shows up against a populated \
         session table, not a fresh one"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// The frozen classifier's precondition, asserted directly
// ───────────────────────────────────────────────────────────────────────────

fn tag_tuple(t: &ConTags) -> (u64, u64, u64, u64, u64) {
    (t.val, t.e, t.union, t.leaf, t.node)
}

/// `add_function` re-resolves `JitEffectMachine.tags` against each fragment's
/// own table (see the GLOBAL-ID INVARIANT comment on `add_function`), but this
/// file's suspend flow never calls `add_function` before suspending, so the
/// parent classifies against the bootstrap table's `Val`/`E` ids. That is
/// sound exactly while re-resolving against the accumulated table would give
/// the same answer. Assert it does.
#[test]
fn bootstrap_contags_still_classify_the_accumulated_table() {
    let boot = bootstrap_table();
    let mut session = boot.clone();
    merge_table(&mut session, &turn2_table());

    let boot_tags = ConTags::from_table(&boot).expect("bootstrap table resolves the freer five");
    let accumulated_tags =
        ConTags::from_table(&session).expect("accumulated table resolves the freer five");

    assert_eq!(
        tag_tuple(&boot_tags),
        tag_tuple(&accumulated_tags),
        "the bootstrap turn's ConTags no longer classify the accumulated session \
         table; a later turn's result would fail as YieldError::UnexpectedConTag"
    );
}

/// Positive control for the assertion above: it must actually fire when a later
/// turn re-mints a freer constructor under the SAME qualified name with a
/// different id. Without this, a passing precondition test proves nothing.
///
/// The re-minted id is inserted LAST and directly, so the winner here is fixed.
/// Reaching the same state through `merge_table` would not be: that feeds
/// `insert` from `DataConTable::iter()` (`by_id.values()` over a
/// `std::collections::HashMap`), and `insert` writes `by_qualified_name`
/// last-writer-wins, so which of two same-qualified-name ids ends up resolving
/// would vary per process. `insert_checked` does not see the collision at all —
/// it guards only the `by_id` axis.
#[test]
fn positive_control_reminted_val_is_detected() {
    let boot = bootstrap_table();
    let boot_tags = ConTags::from_table(&boot).expect("bootstrap table resolves the freer five");

    // A second `Val`, same qualified name, different id — what a per-turn id
    // mint would produce — inserted last, so this table is deterministic.
    let val_gen2 = DataConId(910);
    let mut session = boot.clone();
    session.insert(datacon(BOX2, "Box2", None, 1));
    session.insert(datacon(WRAP2, "Wrap2", None, 1));
    session.insert(datacon(val_gen2, "Val", Some("Control.Monad.Freer.Val"), 1));

    assert!(
        session.get(VAL_ID).is_some() && session.get(val_gen2).is_some(),
        "both Val ids are present in the accumulated table — nothing rejects a \
         qualified-name collision"
    );

    let accumulated_tags =
        ConTags::from_table(&session).expect("accumulated table still resolves the freer five");
    assert_ne!(
        tag_tuple(&boot_tags),
        tag_tuple(&accumulated_tags),
        "the precondition assertion must fire on a re-minted Val — otherwise it \
         has no teeth"
    );
    assert_eq!(
        accumulated_tags.val, val_gen2.0,
        "the last qualified-name writer wins, silently"
    );
}
