use tidepool_repr::{CoreExpr, CoreFrame, DataConId, DataConTable, TreeBuilder, VarId};

/// One constructor binding minted by [`wrap_with_datacon_env`].
///
/// The wrapper RHSs are CLOSED terms — an arity-0 `Con` with no fields, or a
/// curried lambda chain over freshly-minted binders — so they capture nothing
/// from the fragment and are identical for a given constructor on every turn.
/// That closedness is what lets a caller compile a wrapper's closure once per
/// session and reuse it (see `CodegenPipeline`'s constructor-closure cache).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataConWrap {
    /// Index of the `LetNonRec` node this binding occupies in the wrapped tree.
    /// Valid only against the tree returned alongside it, and only while that
    /// tree's indices are untouched — a later pass that rebuilds the tree
    /// (e.g. `lower::lower_jump_crosses_lam`) invalidates the manifest.
    pub let_idx: usize,
    /// The constructor bound at `VarId(tag.0)`.
    pub tag: DataConId,
    /// `rep_arity`: 0 binds a saturated `Con`, N binds an N-deep lambda chain.
    pub arity: usize,
}

/// [`wrap_with_datacon_env`]'s result: the wrapped tree plus a manifest of the
/// bindings it minted, innermost (lowest `DataConId`) first.
pub struct WrappedExpr {
    /// The fragment with its constructor environment prepended.
    pub expr: CoreExpr,
    /// One entry per minted binding, in construction order.
    pub wraps: Vec<DataConWrap>,
}

/// Wrap a CoreExpr with let-bindings for data constructors from the table.
///
/// For each DataCon with arity N:
/// - arity 0: `let dc_var = Con(id, []) in ...`
/// - arity 1: `let dc_var = \v0 -> Con(id, [v0]) in ...`
/// - arity 2: `let dc_var = \v0 -> \v1 -> Con(id, [v0, v1]) in ...`
/// - etc.
///
/// The binding VarId matches `VarId(dc.id.0)`, which is what the GHC Core translator
/// uses to reference data constructors as function values.
pub fn wrap_with_datacon_env(mut expr: CoreExpr, table: &DataConTable) -> WrappedExpr {
    if expr.nodes.is_empty() {
        return WrappedExpr {
            expr,
            wraps: Vec::new(),
        };
    }
    let mut b = TreeBuilder::new();

    // First, push all nodes from the original expression.
    // Since b is empty, we can move the nodes directly without index offsetting.
    let root = b.extend(expr.nodes.drain(..));

    // Collect datacons sorted by id for deterministic output
    let mut datacons: Vec<_> = table.iter().collect();
    datacons.sort_by_key(|dc| dc.id.0);

    let mut body = root;
    let mut wraps = Vec::with_capacity(datacons.len());

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
            wraps.push(DataConWrap {
                let_idx: body,
                tag: dc.id,
                arity,
            });
        } else {
            // Build curried lambda chain: \v0 -> \v1 -> ... -> Con(id, [v0, v1, ...])
            // Fresh vars use a hash of the DataConId to avoid collisions
            let fresh_base = dc
                .id
                .0
                .wrapping_mul(0x517cc1b727220a95)
                .wrapping_add(0xFFFF_0000_0000_0000);
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
            wraps.push(DataConWrap {
                let_idx: body,
                tag: dc.id,
                arity,
            });
        }
    }

    WrappedExpr {
        expr: b.build(),
        wraps,
    }
}
