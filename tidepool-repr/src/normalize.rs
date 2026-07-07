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
//! - **Semantics-preserving (proptest in tidepool-testing/tests/normalize_semantics.rs).**
//!   A normalized expression evaluates to the same value as the input under
//!   the interpreter's evaluation rules.
//! - **Total.** Every well-formed [`CoreExpr`] has a canonical form.

use crate::frame::CoreFrame;
use crate::tree::MapLayer;
use crate::types::{DataConId, Literal, VarId};
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
        let (next, root_idx) = apply_rules_once(&current, table);
        if next == current {
            // Final pass to ensure the tree is "clean" (no unreachable nodes)
            // and canonical.
            return next.extract_subtree(root_idx);
        }
        current = next;
    }
    debug_assert!(
        false,
        "normalize did not reach fixpoint within 100 iterations"
    );
    // In case of timeout, we still need to return a valid tree.
    // We'll re-run apply_rules_once one last time to get the root_idx.
    let (final_tree, root_idx) = apply_rules_once(&current, table);
    final_tree.extract_subtree(root_idx)
}

fn apply_rules_once(expr: &CoreExpr, table: &DataConTable) -> (CoreExpr, usize) {
    let mut out = Vec::with_capacity(expr.nodes.len());
    let mut old_to_new: Vec<usize> = Vec::with_capacity(expr.nodes.len());

    // Scope-respecting bindings: `scoped_bindings[old_idx]` is the
    // Let/LetRec binder -> rhs map actually visible AT that node, computed by
    // a top-down (root-first) walk so an inner binder — or an opaque
    // Lam/Case/Join binder with no resolvable rhs — correctly shadows an
    // outer same-named one, rather than the last binding encountered in
    // array order winning irrespective of scope. See `collect_scoped_bindings`.
    let scoped_bindings = collect_scoped_bindings(expr);

    for (old_idx, frame) in expr.nodes.iter().enumerate() {
        let mut mapped = frame.clone().map_layer(|child_old| old_to_new[child_old]);
        let var_map = &scoped_bindings[old_idx];

        // Rules that transform the node in-place
        transform_unbox_prim_args(&mut mapped, &out, table, var_map, &old_to_new);
        transform_canonicalize_effect_tag(&mut mapped, &out, table, var_map, &old_to_new);

        // Rules that collapse the node to an existing index
        let new_idx = if let Some(replacement_idx) = try_flatten_box(&mapped, &out, table) {
            replacement_idx
        } else {
            out.push(mapped.clone());
            out.len() - 1
        };

        debug_assert_eq!(old_to_new.len(), old_idx);
        old_to_new.push(new_idx);
    }
    let last_mapped_idx = *old_to_new
        .last()
        .expect("normalize: old_to_new is non-empty (input RecursiveTree had ≥1 node)");
    (RecursiveTree { nodes: out }, last_mapped_idx)
}

