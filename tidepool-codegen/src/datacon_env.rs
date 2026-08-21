use tidepool_repr::{CoreExpr, CoreFrame, DataConTable, TreeBuilder, VarId};

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
pub fn wrap_with_datacon_env(mut expr: CoreExpr, table: &DataConTable) -> CoreExpr {
    if expr.nodes.is_empty() {
        return expr;
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
        }
    }

    b.build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::{DataConId, RecursiveTree};

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

    /// Walk the top-level `LetNonRec` chain a wrapped tree begins with,
    /// collecting each minted binding's `(binder, arity)` — arity 0 for a
    /// saturated `Con` RHS, N for an N-deep `Lam` chain over a `Con`. Minted
    /// nodes are always pushed AFTER the original fragment's own nodes
    /// (`wrap_with_datacon_env` drains the fragment into a fresh builder
    /// first), so `original_len` — the pre-wrap fragment's node count —
    /// is the boundary: the walk stops as soon as it steps into a node
    /// index below it, i.e. into the original fragment's own tree (which
    /// may itself start with an unrelated `LetNonRec`).
    fn top_level_wraps(expr: &CoreExpr, original_len: usize) -> Vec<(VarId, usize)> {
        let mut out = Vec::new();
        if expr.nodes.is_empty() {
            return out;
        }
        let mut idx = expr.nodes.len() - 1;
        while idx >= original_len {
            let CoreFrame::LetNonRec { binder, rhs, body } = &expr.nodes[idx] else {
                break;
            };
            let mut arity = 0;
            let mut node = &expr.nodes[*rhs];
            while let CoreFrame::Lam { body, .. } = node {
                arity += 1;
                node = &expr.nodes[*body];
            }
            out.push((*binder, arity));
            idx = *body;
        }
        out
    }

    #[test]
    fn wraps_only_referenced_constructor() {
        let table = table_with([make_datacon(1, 0), make_datacon(2, 0), make_datacon(3, 0)]);
        let fragment = var_fragment(2);
        let original_len = fragment.nodes.len();
        let wrapped = wrap_with_datacon_env(fragment, &table);
        let wraps = top_level_wraps(&wrapped, original_len);
        assert_eq!(wraps, vec![(VarId(2), 0)]);
    }

    #[test]
    fn wraps_nothing_when_fragment_references_no_constructor() {
        let table = table_with([make_datacon(1, 0), make_datacon(2, 1)]);
        // Fragment free in an unrelated var, not any constructor binder.
        let fragment = var_fragment(999);
        let original_len = fragment.nodes.len();
        let wrapped = wrap_with_datacon_env(fragment, &table);
        assert!(top_level_wraps(&wrapped, original_len).is_empty());
        // Unchanged in shape: still the single Var node.
        assert_eq!(wrapped.nodes.len(), 1);
        assert_eq!(wrapped.nodes[0], CoreFrame::Var(VarId(999)));
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
        let original_len = fragment.nodes.len();

        let wrapped = wrap_with_datacon_env(fragment, &table);
        let wraps: std::collections::BTreeMap<VarId, usize> =
            top_level_wraps(&wrapped, original_len)
                .into_iter()
                .collect();
        assert_eq!(wraps.len(), 2);
        assert_eq!(wraps[&VarId(1)], 0, "arity-0 con wrapped");
        assert_eq!(wraps[&VarId(2)], 2, "arity-2 con wrapped");
    }

    #[test]
    fn empty_expression_returns_empty_manifest() {
        let table = table_with([make_datacon(1, 0)]);
        let empty = RecursiveTree { nodes: vec![] };
        let wrapped = wrap_with_datacon_env(empty, &table);
        assert!(wrapped.nodes.is_empty());
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
        let original_len = fragment.nodes.len();

        let wrapped = wrap_with_datacon_env(fragment, &table);
        let wraps = top_level_wraps(&wrapped, original_len);
        assert_eq!(wraps.len(), 2);

        let wrapped_binders: std::collections::BTreeSet<VarId> =
            wraps.iter().map(|(binder, _)| *binder).collect();

        let wrapped_fvs = tidepool_repr::free_vars::free_vars(&wrapped);
        let expected: std::collections::BTreeSet<VarId> = fragment_fvs
            .iter()
            .copied()
            .filter(|v| !wrapped_binders.contains(v))
            .collect();
        let actual: std::collections::BTreeSet<VarId> = wrapped_fvs.into_iter().collect();
        assert_eq!(actual, expected);
    }

    /// A constructor's binder id, shadowed by an inner `LetNonRec` in the
    /// fragment itself: `let dc_var = <free var> in dc_var`. Because the
    /// whole fragment IS that let, `VarId(dc.id.0)` is bound at the fragment's
    /// own top level — `free_vars` correctly reports it as NOT free, so the
    /// filter binds no wrapper for it.
    ///
    /// That is the right answer, not a dropped reference. Emission resolves
    /// `Var(VarId(dc.id.0))` in the body against the INNER binder either way,
    /// so an outer wrapper for it would be dead code nothing in the fragment
    /// could ever reach.
    #[test]
    fn shadowed_binder_is_not_wrapped() {
        let dc_id = 7u64;
        let table = table_with([make_datacon(dc_id, 0)]);

        // let VarId(7) = Var(999) in Var(7)  — 999 is an ordinary free var,
        // not any constructor in the table.
        let mut b = TreeBuilder::new();
        let rhs = b.push(CoreFrame::Var(VarId(999)));
        let body = b.push(CoreFrame::Var(VarId(dc_id)));
        b.push(CoreFrame::LetNonRec {
            binder: VarId(dc_id),
            rhs,
            body,
        });
        let fragment = b.build();
        let original_len = fragment.nodes.len();

        let wrapped = wrap_with_datacon_env(fragment, &table);

        assert!(
            top_level_wraps(&wrapped, original_len).is_empty(),
            "the constructor's own binder id is bound by an inner let inside the \
             fragment, so it is not free in the fragment as a whole and must not \
             be wrapped — a wrapper here would be shadowed, dead code, exactly as \
             it was pre-prune"
        );
    }

    /// A constructor mentioned ONLY as a saturated `Con` frame's tag, and only
    /// as a `Case` alternative's `AltCon::DataAlt`, never as a `Var`. Neither
    /// use needs the constructor's function-value wrapper: a saturated `Con`
    /// frame is emitted directly from its tag, and a `DataAlt` is matched
    /// directly against a scrutinee's tag. Both are baked into the frame at
    /// emission time, not resolved through `VarId(dc.id.0)`.
    ///
    /// Guards against widening the filter to also union in `Con` tags and
    /// `DataAlt` ids: that looks like a safety improvement but silently
    /// restores the quadratic behavior this prune exists to avoid, since
    /// nearly every constructor in a real fragment appears as a `Con` tag or
    /// `DataAlt` somewhere.
    #[test]
    fn constructor_used_only_as_con_tag_or_data_alt_is_not_wrapped() {
        let con_tag_id = 30u64;
        let alt_id = 31u64;
        let table = table_with([make_datacon(con_tag_id, 0), make_datacon(alt_id, 0)]);

        // case Var(100) of { DataAlt(alt_id) -> Con { tag: con_tag_id, [] } }
        let mut b = TreeBuilder::new();
        let scrutinee = b.push(CoreFrame::Var(VarId(100)));
        let con_frame = b.push(CoreFrame::Con {
            tag: DataConId(con_tag_id),
            fields: vec![],
        });
        b.push(CoreFrame::Case {
            scrutinee,
            binder: VarId(101),
            alts: vec![tidepool_repr::Alt {
                con: tidepool_repr::AltCon::DataAlt(DataConId(alt_id)),
                binders: vec![],
                body: con_frame,
            }],
        });
        let fragment = b.build();
        let original_len = fragment.nodes.len();

        let wrapped = wrap_with_datacon_env(fragment, &table);

        assert!(
            top_level_wraps(&wrapped, original_len).is_empty(),
            "a constructor referenced only as a Con tag or a DataAlt id must not \
             be wrapped — neither use is a Var reference to VarId(dc.id.0), and \
             widening the filter to catch these would restore most of the \
             quadratic wrapping the prune exists to remove"
        );
    }
}
