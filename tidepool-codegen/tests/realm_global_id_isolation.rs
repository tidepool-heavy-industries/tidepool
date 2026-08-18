//! REALM GLOBAL-ID ISOLATION — pins the GLOBAL-ID invariant documented at its
//! definition (`jit_machine.rs`, `add_function`'s
//! `tags`/`json_con_ids`/`time_con_ids` accumulation): two
//! realms sharing ONE machine may each carry a DIFFERENT `DataConTable`, and
//! nothing prevents those tables from assigning the SAME numeric
//! `DataConId`/runtime tag to DIFFERENT domain constructors. This file proves
//! that collision is harmless for ordinary data: each realm's `add_function`
//! fragment is compiled against, and its resume threads through, ITS OWN
//! `ContinuationFrame::table` (A4) and its own independently-allocated heap
//! objects — never a shared, overwritable slot. The machine-global slots the
//! doc comment actually warns about (`tags`/`json_con_ids`/`time_con_ids`)
//! are reserved for the freer-simple envelope and JSON/time constructors,
//! which every realm on a machine is expected to agree on; a realm's own
//! domain constructors never touch them.
//!
//! Realm A's table maps id 50 to `Apple :: Int -> T` (arity 1); realm B's
//! table maps the SAME id 50 — and the SAME runtime tag, 50 — to
//! `Banana :: Int -> Int -> T` (arity 2): identical id AND identical runtime
//! tag, divergent shape, the sharpest collision construable. Both realms
//! park a value built from their own colliding constructor, interleaved on
//! one machine (each realm's `add_function`/park call is itself one of the
//! machine-global-cache "accumulate" events the doc comment describes), and
//! each resumes with ITS OWN shape intact, in the opposite order they parked
//! — so neither resume can coincidentally "coast" on being the most recent
//! `add_function` call.

use tidepool_codegen::emit::ExternalEnv;
use tidepool_codegen::jit_machine::{
    JitEffectMachine, ParkKind, ParkedOutcome, RealmId, ResumeInput,
};
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_effect::dispatch::EffectContext;
use tidepool_effect::error::EffectError;
use tidepool_effect::Response;
use tidepool_eval::value::Value;
use tidepool_repr::datacon::DataCon;
use tidepool_repr::datacon_table::DataConTable;
use tidepool_repr::frame::CoreFrame;
use tidepool_repr::types::*;
use tidepool_repr::{CoreExpr, Literal, TreeBuilder};

use serial_test::serial;

#[path = "support/session_scaffold.rs"]
mod session_scaffold;
#[path = "support/session_scaffold_expect.rs"]
mod session_scaffold_expect;
use session_scaffold::C1;
use session_scaffold_expect::expect_int;

const PAIR_ID: DataConId = DataConId(2);
const VAL_ID: DataConId = DataConId(10);
const E_ID: DataConId = DataConId(11);
const UNION_ID: DataConId = DataConId(12);
const LEAF_ID: DataConId = DataConId(13);
const NODE_ID: DataConId = DataConId(14);

/// The colliding id: `Apple` (arity 1) in realm A's table, `Banana`
/// (arity 2) in realm B's — SAME `DataConId`, SAME runtime tag (50), two
/// unrelated shapes.
const COLLIDING_ID: DataConId = DataConId(50);
const COLLIDING_TAG: u32 = 50;

const ASK_TAG: u64 = 0;