/// Build, for every node index, the `Let`/`LetRec` binder -> rhs bindings
/// that are actually in lexical scope there.
///
/// A single top-down (root-first) walk, threading an environment that is
/// cloned-and-extended on entry to a binding scope — mirroring how the
/// shadow-aware passes (`PartialEval`, `subst`) thread their envs — so an
/// inner binder correctly shadows an outer same-named one. `Lam`/`Case`-alt/
/// `Join`-param binders have no resolvable rhs, but still MASK (remove) any
/// outer entry for the same `VarId` while inside their scope, so a variable
/// shadowed by one of those opaque binders is never incorrectly looked
/// through to an unrelated outer `Let`'s rhs.
///
/// Explicit-stack (not recursive), matching this crate's convention for
/// whole-tree walks (see `tree.rs`'s `extract_subtree`/`replace_subtree`).
/// DAG-shared nodes are visited once, via whichever parent path reaches them
/// first — acceptable because a node's meaning only depends on the SAME free
/// variable being bound consistently across the paths that share it.
fn collect_scoped_bindings(expr: &CoreExpr) -> Vec<HashMap<VarId, usize>> {
    let len = expr.nodes.len();
    let mut result = vec![HashMap::new(); len];
    if len == 0 {
        return result;
    }
    let root = len - 1;
    let mut visited = vec![false; len];
    let mut stack = vec![(root, HashMap::new())];
    while let Some((idx, env)) = stack.pop() {
        if visited[idx] {
            continue;
        }
        visited[idx] = true;
        result[idx] = env.clone();
        match &expr.nodes[idx] {
            CoreFrame::Var(_) | CoreFrame::Lit(_) => {}
            CoreFrame::App { fun, arg } => {
                stack.push((*fun, env.clone()));
                stack.push((*arg, env));
            }
            CoreFrame::Lam { binder, body } => {
                let mut body_env = env;
                body_env.remove(binder);
                stack.push((*body, body_env));
            }
            CoreFrame::LetNonRec { binder, rhs, body } => {
                // Non-recursive: rhs does NOT see its own binder.
                stack.push((*rhs, env.clone()));
                let mut body_env = env;
                body_env.insert(*binder, *rhs);
                stack.push((*body, body_env));
            }
            CoreFrame::LetRec { bindings, body } => {
                let mut rec_env = env;
                for (b, r) in bindings {
                    rec_env.insert(*b, *r);
                }
                for (_, r) in bindings {
                    stack.push((*r, rec_env.clone()));
                }
                stack.push((*body, rec_env));
            }
            CoreFrame::Case {
                scrutinee,
                binder,
                alts,
            } => {
                stack.push((*scrutinee, env.clone()));
                for alt in alts {
                    let mut alt_env = env.clone();
                    alt_env.remove(binder);
                    for b in &alt.binders {
                        alt_env.remove(b);
                    }
                    stack.push((alt.body, alt_env));
                }
            }
            CoreFrame::Con { fields, .. } => {
                for &f in fields {
                    stack.push((f, env.clone()));
                }
            }
            CoreFrame::Join {
                params, rhs, body, ..
            } => {
                // Join params scope over rhs only, mirroring how the other
                // shadow-aware passes treat join points.
                let mut rhs_env = env.clone();
                for p in params {
                    rhs_env.remove(p);
                }
                stack.push((*rhs, rhs_env));
                stack.push((*body, env));
            }
            CoreFrame::Jump { args, .. } => {
                for &a in args {
                    stack.push((a, env.clone()));
                }
            }
            CoreFrame::PrimOp { args, .. } => {
                for &a in args {
                    stack.push((a, env.clone()));
                }
            }
        }
    }
    result
}

const BOX_NAMES: &[&str] = &["I#", "W#", "C#", "F#", "D#"];

fn known_box_dataconid(table: &DataConTable, id: DataConId) -> bool {
    table
        .name_of(id)
        .is_some_and(|name| BOX_NAMES.contains(&name))
}

