//! Immutable binding tips retain one shared lazy value rather than copying it.

use tidepool_codegen::binding_table::{BindingEntry, BindingTable, BoundValue};
use tidepool_codegen::emit::ExternalEnv;
use tidepool_codegen::jit_machine::JitEffectMachine;
use tidepool_codegen::scope::{ScopeId, ScopeTree};
use tidepool_repr::datacon::DataCon;
use tidepool_repr::types::{Alt, AltCon, DataConId, Literal, VarId};
use tidepool_repr::{
    BindingName, CoreExpr, CoreFrame, DataConTable, Generation, SessionModule, SessionVarId,
    TreeBuilder,
};

use crate::session_scaffold_expect;
use session_scaffold_expect::expect_int;

const I_HASH: DataConId = DataConId(7);
const SHARED: VarId = VarId(0xFE00_0000_0000_2002);

fn table() -> DataConTable {
    let mut table = DataConTable::new();
    table.insert(DataCon {
        id: I_HASH,
        name: "I#".into(),
        tag: 7,
        rep_arity: 1,
        field_bangs: vec![],
        qualified_name: None,
        type_name: String::new(),
    });
    table
}

fn boxed(value: i64) -> CoreExpr {
    let mut tree = TreeBuilder::new();
    let value = tree.push(CoreFrame::Lit(Literal::LitInt(value)));
    tree.push(CoreFrame::Con {
        tag: I_HASH,
        fields: vec![value],
    });
    tree.build()
}

/// `let mkBox = \n -> I# n; x = mkBox 555; f = \_ -> x in f`.
fn lazy_holder() -> CoreExpr {
    let mut tree = TreeBuilder::new();
    let n = VarId(1);
    let n_ref = tree.push(CoreFrame::Var(n));
    let box_body = tree.push(CoreFrame::Con {
        tag: I_HASH,
        fields: vec![n_ref],
    });
    let mk_box = tree.push(CoreFrame::Lam {
        binder: n,
        body: box_body,
    });
    let mk_box_name = VarId(2);
    let mk_box_ref = tree.push(CoreFrame::Var(mk_box_name));
    let argument = tree.push(CoreFrame::Lit(Literal::LitInt(555)));
    let lazy_rhs = tree.push(CoreFrame::App {
        fun: mk_box_ref,
        arg: argument,
    });
    let lazy_name = VarId(3);
    let lazy_ref = tree.push(CoreFrame::Var(lazy_name));
    let function = tree.push(CoreFrame::Lam {
        binder: VarId(4),
        body: lazy_ref,
    });
    let function_name = VarId(5);
    let function_ref = tree.push(CoreFrame::Var(function_name));
    let bind_function = tree.push(CoreFrame::LetNonRec {
        binder: function_name,
        rhs: function,
        body: function_ref,
    });
    let bind_lazy = tree.push(CoreFrame::LetNonRec {
        binder: lazy_name,
        rhs: lazy_rhs,
        body: bind_function,
    });
    tree.push(CoreFrame::LetNonRec {
        binder: mk_box_name,
        rhs: mk_box,
        body: bind_lazy,
    });
    tree.build()
}

/// `case shared 0# of I# n -> n`.
fn force_shared() -> CoreExpr {
    let mut tree = TreeBuilder::new();
    let function = tree.push(CoreFrame::Var(SHARED));
    let argument = tree.push(CoreFrame::Lit(Literal::LitInt(0)));
    let call = tree.push(CoreFrame::App {
        fun: function,
        arg: argument,
    });
    let n = VarId(10);
    let n_ref = tree.push(CoreFrame::Var(n));
    tree.push(CoreFrame::Case {
        scrutinee: call,
        binder: VarId(11),
        alts: vec![Alt {
            con: AltCon::DataAlt(I_HASH),
            binders: vec![n],
            body: n_ref,
        }],
    });
    tree.build()
}

#[test]
fn parent_and_child_force_one_leased_thunk_sequentially() {
    let table = table();
    let mut machine =
        JitEffectMachine::compile_session(&boxed(0), &table, 1 << 16).expect("session");
    let holder = machine
        .add_function("lazy_holder", &lazy_holder(), &table, &ExternalEnv::new())
        .expect("compile holder");
    let slot = machine.run_pure_and_bind(holder).expect("tenure holder");

    let mut bindings = BindingTable::new();
    bindings.bind(BindingEntry {
        name: BindingName("shared".into()),
        id: SessionVarId::from_var(SHARED),
        module: SessionModule::val(Generation(1)),
        value: BoundValue::Tier1Closure(slot),
        type_display: Some("Int -> Int".into()),
        defining_expr: None,
        scope: ScopeId::ROOT,
    });
    let mut scopes = ScopeTree::new();
    let child = scopes.mint_child(ScopeId::ROOT).expect("root is live");
    bindings.seed_scope(&scopes, ScopeId::ROOT, child);

    let parent_slot = bindings.resolve("shared").unwrap().value.root();
    let child_slot = bindings
        .resolve_in(&scopes, child, "shared")
        .unwrap()
        .value
        .root();
    assert!(std::ptr::eq(parent_slot.addr(), child_slot.addr()));

    let env = bindings.seed_external_env(&[SHARED]);
    let force = machine
        .add_function("force_shared", &force_shared(), &table, &env)
        .expect("compile force");
    assert_eq!(expect_int(&machine.run_fragment_pure(force).unwrap()), 555);
    assert_eq!(expect_int(&machine.run_fragment_pure(force).unwrap()), 555);
}
