//! Core normalization pass: `CoreExpr` → `CoreExpr` canonicalization.
//!
//! Rationale: GHC Core post-optimizer shape varies across compilation modes.
//! Cross-module inlining via `resolveExternals` happens *after* GHC's
//! optimizer would normally collapse certain shapes (e.g. boxed primitive
//! Cons over `Lit(Word, n)`, `case unsafeEqualityProof of UnsafeRefl -> _`,
//! redundant DataCon wrappers). Consumers of [`CoreExpr`] (codegen,
//! `effect_machine`, `heap_bridge`) historically grew ad-hoc peeling
//! branches to handle the unoptimized variants. This pass centralizes
//! that canonicalization so each consumer can assert a single canonical
//! shape via `debug_assert!`.
//!
//! # Properties
//!
//! - **Idempotent.** `normalize(normalize(x)) == normalize(x)` for all `x`.
//! - **Semantics-preserving.** A normalized expression evaluates to the
//!   same value as the input under the JIT's evaluation rules.
//! - **Total.** Every well-formed [`CoreExpr`] has a canonical form.

use crate::frame::CoreFrame;
use crate::tree::MapLayer;
use crate::types::{DataConId, Literal};
use crate::{CoreExpr, DataConTable, RecursiveTree};
use std::collections::HashMap;

/// Canonicalize a [`CoreExpr`] by applying all normalization rules to
/// fixpoint.
///
/// This pass ensures that:
/// 1. Nested boxes of the same type (e.g., `I# (I# x)`) are flattened.
/// 2. Effect tags in `Union` constructors are unboxed literals.
/// 3. Primitive operation arguments are unboxed when they are simple boxed literals.
pub fn normalize(expr: &CoreExpr, table: &DataConTable) -> CoreExpr {
    if expr.nodes.is_empty() {
        return expr.clone();
    }
    let mut current = expr.clone();
    for _ in 0..100 {
        let next = apply_rules_once(&current, table);
        if next == current {
            // Final pass to ensure the tree is "clean" (no unreachable nodes)
            // and canonical.
            return next.extract_subtree(next.nodes.len() - 1);
        }
        current = next;
    }
    debug_assert!(
        false,
        "normalize did not reach fixpoint within 100 iterations"
    );
    current.extract_subtree(current.nodes.len() - 1)
}

fn apply_rules_once(expr: &CoreExpr, table: &DataConTable) -> CoreExpr {
    let mut out = Vec::with_capacity(expr.nodes.len());
    let mut old_to_new = HashMap::new();

    for (old_idx, frame) in expr.nodes.iter().enumerate() {
        let mut mapped = frame
            .clone()
            .map_layer(|child_old| *old_to_new.get(&child_old).expect("bottom-up order"));

        // Rules that transform the node in-place
        transform_unbox_prim_args(&mut mapped, &out, table);
        transform_canonicalize_effect_tag(&mut mapped, &out, table);

        // Rules that collapse the node to an existing index
        let new_idx = if let Some(replacement_idx) = try_flatten_box(&mapped, &out, table) {
            replacement_idx
        } else {
            out.push(mapped);
            out.len() - 1
        };
        old_to_new.insert(old_idx, new_idx);
    }
    RecursiveTree { nodes: out }
}

const BOX_NAMES: &[&str] = &["I#", "W#", "C#", "F#", "D#"];

fn known_box_dataconid(table: &DataConTable, id: DataConId) -> bool {
    table
        .name_of(id)
        .is_some_and(|name| BOX_NAMES.contains(&name))
}

/// Rule 1: flattenBoxRecursion
/// `Con(tag, [Con(tag, [inner])])` -> `Con(tag, [inner])`
fn try_flatten_box(
    frame: &CoreFrame<usize>,
    out: &[CoreFrame<usize>],
    table: &DataConTable,
) -> Option<usize> {
    if let CoreFrame::Con { tag, fields } = frame {
        if fields.len() == 1 && known_box_dataconid(table, *tag) {
            let field_idx = fields[0];
            if let CoreFrame::Con {
                tag: inner_tag,
                fields: inner_fields,
            } = &out[field_idx]
            {
                if inner_tag == tag && inner_fields.len() == 1 {
                    return Some(inner_fields[0]);
                }
            }
        }
    }
    None
}

