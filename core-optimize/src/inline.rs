use core_eval::{Changed, Pass};
use core_repr::{CoreExpr, CoreFrame, MapLayer};
use crate::occ::{occ_analysis, get_occ, Occ};
use std::collections::HashMap;

/// Inlining pass: eliminates single-use `LetNonRec` bindings by substituting the RHS directly at the use site.
pub struct Inline;

impl Pass for Inline {
    fn run(&self, expr: &mut CoreExpr) -> Changed {
        if expr.nodes.is_empty() {
            return false;
        }
        let occ_map = occ_analysis(expr);
        match try_inline(expr, &occ_map) {
            Some(new_expr) => {
                *expr = new_expr;
                true
            }
            None => false,
        }
    }

    fn name(&self) -> &str {
        "Inline"
    }
}

fn try_inline(expr: &CoreExpr, occ_map: &crate::occ::OccMap) -> Option<CoreExpr> {
    try_inline_at(expr, expr.nodes.len() - 1, occ_map)
}

fn try_inline_at(expr: &CoreExpr, idx: usize, occ_map: &crate::occ::OccMap) -> Option<CoreExpr> {
    match &expr.nodes[idx] {
        CoreFrame::LetNonRec { binder, rhs, body } => {
            if get_occ(occ_map, *binder) == Occ::Once {
                // Inline: substitute binder -> rhs in body
                let body_tree = expr.extract_subtree(*body);
                let rhs_tree = expr.extract_subtree(*rhs);
                let inlined = core_repr::subst::subst(&body_tree, *binder, &rhs_tree);
                Some(replace_subtree(expr, idx, &inlined))
            } else {
                // Try children
                try_inline_at(expr, *rhs, occ_map)
                    .or_else(|| try_inline_at(expr, *body, occ_map))
            }
        }
        // Never inline LetRec, even if Once (it might be recursive via own RHS)
        _ => try_children(expr, idx, occ_map),
    }
}

fn try_children(expr: &CoreExpr, idx: usize, occ_map: &crate::occ::OccMap) -> Option<CoreExpr> {
    let children = get_children(&expr.nodes[idx]);
    for child in children {
        if let Some(result) = try_inline_at(expr, child, occ_map) {
            return Some(result);
        }
    }
    None
}

fn get_children(frame: &CoreFrame<usize>) -> Vec<usize> {
    match frame {
        CoreFrame::Var(_) | CoreFrame::Lit(_) => vec![],
        CoreFrame::App { fun, arg } => vec![*fun, *arg],
        CoreFrame::Lam { body, .. } => vec![*body],
        CoreFrame::LetNonRec { rhs, body, .. } => vec![*rhs, *body],
        CoreFrame::LetRec { bindings, body, .. } => {
            let mut c: Vec<usize> = bindings.iter().map(|(_, r)| *r).collect();
            c.push(*body);
            c
        }
        CoreFrame::Case {
            scrutinee, alts, ..
        } => {
            let mut c = vec![*scrutinee];
            for alt in alts {
                c.push(alt.body);
            }
            c
        }
        CoreFrame::Con { fields, .. } => fields.clone(),
        CoreFrame::Join { rhs, body, .. } => vec![*rhs, *body],
        CoreFrame::Jump { args, .. } => args.clone(),
        CoreFrame::PrimOp { args, .. } => args.clone(),
    }
}

fn replace_subtree(expr: &CoreExpr, target_idx: usize, replacement: &CoreExpr) -> CoreExpr {
    let mut new_nodes = Vec::new();
    let mut old_to_new = HashMap::new();
    rebuild(
        expr,
        expr.nodes.len() - 1,
        target_idx,
        replacement,
        &mut new_nodes,
        &mut old_to_new,
    );
    CoreExpr { nodes: new_nodes }
}

