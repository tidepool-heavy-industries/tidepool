use core_repr::{AltCon, CoreExpr, CoreFrame, DataConId, Literal, PrimOpKind, VarId};
use crate::env::Env;
use crate::value::Value;
use crate::error::EvalError;
use crate::heap::{Heap, ThunkState};

/// Evaluate a CoreExpr to a Value.
pub fn eval(expr: &CoreExpr, env: &Env, heap: &mut dyn Heap) -> Result<Value, EvalError> {
    if expr.nodes.is_empty() {
        return Err(EvalError::TypeMismatch {
            expected: "non-empty expression".into(),
            got: "empty expression".into(),
        });
    }
    let res = eval_at(expr, expr.nodes.len() - 1, env, heap)?;
    force(res, heap)
}

/// Force a thunk to a value.
fn force(val: Value, heap: &mut dyn Heap) -> Result<Value, EvalError> {
    match val {
        Value::ThunkRef(id) => {
            match heap.read(id).clone() {
                ThunkState::Evaluated(v) => force(v, heap),
                ThunkState::BlackHole => Err(EvalError::InfiniteLoop(id)),
                ThunkState::Unevaluated(env, expr) => {
                    heap.write(id, ThunkState::BlackHole);
                    match eval(&expr, &env, heap) {
                        Ok(result) => {
                            heap.write(id, ThunkState::Evaluated(result.clone()));
                            Ok(result)
                        }
                        Err(err) => {
                            // Restore state on error to avoid masking original failure
                            // with InfiniteLoop on subsequent forces.
                            heap.write(id, ThunkState::Unevaluated(env, expr));
                            Err(err)
                        }
                    }
                }
            }
        }
        other => Ok(other),
    }
}