/// The envelope + wrapper constructors every realm on a machine MUST agree
/// on (the global-ID invariant's precondition) — identical across
/// `table_a`/`table_b` below.
fn base_table() -> DataConTable {
    let mut t = DataConTable::new();
    t.insert(DataCon {
        id: C1,
        name: "C1".to_string(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![],
        qualified_name: None,
        type_name: String::new(),
    });
    t.insert(DataCon {
        id: PAIR_ID,
        name: "Pair".to_string(),
        tag: 2,
        rep_arity: 2,
        field_bangs: vec![],
        qualified_name: None,
        type_name: String::new(),
    });
    for (id, name, qual, arity) in [
        (VAL_ID, "Val", "Control.Monad.Freer.Val", 1u32),
        (E_ID, "E", "Control.Monad.Freer.E", 2),
        (UNION_ID, "Union", "Data.OpenUnion.Union", 2),
        (LEAF_ID, "Leaf", "Data.FTCQueue.Leaf", 1),
        (NODE_ID, "Node", "Data.FTCQueue.Node", 2),
    ] {
        t.insert(DataCon {
            id,
            name: name.to_string(),
            tag: 0,
            rep_arity: arity,
            field_bangs: vec![],
            qualified_name: Some(qual.to_string()),
            type_name: String::new(),
        });
    }
    t
}

fn table_a() -> DataConTable {
    let mut t = base_table();
    t.insert(DataCon {
        id: COLLIDING_ID,
        name: "Apple".to_string(),
        tag: COLLIDING_TAG,
        rep_arity: 1,
        field_bangs: vec![],
        qualified_name: None,
        type_name: String::new(),
    });
    t
}

fn table_b() -> DataConTable {
    let mut t = base_table();
    t.insert(DataCon {
        id: COLLIDING_ID,
        name: "Banana".to_string(),
        tag: COLLIDING_TAG,
        rep_arity: 2,
        field_bangs: vec![],
        qualified_name: None,
        type_name: String::new(),
    });
    t
}

/// `let captured = Apple n in E (Union (W# ASK_TAG) (I# req)) (Leaf (\v -> Val (Pair captured (C1 v))))`
fn build_apple_suspend(n: i64, req: i64) -> CoreExpr {
    let mut b = TreeBuilder::new();
    let n_lit = b.push(CoreFrame::Lit(Literal::LitInt(n)));
    let captured = b.push(CoreFrame::Con {
        tag: COLLIDING_ID,
        fields: vec![n_lit],
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

/// `let captured = Banana n0 n1 in E (Union (W# ASK_TAG) (I# req)) (Leaf (\v -> Val (Pair captured (C1 v))))`
fn build_banana_suspend(n0: i64, n1: i64, req: i64) -> CoreExpr {
    let mut b = TreeBuilder::new();
    let n0_lit = b.push(CoreFrame::Lit(Literal::LitInt(n0)));
    let n1_lit = b.push(CoreFrame::Lit(Literal::LitInt(n1)));
    let captured = b.push(CoreFrame::Con {
        tag: COLLIDING_ID,
        fields: vec![n0_lit, n1_lit],
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

struct NoDispatch;
impl DispatchEffect<()> for NoDispatch {
    fn dispatch(
        &mut self,
        tag: u64,
        _request: &Value,
        _cx: &EffectContext<'_, ()>,
    ) -> Result<Response, EffectError> {
        panic!("handler dispatched tag {tag} — the ask should have suspended instead");
    }
}

fn in_test_thread(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

#[test]
#[serial]
fn two_realms_with_colliding_domain_ids_do_not_shadow_each_other() {
    in_test_thread(|| {
        let table_a = table_a();
        let table_b = table_b();
        let mut machine =
            JitEffectMachine::compile_session(&build_apple_suspend(0, 0), &table_a, 1 << 16)
                .expect("compile_session");

        // Realm A: compiled + will be decoded against table_a, where id 50
        // is `Apple` (arity 1).
        let frag_a = machine
            .add_function(
                "apple_frag",
                &build_apple_suspend(111, 1),
                &table_a,
                &ExternalEnv::new(),
            )
            .expect("add apple fragment");
        let id_a = match machine
            .run_fragment_suspendable_parked(
                frag_a,
                &table_a,
                &mut NoDispatch,
                &(),
                ASK_TAG,
                RealmId(0),
                ParkKind::Plain,
                &[],
            )
            .expect("realm A run_fragment_suspendable_parked")
        {
            ParkedOutcome::CompletedProject { .. } | ParkedOutcome::CompletedRender { .. } => {
                unreachable!(
                    "this test parks only Plain/Binding turns - Project/Render \
                     completions cannot be produced for them"
                )
            }
            ParkedOutcome::Suspended { id, request, .. } => {
                assert_eq!(expect_int(&request), 1);
                id
            }
            ParkedOutcome::CompletedValue(..) | ParkedOutcome::CompletedBinding { .. } => {
                panic!("realm A must suspend at its ask")
            }
        };

        // Realm B: compiled + will be decoded against table_b, where the
        // SAME id 50 (and the SAME runtime tag) is `Banana` (arity 2). This
        // `add_function` call is itself one of the machine-global
        // `tags`/`json_con_ids`/`time_con_ids` "accumulate" events the
        // invariant doc warns about.
        let frag_b = machine
            .add_function(
                "banana_frag",
                &build_banana_suspend(222, 333, 2),
                &table_b,
                &ExternalEnv::new(),
            )
            .expect("add banana fragment");
        let id_b = match machine
            .run_fragment_suspendable_parked(
                frag_b,
                &table_b,
                &mut NoDispatch,
                &(),
                ASK_TAG,
                RealmId(1),
                ParkKind::Plain,
                &[],
            )
            .expect("realm B run_fragment_suspendable_parked")
        {
            ParkedOutcome::CompletedProject { .. } | ParkedOutcome::CompletedRender { .. } => {
                unreachable!(
                    "this test parks only Plain/Binding turns - Project/Render \
                     completions cannot be produced for them"
                )
            }
            ParkedOutcome::Suspended { id, request, .. } => {
                assert_eq!(expect_int(&request), 2);
                id
            }
            ParkedOutcome::CompletedValue(..) | ParkedOutcome::CompletedBinding { .. } => {
                panic!("realm B must suspend at its ask")
            }
        };
        assert_eq!(machine.parked_count(), 2);

        // Resume B FIRST — the opposite of park order — then A: neither
        // resume gets to coast on being the most recent `add_function` call.
        match machine
            .resume_parked(
                id_b,
                &mut NoDispatch,
                &(),
                ResumeInput::Answer(Value::Lit(Literal::LitInt(20))),
            )
            .expect("resume realm B")
        {
            ParkedOutcome::CompletedValue(value)
            | ParkedOutcome::CompletedBinding { value, .. } => match &value {
                Value::Con(id, fields) if id.0 == PAIR_ID.0 && fields.len() == 2 => {
                    match &fields[0] {
                        Value::Con(cid, cf) if cid.0 == COLLIDING_ID.0 && cf.len() == 2 => {
                            assert_eq!(expect_int(&cf[0]), 222, "realm B's OWN Banana field 0");
                            assert_eq!(expect_int(&cf[1]), 333, "realm B's OWN Banana field 1");
                        }
                        other => panic!(
                            "realm B must decode its OWN Banana (arity 2, id/tag {COLLIDING_TAG}), \
                             not realm A's Apple — got {other:?}"
                        ),
                    }
                    assert_eq!(expect_int(&fields[1]), 20, "realm B's own resumed answer");
                }
                other => panic!("expected Pair(Banana, C1 answer), got {other:?}"),
            },
            ParkedOutcome::CompletedProject { .. } | ParkedOutcome::CompletedRender { .. } => {
                unreachable!(
                    "this test parks only Plain/Binding turns - Project/Render \
                     completions cannot be produced for them"
                )
            }
            ParkedOutcome::Suspended { .. } => panic!("resume should complete, not re-suspend"),
        }
        assert_eq!(machine.parked_count(), 1);

        match machine
            .resume_parked(
                id_a,
                &mut NoDispatch,
                &(),
                ResumeInput::Answer(Value::Lit(Literal::LitInt(10))),
            )
            .expect("resume realm A")
        {
            ParkedOutcome::CompletedValue(value)
            | ParkedOutcome::CompletedBinding { value, .. } => match &value {
                Value::Con(id, fields) if id.0 == PAIR_ID.0 && fields.len() == 2 => {
                    match &fields[0] {
                        Value::Con(cid, cf) if cid.0 == COLLIDING_ID.0 && cf.len() == 1 => {
                            assert_eq!(expect_int(&cf[0]), 111, "realm A's OWN Apple field");
                        }
                        other => panic!(
                            "realm A must decode its OWN Apple (arity 1, id/tag {COLLIDING_TAG}), \
                             not realm B's Banana — got {other:?}"
                        ),
                    }
                    assert_eq!(expect_int(&fields[1]), 10, "realm A's own resumed answer");
                }
                other => panic!("expected Pair(Apple, C1 answer), got {other:?}"),
            },
            ParkedOutcome::CompletedProject { .. } | ParkedOutcome::CompletedRender { .. } => {
                unreachable!(
                    "this test parks only Plain/Binding turns - Project/Render \
                     completions cannot be produced for them"
                )
            }
            ParkedOutcome::Suspended { .. } => panic!("resume should complete, not re-suspend"),
        }
        assert_eq!(machine.parked_count(), 0);

        drop(machine);
    });
}
