use tidepool_repr::{CoreExpr, CoreFrame, DataConTable, TreeBuilder, VarId};

/// Wrap a CoreExpr with let-bindings for all data constructors from the table.
///
/// For each DataCon with arity N:
/// - arity 0: `let dc_var = Con(id, []) in ...`
/// - arity 1: `let dc_var = \v0 -> Con(id, [v0]) in ...`
/// - arity 2: `let dc_var = \v0 -> \v1 -> Con(id, [v0, v1]) in ...`
/// - etc.
///
/// The binding VarId matches `VarId(dc.id.0)`, which is what the GHC Core translator
/// uses to reference data constructors as function values.
pub fn wrap_with_datacon_env(expr: &CoreExpr, table: &DataConTable) -> CoreExpr {
    let mut b = TreeBuilder::new();

    // First, push all nodes from the original expression
    let mut src = TreeBuilder::new();
    for node in &expr.nodes {
        src.push(node.clone());
    }
    let base = b.push_tree(src);
    let root = base + expr.nodes.len() - 1;

    // Collect datacons sorted by id for deterministic output
    let mut datacons: Vec<_> = table.iter().collect();
    datacons.sort_by_key(|dc| dc.id.0);

    let mut body = root;

    for dc in &datacons {
        let binder = VarId(dc.id.0);
        let arity = dc.rep_arity as usize;

        if arity == 0 {
            // Con(id, [])
            let con = b.push(CoreFrame::Con {
                tag: dc.id,
                fields: vec![],
            });
            body = b.push(CoreFrame::LetNonRec {
                binder,
                rhs: con,
                body,
            });
        } else {
            // Build curried lambda chain: \v0 -> \v1 -> ... -> Con(id, [v0, v1, ...])
            // Fresh vars use a hash of the DataConId to avoid collisions
            let fresh_base = dc.id.0.wrapping_mul(0x517cc1b727220a95).wrapping_add(0xFFFF_0000_0000_0000);
            let fresh_vars: Vec<VarId> = (0..arity)
                .map(|i| VarId(fresh_base.wrapping_add(i as u64)))
                .collect();

            // Build Con(id, [v0, v1, ...]) — innermost
            let var_indices: Vec<usize> = fresh_vars
                .iter()
                .map(|v| b.push(CoreFrame::Var(*v)))
                .collect();
            let mut inner = b.push(CoreFrame::Con {
                tag: dc.id,
                fields: var_indices,
            });

            // Wrap in lambdas from inside out
            for v in fresh_vars.iter().rev() {
                inner = b.push(CoreFrame::Lam {
                    binder: *v,
                    body: inner,
                });
            }

            body = b.push(CoreFrame::LetNonRec {
                binder,
                rhs: inner,
                body,
            });
        }
    }

    b.build()
}
