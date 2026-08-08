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

/// Wrap a CoreExpr with let-bindings for the data constructors it actually
/// references from the table.
///
/// For each referenced DataCon with arity N:
/// - arity 0: `let dc_var = Con(id, []) in ...`
/// - arity 1: `let dc_var = \v0 -> Con(id, [v0]) in ...`
/// - arity 2: `let dc_var = \v0 -> \v1 -> Con(id, [v0, v1]) in ...`
/// - etc.
///
/// The binding VarId matches `VarId(dc.id.0)`, which is what the GHC Core translator
/// uses to reference data constructors as function values.
///
/// Only constructors whose binder is free in `expr` are bound. Every wrapper
/// RHS is a CLOSED term (a saturated `Con` or a lambda chain over its own
/// freshly-minted binders), so no wrapper can reference another constructor —
/// there is no transitive closure to chase over the table, and a single
/// free-variables pass over the incoming fragment is sufficient to decide the
/// full referenced set.
pub fn wrap_with_datacon_env(mut expr: CoreExpr, table: &DataConTable) -> WrappedExpr {
    if expr.nodes.is_empty() {
        return WrappedExpr {
            expr,
            wraps: Vec::new(),
        };
    }
    let fvs = tidepool_repr::free_vars::free_vars(&expr);

    let mut b = TreeBuilder::new();

    // First, push all nodes from the original expression.
    // Since b is empty, we can move the nodes directly without index offsetting.
    let root = b.extend(expr.nodes.drain(..));

    // Collect referenced datacons, sorted by id for deterministic output.
    let mut datacons: Vec<_> = table
        .iter()
        .filter(|dc| fvs.binary_search(&VarId(dc.id.0)).is_ok())
        .collect();
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

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::RecursiveTree;

    fn make_datacon(id: u64, rep_arity: u32) -> tidepool_repr::DataCon {
        tidepool_repr::DataCon {
            id: DataConId(id),
            name: format!("C{id}"),
            tag: 1,
            rep_arity,
            field_bangs: vec![],
            qualified_name: None,
            type_name: String::new(),
        }
    }

    fn table_with(datacons: impl IntoIterator<Item = tidepool_repr::DataCon>) -> DataConTable {
        let mut table = DataConTable::new();
        for dc in datacons {
            table.insert_checked(dc).expect("no collisions in tests");
        }
        table
    }

    /// A fragment that is just `Var(VarId(id))` — free in exactly that var.
    fn var_fragment(id: u64) -> CoreExpr {
        RecursiveTree {
            nodes: vec![CoreFrame::Var(VarId(id))],
        }
    }

    #[test]
    fn wraps_only_referenced_constructor() {
        let table = table_with([make_datacon(1, 0), make_datacon(2, 0), make_datacon(3, 0)]);
        let wrapped = wrap_with_datacon_env(var_fragment(2), &table);
        assert_eq!(wrapped.wraps.len(), 1);
        assert_eq!(wrapped.wraps[0].tag, DataConId(2));
    }

    #[test]
    fn wraps_nothing_when_fragment_references_no_constructor() {
        let table = table_with([make_datacon(1, 0), make_datacon(2, 1)]);
        // Fragment free in an unrelated var, not any constructor binder.
        let wrapped = wrap_with_datacon_env(var_fragment(999), &table);
        assert!(wrapped.wraps.is_empty());
        // Unchanged in shape: still the single Var node.
        assert_eq!(wrapped.expr.nodes.len(), 1);
        assert_eq!(wrapped.expr.nodes[0], CoreFrame::Var(VarId(999)));
    }

    #[test]
    fn wraps_arity_0_and_arity_2_when_referenced() {
        let table = table_with([make_datacon(1, 0), make_datacon(2, 2)]);
        // Fragment referencing both constructor binders: App(Var(1), Var(2)).
        let mut b = TreeBuilder::new();
        let v1 = b.push(CoreFrame::Var(VarId(1)));
        let v2 = b.push(CoreFrame::Var(VarId(2)));
        b.push(CoreFrame::App { fun: v1, arg: v2 });
        let fragment = b.build();

        let wrapped = wrap_with_datacon_env(fragment, &table);
        assert_eq!(wrapped.wraps.len(), 2);

        let w0 = wrapped
            .wraps
            .iter()
            .find(|w| w.tag == DataConId(1))
            .expect("arity-0 con wrapped");
        assert_eq!(w0.arity, 0);
        assert!(matches!(
            wrapped.expr.nodes[w0.let_idx],
            CoreFrame::LetNonRec { binder, .. } if binder == VarId(1)
        ));

        let w2 = wrapped
            .wraps
            .iter()
            .find(|w| w.tag == DataConId(2))
            .expect("arity-2 con wrapped");
        assert_eq!(w2.arity, 2);
        assert!(matches!(
            wrapped.expr.nodes[w2.let_idx],
            CoreFrame::LetNonRec { binder, .. } if binder == VarId(2)
        ));
    }

    #[test]
    fn empty_expression_returns_empty_manifest() {
        let table = table_with([make_datacon(1, 0)]);
        let empty = RecursiveTree { nodes: vec![] };
        let wrapped = wrap_with_datacon_env(empty, &table);
        assert!(wrapped.wraps.is_empty());
        assert!(wrapped.expr.nodes.is_empty());
    }

    /// The equivalence guard: pruning must not orphan a reference. The free
    /// variables of the wrapped tree are exactly the fragment's free variables
    /// minus the binders actually wrapped — nothing referenced was dropped,
    /// and nothing unreferenced leaked in.
    #[test]
    fn wrapping_orphans_no_reference() {
        let table = table_with([
            make_datacon(1, 0),
            make_datacon(2, 1),
            make_datacon(3, 0),
            make_datacon(4, 3),
        ]);
        // Fragment references a subset: constructors 2 and 4, plus a free
        // ordinary variable that is not any constructor binder.
        let mut b = TreeBuilder::new();
        let v2 = b.push(CoreFrame::Var(VarId(2)));
        let v4 = b.push(CoreFrame::Var(VarId(4)));
        let app = b.push(CoreFrame::App { fun: v2, arg: v4 });
        let other = b.push(CoreFrame::Var(VarId(42)));
        b.push(CoreFrame::App {
            fun: app,
            arg: other,
        });
        let fragment = b.build();
        let fragment_fvs = tidepool_repr::free_vars::free_vars(&fragment);

        let wrapped = wrap_with_datacon_env(fragment, &table);
        assert_eq!(wrapped.wraps.len(), 2);

        let wrapped_binders: std::collections::BTreeSet<VarId> =
            wrapped.wraps.iter().map(|w| VarId(w.tag.0)).collect();

        let wrapped_fvs = tidepool_repr::free_vars::free_vars(&wrapped.expr);
        let expected: std::collections::BTreeSet<VarId> = fragment_fvs
            .iter()
            .copied()
            .filter(|v| !wrapped_binders.contains(v))
            .collect();
        let actual: std::collections::BTreeSet<VarId> = wrapped_fvs.into_iter().collect();
        assert_eq!(actual, expected);
    }
}
