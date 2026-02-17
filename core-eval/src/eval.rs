use core_repr::{AltCon, CoreExpr, CoreFrame, DataConId, Literal, PrimOpKind};
use crate::env::Env;
use crate::value::Value;
use crate::error::EvalError;

/// Evaluate a CoreExpr to a Value.
pub fn eval(expr: &CoreExpr, env: &Env) -> Result<Value, EvalError> {
    if expr.nodes.is_empty() {
        return Err(EvalError::TypeMismatch {
            expected: "non-empty expression".into(),
            got: "empty expression".into(),
        });
    }
    eval_at(expr, expr.nodes.len() - 1, env)
}

/// Evaluate the node at `idx` in the expression tree.
fn eval_at(expr: &CoreExpr, idx: usize, env: &Env) -> Result<Value, EvalError> {
    match &expr.nodes[idx] {
        CoreFrame::Var(v) => env
            .get(v)
            .cloned()
            .ok_or(EvalError::UnboundVar(*v)),
        CoreFrame::Lit(lit) => Ok(Value::Lit(lit.clone())),
        CoreFrame::App { fun, arg } => {
            let fun_val = eval_at(expr, *fun, env)?;
            let arg_val = eval_at(expr, *arg, env)?;
            match fun_val {
                Value::Closure(clos_env, binder, body) => {
                    let mut new_env = clos_env;
                    new_env.insert(binder, arg_val);
                    eval_at(&body, body.nodes.len() - 1, &new_env)
                }
                _ => Err(EvalError::NotAFunction),
            }
        }
        CoreFrame::Lam { binder, body } => {
            let body_expr = extract_subtree(expr, *body);
            Ok(Value::Closure(env.clone(), *binder, body_expr))
        }
        CoreFrame::LetNonRec { binder, rhs, body } => {
            let rhs_val = eval_at(expr, *rhs, env)?;
            let mut new_env = env.clone();
            new_env.insert(*binder, rhs_val);
            eval_at(expr, *body, &new_env)
        }
        CoreFrame::LetRec { bindings, body } => {
            let mut new_env = env.clone();
            for (binder, rhs) in bindings {
                let rhs_val = eval_at(expr, *rhs, &new_env)?;
                new_env.insert(*binder, rhs_val);
            }
            eval_at(expr, *body, &new_env)
        }
        CoreFrame::Con { tag, fields } => {
            let mut field_vals = Vec::with_capacity(fields.len());
            for &f in fields {
                field_vals.push(eval_at(expr, f, env)?);
            }
            Ok(Value::Con(*tag, field_vals))
        }
        CoreFrame::Case { scrutinee, binder, alts } => {
            let scrut_val = eval_at(expr, *scrutinee, env)?;
            let mut case_env = env.clone();
            case_env.insert(*binder, scrut_val.clone());
            
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
                                    alt_env.insert(*b, v.clone());
                                }
                                return eval_at(expr, alt.body, &alt_env);
                            }
                        }
                    }
                    AltCon::LitAlt(lit) => {
                        if let Value::Lit(l) = &scrut_val {
                            if l == lit {
                                return eval_at(expr, alt.body, &case_env);
                            }
                        }
                    }
                    AltCon::Default => {
                        return eval_at(expr, alt.body, &case_env);
                    }
                }
            }
            Err(EvalError::NoMatchingAlt)
        }
        CoreFrame::PrimOp { op, args } => {
            let mut arg_vals = Vec::with_capacity(args.len());
            for &arg in args {
                arg_vals.push(eval_at(expr, arg, env)?);
            }
            dispatch_primop(*op, arg_vals)
        }
        CoreFrame::Join { .. } => Err(EvalError::TypeMismatch {
            expected: "supported node".into(),
            got: "Join (not yet supported)".into(),
        }),
        CoreFrame::Jump { .. } => Err(EvalError::TypeMismatch {
            expected: "supported node".into(),
            got: "Jump (not yet supported)".into(),
        }),
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

fn extract_subtree(expr: &CoreExpr, root_idx: usize) -> CoreExpr {
    let mut new_nodes = Vec::new();
    let mut old_to_new = std::collections::HashMap::new();
    
    fn collect(idx: usize, expr: &CoreExpr, new_nodes: &mut Vec<CoreFrame<usize>>, old_to_new: &mut std::collections::HashMap<usize, usize>) -> usize {
        if let Some(&new_idx) = old_to_new.get(&idx) {
            return new_idx;
        }
        
        use core_repr::MapLayer;
        let frame = &expr.nodes[idx];
        let mapped_frame = frame.clone().map_layer(|child_idx| {
            collect(child_idx, expr, new_nodes, old_to_new)
        });
        
        let new_idx = new_nodes.len();
        new_nodes.push(mapped_frame);
        old_to_new.insert(idx, new_idx);
        new_idx
    }
    
    collect(root_idx, expr, &mut new_nodes, &mut old_to_new);
    CoreExpr { nodes: new_nodes }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_repr::{RecursiveTree, CoreFrame, Literal, VarId, DataConId, Alt, AltCon};

    #[test]
    fn test_eval_lit() {
        let expr = RecursiveTree { nodes: vec![CoreFrame::Lit(Literal::LitInt(42))] };
        let res = eval(&expr, &Env::new()).unwrap();
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
        let res = eval(&expr, &env).unwrap();
        if let Value::Lit(Literal::LitInt(n)) = res {
            assert_eq!(n, 42);
        } else {
            panic!("Expected LitInt(42), got {:?}", res);
        }
    }

    #[test]
    fn test_eval_unbound_var() {
        let expr = RecursiveTree { nodes: vec![CoreFrame::Var(VarId(1))] };
        let res = eval(&expr, &Env::new());
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
        let res = eval(&expr, &Env::new()).unwrap();
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
        let res = eval(&expr, &Env::new()).unwrap();
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
        let res = eval(&expr, &Env::new()).unwrap();
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
        let res = eval(&expr, &Env::new()).unwrap();
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
        let res = eval(&expr, &Env::new()).unwrap();
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
        let res = eval(&expr, &Env::new()).unwrap();
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
        let res = eval(&expr, &Env::new()).unwrap();
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
        let res = eval(&expr, &Env::new()).unwrap();
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
        let res = eval(&expr, &Env::new()).unwrap();
        if let Value::Lit(Literal::LitInt(n)) = res {
            assert_eq!(n, 1);
        } else {
            panic!("Expected LitInt(1)");
        }
    }

    #[test]
    fn test_eval_unsupported_join() {
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(1)),
            CoreFrame::Join { label: core_repr::JoinId(1), params: vec![], rhs: 0, body: 0 },
        ];
        let expr = CoreExpr { nodes };
        let res = eval(&expr, &Env::new());
        assert!(res.is_err());
    }

    #[test]
    fn test_eval_unsupported_jump() {
        let nodes = vec![
            CoreFrame::Jump { label: core_repr::JoinId(1), args: vec![] },
        ];
        let expr = CoreExpr { nodes };
        let res = eval(&expr, &Env::new());
        assert!(res.is_err());
    }
}
