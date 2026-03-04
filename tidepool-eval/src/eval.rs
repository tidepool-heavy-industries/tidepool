use crate::env::Env;
use crate::error::EvalError;
use crate::heap::{Heap, ThunkState};
use crate::value::Value;
use tidepool_repr::{
    AltCon, CoreExpr, CoreFrame, DataConId, DataConTable, Literal, PrimOpKind, VarId,
};

/// Create an environment pre-populated with data constructor functions.
/// Each constructor with arity N becomes a `ConFun(tag, N, [])` value
/// bound to its worker VarId, so that `Var` references to constructors
/// in the expression tree resolve correctly.
pub fn env_from_datacon_table(table: &DataConTable) -> Env {
    let mut env = Env::new();
    for dc in table.iter() {
        let var = VarId(dc.id.0);
        if dc.rep_arity == 0 {
            // Nullary constructor: just a Con value
            env.insert(var, Value::Con(dc.id, vec![]));
        } else {
            env.insert(var, Value::ConFun(dc.id, dc.rep_arity as usize, vec![]));
        }
    }
    env
}

/// Evaluate a CoreExpr to a Value.
pub fn eval(expr: &CoreExpr, env: &Env, heap: &mut dyn Heap) -> Result<Value, EvalError> {
    if expr.nodes.is_empty() {
        return Err(EvalError::TypeMismatch {
            expected: "non-empty expression",
            got: crate::error::ValueKind::Other("empty tree".into()),
        });
    }
    let res = eval_at(expr, expr.nodes.len() - 1, env, heap)?;
    force(res, heap)
}

