//! `build_reference_fragment`, layered on `session_scaffold.rs`'s `C1`. A
//! consumer of this module must also `#[path] mod session_scaffold;` — this
//! file resolves `C1` through `crate::session_scaffold::C1`.

use tidepool_repr::types::{Alt, AltCon, VarId};
use tidepool_repr::{CoreExpr, CoreFrame, TreeBuilder};

/// `case x of C1 n -> n` — x is the seeded external session binder. Resolves
/// the tenured value built by `build_value_fragment` and projects its field.
pub fn build_reference_fragment(x: VarId) -> CoreExpr {
    let mut b = TreeBuilder::new();
    let body = b.push(CoreFrame::Var(VarId(11)));
    let scrut = b.push(CoreFrame::Var(x));
    b.push(CoreFrame::Case {
        scrutinee: scrut,
        binder: VarId(10),
        alts: vec![Alt {
            con: AltCon::DataAlt(crate::session_scaffold::C1),
            binders: vec![VarId(11)],
            body,
        }],
    });
    b.build()
}