/// Evaluate the node at `idx` in the expression tree.
fn eval_at(expr: &CoreExpr, idx: usize, env: &Env, heap: &mut dyn Heap) -> Result<Value, EvalError> {
    match &expr.nodes[idx] {
        CoreFrame::Var(v) => env
            .get(v)
            .cloned()
            .ok_or(EvalError::UnboundVar(*v)),
        CoreFrame::Lit(lit) => Ok(Value::Lit(lit.clone())),
        CoreFrame::App { fun, arg } => {
            let fun_val = force(eval_at(expr, *fun, env, heap)?, heap)?;
            let arg_val = eval_at(expr, *arg, env, heap)?;
            match fun_val {
                Value::Closure(clos_env, binder, body) => {
                    let mut new_env = clos_env;
                    new_env.insert(binder, arg_val);
                    eval(&body, &new_env, heap)
                }
                _ => Err(EvalError::NotAFunction),
            }
        }
        CoreFrame::Lam { binder, body } => {
            let body_expr = expr.extract_subtree(*body);
            Ok(Value::Closure(env.clone(), *binder, body_expr))
        }
        CoreFrame::LetNonRec { binder, rhs, body } => {
            let rhs_val = if matches!(&expr.nodes[*rhs], CoreFrame::Lam { .. }) {
                eval_at(expr, *rhs, env, heap)?  // Lambdas are values
            } else {
                let thunk_id = heap.alloc(env.clone(), expr.extract_subtree(*rhs));
                Value::ThunkRef(thunk_id)
            };
            let new_env = env.update(*binder, rhs_val);
            eval_at(expr, *body, &new_env, heap)
        }
        CoreFrame::LetRec { bindings, body } => {
            let mut new_env = env.clone();
            let mut thunks = Vec::new();
            
            // 1. Allocate thunks for all binders to allow full knot-tying.
            // (Spec: non-lambdas -> ThunkRef, but for knot-tying lambdas also need to be accessible)
            for (binder, rhs_idx) in bindings {
                let tid = heap.alloc(Env::new(), CoreExpr { nodes: vec![] });
                new_env = new_env.update(*binder, Value::ThunkRef(tid));
                thunks.push((*binder, tid, *rhs_idx));
            }

            // 2. Evaluate lambda RHSes and back-patch thunks. Update env with Closures.
            for (binder, tid, rhs_idx) in &thunks {
                if matches!(&expr.nodes[*rhs_idx], CoreFrame::Lam { .. }) {
                    let lam_val = eval_at(expr, *rhs_idx, &new_env, heap)?;
                    heap.write(*tid, ThunkState::Evaluated(lam_val.clone()));
                    new_env = new_env.update(*binder, lam_val);
                } else {
                    let rhs_subtree = expr.extract_subtree(*rhs_idx);
                    heap.write(*tid, ThunkState::Unevaluated(new_env.clone(), rhs_subtree));
                }
            }

            eval_at(expr, *body, &new_env, heap)
        }
        CoreFrame::Con { tag, fields } => {
            let mut field_vals = Vec::with_capacity(fields.len());
            for &f in fields {
                field_vals.push(eval_at(expr, f, env, heap)?);
            }
            Ok(Value::Con(*tag, field_vals))
        }
        CoreFrame::Case { scrutinee, binder, alts } => {
            let scrut_val = force(eval_at(expr, *scrutinee, env, heap)?, heap)?;
            let case_env = env.update(*binder, scrut_val.clone());
            
            for alt in alts {
                match &alt.con {
                    AltCon::DataAlt(tag) => {
                        if let Value::Con(con_tag, fields) = &scrut_val {
                            if con_tag == tag {
                                if fields.len() != alt.binders.len() {
                                    return Err(EvalError::TypeMismatch {
                                        expected: format!("{} fields", alt.binders.len()),
                                        got: format!("{} fields", fields.len()),
                                    });
                                }
                                let mut alt_env = case_env;
                                for (b, v) in alt.binders.iter().zip(fields.iter()) {
                                    alt_env = alt_env.update(*b, v.clone());
                                }
                                return eval_at(expr, alt.body, &alt_env, heap);
                            }
                        }
                    }
                    AltCon::LitAlt(lit) => {
                        if let Value::Lit(l) = &scrut_val {
                            if l == lit {
                                return eval_at(expr, alt.body, &case_env, heap);
                            }
                        }
                    }
                    AltCon::Default => {
                        return eval_at(expr, alt.body, &case_env, heap);
                    }
                }
            }
            Err(EvalError::NoMatchingAlt)
        }
        CoreFrame::PrimOp { op, args } => {
            let mut arg_vals = Vec::with_capacity(args.len());
            for &arg in args {
                let val = force(eval_at(expr, arg, env, heap)?, heap)?;
                arg_vals.push(val);
            }
            dispatch_primop(*op, arg_vals)
        }
        CoreFrame::Join { label, params, rhs, body } => {
            let join_val = Value::JoinCont(params.clone(), expr.extract_subtree(*rhs), env.clone());
            let join_var = VarId(label.0 | (1u64 << 63)); // high bit distinguishes join labels
            let new_env = env.update(join_var, join_val);
            eval_at(expr, *body, &new_env, heap)
        }
        CoreFrame::Jump { label, args } => {
            let join_var = VarId(label.0 | (1u64 << 63));
            match env.get(&join_var) {
                Some(Value::JoinCont(params, rhs_expr, join_env)) => {
                    if params.len() != args.len() {
                        return Err(EvalError::TypeMismatch {
                            expected: format!("{} arguments", params.len()),
                            got: format!("{} arguments", args.len()),
                        });
                    }
                    let params = params.clone();
                    let rhs_expr = rhs_expr.clone();
                    let mut new_env = join_env.clone();
                    for (param, arg_idx) in params.iter().zip(args.iter()) {
                        let arg_val = eval_at(expr, *arg_idx, env, heap)?;
                        new_env = new_env.update(*param, arg_val);
                    }
                    eval(&rhs_expr, &new_env, heap)
                }
                _ => Err(EvalError::UnboundJoin(*label)),
            }
        }
    }
}

