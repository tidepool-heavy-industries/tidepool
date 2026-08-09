//! `build_value_fragment`, layered on `session_scaffold.rs`'s `C1`. A
//! consumer of this module must also `#[path] mod session_scaffold;` — this
//! file resolves `C1` through `crate::session_scaffold::C1`.

use tidepool_repr::types::Literal;
use tidepool_repr::{CoreExpr, CoreFrame, TreeBuilder};

/// `C1 n` — a plain bindable value fragment.
pub fn build_value_fragment(n: i64) -> CoreExpr {
    let mut b = TreeBuilder::new();
    let lit = b.push(CoreFrame::Lit(Literal::LitInt(n)));
    b.push(CoreFrame::Con {
        tag: crate::session_scaffold::C1,
        fields: vec![lit],
    });
    b.build()
}