/// Force a thunk to a value.
pub fn force(val: Value, heap: &mut dyn Heap) -> Result<Value, EvalError> {
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

/// Iteratively force a value — forces all thunks inside constructors,
/// producing a fully-evaluated tree with no `ThunkRef` values.
/// Uses an explicit work stack instead of recursion to handle deep
/// structures (e.g. 1000-element lists) without overflowing the Rust stack.
pub fn deep_force(val: Value, heap: &mut dyn Heap) -> Result<Value, EvalError> {
    use tidepool_repr::DataConId;

    const MAX_DEPTH: usize = 100_000;

    enum Work {
        Force(Value),
        BuildCon(DataConId, usize),           // (tag, num_fields)
        BuildConFun(DataConId, usize, usize), // (tag, arity, num_args)
    }

    let mut stack: Vec<Work> = vec![Work::Force(val)];
    let mut results: Vec<Value> = Vec::new();

    while let Some(work) = stack.pop() {
        if stack.len() > MAX_DEPTH {
            return Err(EvalError::DepthLimit);
        }
        match work {
            Work::Force(v) => match v {
                Value::ThunkRef(id) => {
                    let forced = force(Value::ThunkRef(id), heap)?;
                    stack.push(Work::Force(forced));
                }
                Value::Con(tag, fields) => {
                    let n = fields.len();
                    stack.push(Work::BuildCon(tag, n));
                    // Push fields in reverse so they're processed in order
                    for f in fields.into_iter().rev() {
                        stack.push(Work::Force(f));
                    }
                }
                Value::ConFun(tag, arity, args) => {
                    let n = args.len();
                    stack.push(Work::BuildConFun(tag, arity, n));
                    for a in args.into_iter().rev() {
                        stack.push(Work::Force(a));
                    }
                }
                other => results.push(other),
            },
            Work::BuildCon(tag, n) => {
                let start = results.len() - n;
                let fields = results.split_off(start);
                results.push(Value::Con(tag, fields));
            }
            Work::BuildConFun(tag, arity, n) => {
                let start = results.len() - n;
                let args = results.split_off(start);
                results.push(Value::ConFun(tag, arity, args));
            }
        }
    }

    Ok(results.pop().expect("deep_force produced no result"))
}

/// Evaluate the node at `idx` in the expression tree.
fn eval_at(
    expr: &CoreExpr,
    idx: usize,
    env: &Env,
    heap: &mut dyn Heap,
) -> Result<Value, EvalError> {
    match &expr.nodes[idx] {
        CoreFrame::Var(v) => {
            let tag = (v.0 >> 56) as u8;
            if tag == 0x45 {
                // 'E' = error tag: synthetic error VarIds from Translate.hs
                let kind = v.0 & 0xFF;
                return Err(match kind {
                    0 => EvalError::TypeMismatch {
                        expected: "non-zero divisor",
                        got: crate::error::ValueKind::Other("division by zero".into()),
                    },
                    1 => EvalError::TypeMismatch {
                        expected: "no overflow",
                        got: crate::error::ValueKind::Other("arithmetic overflow".into()),
                    },
                    2 => EvalError::UserError,
                    3 => EvalError::Undefined,
                    _ => EvalError::UserError,
                });
            }
            env.get(v).cloned().ok_or(EvalError::UnboundVar(*v))
        }
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
                Value::ConFun(tag, arity, mut args) => {
                    args.push(arg_val);
                    if args.len() == arity {
                        // Force all fields when constructor is saturated
                        let mut forced_args = Vec::with_capacity(args.len());
                        for a in args {
                            forced_args.push(force(a, heap)?);
                        }
                        Ok(Value::Con(tag, forced_args))
                    } else {
                        Ok(Value::ConFun(tag, arity, args))
                    }
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
                eval_at(expr, *rhs, env, heap)? // Lambdas are values
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
        CoreFrame::Case {
            scrutinee,
            binder,
            alts,
        } => {
            let scrut_val = force(eval_at(expr, *scrutinee, env, heap)?, heap)?;
            let case_env = env.update(*binder, scrut_val.clone());

            // Try specific alternatives first; Default is a fallback, not positional.
            // GHC Core can place DEFAULT first in the alt list.
            let mut default_alt = None;
            for alt in alts {
                match &alt.con {
                    AltCon::DataAlt(tag) => {
                        if let Value::Con(con_tag, fields) = &scrut_val {
                            if con_tag == tag {
                                if fields.len() != alt.binders.len() {
                                    return Err(EvalError::ArityMismatch {
                                        context: "case binders",
                                        expected: alt.binders.len(),
                                        got: fields.len(),
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
                        default_alt = Some(alt);
                    }
                }
            }
            if let Some(alt) = default_alt {
                return eval_at(expr, alt.body, &case_env, heap);
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
        CoreFrame::Join {
            label,
            params,
            rhs,
            body,
        } => {
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
                        return Err(EvalError::ArityMismatch {
                            context: "arguments",
                            expected: params.len(),
                            got: args.len(),
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
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 1,
                    got: args.len(),
                });
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
        PrimOpKind::IntAnd => {
            let (a, b) = bin_op_int(op, &args)?;
            Ok(Value::Lit(Literal::LitInt(a & b)))
        }
        PrimOpKind::IntOr => {
            let (a, b) = bin_op_int(op, &args)?;
            Ok(Value::Lit(Literal::LitInt(a | b)))
        }
        PrimOpKind::IntXor => {
            let (a, b) = bin_op_int(op, &args)?;
            Ok(Value::Lit(Literal::LitInt(a ^ b)))
        }
        PrimOpKind::IntNot => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 1,
                    got: args.len(),
                });
            }
            let a = expect_int(&args[0])?;
            Ok(Value::Lit(Literal::LitInt(!a)))
        }
        PrimOpKind::IntShl => {
            let (a, b) = bin_op_int(op, &args)?;
            Ok(Value::Lit(Literal::LitInt(a.wrapping_shl(b as u32))))
        }
        PrimOpKind::IntShra => {
            let (a, b) = bin_op_int(op, &args)?;
            Ok(Value::Lit(Literal::LitInt(a.wrapping_shr(b as u32))))
        }
        PrimOpKind::IntShrl => {
            let (a, b) = bin_op_int(op, &args)?;
            Ok(Value::Lit(Literal::LitInt(
                (a as u64).wrapping_shr(b as u32) as i64,
            )))
        }

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
        PrimOpKind::WordQuot => {
            let (a, b) = bin_op_word(op, &args)?;
            Ok(Value::Lit(Literal::LitWord(a.wrapping_div(b))))
        }
        PrimOpKind::WordRem => {
            let (a, b) = bin_op_word(op, &args)?;
            Ok(Value::Lit(Literal::LitWord(a.wrapping_rem(b))))
        }
        PrimOpKind::WordAnd => {
            let (a, b) = bin_op_word(op, &args)?;
            Ok(Value::Lit(Literal::LitWord(a & b)))
        }
        PrimOpKind::WordOr => {
            let (a, b) = bin_op_word(op, &args)?;
            Ok(Value::Lit(Literal::LitWord(a | b)))
        }
        PrimOpKind::WordXor => {
            let (a, b) = bin_op_word(op, &args)?;
            Ok(Value::Lit(Literal::LitWord(a ^ b)))
        }
        PrimOpKind::WordNot => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 1,
                    got: args.len(),
                });
            }
            let a = expect_word(&args[0])?;
            Ok(Value::Lit(Literal::LitWord(!a)))
        }
        PrimOpKind::WordShl => {
            if args.len() != 2 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 2,
                    got: args.len(),
                });
            }
            let a = expect_word(&args[0])?;
            let b = expect_int(&args[1])?;
            Ok(Value::Lit(Literal::LitWord(a.wrapping_shl(b as u32))))
        }
        PrimOpKind::WordShrl => {
            if args.len() != 2 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 2,
                    got: args.len(),
                });
            }
            let a = expect_word(&args[0])?;
            let b = expect_int(&args[1])?;
            Ok(Value::Lit(Literal::LitWord(a.wrapping_shr(b as u32))))
        }

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
        PrimOpKind::DoubleNegate => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 1,
                    got: args.len(),
                });
            }
            let a = expect_double(&args[0])?;
            Ok(Value::Lit(Literal::LitDouble((-a).to_bits())))
        }
        PrimOpKind::DoubleFabs => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 1,
                    got: args.len(),
                });
            }
            let a = expect_double(&args[0])?;
            Ok(Value::Lit(Literal::LitDouble(a.abs().to_bits())))
        }
        PrimOpKind::DoubleSqrt => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 1,
                    got: args.len(),
                });
            }
            let a = expect_double(&args[0])?;
            Ok(Value::Lit(Literal::LitDouble(a.sqrt().to_bits())))
        }
        PrimOpKind::DoubleExp => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 1,
                    got: args.len(),
                });
            }
            let a = expect_double(&args[0])?;
            Ok(Value::Lit(Literal::LitDouble(a.exp().to_bits())))
        }
        PrimOpKind::DoubleExpM1 => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 1,
                    got: args.len(),
                });
            }
            let a = expect_double(&args[0])?;
            Ok(Value::Lit(Literal::LitDouble(a.exp_m1().to_bits())))
        }
        PrimOpKind::DoubleLog => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 1,
                    got: args.len(),
                });
            }
            let a = expect_double(&args[0])?;
            Ok(Value::Lit(Literal::LitDouble(a.ln().to_bits())))
        }
        PrimOpKind::DoubleLog1P => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 1,
                    got: args.len(),
                });
            }
            let a = expect_double(&args[0])?;
            Ok(Value::Lit(Literal::LitDouble(a.ln_1p().to_bits())))
        }
        PrimOpKind::DoubleSin => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 1,
                    got: args.len(),
                });
            }
            let a = expect_double(&args[0])?;
            Ok(Value::Lit(Literal::LitDouble(a.sin().to_bits())))
        }
        PrimOpKind::DoubleCos => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 1,
                    got: args.len(),
                });
            }
            let a = expect_double(&args[0])?;
            Ok(Value::Lit(Literal::LitDouble(a.cos().to_bits())))
        }
        PrimOpKind::DoubleTan => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 1,
                    got: args.len(),
                });
            }
            let a = expect_double(&args[0])?;
            Ok(Value::Lit(Literal::LitDouble(a.tan().to_bits())))
        }
        PrimOpKind::DoubleAsin => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 1,
                    got: args.len(),
                });
            }
            let a = expect_double(&args[0])?;
            Ok(Value::Lit(Literal::LitDouble(a.asin().to_bits())))
        }
        PrimOpKind::DoubleAcos => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 1,
                    got: args.len(),
                });
            }
            let a = expect_double(&args[0])?;
            Ok(Value::Lit(Literal::LitDouble(a.acos().to_bits())))
        }
        PrimOpKind::DoubleAtan => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 1,
                    got: args.len(),
                });
            }
            let a = expect_double(&args[0])?;
            Ok(Value::Lit(Literal::LitDouble(a.atan().to_bits())))
        }
        PrimOpKind::DoubleSinh => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 1,
                    got: args.len(),
                });
            }
            let a = expect_double(&args[0])?;
            Ok(Value::Lit(Literal::LitDouble(a.sinh().to_bits())))
        }
        PrimOpKind::DoubleCosh => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 1,
                    got: args.len(),
                });
            }
            let a = expect_double(&args[0])?;
            Ok(Value::Lit(Literal::LitDouble(a.cosh().to_bits())))
        }
        PrimOpKind::DoubleTanh => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 1,
                    got: args.len(),
                });
            }
            let a = expect_double(&args[0])?;
            Ok(Value::Lit(Literal::LitDouble(a.tanh().to_bits())))
        }
        PrimOpKind::DoubleAsinh => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 1,
                    got: args.len(),
                });
            }
            let a = expect_double(&args[0])?;
            Ok(Value::Lit(Literal::LitDouble(a.asinh().to_bits())))
        }
        PrimOpKind::DoubleAcosh => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 1,
                    got: args.len(),
                });
            }
            let a = expect_double(&args[0])?;
            Ok(Value::Lit(Literal::LitDouble(a.acosh().to_bits())))
        }
        PrimOpKind::DoubleAtanh => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 1,
                    got: args.len(),
                });
            }
            let a = expect_double(&args[0])?;
            Ok(Value::Lit(Literal::LitDouble(a.atanh().to_bits())))
        }
        PrimOpKind::DoublePower => {
            if args.len() != 2 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 2,
                    got: args.len(),
                });
            }
            let a = expect_double(&args[0])?;
            let b = expect_double(&args[1])?;
            Ok(Value::Lit(Literal::LitDouble(a.powf(b).to_bits())))
        }
        PrimOpKind::FloatAdd => {
            let (a, b) = bin_op_float(op, &args)?;
            Ok(Value::Lit(Literal::LitFloat((a + b).to_bits() as u64)))
        }
        PrimOpKind::FloatSub => {
            let (a, b) = bin_op_float(op, &args)?;
            Ok(Value::Lit(Literal::LitFloat((a - b).to_bits() as u64)))
        }
        PrimOpKind::FloatMul => {
            let (a, b) = bin_op_float(op, &args)?;
            Ok(Value::Lit(Literal::LitFloat((a * b).to_bits() as u64)))
        }
        PrimOpKind::FloatDiv => {
            let (a, b) = bin_op_float(op, &args)?;
            Ok(Value::Lit(Literal::LitFloat((a / b).to_bits() as u64)))
        }
        PrimOpKind::FloatNegate => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 1,
                    got: args.len(),
                });
            }
            let a = expect_float(&args[0])?;
            Ok(Value::Lit(Literal::LitFloat((-a).to_bits() as u64)))
        }
        PrimOpKind::FloatEq => cmp_float(op, &args, |a, b| a == b),
        PrimOpKind::FloatNe => cmp_float(op, &args, |a, b| a != b),
        PrimOpKind::FloatLt => cmp_float(op, &args, |a, b| a < b),
        PrimOpKind::FloatLe => cmp_float(op, &args, |a, b| a <= b),
        PrimOpKind::FloatGt => cmp_float(op, &args, |a, b| a > b),
        PrimOpKind::FloatGe => cmp_float(op, &args, |a, b| a >= b),

        PrimOpKind::CharEq => cmp_char(op, &args, |a, b| a == b),
        PrimOpKind::CharNe => cmp_char(op, &args, |a, b| a != b),
        PrimOpKind::CharLt => cmp_char(op, &args, |a, b| a < b),
        PrimOpKind::CharLe => cmp_char(op, &args, |a, b| a <= b),
        PrimOpKind::CharGt => cmp_char(op, &args, |a, b| a > b),
        PrimOpKind::CharGe => cmp_char(op, &args, |a, b| a >= b),
        PrimOpKind::Int2Word => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 1,
                    got: args.len(),
                });
            }
            let a = expect_int(&args[0])?;
            Ok(Value::Lit(Literal::LitWord(a as u64)))
        }
        PrimOpKind::Word2Int => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 1,
                    got: args.len(),
                });
            }
            let a = expect_word(&args[0])?;
            Ok(Value::Lit(Literal::LitInt(a as i64)))
        }
        PrimOpKind::Narrow8Int => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 1,
                    got: args.len(),
                });
            }
            let a = expect_int(&args[0])?;
            Ok(Value::Lit(Literal::LitInt(a as i8 as i64)))
        }
        PrimOpKind::Narrow16Int => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 1,
                    got: args.len(),
                });
            }
            let a = expect_int(&args[0])?;
            Ok(Value::Lit(Literal::LitInt(a as i16 as i64)))
        }
        PrimOpKind::Narrow32Int => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 1,
                    got: args.len(),
                });
            }
            let a = expect_int(&args[0])?;
            Ok(Value::Lit(Literal::LitInt(a as i32 as i64)))
        }
        PrimOpKind::Narrow8Word => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 1,
                    got: args.len(),
                });
            }
            let a = expect_word(&args[0])?;
            Ok(Value::Lit(Literal::LitWord(a as u8 as u64)))
        }
        PrimOpKind::Narrow16Word => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 1,
                    got: args.len(),
                });
            }
            let a = expect_word(&args[0])?;
            Ok(Value::Lit(Literal::LitWord(a as u16 as u64)))
        }
        PrimOpKind::Narrow32Word => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 1,
                    got: args.len(),
                });
            }
            let a = expect_word(&args[0])?;
            Ok(Value::Lit(Literal::LitWord(a as u32 as u64)))
        }
        PrimOpKind::Int2Double => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 1,
                    got: args.len(),
                });
            }
            let a = expect_int(&args[0])?;
            Ok(Value::Lit(Literal::LitDouble((a as f64).to_bits())))
        }
        PrimOpKind::Double2Int => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 1,
                    got: args.len(),
                });
            }
            let a = expect_double(&args[0])?;
            Ok(Value::Lit(Literal::LitInt(a as i64)))
        }
        PrimOpKind::DecodeDoubleMantissa => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 1,
                    got: args.len(),
                });
            }
            let d = expect_double(&args[0])?;
            let (man, _) = eval_decode_double_int64(d);
            Ok(Value::Lit(Literal::LitInt(man)))
        }
        PrimOpKind::DecodeDoubleExponent => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 1,
                    got: args.len(),
                });
            }
            let d = expect_double(&args[0])?;
            let (_, exp) = eval_decode_double_int64(d);
            Ok(Value::Lit(Literal::LitInt(exp)))
        }
        PrimOpKind::ShowDoubleAddr => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 1,
                    got: args.len(),
                });
            }
            // Accept both unboxed Double# and boxed D# Double#
            let d = match &args[0] {
                Value::Lit(Literal::LitDouble(bits)) => f64::from_bits(*bits),
                Value::Con(_, fields) if fields.len() == 1 => expect_double(&fields[0])?,
                other => {
                    return Err(EvalError::TypeMismatch {
                        expected: "Double# or D# Double#",
                        got: crate::error::ValueKind::Other(format!("{:?}", other)),
                    })
                }
            };
            let s = eval_haskell_show_double(d);
            let mut bytes = s.into_bytes();
            bytes.push(0); // null terminator for IndexCharOffAddr
            Ok(Value::Lit(Literal::LitString(bytes)))
        }
        PrimOpKind::Int2Float => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 1,
                    got: args.len(),
                });
            }
            let a = expect_int(&args[0])?;
            Ok(Value::Lit(Literal::LitFloat((a as f32).to_bits() as u64)))
        }
        PrimOpKind::Float2Int => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 1,
                    got: args.len(),
                });
            }
            let a = expect_float(&args[0])?;
            Ok(Value::Lit(Literal::LitInt(a as i64)))
        }
        PrimOpKind::Double2Float => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 1,
                    got: args.len(),
                });
            }
            let a = expect_double(&args[0])?;
            Ok(Value::Lit(Literal::LitFloat((a as f32).to_bits() as u64)))
        }
        PrimOpKind::Float2Double => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 1,
                    got: args.len(),
                });
            }
            let a = expect_float(&args[0])?;
            Ok(Value::Lit(Literal::LitDouble((a as f64).to_bits())))
        }

        PrimOpKind::SeqOp => {
            if args.len() != 2 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 2,
                    got: args.len(),
                });
            }
            Ok(args[1].clone())
        }
        PrimOpKind::DataToTag => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 1,
                    got: args.len(),
                });
            }
            if let Value::Con(DataConId(tag), _) = &args[0] {
                Ok(Value::Lit(Literal::LitInt(*tag as i64)))
            } else {
                Err(EvalError::TypeMismatch {
                    expected: "Data constructor",
                    got: crate::error::ValueKind::Other(format!("{:?}", args[0])),
                })
            }
        }
        PrimOpKind::IntQuot => {
            let (a, b) = bin_op_int(op, &args)?;
            Ok(Value::Lit(Literal::LitInt(a.wrapping_div(b))))
        }
        PrimOpKind::IntRem => {
            let (a, b) = bin_op_int(op, &args)?;
            Ok(Value::Lit(Literal::LitInt(a.wrapping_rem(b))))
        }
        PrimOpKind::Chr => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 1,
                    got: args.len(),
                });
            }
            let n = expect_int(&args[0])?;
            Ok(Value::Lit(Literal::LitChar(
                char::from_u32(n as u32).unwrap_or('\0'),
            )))
        }
        PrimOpKind::Ord => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 1,
                    got: args.len(),
                });
            }
            let c = expect_char(&args[0])?;
            Ok(Value::Lit(Literal::LitInt(c as i64)))
        }
        PrimOpKind::IndexCharOffAddr => {
            if args.len() != 2 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 2,
                    got: args.len(),
                });
            }
            let bytes = match &args[0] {
                Value::Lit(Literal::LitString(bs)) => bs,
                other => {
                    return Err(EvalError::TypeMismatch {
                        expected: "Addr# (LitString)",
                        got: crate::error::ValueKind::Other(format!("{:?}", other)),
                    })
                }
            };
            let offset = expect_int(&args[1])? as usize;
            let ch = bytes.get(offset).copied().unwrap_or(0);
            Ok(Value::Lit(Literal::LitChar(ch as char)))
        }
        PrimOpKind::PlusAddr => {
            // plusAddr# :: Addr# -> Int# -> Addr#
            // In interpreter, Addr# is LitString. plusAddr# slices the byte vec.
            if args.len() != 2 {
                return Err(EvalError::ArityMismatch {
                    context: "arguments",
                    expected: 2,
                    got: args.len(),
                });
            }
            let bytes = match &args[0] {
                Value::Lit(Literal::LitString(bs)) => bs,
                other => {
                    return Err(EvalError::TypeMismatch {
                        expected: "Addr# (LitString)",
                        got: crate::error::ValueKind::Other(format!("{:?}", other)),
                    })
                }
            };
            let offset = expect_int(&args[1])? as usize;
            Ok(Value::Lit(Literal::LitString(bytes[offset..].to_vec())))
        }
        PrimOpKind::ReallyUnsafePtrEquality => Ok(Value::Lit(Literal::LitInt(0))),
        PrimOpKind::Raise => Err(EvalError::UserError),
        PrimOpKind::IndexArray | PrimOpKind::TagToEnum => Err(EvalError::UnsupportedPrimOp(op)),

        // --- ByteArray# / MutableByteArray# ---
        PrimOpKind::NewByteArray => {
            let size = expect_int(&args[0])? as usize;
            Ok(Value::ByteArray(std::sync::Arc::new(
                std::sync::Mutex::new(vec![0u8; size]),
            )))
        }
        PrimOpKind::ReadWord8Array => {
            let ba = expect_byte_array(&args[0])?;
            let idx = expect_int(&args[1])? as usize;
            let bytes = ba.lock().unwrap();
            let val = *bytes.get(idx).unwrap_or(&0);
            Ok(Value::Lit(Literal::LitWord(val as u64)))
        }
        PrimOpKind::WriteWord8Array => {
            let ba = expect_byte_array(&args[0])?;
            let idx = expect_int(&args[1])? as usize;
            let val = expect_int_like(&args[2])? as u8;
            let mut bytes = ba.lock().unwrap();
            if idx < bytes.len() {
                bytes[idx] = val;
            }
            Ok(Value::ByteArray(ba.clone()))
        }
        PrimOpKind::SizeofMutableByteArray => {
            let ba = expect_byte_array(&args[0])?;
            let len = ba.lock().unwrap().len();
            Ok(Value::Lit(Literal::LitInt(len as i64)))
        }
        PrimOpKind::UnsafeFreezeByteArray => {
            // Freeze = identity (same Arc, just typed differently)
            Ok(args[0].clone())
        }
        PrimOpKind::CopyByteArray | PrimOpKind::CopyMutableByteArray => {
            // copyByteArray# src src_off dst dst_off len
            let src_ba = expect_byte_array(&args[0])?;
            let src_off = expect_int(&args[1])? as usize;
            let dst_ba = expect_byte_array(&args[2])?;
            let dst_off = expect_int(&args[3])? as usize;
            let len = expect_int(&args[4])? as usize;
            // Clone src data first to avoid double-lock deadlock when src == dst
            let src_data: Vec<u8> = {
                let src = src_ba.lock().unwrap();
                src[src_off..src_off + len].to_vec()
            };
            {
                let mut dst = dst_ba.lock().unwrap();
                dst[dst_off..dst_off + len].copy_from_slice(&src_data);
            }
            Ok(Value::ByteArray(dst_ba.clone()))
        }
        PrimOpKind::CopyAddrToByteArray => {
            // copyAddrToByteArray# src dst dst_off len
            let src_bytes = match &args[0] {
                Value::Lit(Literal::LitString(bs)) => bs,
                other => {
                    return Err(EvalError::TypeMismatch {
                        expected: "Addr# (LitString)",
                        got: crate::error::ValueKind::Other(format!("{:?}", other)),
                    })
                }
            };
            let dst_ba = expect_byte_array(&args[1])?;
            let dst_off = expect_int(&args[2])? as usize;
            let len = expect_int(&args[3])? as usize;
            let mut dst = dst_ba.lock().unwrap();
            let src_end = std::cmp::min(src_bytes.len(), len);
            dst[dst_off..dst_off + src_end].copy_from_slice(&src_bytes[..src_end]);
            drop(dst);
            Ok(Value::ByteArray(dst_ba.clone()))
        }
        PrimOpKind::ShrinkMutableByteArray => {
            let ba = expect_byte_array(&args[0])?;
            let new_size = expect_int(&args[1])? as usize;
            ba.lock().unwrap().truncate(new_size);
            Ok(Value::ByteArray(ba.clone()))
        }
        PrimOpKind::ResizeMutableByteArray => {
            let ba = expect_byte_array(&args[0])?;
            let new_size = expect_int(&args[1])? as usize;
            let mut bytes = ba.lock().unwrap();
            bytes.resize(new_size, 0);
            drop(bytes);
            Ok(Value::ByteArray(ba.clone()))
        }
        PrimOpKind::Clz8 => {
            let w = expect_word(&args[0])?;
            Ok(Value::Lit(Literal::LitWord(
                (w as u8).leading_zeros() as u64
            )))
        }
        PrimOpKind::IntToInt64 => {
            // Identity on 64-bit: Int# == Int64#
            Ok(args[0].clone())
        }
        PrimOpKind::Int64ToWord64 => {
            let n = expect_int(&args[0])?;
            Ok(Value::Lit(Literal::LitWord(n as u64)))
        }
        PrimOpKind::TimesInt2Hi | PrimOpKind::TimesInt2Lo | PrimOpKind::TimesInt2Overflow => {
            let a = expect_int(&args[0])? as i128;
            let b = expect_int(&args[1])? as i128;
            let result = a * b;
            match op {
                PrimOpKind::TimesInt2Overflow => {
                    let overflowed = result > i64::MAX as i128 || result < i64::MIN as i128;
                    Ok(Value::Lit(Literal::LitInt(if overflowed { 1 } else { 0 })))
                }
                PrimOpKind::TimesInt2Hi => Ok(Value::Lit(Literal::LitInt((result >> 64) as i64))),
                PrimOpKind::TimesInt2Lo => Ok(Value::Lit(Literal::LitInt(result as i64))),
                _ => unreachable!(),
            }
        }

        PrimOpKind::IndexWord8Array => {
            let ba = expect_byte_array(&args[0])?;
            let idx = expect_int(&args[1])? as usize;
            let bytes = ba.lock().unwrap();
            let val = *bytes.get(idx).unwrap_or(&0);
            Ok(Value::Lit(Literal::LitWord(val as u64)))
        }
        PrimOpKind::IndexWord8OffAddr => {
            // indexWord8OffAddr# :: Addr# -> Int# -> Word8#
            let bytes = match &args[0] {
                Value::Lit(Literal::LitString(bs)) => bs,
                other => {
                    return Err(EvalError::TypeMismatch {
                        expected: "Addr# (LitString)",
                        got: crate::error::ValueKind::Other(format!("{:?}", other)),
                    })
                }
            };
            let idx = expect_int(&args[1])? as usize;
            let val = bytes.get(idx).copied().unwrap_or(0);
            Ok(Value::Lit(Literal::LitWord(val as u64)))
        }
        PrimOpKind::CompareByteArrays => {
            // compareByteArrays# ba1 off1 ba2 off2 len -> Int#
            let ba1 = expect_byte_array(&args[0])?;
            let off1 = expect_int(&args[1])? as usize;
            let ba2 = expect_byte_array(&args[2])?;
            let off2 = expect_int(&args[3])? as usize;
            let len = expect_int(&args[4])? as usize;
            // Clone to avoid potential double-lock if ba1 == ba2
            let slice1: Vec<u8> = {
                let b = ba1.lock().unwrap();
                b[off1..off1 + len].to_vec()
            };
            let slice2: Vec<u8> = {
                let b = ba2.lock().unwrap();
                b[off2..off2 + len].to_vec()
            };
            let result = slice1.cmp(&slice2);
            Ok(Value::Lit(Literal::LitInt(match result {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            })))
        }
        PrimOpKind::WordToWord8 => {
            // Narrow Word# to Word8# (mask to 8 bits)
            let w = expect_word(&args[0])?;
            Ok(Value::Lit(Literal::LitWord(w & 0xFF)))
        }
        PrimOpKind::Word64And => {
            let (a, b) = bin_op_word(op, &args)?;
            Ok(Value::Lit(Literal::LitWord(a & b)))
        }
        PrimOpKind::Int64ToInt => {
            // Identity on 64-bit
            Ok(args[0].clone())
        }
        PrimOpKind::Word64ToInt64 => {
            // Identity on 64-bit (reinterpret Word64 as Int64)
            let w = match &args[0] {
                Value::Lit(Literal::LitWord(w)) => *w,
                Value::Lit(Literal::LitInt(i)) => *i as u64,
                other => {
                    return Err(EvalError::TypeMismatch {
                        expected: "Word64#",
                        got: crate::error::ValueKind::Other(format!("{:?}", other)),
                    })
                }
            };
            Ok(Value::Lit(Literal::LitInt(w as i64)))
        }
        PrimOpKind::Word8ToWord => {
            // Widen Word8 to Word (identity, already fits)
            Ok(args[0].clone())
        }
        PrimOpKind::Word8Lt => {
            let (a, b) = bin_op_word(op, &args)?;
            Ok(Value::Lit(Literal::LitInt(if a < b { 1 } else { 0 })))
        }
        PrimOpKind::Int64Ge => {
            let (a, b) = bin_op_int(op, &args)?;
            Ok(Value::Lit(Literal::LitInt(if a >= b { 1 } else { 0 })))
        }
        PrimOpKind::Int64Negate => {
            let a = expect_int_like(&args[0])?;
            Ok(Value::Lit(Literal::LitInt(-a)))
        }
        PrimOpKind::Int64Shra => {
            let (a, b) = bin_op_int(op, &args)?;
            Ok(Value::Lit(Literal::LitInt(a >> (b as u32))))
        }
        PrimOpKind::Word64Shl => {
            let (a, b) = bin_op_word(op, &args)?;
            Ok(Value::Lit(Literal::LitWord(a << (b as u32))))
        }
        PrimOpKind::Word8Ge => {
            let (a, b) = bin_op_word(op, &args)?;
            Ok(Value::Lit(Literal::LitInt(if a >= b { 1 } else { 0 })))
        }
        PrimOpKind::Word8Sub => {
            let (a, b) = bin_op_word(op, &args)?;
            Ok(Value::Lit(Literal::LitWord(a.wrapping_sub(b) & 0xFF)))
        }
        PrimOpKind::SizeofByteArray => {
            let ba = expect_byte_array(&args[0])?;
            let len = ba.lock().unwrap().len();
            Ok(Value::Lit(Literal::LitInt(len as i64)))
        }
        PrimOpKind::IndexWordArray => {
            // indexWordArray# :: ByteArray# -> Int# -> Word#
            // Read a machine word (8 bytes on 64-bit) at index i (word-sized offset)
            let ba = expect_byte_array(&args[0])?;
            let idx = expect_int_like(&args[1])? as usize;
            let bytes = ba.lock().unwrap();
            let offset = idx * 8;
            if offset + 8 > bytes.len() {
                return Err(EvalError::TypeMismatch {
                    expected: "valid IndexWordArray index",
                    got: crate::error::ValueKind::Other(format!(
                        "index {} out of bounds (len={})",
                        idx,
                        bytes.len()
                    )),
                });
            }
            let word = u64::from_ne_bytes(bytes[offset..offset + 8].try_into().unwrap());
            Ok(Value::Lit(Literal::LitWord(word)))
        }
        PrimOpKind::Int64Mul => {
            let (a, b) = bin_op_int(op, &args)?;
            Ok(Value::Lit(Literal::LitInt(a.wrapping_mul(b))))
        }
        PrimOpKind::Word64Or => {
            let (a, b) = bin_op_word(op, &args)?;
            Ok(Value::Lit(Literal::LitWord(a | b)))
        }
        PrimOpKind::Word8Le => {
            let (a, b) = bin_op_word(op, &args)?;
            Ok(Value::Lit(Literal::LitInt(if a <= b { 1 } else { 0 })))
        }
        PrimOpKind::Int64Add => {
            let (a, b) = bin_op_int(op, &args)?;
            Ok(Value::Lit(Literal::LitInt(a.wrapping_add(b))))
        }
        PrimOpKind::Int64Gt => {
            let (a, b) = bin_op_int(op, &args)?;
            Ok(Value::Lit(Literal::LitInt(if a > b { 1 } else { 0 })))
        }
        PrimOpKind::Int64Lt => {
            let (a, b) = bin_op_int(op, &args)?;
            Ok(Value::Lit(Literal::LitInt(if a < b { 1 } else { 0 })))
        }
        PrimOpKind::Int64Le => {
            let (a, b) = bin_op_int(op, &args)?;
            Ok(Value::Lit(Literal::LitInt(if a <= b { 1 } else { 0 })))
        }
        PrimOpKind::Int64Sub => {
            let (a, b) = bin_op_int(op, &args)?;
            Ok(Value::Lit(Literal::LitInt(a.wrapping_sub(b))))
        }
        PrimOpKind::Int64Shl => {
            let (a, b) = bin_op_int(op, &args)?;
            Ok(Value::Lit(Literal::LitInt(a.wrapping_shl(b as u32))))
        }
        PrimOpKind::Word8Add => {
            let (a, b) = bin_op_word(op, &args)?;
            Ok(Value::Lit(Literal::LitWord((a.wrapping_add(b)) & 0xFF)))
        }
        PrimOpKind::WriteWordArray => {
            // writeWordArray# :: MutableByteArray# -> Int# -> Word# -> State# -> State#
            let ba = expect_byte_array(&args[0])?;
            let idx = expect_int_like(&args[1])? as usize;
            let val = expect_word(&args[2])?;
            let offset = idx * 8;
            let mut bytes = ba.lock().unwrap();
            if offset + 8 <= bytes.len() {
                bytes[offset..offset + 8].copy_from_slice(&val.to_ne_bytes());
            }
            Ok(Value::ByteArray(ba.clone()))
        }
        PrimOpKind::ReadWordArray => {
            // readWordArray# :: MutableByteArray# -> Int# -> State# -> (# State#, Word# #)
            let ba = expect_byte_array(&args[0])?;
            let idx = expect_int_like(&args[1])? as usize;
            let bytes = ba.lock().unwrap();
            let offset = idx * 8;
            if offset + 8 > bytes.len() {
                return Err(EvalError::TypeMismatch {
                    expected: "valid ReadWordArray index",
                    got: crate::error::ValueKind::Other(format!(
                        "index {} out of bounds (len={})",
                        idx,
                        bytes.len()
                    )),
                });
            }
            let word = u64::from_ne_bytes(bytes[offset..offset + 8].try_into().unwrap());
            Ok(Value::Lit(Literal::LitWord(word)))
        }
        PrimOpKind::SetByteArray => {
            // setByteArray# :: MutableByteArray# -> Int# -> Int# -> Int# -> State# -> State#
            // Fill `count` bytes starting at `offset` with byte `val`
            let ba = expect_byte_array(&args[0])?;
            let offset = expect_int_like(&args[1])? as usize;
            let count = expect_int_like(&args[2])? as usize;
            let val = expect_int_like(&args[3])? as u8;
            let mut bytes = ba.lock().unwrap();
            let end = (offset + count).min(bytes.len());
            for b in &mut bytes[offset..end] {
                *b = val;
            }
            Ok(Value::ByteArray(ba.clone()))
        }
        PrimOpKind::AddIntCVal => {
            // addIntC# returns (# result, carry #). This is the result component.
            let (a, b) = bin_op_int(op, &args)?;
            Ok(Value::Lit(Literal::LitInt(a.wrapping_add(b))))
        }
        PrimOpKind::AddIntCCarry => {
            // addIntC# carry flag: 1 if overflow, 0 otherwise
            let (a, b) = bin_op_int(op, &args)?;
            let overflow = a.checked_add(b).is_none();
            Ok(Value::Lit(Literal::LitInt(if overflow { 1 } else { 0 })))
        }
        PrimOpKind::SubWordCVal => {
            // subWordC# result component: a - b (wrapping)
            let (a, b) = bin_op_word(op, &args)?;
            Ok(Value::Lit(Literal::LitWord(a.wrapping_sub(b))))
        }
        PrimOpKind::SubWordCCarry => {
            // subWordC# carry: 1 if borrow (a < b), 0 otherwise
            let (a, b) = bin_op_word(op, &args)?;
            Ok(Value::Lit(Literal::LitInt(if a < b { 1 } else { 0 })))
        }
        PrimOpKind::AddWordCVal => {
            let (a, b) = bin_op_word(op, &args)?;
            Ok(Value::Lit(Literal::LitWord(a.wrapping_add(b))))
        }
        PrimOpKind::AddWordCCarry => {
            let (a, b) = bin_op_word(op, &args)?;
            let carry = a.checked_add(b).is_none();
            Ok(Value::Lit(Literal::LitInt(if carry { 1 } else { 0 })))
        }
        PrimOpKind::TimesWord2Hi => {
            // timesWord2# high word: (a * b) >> 64
            let (a, b) = bin_op_word(op, &args)?;
            let product = (a as u128) * (b as u128);
            Ok(Value::Lit(Literal::LitWord((product >> 64) as u64)))
        }
        PrimOpKind::TimesWord2Lo => {
            // timesWord2# low word: (a * b) & 0xFFFFFFFFFFFFFFFF
            let (a, b) = bin_op_word(op, &args)?;
            Ok(Value::Lit(Literal::LitWord(a.wrapping_mul(b))))
        }
        PrimOpKind::QuotRemWordVal => {
            // quotRemWord# quotient: a / b
            let (a, b) = bin_op_word(op, &args)?;
            Ok(Value::Lit(Literal::LitWord(a / b)))
        }
        PrimOpKind::QuotRemWordRem => {
            // quotRemWord# remainder: a % b
            let (a, b) = bin_op_word(op, &args)?;
            Ok(Value::Lit(Literal::LitWord(a % b)))
        }

        // --- FFI intrinsics ---
        PrimOpKind::FfiStrlen => {
            // strlen(Addr#) -> Int#: count bytes until null terminator
            let bytes = match &args[0] {
                Value::Lit(Literal::LitString(bs)) => bs,
                other => {
                    return Err(EvalError::TypeMismatch {
                        expected: "Addr# (LitString)",
                        got: crate::error::ValueKind::Other(format!("{:?}", other)),
                    })
                }
            };
            let len = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
            Ok(Value::Lit(Literal::LitInt(len as i64)))
        }
        PrimOpKind::FfiTextMeasureOff => {
            // _hs_text_measure_off(ByteArray#, off, len) -> Int#
            // Walk UTF-8 bytes counting `len` chars, return byte offset
            let ba = expect_byte_array(&args[0])?;
            let off = expect_int_like(&args[1])? as usize;
            let n_chars = expect_int_like(&args[2])? as usize;
            let bytes = ba.lock().unwrap();
            let slice = &bytes[off..];
            let mut byte_count = 0usize;
            let mut chars_counted = 0usize;
            while chars_counted < n_chars && byte_count < slice.len() {
                let b = slice[byte_count];
                let char_len = if b < 0x80 {
                    1
                } else if b < 0xE0 {
                    2
                } else if b < 0xF0 {
                    3
                } else {
                    4
                };
                byte_count += char_len;
                chars_counted += 1;
            }
            Ok(Value::Lit(Literal::LitInt(byte_count as i64)))
        }
        PrimOpKind::FfiTextMemchr => {
            // _hs_text_memchr(ByteArray#, off, len, byte) -> Int#
            // Find byte in array starting at off, return RELATIVE offset from off, or -1
            // Matches C: ptr - (arr + off), NOT absolute position
            let ba = expect_byte_array(&args[0])?;
            let off = expect_int_like(&args[1])? as usize;
            let len = expect_int_like(&args[2])? as usize;
            let needle = expect_int_like(&args[3])? as u8;
            let bytes = ba.lock().unwrap();
            let end = std::cmp::min(off + len, bytes.len());
            let result = bytes[off..end].iter().position(|&b| b == needle);
            Ok(Value::Lit(Literal::LitInt(match result {
                Some(pos) => pos as i64,
                None => -1,
            })))
        }
        PrimOpKind::FfiTextReverse => {
            // _hs_text_reverse(dst, src, off, len) -> void
            // Reverse UTF-8 chars from src[off..off+len] into dst
            let dst_ba = expect_byte_array(&args[0])?;
            let src_ba = expect_byte_array(&args[1])?;
            let off = expect_int_like(&args[2])? as usize;
            let len = expect_int_like(&args[3])? as usize;
            // Clone src to avoid double-lock if src == dst
            let src_slice: Vec<u8> = {
                let src = src_ba.lock().unwrap();
                src[off..off + len].to_vec()
            };
            // Parse UTF-8 chars and reverse
            let mut chars: Vec<&[u8]> = Vec::new();
            let mut i = 0;
            while i < src_slice.len() {
                let b = src_slice[i];
                let char_len = if b < 0x80 {
                    1
                } else if b < 0xE0 {
                    2
                } else if b < 0xF0 {
                    3
                } else {
                    4
                };
                let end = std::cmp::min(i + char_len, src_slice.len());
                chars.push(&src_slice[i..end]);
                i = end;
            }
            chars.reverse();
            let reversed: Vec<u8> = chars.into_iter().flatten().copied().collect();
            {
                let mut dst = dst_ba.lock().unwrap();
                let copy_len = std::cmp::min(reversed.len(), dst.len());
                dst[..copy_len].copy_from_slice(&reversed[..copy_len]);
            }
            Ok(Value::ByteArray(dst_ba.clone()))
        }

        // SmallArray# / Array# / PopCnt / Ctz — only used by JIT, not tree-walker
        PrimOpKind::NewSmallArray
        | PrimOpKind::ReadSmallArray
        | PrimOpKind::WriteSmallArray
        | PrimOpKind::IndexSmallArray
        | PrimOpKind::SizeofSmallArray
        | PrimOpKind::SizeofSmallMutableArray
        | PrimOpKind::UnsafeFreezeSmallArray
        | PrimOpKind::UnsafeThawSmallArray
        | PrimOpKind::CopySmallArray
        | PrimOpKind::CopySmallMutableArray
        | PrimOpKind::CloneSmallArray
        | PrimOpKind::CloneSmallMutableArray
        | PrimOpKind::ShrinkSmallMutableArray
        | PrimOpKind::CasSmallArray
        | PrimOpKind::NewArray
        | PrimOpKind::ReadArray
        | PrimOpKind::WriteArray
        | PrimOpKind::SizeofArray
        | PrimOpKind::SizeofMutableArray
        | PrimOpKind::UnsafeFreezeArray
        | PrimOpKind::UnsafeThawArray
        | PrimOpKind::CopyArray
        | PrimOpKind::CopyMutableArray
        | PrimOpKind::CloneArray
        | PrimOpKind::CloneMutableArray
        | PrimOpKind::PopCnt
        | PrimOpKind::PopCnt8
        | PrimOpKind::PopCnt16
        | PrimOpKind::PopCnt32
        | PrimOpKind::PopCnt64
        | PrimOpKind::Ctz
        | PrimOpKind::Ctz8
        | PrimOpKind::Ctz16
        | PrimOpKind::Ctz32
        | PrimOpKind::Ctz64 => Err(EvalError::UnsupportedPrimOp(op)),
    }
}