fn dispatch_primop(op: PrimOpKind, args: Vec<Value>) -> Result<Value, EvalError> {
    match op {
        PrimOpKind::IntAdd => {
            let (a, b) = bin_op_int(op, &args)?;
            Ok(Value::Lit(Literal::LitInt(a.wrapping_add(b))))
        }
        PrimOpKind::IntSub => {
            let (a, b) = bin_op_int(op, &args)?;
            Ok(Value::Lit(Literal::LitInt(a.wrapping_sub(b))))
        }
        PrimOpKind::IntMul => {
            let (a, b) = bin_op_int(op, &args)?;
            Ok(Value::Lit(Literal::LitInt(a.wrapping_mul(b))))
        }
        PrimOpKind::IntNegate => {
            if args.len() != 1 {
                return Err(EvalError::TypeMismatch { expected: "1 argument".into(), got: format!("{} arguments", args.len()) });
            }
            let a = expect_int(&args[0])?;
            Ok(Value::Lit(Literal::LitInt(a.wrapping_neg())))
        }
        PrimOpKind::IntEq => cmp_int(op, &args, |a, b| a == b),
        PrimOpKind::IntNe => cmp_int(op, &args, |a, b| a != b),
        PrimOpKind::IntLt => cmp_int(op, &args, |a, b| a < b),
        PrimOpKind::IntLe => cmp_int(op, &args, |a, b| a <= b),
        PrimOpKind::IntGt => cmp_int(op, &args, |a, b| a > b),
        PrimOpKind::IntGe => cmp_int(op, &args, |a, b| a >= b),

        PrimOpKind::WordAdd => {
            let (a, b) = bin_op_word(op, &args)?;
            Ok(Value::Lit(Literal::LitWord(a.wrapping_add(b))))
        }
        PrimOpKind::WordSub => {
            let (a, b) = bin_op_word(op, &args)?;
            Ok(Value::Lit(Literal::LitWord(a.wrapping_sub(b))))
        }
        PrimOpKind::WordMul => {
            let (a, b) = bin_op_word(op, &args)?;
            Ok(Value::Lit(Literal::LitWord(a.wrapping_mul(b))))
        }
        PrimOpKind::WordEq => cmp_word(op, &args, |a, b| a == b),
        PrimOpKind::WordNe => cmp_word(op, &args, |a, b| a != b),
        PrimOpKind::WordLt => cmp_word(op, &args, |a, b| a < b),
        PrimOpKind::WordLe => cmp_word(op, &args, |a, b| a <= b),
        PrimOpKind::WordGt => cmp_word(op, &args, |a, b| a > b),
        PrimOpKind::WordGe => cmp_word(op, &args, |a, b| a >= b),

        PrimOpKind::DoubleAdd => {
            let (a, b) = bin_op_double(op, &args)?;
            Ok(Value::Lit(Literal::LitDouble((a + b).to_bits())))
        }
        PrimOpKind::DoubleSub => {
            let (a, b) = bin_op_double(op, &args)?;
            Ok(Value::Lit(Literal::LitDouble((a - b).to_bits())))
        }
        PrimOpKind::DoubleMul => {
            let (a, b) = bin_op_double(op, &args)?;
            Ok(Value::Lit(Literal::LitDouble((a * b).to_bits())))
        }
        PrimOpKind::DoubleDiv => {
            let (a, b) = bin_op_double(op, &args)?;
            Ok(Value::Lit(Literal::LitDouble((a / b).to_bits())))
        }
        PrimOpKind::DoubleEq => cmp_double(op, &args, |a, b| a == b),
        PrimOpKind::DoubleNe => cmp_double(op, &args, |a, b| a != b),
        PrimOpKind::DoubleLt => cmp_double(op, &args, |a, b| a < b),
        PrimOpKind::DoubleLe => cmp_double(op, &args, |a, b| a <= b),
        PrimOpKind::DoubleGt => cmp_double(op, &args, |a, b| a > b),
        PrimOpKind::DoubleGe => cmp_double(op, &args, |a, b| a >= b),

        PrimOpKind::CharEq => cmp_char(op, &args, |a, b| a == b),
        PrimOpKind::CharNe => cmp_char(op, &args, |a, b| a != b),
        PrimOpKind::CharLt => cmp_char(op, &args, |a, b| a < b),
        PrimOpKind::CharLe => cmp_char(op, &args, |a, b| a <= b),
        PrimOpKind::CharGt => cmp_char(op, &args, |a, b| a > b),
        PrimOpKind::CharGe => cmp_char(op, &args, |a, b| a >= b),

        PrimOpKind::SeqOp => {
            if args.len() != 2 {
                return Err(EvalError::TypeMismatch { expected: "2 arguments".into(), got: format!("{} arguments", args.len()) });
            }
            Ok(args[1].clone())
        }
        PrimOpKind::DataToTag => {
            if args.len() != 1 {
                return Err(EvalError::TypeMismatch { expected: "1 argument".into(), got: format!("{} arguments", args.len()) });
            }
            if let Value::Con(DataConId(tag), _) = &args[0] {
                Ok(Value::Lit(Literal::LitInt(*tag as i64)))
            } else {
                Err(EvalError::TypeMismatch { expected: "Data constructor".into(), got: format!("{:?}", args[0]) })
            }
        }
        PrimOpKind::IndexArray | PrimOpKind::TagToEnum => Err(EvalError::UnsupportedPrimOp(op)),
    }
}

