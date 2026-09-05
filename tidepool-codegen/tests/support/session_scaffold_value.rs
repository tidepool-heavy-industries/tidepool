//! Value fragments using the shared session constructor identity.

use tidepool_repr::types::Literal;
use tidepool_repr::{CoreExpr, CoreFrame, TreeBuilder};

/// `C1 n` — a plain bindable value fragment.
pub fn build_value_fragment(n: i64) -> CoreExpr {
    let mut b = TreeBuilder::new();
    let lit = b.push(CoreFrame::Lit(Literal::LitInt(n)));
    b.push(CoreFrame::Con {
        tag: super::session_scaffold::C1,
        fields: vec![lit],
    });
    b.build()
}