fn expect_byte_array(v: &Value) -> Result<&crate::value::SharedByteArray, EvalError> {
    if let Value::ByteArray(ba) = v {
        Ok(ba)
    } else {
        Err(EvalError::TypeMismatch {
            expected: "ByteArray#",
            got: crate::error::ValueKind::Other(format!("{:?}", v)),
        })
    }
}

/// Accept both LitInt and LitWord — FFI args go through Int→Int64→Word64 conversions
fn expect_int_like(v: &Value) -> Result<i64, EvalError> {
    match v {
        Value::Lit(Literal::LitInt(n)) => Ok(*n),
        Value::Lit(Literal::LitWord(n)) => Ok(*n as i64),
        _ => Err(EvalError::TypeMismatch {
            expected: "Int# or Word#",
            got: crate::error::ValueKind::Other(format!("{:?}", v)),
        }),
    }
}

fn expect_int(v: &Value) -> Result<i64, EvalError> {
    if let Value::Lit(Literal::LitInt(n)) = v {
        Ok(*n)
    } else {
        Err(EvalError::TypeMismatch {
            expected: "Int#",
            got: crate::error::ValueKind::Other(format!("{:?}", v)),
        })
    }
}

fn expect_word(v: &Value) -> Result<u64, EvalError> {
    match v {
        Value::Lit(Literal::LitWord(n)) => Ok(*n),
        Value::Lit(Literal::LitInt(n)) => Ok(*n as u64),
        _ => Err(EvalError::TypeMismatch {
            expected: "Word#",
            got: crate::error::ValueKind::Other(format!("{:?}", v)),
        }),
    }
}