fn expect_int(v: &Value) -> Result<i64, EvalError> {
    if let Value::Lit(Literal::LitInt(n)) = v {
        Ok(*n)
    } else {
        Err(EvalError::TypeMismatch { expected: "Int#".into(), got: format!("{:?}", v) })
    }
}

fn expect_word(v: &Value) -> Result<u64, EvalError> {
    if let Value::Lit(Literal::LitWord(n)) = v {
        Ok(*n)
    } else {
        Err(EvalError::TypeMismatch { expected: "Word#".into(), got: format!("{:?}", v) })
    }
}

fn expect_double(v: &Value) -> Result<f64, EvalError> {
    if let Value::Lit(Literal::LitDouble(bits)) = v {
        Ok(f64::from_bits(*bits))
    } else {
        Err(EvalError::TypeMismatch { expected: "Double#".into(), got: format!("{:?}", v) })
    }
}

fn expect_char(v: &Value) -> Result<char, EvalError> {
    if let Value::Lit(Literal::LitChar(c)) = v {
        Ok(*c)
    } else {
        Err(EvalError::TypeMismatch { expected: "Char#".into(), got: format!("{:?}", v) })
    }
}

fn bin_op_int(_op: PrimOpKind, args: &[Value]) -> Result<(i64, i64), EvalError> {
    if args.len() != 2 {
        return Err(EvalError::TypeMismatch { expected: "2 arguments".into(), got: format!("{} arguments", args.len()) });
    }
    Ok((expect_int(&args[0])?, expect_int(&args[1])?))
}

fn bin_op_word(_op: PrimOpKind, args: &[Value]) -> Result<(u64, u64), EvalError> {
    if args.len() != 2 {
        return Err(EvalError::TypeMismatch { expected: "2 arguments".into(), got: format!("{} arguments", args.len()) });
    }
    Ok((expect_word(&args[0])?, expect_word(&args[1])?))
}

fn bin_op_double(_op: PrimOpKind, args: &[Value]) -> Result<(f64, f64), EvalError> {
    if args.len() != 2 {
        return Err(EvalError::TypeMismatch { expected: "2 arguments".into(), got: format!("{} arguments", args.len()) });
    }
    Ok((expect_double(&args[0])?, expect_double(&args[1])?))
}

fn cmp_int(op: PrimOpKind, args: &[Value], f: impl Fn(i64, i64) -> bool) -> Result<Value, EvalError> {
    let (a, b) = bin_op_int(op, args)?;
    Ok(Value::Lit(Literal::LitInt(if f(a, b) { 1 } else { 0 })))
}

fn cmp_word(op: PrimOpKind, args: &[Value], f: impl Fn(u64, u64) -> bool) -> Result<Value, EvalError> {
    let (a, b) = bin_op_word(op, args)?;
    Ok(Value::Lit(Literal::LitInt(if f(a, b) { 1 } else { 0 })))
}

fn cmp_double(op: PrimOpKind, args: &[Value], f: impl Fn(f64, f64) -> bool) -> Result<Value, EvalError> {
    let (a, b) = bin_op_double(op, args)?;
    Ok(Value::Lit(Literal::LitInt(if f(a, b) { 1 } else { 0 })))
}