fn rebuild(
    expr: &CoreExpr,
    idx: usize,
    target: usize,
    replacement: &CoreExpr,
    new_nodes: &mut Vec<CoreFrame<usize>>,
    old_to_new: &mut HashMap<usize, usize>,
) -> usize {
    if let Some(&ni) = old_to_new.get(&idx) {
        return ni;
    }
    if idx == target {
        let offset = new_nodes.len();
        for node in &replacement.nodes {
            new_nodes.push(node.clone().map_layer(|i| i + offset));
        }
        let root = new_nodes.len() - 1;
        old_to_new.insert(idx, root);
        return root;
    }
    let mapped = expr.nodes[idx].clone().map_layer(|child| {
        rebuild(expr, child, target, replacement, new_nodes, old_to_new)
    });
    let new_idx = new_nodes.len();
    new_nodes.push(mapped);
    old_to_new.insert(idx, new_idx);
    new_idx
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_eval::{eval, Env, VecHeap};
    use core_repr::{Literal, VarId, PrimOpKind};

    fn tree(nodes: Vec<CoreFrame<usize>>) -> CoreExpr {
        CoreExpr { nodes }
    }

    // 1. let x = 42 in x -> 42. Binder Once, inlined.
    #[test]
    fn test_inline_single_use() {
        let x = VarId(1);
        let mut expr = tree(vec![
            CoreFrame::Lit(Literal::LitInt(42)),            // 0
            CoreFrame::Var(x),                              // 1
            CoreFrame::LetNonRec { binder: x, rhs: 0, body: 1 }, // 2
        ]);
        let pass = Inline;
        let changed = pass.run(&mut expr);
        assert!(changed);
        assert_eq!(expr.nodes.len(), 1);
        assert_eq!(expr.nodes[0], CoreFrame::Lit(Literal::LitInt(42)));
    }

    // 2. let x = 42 in x + x -> unchanged. Binder Many, not inlined.
    #[test]
    fn test_inline_multi_use_preserved() {
        let x = VarId(1);
        let mut expr = tree(vec![
            CoreFrame::Lit(Literal::LitInt(42)), // 0
            CoreFrame::Var(x),                   // 1
            CoreFrame::Var(x),                   // 2
            CoreFrame::PrimOp { op: PrimOpKind::IntAdd, args: vec![1, 2] }, // 3
            CoreFrame::LetNonRec { binder: x, rhs: 0, body: 3 },            // 4
        ]);
        let pass = Inline;
        let changed = pass.run(&mut expr);
        assert!(!changed);
    }

    // 3. let x = 42 in 0 -> unchanged by inline (DCE will handle dead bindings).
    #[test]
    fn test_inline_dead_preserved() {
        let x = VarId(1);
        let mut expr = tree(vec![
            CoreFrame::Lit(Literal::LitInt(42)), // 0
            CoreFrame::Lit(Literal::LitInt(0)),  // 1
            CoreFrame::LetNonRec { binder: x, rhs: 0, body: 1 }, // 2
        ]);
        let pass = Inline;
        let changed = pass.run(&mut expr);
        assert!(!changed);
    }

    // 4. let x = 1 in let y = x in y -> after two passes: 1.
    #[test]
    fn test_inline_nested() {
        let x = VarId(1);
        let y = VarId(2);
        let mut expr = tree(vec![
            CoreFrame::Lit(Literal::LitInt(1)),                 // 0
            CoreFrame::Var(x),                                   // 1
            CoreFrame::Var(y),                                   // 2
            CoreFrame::LetNonRec { binder: y, rhs: 1, body: 2 }, // 3
            CoreFrame::LetNonRec { binder: x, rhs: 0, body: 3 }, // 4
        ]);
        let pass = Inline;
        
        // Pass 1: inline y = x
        assert!(pass.run(&mut expr));
        // Pass 2: inline x = 1
        assert!(pass.run(&mut expr));
        // Result should be 1
        assert_eq!(expr.nodes.len(), 1);
        assert_eq!(expr.nodes[0], CoreFrame::Lit(Literal::LitInt(1)));
    }

    // 5. letrec f = f in f -> unchanged. LetRec binder Once but must NOT inline.
    #[test]
    fn test_inline_letrec_not_inlined() {
        let f = VarId(1);
        let mut expr = tree(vec![
            CoreFrame::Var(f), // 0
            CoreFrame::Var(f), // 1
            CoreFrame::LetRec { bindings: vec![(f, 0)], body: 1 }, // 2
        ]);
        let pass = Inline;
        let changed = pass.run(&mut expr);
        assert!(!changed);
    }

    // 6. let x = y in \y. x -> \y'. y (fresh y').
    #[test]
    fn test_inline_capture_avoiding() {
        let x = VarId(1);
        let y = VarId(2);
        let mut expr = tree(vec![
            CoreFrame::Var(y),                                  // 0: rhs
            CoreFrame::Var(x),                                  // 1
            CoreFrame::Lam { binder: y, body: 1 },              // 2: body
            CoreFrame::LetNonRec { binder: x, rhs: 0, body: 2 }, // 3
        ]);
        let pass = Inline;
        let changed = pass.run(&mut expr);
        assert!(changed);
        
        // Result should be \y'. y
        let root = expr.nodes.len() - 1;
        if let CoreFrame::Lam { binder, body } = &expr.nodes[root] {
            assert_ne!(*binder, y);
            if let CoreFrame::Var(v) = &expr.nodes[*body] {
                assert_eq!(*v, y);
            } else {
                panic!("Body should be Var(y)");
            }
        } else {
            panic!("Result should be Lam");
        }
    }

    // 7. test_inline_preserves_eval: Build let x = 21 in x + x (Many, no inline) and let x = 21 in x (Once, inline). Eval before/after, verify match.
    #[test]
    fn test_inline_preserves_eval() {
        let x = VarId(1);
        
        // Case A: Once (should inline)
        let expr_once = tree(vec![
            CoreFrame::Lit(Literal::LitInt(21)),
            CoreFrame::Var(x),
            CoreFrame::LetNonRec { binder: x, rhs: 0, body: 1 },
        ]);
        let mut expr_once_reduced = expr_once.clone();
        Inline.run(&mut expr_once_reduced);
        
        let mut heap = VecHeap::new();
        let env = Env::new();
        let v1 = eval(&expr_once, &env, &mut heap).unwrap();
        let v2 = eval(&expr_once_reduced, &env, &mut heap).unwrap();
        match (v1, v2) {
            (core_eval::Value::Lit(l1), core_eval::Value::Lit(l2)) => assert_eq!(l1, l2),
            _ => panic!("Expected literals"),
        }

        // Case B: Many (should NOT inline)
        let mut expr_many = tree(vec![
            CoreFrame::Lit(Literal::LitInt(21)),
            CoreFrame::Var(x),
            CoreFrame::Var(x),
            CoreFrame::PrimOp { op: PrimOpKind::IntAdd, args: vec![1, 2] },
            CoreFrame::LetNonRec { binder: x, rhs: 0, body: 3 },
        ]);
        let expr_many_orig = expr_many.clone();
        Inline.run(&mut expr_many);
        assert_eq!(expr_many, expr_many_orig);
    }
}