fn expect_double(v: &Value) -> Result<f64, EvalError> {
    if let Value::Lit(Literal::LitDouble(bits)) = v {
        Ok(f64::from_bits(*bits))
    } else {
        Err(EvalError::TypeMismatch {
            expected: "Double#",
            got: crate::error::ValueKind::Other(format!("{:?}", v)),
        })
    }
}

fn expect_float(v: &Value) -> Result<f32, EvalError> {
    if let Value::Lit(Literal::LitFloat(bits)) = v {
        Ok(f32::from_bits(*bits as u32))
    } else {
        Err(EvalError::TypeMismatch {
            expected: "Float#",
            got: crate::error::ValueKind::Other(format!("{:?}", v)),
        })
    }
}

fn expect_char(v: &Value) -> Result<char, EvalError> {
    if let Value::Lit(Literal::LitChar(c)) = v {
        Ok(*c)
    } else {
        Err(EvalError::TypeMismatch {
            expected: "Char#",
            got: crate::error::ValueKind::Other(format!("{:?}", v)),
        })
    }
}

fn bin_op_int(_op: PrimOpKind, args: &[Value]) -> Result<(i64, i64), EvalError> {
    if args.len() != 2 {
        return Err(EvalError::ArityMismatch {
            context: "arguments",
            expected: 2,
            got: args.len(),
        });
    }
    Ok((expect_int(&args[0])?, expect_int(&args[1])?))
}