fn cmp_char(_op: PrimOpKind, args: &[Value], f: impl Fn(char, char) -> bool) -> Result<Value, EvalError> {
    if args.len() != 2 {
        return Err(EvalError::TypeMismatch { expected: "2 arguments".into(), got: format!("{} arguments", args.len()) });
    }
    let a = expect_char(&args[0])?;
    let b = expect_char(&args[1])?;
    Ok(Value::Lit(Literal::LitInt(if f(a, b) { 1 } else { 0 })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_repr::{RecursiveTree, CoreFrame, Literal, VarId, DataConId, Alt, AltCon, JoinId};

    #[test]
    fn test_eval_lit() {
        let expr = RecursiveTree { nodes: vec![CoreFrame::Lit(Literal::LitInt(42))] };
        let mut heap = crate::heap::VecHeap::new();
        let res = eval(&expr, &Env::new(), &mut heap).unwrap();
        if let Value::Lit(Literal::LitInt(n)) = res {
            assert_eq!(n, 42);
        } else {
            panic!("Expected LitInt(42), got {:?}", res);
        }
    }

    #[test]
    fn test_eval_var() {
        let expr = RecursiveTree { nodes: vec![CoreFrame::Var(VarId(1))] };
        let mut env = Env::new();
        env.insert(VarId(1), Value::Lit(Literal::LitInt(42)));
        let mut heap = crate::heap::VecHeap::new();
        let res = eval(&expr, &env, &mut heap).unwrap();
        if let Value::Lit(Literal::LitInt(n)) = res {
            assert_eq!(n, 42);
        } else {
            panic!("Expected LitInt(42), got {:?}", res);
        }
    }

    #[test]
    fn test_eval_unbound_var() {
        let expr = RecursiveTree { nodes: vec![CoreFrame::Var(VarId(1))] };
        let mut heap = crate::heap::VecHeap::new();
        let res = eval(&expr, &Env::new(), &mut heap);
        assert!(matches!(res, Err(EvalError::UnboundVar(VarId(1)))));
    }

    #[test]
    fn test_eval_lam_identity() {
        let nodes = vec![
            CoreFrame::Var(VarId(1)), 
            CoreFrame::Lam { binder: VarId(1), body: 0 }, 
            CoreFrame::Lit(Literal::LitInt(42)), 
            CoreFrame::App { fun: 1, arg: 2 }, 
        ];
        let expr = CoreExpr { nodes };
        let mut heap = crate::heap::VecHeap::new();
        let res = eval(&expr, &Env::new(), &mut heap).unwrap();
        if let Value::Lit(Literal::LitInt(n)) = res {
            assert_eq!(n, 42);
        } else {
            panic!("Expected LitInt(42), got {:?}", res);
        }
    }

    #[test]
    fn test_eval_let_nonrec() {
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(1)), 
            CoreFrame::Var(VarId(1)), 
            CoreFrame::LetNonRec { binder: VarId(1), rhs: 0, body: 1 }, 
        ];
        let expr = CoreExpr { nodes };
        let mut heap = crate::heap::VecHeap::new();
        let res = eval(&expr, &Env::new(), &mut heap).unwrap();
        if let Value::Lit(Literal::LitInt(n)) = res {
            assert_eq!(n, 1);
        } else {
            panic!("Expected LitInt(1), got {:?}", res);
        }
    }

    #[test]
    fn test_eval_con() {
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(42)),
            CoreFrame::Con { tag: DataConId(1), fields: vec![0] },
        ];
        let expr = CoreExpr { nodes };
        let mut heap = crate::heap::VecHeap::new();
        let res = eval(&expr, &Env::new(), &mut heap).unwrap();
        if let Value::Con(tag, fields) = res {
            assert_eq!(tag.0, 1);
            assert_eq!(fields.len(), 1);
            if let Value::Lit(Literal::LitInt(n)) = fields[0] {
                assert_eq!(n, 42);
            } else {
                panic!("Expected LitInt(42)");
            }
        } else {
            panic!("Expected Con");
        }
    }

    #[test]
    fn test_eval_primop_add() {
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(1)),
            CoreFrame::Lit(Literal::LitInt(2)),
            CoreFrame::PrimOp { op: PrimOpKind::IntAdd, args: vec![0, 1] },
        ];
        let expr = CoreExpr { nodes };
        let mut heap = crate::heap::VecHeap::new();
        let res = eval(&expr, &Env::new(), &mut heap).unwrap();
        if let Value::Lit(Literal::LitInt(n)) = res {
            assert_eq!(n, 3);
        } else {
            panic!("Expected LitInt(3)");
        }
    }

    #[test]
    fn test_eval_case_data() {
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(42)), 
            CoreFrame::Con { tag: DataConId(1), fields: vec![0] }, 
            CoreFrame::Var(VarId(10)), 
            CoreFrame::Case {
                scrutinee: 1,
                binder: VarId(11), 
                alts: vec![Alt {
                    con: AltCon::DataAlt(DataConId(1)),
                    binders: vec![VarId(10)], 
                    body: 2,
                }],
            }, 
        ];
        let expr = CoreExpr { nodes };
        let mut heap = crate::heap::VecHeap::new();
        let res = eval(&expr, &Env::new(), &mut heap).unwrap();
        if let Value::Lit(Literal::LitInt(n)) = res {
            assert_eq!(n, 42);
        } else {
            panic!("Expected LitInt(42)");
        }
    }

    #[test]
    fn test_eval_case_binder() {
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(42)), 
            CoreFrame::Con { tag: DataConId(1), fields: vec![0] }, 
            CoreFrame::Var(VarId(11)), 
            CoreFrame::Case {
                scrutinee: 1,
                binder: VarId(11), 
                alts: vec![Alt {
                    con: AltCon::DataAlt(DataConId(1)),
                    binders: vec![VarId(10)], 
                    body: 2,
                }],
            }, 
        ];
        let expr = CoreExpr { nodes };
        let mut heap = crate::heap::VecHeap::new();
        let res = eval(&expr, &Env::new(), &mut heap).unwrap();
        if let Value::Con(tag, _) = res {
            assert_eq!(tag.0, 1);
        } else {
            panic!("Expected Con");
        }
    }

    #[test]
    fn test_eval_case_lit_default() {
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(3)), 
            CoreFrame::Lit(Literal::LitInt(10)), 
            CoreFrame::Lit(Literal::LitInt(20)), 
            CoreFrame::Lit(Literal::LitInt(30)), 
            CoreFrame::Case {
                scrutinee: 0,
                binder: VarId(10),
                alts: vec![
                    Alt { con: AltCon::LitAlt(Literal::LitInt(1)), binders: vec![], body: 1 },
                    Alt { con: AltCon::LitAlt(Literal::LitInt(2)), binders: vec![], body: 2 },
                    Alt { con: AltCon::Default, binders: vec![], body: 3 },
                ],
            }, 
        ];
        let expr = CoreExpr { nodes };
        let mut heap = crate::heap::VecHeap::new();
        let res = eval(&expr, &Env::new(), &mut heap).unwrap();
        if let Value::Lit(Literal::LitInt(n)) = res {
            assert_eq!(n, 30);
        } else {
            panic!("Expected LitInt(30)");
        }
    }

    #[test]
    fn test_eval_data_to_tag() {
        let nodes = vec![
            CoreFrame::Con { tag: DataConId(5), fields: vec![] },
            CoreFrame::PrimOp { op: PrimOpKind::DataToTag, args: vec![0] },
        ];
        let expr = CoreExpr { nodes };
        let mut heap = crate::heap::VecHeap::new();
        let res = eval(&expr, &Env::new(), &mut heap).unwrap();
        if let Value::Lit(Literal::LitInt(n)) = res {
            assert_eq!(n, 5);
        } else {
            panic!("Expected LitInt(5)");
        }
    }

    #[test]
    fn test_eval_let_rec_forward_refs() {
        // let { x = 1; y = x } in y
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(1)), // 0
            CoreFrame::Var(VarId(1)), // 1: x
            CoreFrame::Var(VarId(2)), // 2: y
            CoreFrame::LetRec {
                bindings: vec![(VarId(1), 0), (VarId(2), 1)],
                body: 2,
            }, // 3
        ];
        let expr = CoreExpr { nodes };
        let mut heap = crate::heap::VecHeap::new();
        let res = eval(&expr, &Env::new(), &mut heap).unwrap();
        if let Value::Lit(Literal::LitInt(n)) = res {
            assert_eq!(n, 1);
        } else {
            panic!("Expected LitInt(1)");
        }
    }

    #[test]
    fn test_eval_join_simple() {
        // join j(x) = x in jump j(42)
        let nodes = vec![
            CoreFrame::Var(VarId(10)), // 0: x
            CoreFrame::Lit(Literal::LitInt(42)), // 1
            CoreFrame::Jump { label: JoinId(1), args: vec![1] }, // 2
            CoreFrame::Join { label: JoinId(1), params: vec![VarId(10)], rhs: 0, body: 2 }, // 3
        ];
        let expr = CoreExpr { nodes };
        let mut heap = crate::heap::VecHeap::new();
        let res = eval(&expr, &Env::new(), &mut heap).unwrap();
        if let Value::Lit(Literal::LitInt(n)) = res {
            assert_eq!(n, 42);
        } else {
            panic!("Expected LitInt(42), got {:?}", res);
        }
    }

    #[test]
    fn test_eval_join_multi_param() {
        // join j(x, y) = x + y in jump j(1, 2)
        let nodes = vec![
            CoreFrame::Var(VarId(10)), // 0: x
            CoreFrame::Var(VarId(11)), // 1: y
            CoreFrame::PrimOp { op: PrimOpKind::IntAdd, args: vec![0, 1] }, // 2: x + y
            CoreFrame::Lit(Literal::LitInt(1)), // 3
            CoreFrame::Lit(Literal::LitInt(2)), // 4
            CoreFrame::Jump { label: JoinId(1), args: vec![3, 4] }, // 5
            CoreFrame::Join { label: JoinId(1), params: vec![VarId(10), VarId(11)], rhs: 2, body: 5 }, // 6
        ];
        let expr = CoreExpr { nodes };
        let mut heap = crate::heap::VecHeap::new();
        let res = eval(&expr, &Env::new(), &mut heap).unwrap();
        if let Value::Lit(Literal::LitInt(n)) = res {
            assert_eq!(n, 3);
        } else {
            panic!("Expected LitInt(3)");
        }
    }

    #[test]
    fn test_eval_unbound_jump() {
        let nodes = vec![
            CoreFrame::Jump { label: JoinId(1), args: vec![] },
        ];
        let expr = CoreExpr { nodes };
        let mut heap = crate::heap::VecHeap::new();
        let res = eval(&expr, &Env::new(), &mut heap);
        assert!(matches!(res, Err(EvalError::UnboundJoin(JoinId(1)))));
    }

    #[test]
    fn test_thunk_lazy() {
        // let x = <unbound> in 42
        let nodes = vec![
            CoreFrame::Var(VarId(999)), // 0: unbound
            CoreFrame::Lit(Literal::LitInt(42)), // 1
            CoreFrame::LetNonRec { binder: VarId(1), rhs: 0, body: 1 }, // 2
        ];
        let expr = CoreExpr { nodes };
        let mut heap = crate::heap::VecHeap::new();
        let res = eval(&expr, &Env::new(), &mut heap).unwrap();
        if let Value::Lit(Literal::LitInt(n)) = res {
            assert_eq!(n, 42);
        } else {
            panic!("Expected LitInt(42)");
        }
    }

    #[test]
    fn test_thunk_caching() {
        // let x = 1 + 1 in x + x
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(1)), // 0
            CoreFrame::PrimOp { op: PrimOpKind::IntAdd, args: vec![0, 0] }, // 1: 1 + 1
            CoreFrame::Var(VarId(1)), // 2: x
            CoreFrame::PrimOp { op: PrimOpKind::IntAdd, args: vec![2, 2] }, // 3: x + x
            CoreFrame::LetNonRec { binder: VarId(1), rhs: 1, body: 3 }, // 4
        ];
        let expr = CoreExpr { nodes };
        let mut heap = crate::heap::VecHeap::new();
        let res = eval(&expr, &Env::new(), &mut heap).unwrap();
        if let Value::Lit(Literal::LitInt(n)) = res {
            assert_eq!(n, 4);
        } else {
            panic!("Expected LitInt(4)");
        }
    }

    #[test]
    fn test_thunk_blackhole() {
        // letrec x = x in x
        let nodes = vec![
            CoreFrame::Var(VarId(1)), // 0: x
            CoreFrame::LetRec { bindings: vec![(VarId(1), 0)], body: 0 }, // 1
        ];
        let expr = CoreExpr { nodes };
        let mut heap = crate::heap::VecHeap::new();
        let res = eval(&expr, &Env::new(), &mut heap);
        assert!(matches!(res, Err(EvalError::InfiniteLoop(_))));
    }

    #[test]
    fn test_letrec_mutual_recursion() {
        // letrec { f = \a -> g a; g = \b -> b } in f 42
        let nodes = vec![
            CoreFrame::Var(VarId(11)), // 0: a
            CoreFrame::Var(VarId(2)), // 1: g
            CoreFrame::App { fun: 1, arg: 0 }, // 2: g a
            CoreFrame::Lam { binder: VarId(11), body: 2 }, // 3: \a -> g a (f)
            
            CoreFrame::Var(VarId(12)), // 4: b
            CoreFrame::Lam { binder: VarId(12), body: 4 }, // 5: \b -> b (g)
            
            CoreFrame::Var(VarId(1)), // 6: f
            CoreFrame::Lit(Literal::LitInt(42)), // 7
            CoreFrame::App { fun: 6, arg: 7 }, // 8: f 42
            
            CoreFrame::LetRec {
                bindings: vec![(VarId(1), 3), (VarId(2), 5)],
                body: 8,
            }, // 9
        ];
        let expr = CoreExpr { nodes };
        let mut heap = crate::heap::VecHeap::new();
        let res = eval(&expr, &Env::new(), &mut heap).unwrap();
        if let Value::Lit(Literal::LitInt(n)) = res {
            assert_eq!(n, 42);
        } else {
            panic!("Expected LitInt(42)");
        }
    }

    #[test]
    fn test_eval_join_scoping() {
        // let y = 100 in
        // join j(x) = x + y in
        // let y = 200 in
        // jump j(1)
        // Should be 101, not 201.
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(100)), // 0
            CoreFrame::Var(VarId(10)), // 1: x
            CoreFrame::Var(VarId(20)), // 2: y (captured)
            CoreFrame::PrimOp { op: PrimOpKind::IntAdd, args: vec![1, 2] }, // 3: x + y
            CoreFrame::Lit(Literal::LitInt(200)), // 4
            CoreFrame::Lit(Literal::LitInt(1)), // 5
            CoreFrame::Jump { label: JoinId(1), args: vec![5] }, // 6
            CoreFrame::LetNonRec { binder: VarId(20), rhs: 4, body: 6 }, // 7: let y = 200 in jump j(1)
            CoreFrame::Join { label: JoinId(1), params: vec![VarId(10)], rhs: 3, body: 7 }, // 8: join j(x) = x+y in ...
            CoreFrame::LetNonRec { binder: VarId(20), rhs: 0, body: 8 }, // 9: let y = 100 in join ...
        ];
        let expr = CoreExpr { nodes };
        let mut heap = crate::heap::VecHeap::new();
        let res = eval(&expr, &Env::new(), &mut heap).unwrap();
        if let Value::Lit(Literal::LitInt(n)) = res {
            assert_eq!(n, 101);
        } else {
            panic!("Expected LitInt(101)");
        }
    }

    #[test]
    fn test_thunk_poison_restoration() {
        // let x = <unbound> in x
        let nodes = vec![
            CoreFrame::Var(VarId(999)), // 0: unbound
            CoreFrame::LetNonRec { binder: VarId(1), rhs: 0, body: 0 }, // 1: let x = unbound in x
        ];
        let expr = CoreExpr { nodes };
        let mut heap = crate::heap::VecHeap::new();
        
        // First force fails with UnboundVar
        let res1 = eval(&expr, &Env::new(), &mut heap);
        assert!(matches!(res1, Err(EvalError::UnboundVar(_))));

        // Second force should ALSO fail with UnboundVar, NOT InfiniteLoop (BlackHole)
        let res2 = eval(&expr, &Env::new(), &mut heap);
        assert!(matches!(res2, Err(EvalError::UnboundVar(_))));
    }

    #[test]
    fn test_eval_jump_arity_mismatch() {
        // join j(x) = x in jump j(1, 2)
        let nodes = vec![
            CoreFrame::Var(VarId(10)), // 0: x
            CoreFrame::Lit(Literal::LitInt(1)), // 1
            CoreFrame::Lit(Literal::LitInt(2)), // 2
            CoreFrame::Jump { label: JoinId(1), args: vec![1, 2] }, // 3: jump j(1, 2)
            CoreFrame::Join { label: JoinId(1), params: vec![VarId(10)], rhs: 0, body: 3 }, // 4: join j(x) ...
        ];
        let expr = CoreExpr { nodes };
        let mut heap = crate::heap::VecHeap::new();
        let res = eval(&expr, &Env::new(), &mut heap);
        assert!(matches!(res, Err(EvalError::TypeMismatch { .. })));
    }
}