//! Shared session-machine test scaffolding between `bind_error_then_allocate.rs`
//! and `converge_proof.rs`: the `C1` payload constructor, the value/reference
//! fragment builders, the GC-forcing filler fragment, and the pure-result Int
//! extractor. `tests/*.rs` files are separate crates, so this is included via
//! `#[path]` rather than shared as an ordinary library module.

use tidepool_repr::types::{Alt, AltCon, DataConId, Literal, VarId};
use tidepool_repr::{CoreExpr, CoreFrame, TreeBuilder};

/// The data constructor `C1 :: Int -> T` (arity 1) shared by all fragments.
pub const C1: DataConId = DataConId(1);

/// `C1 n` — a plain bindable value fragment.
pub fn build_value_fragment(n: i64) -> CoreExpr {
    let mut b = TreeBuilder::new();
    let lit = b.push(CoreFrame::Lit(Literal::LitInt(n)));
    b.push(CoreFrame::Con {
        tag: C1,
        fields: vec![lit],
    });
    b.build()
}

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
            con: AltCon::DataAlt(C1),
            binders: vec![VarId(11)],
            body,
        }],
    });
    b.build()
}

/// A heavy allocator that overflows a small session nursery, forcing a real
/// minor GC — same shape as `tidepool_testing::gen::make_gc_forcing_setup`,
/// built directly against `C1` (`DataConId(1)`) rather than that helper's own
/// table.
pub fn build_gc_forcing_fragment(depth: usize) -> CoreExpr {
    tidepool_testing::gen::make_gc_forcing_setup(depth).0
}

/// Extract an `Int` from a pure-run result Value (Lit or 1-arg Con wrapping one).
pub fn expect_int(v: &tidepool_eval::value::Value) -> i64 {
    use tidepool_eval::value::Value;
    match v {
        Value::Lit(Literal::LitInt(n)) => *n,
        Value::Con(_, fields) if fields.len() == 1 => expect_int(&fields[0]),
        other => panic!("expected an Int result, got {other:?}"),
    }
}
