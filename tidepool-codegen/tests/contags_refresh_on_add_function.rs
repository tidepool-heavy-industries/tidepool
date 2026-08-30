//! Prior bug:
//! `JitEffectMachine.tags: Result<ConTags, &'static str>` is resolved once, at
//! bootstrap, in `compile_inner`. Every later `add_function`ed fragment is
//! classified by that same frozen `Result`. If the bootstrap table is missing
//! one of the five freer constructors (`Val`/`E`/`Union`/`Leaf`/`Node`), the
//! session is permanently `JitError::MissingConTags` — even after a later
//! turn's table supplies the missing constructor — unless `add_function`
//! refreshes `tags` from that turn's table.
//!
//! This is deterministic and needs no id-stability precondition: the table
//! literally lacks the constructor at bootstrap, and the fix is exercised the
//! moment a second, complete table arrives via `add_function`.

use tidepool_codegen::emit::ExternalEnv;
use tidepool_codegen::jit_machine::{JitEffectMachine, JitError};
use tidepool_effect::dispatch::{DispatchEffect, EffectContext};
use tidepool_effect::error::EffectError;
use tidepool_effect::Response;
use tidepool_eval::value::Value;
use tidepool_repr::datacon::DataCon;
use tidepool_repr::datacon_table::DataConTable;
use tidepool_repr::frame::CoreFrame;
use tidepool_repr::types::{DataConId, Literal};
use tidepool_repr::{CoreExpr, TreeBuilder};

const VAL_ID: DataConId = DataConId(10);
const E_ID: DataConId = DataConId(11);
const UNION_ID: DataConId = DataConId(12);
const LEAF_ID: DataConId = DataConId(13);
const NODE_ID: DataConId = DataConId(14);

fn datacon(id: DataConId, name: &str, qualified: &str, rep_arity: u32) -> DataCon {
    DataCon {
        id,
        name: name.to_string(),
        tag: 0,
        rep_arity,
        field_bangs: vec![],
        qualified_name: Some(qualified.to_string()),
        type_name: String::new(),
    }
}

/// Four of the five freer constructors — `Node` deliberately absent, so
/// `ConTags::from_table` returns `Err(EffContKind::Node)` at bootstrap.
fn table_missing_node() -> DataConTable {
    let mut table = DataConTable::new();
    table.insert(datacon(VAL_ID, "Val", "Control.Monad.Freer.Val", 1));
    table.insert(datacon(E_ID, "E", "Control.Monad.Freer.E", 2));
    table.insert(datacon(UNION_ID, "Union", "Data.OpenUnion.Union", 2));
    table.insert(datacon(LEAF_ID, "Leaf", "Data.FTCQueue.Leaf", 1));
    table
}

/// All five freer constructors.
fn full_table() -> DataConTable {
    let mut table = table_missing_node();
    table.insert(datacon(NODE_ID, "Node", "Data.FTCQueue.Node", 2));
    table
}

/// `Val n` — a pure freer-simple result carrying an `Int`, effect-free.
fn val_wrapped_int(n: i64) -> CoreExpr {
    let mut b = TreeBuilder::new();
    let lit = b.push(CoreFrame::Lit(Literal::LitInt(n)));
    b.push(CoreFrame::Con {
        tag: VAL_ID,
        fields: vec![lit],
    });
    b.build()
}

/// Never reached: neither turn in this test yields an effect.
struct NoDispatch;
impl DispatchEffect<()> for NoDispatch {
    fn dispatch(
        &mut self,
        _request: &Value,
        _cx: &EffectContext<'_, ()>,
    ) -> Result<Option<Response>, EffectError> {
        panic!("this test's entries never yield an effect");
    }
}

fn expect_int(v: &Value) -> i64 {
    match v {
        Value::Lit(Literal::LitInt(n)) => *n,
        Value::Con(_, fields) if fields.len() == 1 => expect_int(&fields[0]),
        other => panic!("expected an Int result, got {other:?}"),
    }
}

/// The regression: bootstrap on a table missing `Node`, confirm the frozen
/// classifier fails exactly as `MissingConTags` on the first run, then
/// `add_function` a later turn whose table carries all five and confirm the
/// session classifies instead of erroring `MissingConTags` again.
#[test]
fn add_function_refreshes_missing_con_tags_from_a_later_turns_table() {
    let boot = table_missing_node();
    let mut machine = JitEffectMachine::compile_session(&val_wrapped_int(1), &boot, 1 << 16)
        .expect(
            "compile_session succeeds even though the table is missing Node: \
             ConTags resolution is deferred to run time, not compile time",
        );

    // Precondition: the bootstrap table truly lacks Node, so classification
    // fails on the very first run.
    let err = machine
        .run(&boot, &mut NoDispatch, &())
        .expect_err("bootstrap table is missing Node; the first run must fail to classify");
    match err {
        JitError::MissingConTags(kind) => {
            assert_eq!(kind, "Node", "the missing freer constructor must be Node");
        }
        other => panic!("expected JitError::MissingConTags(\"Node\"), got {other:?}"),
    }

    // A later turn's table supplies the missing constructor.
    let full = full_table();
    let func_id = machine
        .add_function("turn2", &val_wrapped_int(2), &full, &ExternalEnv::new())
        .expect("second fragment compiles against the complete table");

    // Without the `add_function` refresh, `self.tags` stays
    // frozen at the bootstrap `Err("Node")` forever, and this second run
    // fails MissingConTags again even though `full` has everything the
    // classifier needs. With the refresh, `tags` is re-resolved against
    // this turn's table and the session classifies.
    let result = machine
        .run_fragment(func_id, &full, &mut NoDispatch, &())
        .expect(
            "add_function must refresh `tags` from the later turn's complete table; \
             the session should classify instead of erroring MissingConTags again",
        );
    assert_eq!(
        expect_int(&result),
        2,
        "the second fragment's own value comes through once classification succeeds"
    );
}
