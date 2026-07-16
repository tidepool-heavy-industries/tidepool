//! GC-forcing program builder shared by heap/GC tests.

use tidepool_repr::datacon::DataCon;
use tidepool_repr::types::{DataConId, Literal, VarId};
use tidepool_repr::{CoreExpr, CoreFrame, DataConTable, TreeBuilder};

/// Build a Con-chain expr + `DataConTable` that forces >=1 real GC under a
/// small nursery: an App-chain calling a function that allocates two Cons per
/// call, `depth` times. The table holds the single unary constructor `C1`
/// (`DataConId(1)`); callers with extra constructor needs (e.g. effect
/// continuation datacons) append to the returned table.
pub fn make_gc_forcing_setup(depth: usize) -> (CoreExpr, DataConTable) {
    let mut bld = TreeBuilder::new();
    let var_x = bld.push(CoreFrame::Var(VarId(0)));
    let g1_rhs = bld.push(CoreFrame::Con {
        tag: DataConId(1),
        fields: vec![var_x],
    });
    let var_g1 = bld.push(CoreFrame::Var(VarId(1)));
    let g2_rhs = bld.push(CoreFrame::Con {
        tag: DataConId(1),
        fields: vec![var_g1],
    });
    let final_con = bld.push(CoreFrame::Con {
        tag: DataConId(1),
        fields: vec![var_x],
    });
    let let_g2 = bld.push(CoreFrame::LetNonRec {
        binder: VarId(2),
        rhs: g2_rhs,
        body: final_con,
    });
    let let_g1 = bld.push(CoreFrame::LetNonRec {
        binder: VarId(1),
        rhs: g1_rhs,
        body: let_g2,
    });
    let lam_x = bld.push(CoreFrame::Lam {
        binder: VarId(0),
        body: let_g1,
    });
    let mut current = bld.push(CoreFrame::Lit(Literal::LitInt(42)));
    for _ in 0..depth {
        let f_var = bld.push(CoreFrame::Var(VarId(99)));
        current = bld.push(CoreFrame::App {
            fun: f_var,
            arg: current,
        });
    }
    bld.push(CoreFrame::LetRec {
        bindings: vec![(VarId(99), lam_x)],
        body: current,
    });
    let expr = bld.build();

    let mut table = DataConTable::new();
    table.insert(DataCon {
        id: DataConId(1),
        name: "C1".to_string(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![],
        qualified_name: None,
    });
    (expr, table)
}