/// Resolves a Var node through its binding if possible.
/// Returns the index in the `out` vector of the actual expression.
fn resolve_var(
    idx: usize,
    out: &[CoreFrame<usize>],
    var_map: &HashMap<crate::VarId, usize>,
    old_to_new: &[usize],
) -> usize {
    let mut current_idx = idx;
    let mut fuel = 10;
    while fuel > 0 {
        if let CoreFrame::Var(id) = &out[current_idx] {
            if let Some(&rhs_old_idx) = var_map.get(id) {
                if rhs_old_idx < old_to_new.len() {
                    current_idx = old_to_new[rhs_old_idx];
                    fuel -= 1;
                    continue;
                }
            }
        }
        break;
    }
    current_idx
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
                    // Flattening `Con(tag, [Con(tag, [inner])])` to `Con(tag, [inner])`.
                    // The inner `Con` already has the correct shape and is at `field_idx`.
                    return Some(field_idx);
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
    var_map: &HashMap<crate::VarId, usize>,
    old_to_new: &[usize],
) {
    if let CoreFrame::PrimOp { args, .. } = frame {
        let mut new_args = Vec::with_capacity(args.len());
        let mut all_boxed_lit = true;

        for &arg_idx in args.iter() {
            let resolved_idx = resolve_var(arg_idx, out, var_map, old_to_new);
            if let CoreFrame::Con { tag, fields } = &out[resolved_idx] {
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
    var_map: &HashMap<crate::VarId, usize>,
    old_to_new: &[usize],
) {
    // F2: resolve qualified-name-first (falling back to the unqualified name
    // only when no qualified entry exists), mirroring `machine.rs`/
    // `effect_machine.rs`'s oracle/JIT resolution — NOT `get_by_name_arity`,
    // which returns the LAST-INSERTED match on ambiguity. A user program
    // defining its own `Union` (rep_arity 2) would otherwise silently become
    // the "last-inserted" match, so this pass would canonicalize the USER's
    // `Con` (splicing a raw `LitWord` where codegen expects a boxed field)
    // and skip the real freer `Union` entirely — a production-path bug only
    // `debug_assert`ed downstream (silent in release). `resolve` doesn't
    // filter by arity, so the `rep_arity == 2` check below still applies to
    // whatever it finds (a qualified match at the wrong arity is not our
    // `Union`, and falls through to the `None` early-return exactly as
    // `get_by_name_arity` would have).
    let union_id = match crate::freer_names::resolve(
        table,
        crate::freer_names::UNION_QUALIFIED,
        crate::freer_names::UNION,
    )
    .filter(|id| table.get(*id).is_some_and(|dc| dc.rep_arity == 2))
    {
        Some(id) => id,
        None => return,
    };
    let w_hash_id = match table.get_by_name_arity("W#", 1) {
        Some(id) => id,
        None => return,
    };

    if let CoreFrame::Con { tag, fields } = frame {
        if *tag == union_id && fields.len() == 2 {
            let resolved_idx = resolve_var(fields[0], out, var_map, old_to_new);

            match &out[resolved_idx] {
                // Rule 2: Unbox boxed effect tag: Union(W#(x)) -> Union(x)
                CoreFrame::Con {
                    tag: inner_tag,
                    fields: inner_fields,
                } if *inner_tag == w_hash_id && inner_fields.len() == 1 => {
                    let lit_resolved_idx = resolve_var(inner_fields[0], out, var_map, old_to_new);
                    if let CoreFrame::Lit(Literal::LitWord(_)) = &out[lit_resolved_idx] {
                        fields[0] = lit_resolved_idx;
                    }
                }
                // Also handle the case where the tag field is already a LitWord
                // but potentially hidden behind a Var.
                CoreFrame::Lit(Literal::LitWord(_)) => {
                    fields[0] = resolved_idx;
                }
                _ => {}
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
    fn flatten_nested_boxes_as_child() {
        let table = setup_table();
        let i_hash = table.get_by_name("I#").unwrap();
        // App(f, I# (I# 5))
        let expr = RecursiveTree {
            nodes: vec![
                CoreFrame::Lit(Literal::LitInt(5)), // 0
                CoreFrame::Con {
                    tag: i_hash,
                    fields: vec![0],
                }, // 1
                CoreFrame::Con {
                    tag: i_hash,
                    fields: vec![1],
                }, // 2
                CoreFrame::Var(VarId(1)),           // 3
                CoreFrame::App { fun: 3, arg: 2 },  // 4
            ],
        };
        let normalized = normalize(&expr, &table);
        // Should become App(f, I# 5)
        let expected_raw = RecursiveTree {
            nodes: vec![
                CoreFrame::Lit(Literal::LitInt(5)), // 0
                CoreFrame::Con {
                    tag: i_hash,
                    fields: vec![0],
                }, // 1
                CoreFrame::Var(VarId(1)),           // 2
                CoreFrame::App { fun: 2, arg: 1 },  // 3
            ],
        };
        // Canonicalize expected tree order by extracting from its root
        let expected = expected_raw.extract_subtree(3);
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
        let expected_raw = RecursiveTree {
            nodes: vec![
                CoreFrame::Lit(Literal::LitWord(7)),
                CoreFrame::Var(VarId(10)),
                CoreFrame::Con {
                    tag: union_id,
                    fields: vec![0, 1],
                },
            ],
        };
        let expected = expected_raw.extract_subtree(2);
        assert_eq!(normalized, expected);
    }

    #[test]
    fn effect_tag_canonicalized_through_var() {
        let table = setup_table();
        let union_id = table.get_by_name("Union").unwrap();
        let w_hash = table.get_by_name("W#").unwrap();
        // let x = W# 7 in Con(Union, [x, Var(request)])
        let expr = RecursiveTree {
            nodes: vec![
                CoreFrame::Lit(Literal::LitWord(7)), // 0
                CoreFrame::Con {
                    tag: w_hash,
                    fields: vec![0],
                }, // 1
                CoreFrame::Var(VarId(10)),           // 2
                CoreFrame::Con {
                    tag: union_id,
                    fields: vec![1, 2],
                }, // 3
                CoreFrame::LetNonRec {
                    binder: VarId(1),
                    rhs: 1,
                    body: 3,
                }, // 4
            ],
        };
        let normalized = normalize(&expr, &table);
        // Should become let x = W# 7 in Con(Union, [0, 2])
        let expected_raw = RecursiveTree {
            nodes: vec![
                CoreFrame::Lit(Literal::LitWord(7)), // 0
                CoreFrame::Con {
                    tag: w_hash,
                    fields: vec![0],
                }, // 1
                CoreFrame::Var(VarId(10)),           // 2
                CoreFrame::Con {
                    tag: union_id,
                    fields: vec![0, 2],
                }, // 3
                CoreFrame::LetNonRec {
                    binder: VarId(1),
                    rhs: 1,
                    body: 3,
                }, // 4
            ],
        };
        let expected = expected_raw.extract_subtree(4);
        assert_eq!(normalized, expected);
    }

    #[test]
    fn effect_tag_canonicalized_nested_var() {
        let table = setup_table();
        let union_id = table.get_by_name("Union").unwrap();
        let w_hash = table.get_by_name("W#").unwrap();
        // let y = 7
        // let x = W# y
        // Union x req
        let expr = RecursiveTree {
            nodes: vec![
                CoreFrame::Lit(Literal::LitWord(7)), // 0
                CoreFrame::Var(VarId(20)),           // 1: dummy
                CoreFrame::Var(VarId(2)),            // 2: y (reference)
                CoreFrame::Con {
                    tag: w_hash,
                    fields: vec![2],
                }, // 3: W# y
                CoreFrame::Var(VarId(1)),            // 4: x (reference)
                CoreFrame::Con {
                    tag: union_id,
                    fields: vec![4, 1],
                }, // 5: Union x dummy
                CoreFrame::LetNonRec {
                    binder: VarId(1),
                    rhs: 3,
                    body: 5,
                }, // 6: let x = W# y in Union x dummy
                CoreFrame::LetNonRec {
                    binder: VarId(2),
                    rhs: 0,
                    body: 6,
                }, // 7: let y = 7 in ...
            ],
        };
        let normalized = normalize(&expr, &table);
        // extraction should find that Union's tag field resolves to Lit(7)
        let expected_raw = RecursiveTree {
            nodes: vec![
                CoreFrame::Lit(Literal::LitWord(7)), // 0
                CoreFrame::Var(VarId(20)),           // 1
                CoreFrame::Var(VarId(2)),            // 2
                CoreFrame::Con {
                    tag: w_hash,
                    fields: vec![2],
                }, // 3
                CoreFrame::Con {
                    tag: union_id,
                    fields: vec![0, 1],
                }, // 4
                CoreFrame::LetNonRec {
                    binder: VarId(1),
                    rhs: 3,
                    body: 4,
                }, // 5
                CoreFrame::LetNonRec {
                    binder: VarId(2),
                    rhs: 0,
                    body: 5,
                }, // 6
            ],
        };
        let expected = expected_raw.extract_subtree(6);
        assert_eq!(normalized, expected);
    }

    /// F2 regression: a user `Union` (rep_arity 2), inserted AFTER the real
    /// freer `Union`, must NOT hijack effect-tag canonicalization.
    /// `get_by_name_arity("Union", 2)` returns the LAST-inserted match — the
    /// user's — so pre-fix this pass unboxed the user's harmless field while
    /// leaving the real freer `Union`'s tag boxed (the production bug: codegen
    /// expects that tag as a raw `LitWord`). Post-fix, `freer_names::resolve`
    /// picks the freer `Union` via its qualified name regardless of insertion
    /// order.
    #[test]
    fn effect_tag_resolves_qualified_freer_union_not_last_inserted_user_union() {
        use crate::datacon::DataCon;
        let mut table = DataConTable::new();
        table.insert(DataCon {
            id: DataConId(101),
            name: "W#".to_string(),
            tag: 0,
            rep_arity: 1,
            field_bangs: vec![],
            qualified_name: None,
        });
        let w_hash = DataConId(101);

        // The real freer Union, inserted FIRST.
        let freer_union_id = DataConId(200);
        table.insert(DataCon {
            id: freer_union_id,
            name: "Union".to_string(),
            tag: 0,
            rep_arity: 2,
            field_bangs: vec![],
            qualified_name: Some(crate::freer_names::UNION_QUALIFIED.to_string()),
        });

        // A user's own `data Union a b = Union a b`, inserted AFTER — same
        // bare name and arity, distinct qualified name.
        let user_union_id = DataConId(300);
        table.insert(DataCon {
            id: user_union_id,
            name: "Union".to_string(),
            tag: 1,
            rep_arity: 2,
            field_bangs: vec![],
            qualified_name: Some("MyMod.Union".to_string()),
        });

        // Sanity: this IS the ambiguity the fix must route around.
        assert_eq!(
            table.get_by_name_arity("Union", 2),
            Some(user_union_id),
            "get_by_name_arity returns the last-inserted match — the bug this test guards against"
        );

        let expr = RecursiveTree {
            nodes: vec![
                CoreFrame::Lit(Literal::LitWord(7)), // 0: freer tag literal
                CoreFrame::Con {
                    tag: w_hash,
                    fields: vec![0],
                }, // 1: W# 7 (freer's boxed tag)
                CoreFrame::Var(VarId(10)),           // 2: freer request
                CoreFrame::Con {
                    tag: freer_union_id,
                    fields: vec![1, 2],
                }, // 3: the real freer Union
                CoreFrame::Lit(Literal::LitWord(9)), // 4: user field literal
                CoreFrame::Con {
                    tag: w_hash,
                    fields: vec![4],
                }, // 5: W# 9 (user's own boxed field — NOT an effect tag)
                CoreFrame::Var(VarId(20)),           // 6: user's second field
                CoreFrame::Con {
                    tag: user_union_id,
                    fields: vec![5, 6],
                }, // 7: the user's own Union
                CoreFrame::App { fun: 3, arg: 7 },   // 8: keep both roots reachable
            ],
        };
        let normalized = normalize(&expr, &table);

        let freer_fields = normalized
            .nodes
            .iter()
            .find_map(|n| match n {
                CoreFrame::Con { tag, fields } if *tag == freer_union_id => Some(fields.clone()),
                _ => None,
            })
            .expect("freer Union Con survives normalization");
        let user_fields = normalized
            .nodes
            .iter()
            .find_map(|n| match n {
                CoreFrame::Con { tag, fields } if *tag == user_union_id => Some(fields.clone()),
                _ => None,
            })
            .expect("user Union Con survives normalization");

        assert!(
            matches!(
                normalized.nodes[freer_fields[0]],
                CoreFrame::Lit(Literal::LitWord(7))
            ),
            "the real freer Union's tag field must be canonicalized to a raw Lit"
        );
        assert!(
            matches!(
                &normalized.nodes[user_fields[0]],
                CoreFrame::Con { tag, .. } if *tag == w_hash
            ),
            "the user's own Union field must stay boxed — it is not an effect tag"
        );
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
        let expected_raw = RecursiveTree {
            nodes: vec![
                CoreFrame::Lit(Literal::LitInt(1)),
                CoreFrame::Lit(Literal::LitInt(2)),
                CoreFrame::PrimOp {
                    op: crate::types::PrimOpKind::IntAdd,
                    args: vec![0, 1],
                },
            ],
        };
        let expected = expected_raw.extract_subtree(2);
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

    /// F3 regression: `let x = I# 1 in \x -> IntAdd(x, x)` — the lambda's own
    /// `x` shadows the outer let's `x`. Before scoping the var_map, a global
    /// last-wins table (or any table not aware of the Lam binder masking the
    /// outer entry) would resolve the lambda-bound `x` THROUGH the outer
    /// let's rhs and incorrectly unbox both args to `Lit(1)` — even though
    /// the lambda's `x` is an opaque runtime parameter, not statically 1.
    #[test]
    fn prim_args_not_unboxed_through_lam_shadowed_var() {
        let table = setup_table();
        let i_hash = table.get_by_name("I#").unwrap();
        let x = VarId(1);
        // let x = I# 1 in \x -> PrimOp(IntAdd, [x, x])
        let expr = RecursiveTree {
            nodes: vec![
                CoreFrame::Lit(Literal::LitInt(1)), // 0
                CoreFrame::Con {
                    tag: i_hash,
                    fields: vec![0],
                }, // 1: I# 1 (outer rhs)
                CoreFrame::Var(x),                  // 2
                CoreFrame::Var(x),                  // 3
                CoreFrame::PrimOp {
                    op: crate::types::PrimOpKind::IntAdd,
                    args: vec![2, 3],
                }, // 4
                CoreFrame::Lam { binder: x, body: 4 }, // 5
                CoreFrame::LetNonRec {
                    binder: x,
                    rhs: 1,
                    body: 5,
                }, // 6
            ],
        };
        let normalized = normalize(&expr, &table);
        // The lambda-bound `x` must stay opaque: no rule can fire (the
        // shadowed var must never resolve through the outer let's rhs), so
        // normalize is identity here.
        assert_eq!(normalized, expr);
    }

    /// F3 regression: nested `LetNonRec`s reusing the SAME `VarId` (the
    /// duplicate-binder-id shadowing class `subst`'s DAG-sharing can produce).
    /// A reference in the inner scope must resolve through the INNER
    /// binding, not whichever binding a plain array-order scan visits last.
    #[test]
    fn effect_tag_var_map_respects_duplicate_binder_shadowing() {
        let table = setup_table();
        let union_id = table.get_by_name("Union").unwrap();
        let w_hash = table.get_by_name("W#").unwrap();
        let x = VarId(1);
        // let x = W# 7 in let x = W# 9 in Con(Union, [x, request])
        let expr = RecursiveTree {
            nodes: vec![
                CoreFrame::Lit(Literal::LitWord(7)), // 0
                CoreFrame::Con {
                    tag: w_hash,
                    fields: vec![0],
                }, // 1: W# 7 (outer rhs)
                CoreFrame::Lit(Literal::LitWord(9)), // 2
                CoreFrame::Con {
                    tag: w_hash,
                    fields: vec![2],
                }, // 3: W# 9 (inner rhs)
                CoreFrame::Var(x),                   // 4: x (reference, inner scope)
                CoreFrame::Var(VarId(10)),           // 5: request
                CoreFrame::Con {
                    tag: union_id,
                    fields: vec![4, 5],
                }, // 6: Union x request
                CoreFrame::LetNonRec {
                    binder: x,
                    rhs: 3,
                    body: 6,
                }, // 7: let x = W# 9 in ...
                CoreFrame::LetNonRec {
                    binder: x,
                    rhs: 1,
                    body: 7,
                }, // 8: let x = W# 7 in ...
            ],
        };
        let normalized = normalize(&expr, &table);
        // The Union's tag field resolves through the INNER let (rhs = W# 9,
        // never the outer W# 7); both `LetNonRec` wrappers and both `W#`
        // Cons stay in the tree (each Let's own `rhs` edge keeps its Con
        // reachable) — only the now-unreferenced `Var(x)` is pruned.
        let expected_raw = RecursiveTree {
            nodes: vec![
                CoreFrame::Lit(Literal::LitWord(7)), // 0
                CoreFrame::Con {
                    tag: w_hash,
                    fields: vec![0],
                }, // 1
                CoreFrame::Lit(Literal::LitWord(9)), // 2
                CoreFrame::Con {
                    tag: w_hash,
                    fields: vec![2],
                }, // 3
                CoreFrame::Var(VarId(10)),           // 4: request
                CoreFrame::Con {
                    tag: union_id,
                    fields: vec![2, 4],
                }, // 5: Union, tag redirected to Lit(9) directly
                CoreFrame::LetNonRec {
                    binder: x,
                    rhs: 3,
                    body: 5,
                }, // 6: inner let
                CoreFrame::LetNonRec {
                    binder: x,
                    rhs: 1,
                    body: 6,
                }, // 7: outer let
            ],
        };
        let expected = expected_raw.extract_subtree(7);
        assert_eq!(normalized, expected);
    }
}

#[cfg(test)]
mod proptest_normalize {
    use super::*;
    use crate::types::{Literal, PrimOpKind, VarId};
    use proptest::prelude::*;

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
        let box_ids = prop_oneof![
            Just(DataConId(100)), // I#
            Just(DataConId(101)), // W#
            Just(DataConId(102)), // C#
            Just(DataConId(103)), // F#
            Just(DataConId(104)), // D#
            Just(DataConId(200)), // Union
        ];
        let arb_dataconid = prop_oneof![
            7 => box_ids,
            3 => any::<u64>().prop_map(DataConId),
        ];

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
                arb_dataconid,
                prop::collection::vec(child_strategy.clone(), 1..3)
            )
                .prop_map(|(tag, fields)| CoreFrame::Con { tag, fields }),
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
        fn prop_bounded_iteration(expr in arb_recursive_tree()) {
            let table = setup_table();
            let mut current = expr;
            let mut count = 0;
            for _ in 0..100 {
                let (next, _) = apply_rules_once(&current, &table);
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