fn bin_op_word(_op: PrimOpKind, args: &[Value]) -> Result<(u64, u64), EvalError> {
    if args.len() != 2 {
        return Err(EvalError::ArityMismatch {
            context: "arguments",
            expected: 2,
            got: args.len(),
        });
    }
    Ok((expect_word(&args[0])?, expect_word(&args[1])?))
}

fn eval_decode_double_int64(d: f64) -> (i64, i64) {
    if d == 0.0 || d.is_nan() {
        return (0, 0);
    }
    if d.is_infinite() {
        return (if d > 0.0 { 1 } else { -1 }, 0);
    }
    let bits = d.to_bits();
    let sign: i64 = if bits >> 63 == 0 { 1 } else { -1 };
    let raw_exp = ((bits >> 52) & 0x7ff) as i32;
    let raw_man = (bits & 0x000f_ffff_ffff_ffff) as i64;
    let (man, exp) = if raw_exp == 0 {
        (raw_man, 1 - 1023 - 52)
    } else {
        (raw_man | (1i64 << 52), raw_exp - 1023 - 52)
    };
    let man = sign * man;
    if man != 0 {
        let tz = man.unsigned_abs().trailing_zeros();
        (man >> tz, (exp + tz as i32) as i64)
    } else {
        (0, 0)
    }
}