fn transform_unbox_prim_args(
    frame: &mut CoreFrame<usize>,
    out: &[CoreFrame<usize>],
    table: &DataConTable,
) {
    if let CoreFrame::PrimOp { args, .. } = frame {
        let mut new_args = Vec::with_capacity(args.len());
        let mut all_boxed_lit = true;

        for &arg_idx in args.iter() {
            if let CoreFrame::Con { tag, fields } = &out[arg_idx] {
                if fields.len() == 1 && known_box_dataconid(table, *tag) {
                    let inner_idx = fields[0];
                    if let CoreFrame::Lit(_) = &out[inner_idx] {
                        new_args.push(inner_idx);
                        continue;
                    }
                }
            }
            all_boxed_lit = false;
            break;
        }

        if all_boxed_lit && !args.is_empty() {
            *args = new_args;
        }
    }
}

fn transform_canonicalize_effect_tag(
    frame: &mut CoreFrame<usize>,
    out: &[CoreFrame<usize>],
    table: &DataConTable,
) {
    let union_id = match table.get_by_name("Union") {
        Some(id) => id,
        None => return,
    };
    let w_hash_id = match table.get_by_name("W#") {
        Some(id) => id,
        None => return,
    };

    if let CoreFrame::Con { tag, fields } = frame {
        if *tag == union_id && fields.len() == 2 {
            let tag_field_idx = fields[0];
            if let CoreFrame::Con {
                tag: inner_tag,
                fields: inner_fields,
            } = &out[tag_field_idx]
            {
                if *inner_tag == w_hash_id && inner_fields.len() == 1 {
                    let lit_idx = inner_fields[0];
                    if let CoreFrame::Lit(Literal::LitWord(_)) = &out[lit_idx] {
                        fields[0] = lit_idx;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
fn setup_table() -> DataConTable {
    use crate::datacon::DataCon;
    let mut table = DataConTable::new();
    let box_names = ["I#", "W#", "C#", "F#", "D#"];
    for (i, name) in box_names.iter().enumerate() {
        table.insert(DataCon {
            id: DataConId(i as u64 + 100),
            name: name.to_string(),
            tag: i as u32,
            rep_arity: 1,
            field_bangs: vec![],
            qualified_name: None,
        });
    }
    table.insert(DataCon {
        id: DataConId(200),
        name: "Union".to_string(),
        tag: 0,
        rep_arity: 2,
        field_bangs: vec![],
        qualified_name: None,
    });
    table
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Alt, AltCon, Literal, VarId};
    use crate::{CoreFrame, RecursiveTree};

    fn lit_int_tree(n: i64) -> CoreExpr {
        RecursiveTree {
            nodes: vec![CoreFrame::Lit(Literal::LitInt(n))],
        }
    }

    fn small_program() -> CoreExpr {
        // `case x of { 0# -> 1; _ -> x }`
        RecursiveTree {
            nodes: vec![
                CoreFrame::Var(VarId(1)),           // 0
                CoreFrame::Lit(Literal::LitInt(1)), // 1
                CoreFrame::Var(VarId(1)),           // 2
                CoreFrame::Case {
                    scrutinee: 0,
                    binder: VarId(2),
                    alts: vec![
                        Alt {
                            con: AltCon::LitAlt(Literal::LitInt(0)),
                            binders: vec![],
                            body: 1,
                        },
                        Alt {
                            con: AltCon::Default,
                            binders: vec![],
                            body: 2,
                        },
                    ],
                },
            ],
        }
    }

    #[test]
    fn identity_on_lit() {
        let table = DataConTable::new();
        let expr = lit_int_tree(42);
        assert_eq!(normalize(&expr, &table), expr);
    }

    #[test]
    fn identity_on_small_program() {
        let table = DataConTable::new();
        let expr = small_program();
        assert_eq!(normalize(&expr, &table), expr);
    }

    #[test]
    fn idempotent_on_lit() {
        let table = DataConTable::new();
        let expr = lit_int_tree(42);
        let once = normalize(&expr, &table);
        let twice = normalize(&once, &table);
        assert_eq!(once, twice);
    }

    #[test]
    fn idempotent_on_small_program() {
        let table = DataConTable::new();
        let expr = small_program();
        let once = normalize(&expr, &table);
        let twice = normalize(&once, &table);
        assert_eq!(once, twice);
    }

    #[test]
    fn flatten_nested_int_boxes() {
        let table = setup_table();
        let i_hash = table.get_by_name("I#").unwrap();
        // Con(I#, [Con(I#, [Lit(5)])])
        let expr = RecursiveTree {
            nodes: vec![
                CoreFrame::Lit(Literal::LitInt(5)),
                CoreFrame::Con {
                    tag: i_hash,
                    fields: vec![0],
                },
                CoreFrame::Con {
                    tag: i_hash,
                    fields: vec![1],
                },
            ],
        };
        let normalized = normalize(&expr, &table);
        let expected = RecursiveTree {
            nodes: vec![
                CoreFrame::Lit(Literal::LitInt(5)),
                CoreFrame::Con {
                    tag: i_hash,
                    fields: vec![0],
                },
            ],
        };
        assert_eq!(normalized, expected);
    }

    #[test]
    fn flatten_does_not_touch_different_boxes() {
        let table = setup_table();
        let i_hash = table.get_by_name("I#").unwrap();
        let w_hash = table.get_by_name("W#").unwrap();
        // Con(I#, [Con(W#, [Lit(5)])])
        let expr = RecursiveTree {
            nodes: vec![
                CoreFrame::Lit(Literal::LitWord(5)),
                CoreFrame::Con {
                    tag: w_hash,
                    fields: vec![0],
                },
                CoreFrame::Con {
                    tag: i_hash,
                    fields: vec![1],
                },
            ],
        };
        let normalized = normalize(&expr, &table);
        assert_eq!(normalized, expr);
    }

    #[test]
    fn flatten_leaves_unboxed_alone() {
        let table = setup_table();
        let expr = lit_int_tree(5);
        let normalized = normalize(&expr, &table);
        assert_eq!(normalized, expr);
    }

    #[test]
    fn effect_tag_canonicalized() {
        let table = setup_table();
        let union_id = table.get_by_name("Union").unwrap();
        let w_hash = table.get_by_name("W#").unwrap();
        // Con(Union, [Con(W#, [Lit(7)]), Var(request)])
        let expr = RecursiveTree {
            nodes: vec![
                CoreFrame::Lit(Literal::LitWord(7)),
                CoreFrame::Con {
                    tag: w_hash,
                    fields: vec![0],
                },
                CoreFrame::Var(VarId(10)),
                CoreFrame::Con {
                    tag: union_id,
                    fields: vec![1, 2],
                },
            ],
        };
        let normalized = normalize(&expr, &table);
        // Should become Con(Union, [Lit(7), Var(request)])
        let expected = RecursiveTree {
            nodes: vec![
                CoreFrame::Lit(Literal::LitWord(7)),
                CoreFrame::Var(VarId(10)),
                CoreFrame::Con {
                    tag: union_id,
                    fields: vec![0, 1],
                },
            ],
        };
        assert_eq!(normalized, expected);
    }

    #[test]
    fn effect_tag_already_canonical() {
        let table = setup_table();
        let union_id = table.get_by_name("Union").unwrap();
        // Con(Union, [Lit(7), Var(request)])
        let expr = RecursiveTree {
            nodes: vec![
                CoreFrame::Lit(Literal::LitWord(7)),
                CoreFrame::Var(VarId(10)),
                CoreFrame::Con {
                    tag: union_id,
                    fields: vec![0, 1],
                },
            ],
        };
        let normalized = normalize(&expr, &table);
        assert_eq!(normalized, expr);
    }

    #[test]
    fn prim_args_unboxed_when_all_boxed() {
        let table = setup_table();
        let i_hash = table.get_by_name("I#").unwrap();
        // PrimOp(IntAdd, [Con(I#, [Lit(1)]), Con(I#, [Lit(2)])])
        let expr = RecursiveTree {
            nodes: vec![
                CoreFrame::Lit(Literal::LitInt(1)),
                CoreFrame::Con {
                    tag: i_hash,
                    fields: vec![0],
                },
                CoreFrame::Lit(Literal::LitInt(2)),
                CoreFrame::Con {
                    tag: i_hash,
                    fields: vec![2],
                },
                CoreFrame::PrimOp {
                    op: crate::types::PrimOpKind::IntAdd,
                    args: vec![1, 3],
                },
            ],
        };
        let normalized = normalize(&expr, &table);
        // Should become PrimOp(IntAdd, [Lit(1), Lit(2)])
        let expected = RecursiveTree {
            nodes: vec![
                CoreFrame::Lit(Literal::LitInt(1)),
                CoreFrame::Lit(Literal::LitInt(2)),
                CoreFrame::PrimOp {
                    op: crate::types::PrimOpKind::IntAdd,
                    args: vec![0, 1],
                },
            ],
        };
        assert_eq!(normalized, expected);
    }

    #[test]
    fn prim_args_not_unboxed_when_mixed() {
        let table = setup_table();
        let i_hash = table.get_by_name("I#").unwrap();
        // PrimOp(IntAdd, [Con(I#, [Lit(1)]), Var(x)])
        let expr = RecursiveTree {
            nodes: vec![
                CoreFrame::Lit(Literal::LitInt(1)),
                CoreFrame::Con {
                    tag: i_hash,
                    fields: vec![0],
                },
                CoreFrame::Var(VarId(1)),
                CoreFrame::PrimOp {
                    op: crate::types::PrimOpKind::IntAdd,
                    args: vec![1, 2],
                },
            ],
        };
        let normalized = normalize(&expr, &table);
        assert_eq!(normalized, expr);
    }
}

#[cfg(test)]
mod proptest_normalize {
    use super::*;
    use crate::types::{Literal, PrimOpKind, VarId};
    use proptest::prelude::*;
    use std::collections::HashSet;

    fn arb_literal() -> impl Strategy<Value = Literal> {
        prop_oneof![
            any::<i64>().prop_map(Literal::LitInt),
            any::<u64>().prop_map(Literal::LitWord),
            any::<char>().prop_map(Literal::LitChar),
            any::<f32>().prop_map(|f| Literal::LitFloat(f.to_bits() as u64)),
            any::<f64>().prop_map(|f| Literal::LitDouble(f.to_bits())),
        ]
    }

    fn arb_core_frame(
        child_strategy: impl Strategy<Value = usize> + Clone,
    ) -> impl Strategy<Value = CoreFrame<usize>> {
        prop_oneof![
            any::<u64>().prop_map(|id| CoreFrame::Var(VarId(id))),
            arb_literal().prop_map(CoreFrame::Lit),
            (child_strategy.clone(), child_strategy.clone())
                .prop_map(|(fun, arg)| CoreFrame::App { fun, arg }),
            (any::<u64>(), child_strategy.clone()).prop_map(|(id, body)| CoreFrame::Lam {
                binder: VarId(id),
                body
            }),
            (any::<u64>(), child_strategy.clone(), child_strategy.clone()).prop_map(
                |(id, rhs, body)| CoreFrame::LetNonRec {
                    binder: VarId(id),
                    rhs,
                    body
                }
            ),
            (
                any::<u64>(),
                prop::collection::vec(child_strategy.clone(), 1..3)
            )
                .prop_map(|(tag, fields)| CoreFrame::Con {
                    tag: DataConId(tag),
                    fields
                }),
            (prop::collection::vec(child_strategy, 1..3)).prop_map(|args| CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args
            }), // Simplified ops
        ]
    }

    fn arb_recursive_tree() -> impl Strategy<Value = CoreExpr> {
        prop::collection::vec(arb_core_frame(0usize..100), 1..20).prop_map(|nodes| {
            let mut valid_nodes = Vec::new();
            for (i, node) in nodes.into_iter().enumerate() {
                let mapped = if i == 0 {
                    match node {
                        CoreFrame::Var(_) | CoreFrame::Lit(_) => node,
                        _ => CoreFrame::Lit(Literal::LitInt(0)),
                    }
                } else {
                    node.map_layer(|idx| idx % i)
                };
                valid_nodes.push(mapped);
            }
            RecursiveTree { nodes: valid_nodes }
        })
    }

    fn collect_reachable_vars(expr: &CoreExpr) -> HashSet<VarId> {
        if expr.nodes.is_empty() {
            return HashSet::new();
        }
        let mut vars = HashSet::new();
        let mut visited = HashSet::new();
        let mut stack = vec![expr.nodes.len() - 1];

        while let Some(idx) = stack.pop() {
            if !visited.insert(idx) {
                continue;
            }
            let node = &expr.nodes[idx];
            match node {
                CoreFrame::Var(id) => {
                    vars.insert(*id);
                }
                CoreFrame::Lam { binder, body } => {
                    vars.insert(*binder);
                    stack.push(*body);
                }
                CoreFrame::LetNonRec { binder, rhs, body } => {
                    vars.insert(*binder);
                    stack.push(*rhs);
                    stack.push(*body);
                }
                CoreFrame::LetRec { bindings, body } => {
                    for (id, rhs) in bindings {
                        vars.insert(*id);
                        stack.push(*rhs);
                    }
                    stack.push(*body);
                }
                CoreFrame::Case {
                    scrutinee,
                    binder,
                    alts,
                } => {
                    vars.insert(*binder);
                    stack.push(*scrutinee);
                    for alt in alts {
                        for b in &alt.binders {
                            vars.insert(*b);
                        }
                        stack.push(alt.body);
                    }
                }
                CoreFrame::Join {
                    label: _,
                    params,
                    rhs,
                    body,
                } => {
                    for p in params {
                        vars.insert(*p);
                    }
                    stack.push(*rhs);
                    stack.push(*body);
                }
                CoreFrame::App { fun, arg } => {
                    stack.push(*fun);
                    stack.push(*arg);
                }
                CoreFrame::Con { fields, .. } => {
                    for f in fields {
                        stack.push(*f);
                    }
                }
                CoreFrame::Jump { args, .. } => {
                    for a in args {
                        stack.push(*a);
                    }
                }
                CoreFrame::PrimOp { args, .. } => {
                    for a in args {
                        stack.push(*a);
                    }
                }
                _ => {}
            }
        }
        vars
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_idempotence(expr in arb_recursive_tree()) {
            let table = setup_table();
            let once = normalize(&expr, &table);
            let twice = normalize(&once, &table);
            prop_assert_eq!(once, twice);
        }

        #[test]
        fn prop_structural_preservation(expr in arb_recursive_tree()) {
            let table = setup_table();
            let normalized = normalize(&expr, &table);
            let vars_before = collect_reachable_vars(&expr);
            let vars_after = collect_reachable_vars(&normalized);
            prop_assert_eq!(vars_before, vars_after);
        }

        #[test]
        fn prop_bounded_iteration(expr in arb_recursive_tree()) {
            let table = setup_table();
            let mut current = expr;
            let mut count = 0;
            for _ in 0..100 {
                let next = apply_rules_once(&current, &table);
                if next == current {
                    break;
                }
                current = next;
                count += 1;
            }
            prop_assert!(count < 100);
        }
    }
}