/// Format a Double matching Haskell's `show` output.
fn eval_haskell_show_double(d: f64) -> String {
    if d.is_nan() {
        return "NaN".to_string();
    }
    if d.is_infinite() {
        return if d > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    if d == 0.0 {
        return if d.is_sign_negative() { "-0.0" } else { "0.0" }.to_string();
    }
    let abs = d.abs();
    if (0.1..1.0e7).contains(&abs) {
        let s = format!("{}", d);
        if s.contains('.') {
            s
        } else {
            format!("{}.0", s)
        }
    } else {
        format!("{:e}", d)
    }
}

fn bin_op_double(_op: PrimOpKind, args: &[Value]) -> Result<(f64, f64), EvalError> {
    if args.len() != 2 {
        return Err(EvalError::ArityMismatch {
            context: "arguments",
            expected: 2,
            got: args.len(),
        });
    }
    Ok((expect_double(&args[0])?, expect_double(&args[1])?))
}

fn bin_op_float(_op: PrimOpKind, args: &[Value]) -> Result<(f32, f32), EvalError> {
    if args.len() != 2 {
        return Err(EvalError::ArityMismatch {
            context: "arguments",
            expected: 2,
            got: args.len(),
        });
    }
    Ok((expect_float(&args[0])?, expect_float(&args[1])?))
}

fn cmp_int(
    op: PrimOpKind,
    args: &[Value],
    f: impl Fn(i64, i64) -> bool,
) -> Result<Value, EvalError> {
    let (a, b) = bin_op_int(op, args)?;
    Ok(Value::Lit(Literal::LitInt(if f(a, b) { 1 } else { 0 })))
}

fn cmp_word(
    op: PrimOpKind,
    args: &[Value],
    f: impl Fn(u64, u64) -> bool,
) -> Result<Value, EvalError> {
    let (a, b) = bin_op_word(op, args)?;
    Ok(Value::Lit(Literal::LitInt(if f(a, b) { 1 } else { 0 })))
}

fn cmp_double(
    op: PrimOpKind,
    args: &[Value],
    f: impl Fn(f64, f64) -> bool,
) -> Result<Value, EvalError> {
    let (a, b) = bin_op_double(op, args)?;
    Ok(Value::Lit(Literal::LitInt(if f(a, b) { 1 } else { 0 })))
}

fn cmp_float(
    op: PrimOpKind,
    args: &[Value],
    f: impl Fn(f32, f32) -> bool,
) -> Result<Value, EvalError> {
    let (a, b) = bin_op_float(op, args)?;
    Ok(Value::Lit(Literal::LitInt(if f(a, b) { 1 } else { 0 })))
}

fn cmp_char(
    _op: PrimOpKind,
    args: &[Value],
    f: impl Fn(char, char) -> bool,
) -> Result<Value, EvalError> {
    if args.len() != 2 {
        return Err(EvalError::ArityMismatch {
            context: "arguments",
            expected: 2,
            got: args.len(),
        });
    }
    let a = expect_char(&args[0])?;
    let b = expect_char(&args[1])?;
    Ok(Value::Lit(Literal::LitInt(if f(a, b) { 1 } else { 0 })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::{Alt, AltCon, CoreFrame, DataConId, JoinId, Literal, RecursiveTree, VarId};

    #[test]
    fn test_eval_lit() {
        let expr = RecursiveTree {
            nodes: vec![CoreFrame::Lit(Literal::LitInt(42))],
        };
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
        let expr = RecursiveTree {
            nodes: vec![CoreFrame::Var(VarId(1))],
        };
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
        let expr = RecursiveTree {
            nodes: vec![CoreFrame::Var(VarId(1))],
        };
        let mut heap = crate::heap::VecHeap::new();
        let res = eval(&expr, &Env::new(), &mut heap);
        assert!(matches!(res, Err(EvalError::UnboundVar(VarId(1)))));
    }

    #[test]
    fn test_eval_lam_identity() {
        let nodes = vec![
            CoreFrame::Var(VarId(1)),
            CoreFrame::Lam {
                binder: VarId(1),
                body: 0,
            },
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
            CoreFrame::LetNonRec {
                binder: VarId(1),
                rhs: 0,
                body: 1,
            },
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
            CoreFrame::Con {
                tag: DataConId(1),
                fields: vec![0],
            },
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
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![0, 1],
            },
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
            CoreFrame::Con {
                tag: DataConId(1),
                fields: vec![0],
            },
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
            CoreFrame::Con {
                tag: DataConId(1),
                fields: vec![0],
            },
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
                    Alt {
                        con: AltCon::LitAlt(Literal::LitInt(1)),
                        binders: vec![],
                        body: 1,
                    },
                    Alt {
                        con: AltCon::LitAlt(Literal::LitInt(2)),
                        binders: vec![],
                        body: 2,
                    },
                    Alt {
                        con: AltCon::Default,
                        binders: vec![],
                        body: 3,
                    },
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
            CoreFrame::Con {
                tag: DataConId(5),
                fields: vec![],
            },
            CoreFrame::PrimOp {
                op: PrimOpKind::DataToTag,
                args: vec![0],
            },
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
            CoreFrame::Var(VarId(1)),           // 1: x
            CoreFrame::Var(VarId(2)),           // 2: y
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
            CoreFrame::Var(VarId(10)),           // 0: x
            CoreFrame::Lit(Literal::LitInt(42)), // 1
            CoreFrame::Jump {
                label: JoinId(1),
                args: vec![1],
            }, // 2
            CoreFrame::Join {
                label: JoinId(1),
                params: vec![VarId(10)],
                rhs: 0,
                body: 2,
            }, // 3
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
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![0, 1],
            }, // 2: x + y
            CoreFrame::Lit(Literal::LitInt(1)), // 3
            CoreFrame::Lit(Literal::LitInt(2)), // 4
            CoreFrame::Jump {
                label: JoinId(1),
                args: vec![3, 4],
            }, // 5
            CoreFrame::Join {
                label: JoinId(1),
                params: vec![VarId(10), VarId(11)],
                rhs: 2,
                body: 5,
            }, // 6
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
        let nodes = vec![CoreFrame::Jump {
            label: JoinId(1),
            args: vec![],
        }];
        let expr = CoreExpr { nodes };
        let mut heap = crate::heap::VecHeap::new();
        let res = eval(&expr, &Env::new(), &mut heap);
        assert!(matches!(res, Err(EvalError::UnboundJoin(JoinId(1)))));
    }

    #[test]
    fn test_thunk_lazy() {
        // let x = <unbound> in 42
        let nodes = vec![
            CoreFrame::Var(VarId(999)),          // 0: unbound
            CoreFrame::Lit(Literal::LitInt(42)), // 1
            CoreFrame::LetNonRec {
                binder: VarId(1),
                rhs: 0,
                body: 1,
            }, // 2
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
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![0, 0],
            }, // 1: 1 + 1
            CoreFrame::Var(VarId(1)),           // 2: x
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![2, 2],
            }, // 3: x + x
            CoreFrame::LetNonRec {
                binder: VarId(1),
                rhs: 1,
                body: 3,
            }, // 4
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
            CoreFrame::LetRec {
                bindings: vec![(VarId(1), 0)],
                body: 0,
            }, // 1
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
            CoreFrame::Var(VarId(11)),         // 0: a
            CoreFrame::Var(VarId(2)),          // 1: g
            CoreFrame::App { fun: 1, arg: 0 }, // 2: g a
            CoreFrame::Lam {
                binder: VarId(11),
                body: 2,
            }, // 3: \a -> g a (f)
            CoreFrame::Var(VarId(12)),         // 4: b
            CoreFrame::Lam {
                binder: VarId(12),
                body: 4,
            }, // 5: \b -> b (g)
            CoreFrame::Var(VarId(1)),          // 6: f
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
            CoreFrame::Var(VarId(10)),            // 1: x
            CoreFrame::Var(VarId(20)),            // 2: y (captured)
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![1, 2],
            }, // 3: x + y
            CoreFrame::Lit(Literal::LitInt(200)), // 4
            CoreFrame::Lit(Literal::LitInt(1)),   // 5
            CoreFrame::Jump {
                label: JoinId(1),
                args: vec![5],
            }, // 6
            CoreFrame::LetNonRec {
                binder: VarId(20),
                rhs: 4,
                body: 6,
            }, // 7: let y = 200 in jump j(1)
            CoreFrame::Join {
                label: JoinId(1),
                params: vec![VarId(10)],
                rhs: 3,
                body: 7,
            }, // 8: join j(x) = x+y in ...
            CoreFrame::LetNonRec {
                binder: VarId(20),
                rhs: 0,
                body: 8,
            }, // 9: let y = 100 in join ...
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
            CoreFrame::LetNonRec {
                binder: VarId(1),
                rhs: 0,
                body: 0,
            }, // 1: let x = unbound in x
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
            CoreFrame::Var(VarId(10)),          // 0: x
            CoreFrame::Lit(Literal::LitInt(1)), // 1
            CoreFrame::Lit(Literal::LitInt(2)), // 2
            CoreFrame::Jump {
                label: JoinId(1),
                args: vec![1, 2],
            }, // 3: jump j(1, 2)
            CoreFrame::Join {
                label: JoinId(1),
                params: vec![VarId(10)],
                rhs: 0,
                body: 3,
            }, // 4: join j(x) ...
        ];
        let expr = CoreExpr { nodes };
        let mut heap = crate::heap::VecHeap::new();
        let res = eval(&expr, &Env::new(), &mut heap);
        assert!(matches!(res, Err(EvalError::ArityMismatch { .. })));
    }

    #[test]
    fn test_eval_case_no_match() {
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(42)), // 0
            CoreFrame::Case {
                scrutinee: 0,
                binder: VarId(1),
                alts: vec![Alt {
                    con: AltCon::LitAlt(Literal::LitInt(1)),
                    binders: vec![],
                    body: 0,
                }],
            }, // 1
        ];
        let expr = CoreExpr { nodes };
        let mut heap = crate::heap::VecHeap::new();
        let res = eval(&expr, &Env::new(), &mut heap);
        assert!(matches!(res, Err(EvalError::NoMatchingAlt)));
    }

    #[test]
    fn test_eval_primop_arity_mismatch() {
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(1)), // 0
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![0], // IntAdd expects 2 args
            }, // 1
        ];
        let expr = CoreExpr { nodes };
        let mut heap = crate::heap::VecHeap::new();
        let res = eval(&expr, &Env::new(), &mut heap);
        assert!(matches!(res, Err(EvalError::ArityMismatch { .. })));
    }

    #[test]
    fn test_eval_force_non_thunk() {
        let mut heap = crate::heap::VecHeap::new();
        let val = Value::Lit(Literal::LitInt(42));
        let res = force(val, &mut heap).unwrap();
        if let Value::Lit(Literal::LitInt(n)) = res {
            assert_eq!(n, 42);
        } else {
            panic!("Expected LitInt(42)");
        }
    }

    #[test]
    fn test_eval_deeply_nested_con() {
        // Con(2, [Con(1, [Lit(42)])])
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(42)), // 0
            CoreFrame::Con {
                tag: DataConId(1),
                fields: vec![0],
            }, // 1
            CoreFrame::Con {
                tag: DataConId(2),
                fields: vec![1],
            }, // 2
        ];
        let expr = CoreExpr { nodes };
        let mut heap = crate::heap::VecHeap::new();
        let res = eval(&expr, &Env::new(), &mut heap).unwrap();
        match res {
            Value::Con(tag, fields) => {
                assert_eq!(tag.0, 2);
                assert_eq!(fields.len(), 1);
                match &fields[0] {
                    Value::Con(tag2, fields2) => {
                        assert_eq!(tag2.0, 1);
                        assert_eq!(fields2.len(), 1);
                        match &fields2[0] {
                            Value::Lit(Literal::LitInt(n)) => assert_eq!(*n, 42),
                            _ => panic!("Expected LitInt(42)"),
                        }
                    }
                    _ => panic!("Expected inner Con"),
                }
            }
            _ => panic!("Expected outer Con"),
        }
    }

    #[test]
    fn test_eval_case_default_binder() {
        // case 42 of x { DEFAULT -> x }
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(42)), // 0
            CoreFrame::Var(VarId(1)),            // 1: x
            CoreFrame::Case {
                scrutinee: 0,
                binder: VarId(1),
                alts: vec![Alt {
                    con: AltCon::Default,
                    binders: vec![],
                    body: 1,
                }],
            }, // 2
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
    fn test_eval_empty_con() {
        let nodes = vec![CoreFrame::Con {
            tag: DataConId(1),
            fields: vec![],
        }];
        let expr = CoreExpr { nodes };
        let mut heap = crate::heap::VecHeap::new();
        let res = eval(&expr, &Env::new(), &mut heap).unwrap();
        if let Value::Con(tag, fields) = res {
            assert_eq!(tag.0, 1);
            assert!(fields.is_empty());
        } else {
            panic!("Expected empty Con");
        }
    }

    #[test]
    fn test_eval_unbound_var_lazy() {
        // let x = 1 in let y = unbound in x
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(1)), // 0
            CoreFrame::Var(VarId(999)),         // 1: unbound
            CoreFrame::Var(VarId(1)),           // 2: x
            CoreFrame::LetNonRec {
                binder: VarId(2),
                rhs: 1,
                body: 2,
            }, // 3: let y = unbound in x
            CoreFrame::LetNonRec {
                binder: VarId(1),
                rhs: 0,
                body: 3,
            }, // 4: let x = 1 in ...
        ];
        let expr = CoreExpr { nodes };
        let mut heap = crate::heap::VecHeap::new();
        // Since y is not forced, this should SUCCEED and return 1
        let res = eval(&expr, &Env::new(), &mut heap).unwrap();
        if let Value::Lit(Literal::LitInt(n)) = res {
            assert_eq!(n, 1);
        } else {
            panic!("Expected LitInt(1)");
        }
    }

    #[test]
    fn test_eval_unbound_var_forced_err() {
        // let x = 1 in let y = unbound in y
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(1)), // 0
            CoreFrame::Var(VarId(999)),         // 1: unbound
            CoreFrame::Var(VarId(2)),           // 2: y
            CoreFrame::LetNonRec {
                binder: VarId(2),
                rhs: 1,
                body: 2,
            }, // 3: let y = unbound in y
            CoreFrame::LetNonRec {
                binder: VarId(1),
                rhs: 0,
                body: 3,
            }, // 4: let x = 1 in ...
        ];
        let expr = CoreExpr { nodes };
        let mut heap = crate::heap::VecHeap::new();
        // Now y is forced, should FAIL
        let res = eval(&expr, &Env::new(), &mut heap);
        assert!(matches!(res, Err(EvalError::UnboundVar(VarId(999)))));
    }
}
