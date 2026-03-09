use super::builder::TreeBuilder;
use super::types::SimpleType;
use proptest::prelude::*;
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use tidepool_repr::*;

/// Generate a random well-typed CoreExpr.
pub fn arb_core_expr() -> impl Strategy<Value = RecursiveTree<CoreFrame<usize>>> {
    arb_simple_type().prop_flat_map(|ty| arb_typed_expr(ty, 3)) // depth limit reduced to 3
}

/// Ground types: no Fun at any level. Values of ground type are always
/// structurally comparable (never closures).
fn arb_ground_type() -> impl Strategy<Value = SimpleType> {
    let leaf = prop_oneof![
        Just(SimpleType::Int),
        Just(SimpleType::Word),
        Just(SimpleType::Double),
        Just(SimpleType::Float),
        Just(SimpleType::Bool),
        Just(SimpleType::Char),
    ];
    leaf.prop_recursive(2, 5, 2, |inner| {
        prop_oneof![
            inner.clone().prop_map(|t| SimpleType::Maybe(Box::new(t))),
            (inner.clone(), inner).prop_map(|(a, b)| SimpleType::Pair(Box::new(a), Box::new(b))),
            // No Fun — ground types only
        ]
    })
}

/// Generate a random well-typed CoreExpr whose result type is ground.
/// The expression itself may contain Lam nodes (via gen_app's internal
/// Fun synthesis), but the top-level value is always non-closure and
/// structurally comparable.
pub fn arb_ground_expr() -> impl Strategy<Value = RecursiveTree<CoreFrame<usize>>> {
    arb_ground_type().prop_flat_map(|ty| arb_typed_expr(ty, 3))
}

fn arb_simple_type() -> impl Strategy<Value = SimpleType> {
    let leaf = prop_oneof![
        Just(SimpleType::Int),
        Just(SimpleType::Word),
        Just(SimpleType::Double),
        Just(SimpleType::Float),
        Just(SimpleType::Bool),
        Just(SimpleType::Char),
    ];
    leaf.prop_recursive(2, 5, 2, |inner| {
        // reduced recursive parameters
        prop_oneof![
            inner.clone().prop_map(|t| SimpleType::Maybe(Box::new(t))),
            (inner.clone(), inner.clone())
                .prop_map(|(a, b)| SimpleType::Pair(Box::new(a), Box::new(b))),
            (inner.clone(), inner).prop_map(|(a, b)| SimpleType::Fun(Box::new(a), Box::new(b))),
        ]
    })
}

#[derive(Clone, Debug)]
struct Context {
    vars: HashMap<VarId, SimpleType>,
    next_var: Rc<Cell<u64>>,
    next_join: Rc<Cell<u64>>,
}

impl Context {
    fn new() -> Self {
        Self {
            vars: HashMap::new(),
            next_var: Rc::new(Cell::new(0)),
            next_join: Rc::new(Cell::new(0)),
        }
    }

    fn add_var(&mut self, ty: SimpleType) -> VarId {
        let val = self.next_var.get();
        let id = VarId(val);
        self.next_var.set(val + 1);
        self.vars.insert(id, ty);
        id
    }

    fn add_join(&mut self) -> JoinId {
        let val = self.next_join.get();
        let id = JoinId(val);
        self.next_join.set(val + 1);
        id
    }

    fn vars_of_type(&self, ty: &SimpleType) -> Vec<VarId> {
        let mut matching = self
            .vars
            .iter()
            .filter(|(_, v_ty)| *v_ty == ty)
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        matching.sort_by_key(|v| v.0); // Deterministic order for proptest
        matching
    }
}

fn arb_typed_expr(
    ty: SimpleType,
    depth: u32,
) -> impl Strategy<Value = RecursiveTree<CoreFrame<usize>>> {
    gen_expr(ty, depth, Context::new()).prop_map(|(builder, _root)| builder.build())
}

fn gen_expr(ty: SimpleType, depth: u32, ctx: Context) -> BoxedStrategy<(TreeBuilder, usize)> {
    if depth == 0 {
        return gen_leaf(ty, ctx);
    }

    // Multiple clones of ty and ctx are needed to satisfy proptest move semantics
    // in the prop_oneof! macro below.
    let ty2 = ty.clone();
    let ty3 = ty.clone();
    let ty4 = ty.clone();
    let ty5 = ty.clone();
    let ty6 = ty.clone();
    let ty7 = ty.clone();
    let ty8 = ty.clone();
    let ctx2 = ctx.clone();
    let ctx3 = ctx.clone();
    let ctx4 = ctx.clone();
    let ctx5 = ctx.clone();
    let ctx6 = ctx.clone();
    let ctx7 = ctx.clone();
    let ctx8 = ctx.clone();

    prop_oneof![
        3 => gen_leaf(ty.clone(), ctx.clone()),
        5 => gen_app(ty2, depth - 1, ctx2),
        2 => gen_lam(ty3, depth - 1, ctx3),
        2 => gen_let_non_rec(ty4, depth - 1, ctx4),
        1 => gen_let_rec(ty5, depth - 1, ctx5),
        2 => gen_case(ty6, depth - 1, ctx6),
        2 => gen_con(ty7, depth - 1, ctx7),
        1 => gen_join_jump(ty8, depth - 1, ctx8),
        1 => gen_prim_op(ty, depth - 1, ctx),
    ]
    .boxed()
}

fn gen_leaf(ty: SimpleType, ctx: Context) -> BoxedStrategy<(TreeBuilder, usize)> {
    let vars = ctx.vars_of_type(&ty);
    let mut strategies = Vec::new();

    if !vars.is_empty() {
        let var_strat = prop::sample::select(vars).prop_map(|v| {
            let mut builder = TreeBuilder::new();
            let idx = builder.push(CoreFrame::Var(v));
            (builder, idx)
        });
        strategies.push(var_strat.boxed());
    }

    // LitWord, LitDouble, and LitFloat are now also supported.
    match &ty {
        SimpleType::Int => {
            strategies.push(
                any::<i64>()
                    .prop_map(|i| {
                        let mut builder = TreeBuilder::new();
                        let idx = builder.push(CoreFrame::Lit(Literal::LitInt(i)));
                        (builder, idx)
                    })
                    .boxed(),
            );
        }
        SimpleType::Word => {
            strategies.push(
                any::<u64>()
                    .prop_map(|w| {
                        let mut builder = TreeBuilder::new();
                        let idx = builder.push(CoreFrame::Lit(Literal::LitWord(w)));
                        (builder, idx)
                    })
                    .boxed(),
            );
        }
        SimpleType::Double => {
            strategies.push(
                any::<f64>()
                    .prop_map(|d| {
                        let mut builder = TreeBuilder::new();
                        let idx = builder.push(CoreFrame::Lit(Literal::LitDouble(d.to_bits())));
                        (builder, idx)
                    })
                    .boxed(),
            );
        }
        SimpleType::Float => {
            strategies.push(
                any::<f32>()
                    .prop_map(|f| {
                        let mut builder = TreeBuilder::new();
                        let idx =
                            builder.push(CoreFrame::Lit(Literal::LitFloat(f.to_bits() as u64)));
                        (builder, idx)
                    })
                    .boxed(),
            );
        }
        SimpleType::Bool => {
            strategies.push(
                any::<bool>()
                    .prop_map(|b| {
                        let mut builder = TreeBuilder::new();
                        let tag = if b { DataConId(3) } else { DataConId(2) };
                        let idx = builder.push(CoreFrame::Con {
                            tag,
                            fields: vec![],
                        });
                        (builder, idx)
                    })
                    .boxed(),
            );
        }
        SimpleType::Char => {
            strategies.push(
                any::<char>()
                    .prop_map(|c| {
                        let mut builder = TreeBuilder::new();
                        let idx = builder.push(CoreFrame::Lit(Literal::LitChar(c)));
                        (builder, idx)
                    })
                    .boxed(),
            );
        }
        SimpleType::Maybe(_) => {
            strategies.push(
                Just(())
                    .prop_map(|_| {
                        let mut builder = TreeBuilder::new();
                        let idx = builder.push(CoreFrame::Con {
                            tag: DataConId(0),
                            fields: vec![],
                        });
                        (builder, idx)
                    })
                    .boxed(),
            );
        }
        _ => {}
    }

    if strategies.is_empty() {
        // Fallback for types that have no literal form (Fun, Pair).
        // CRITICAL: use depth=0 (not 1). Using depth=1 allows gen_expr to pick
        // gen_app, which calls arb_simple_type() to invent new Fun/Pair types.
        // Those types at depth=0 hit this same fallback at depth=1 again →
        // unbounded recursion → stack overflow. With depth=0, gen_expr only
        // calls gen_leaf, and recursion is bounded by type nesting depth.
        match ty {
            SimpleType::Fun(a, b) => {
                return gen_lam(SimpleType::Fun(a, b), 0, ctx);
            }
            SimpleType::Pair(a, b) => {
                return gen_con(SimpleType::Pair(a, b), 0, ctx);
            }
            _ => {
                panic!(
                    "unreachable fallback in gen_leaf: all SimpleType variants should be handled"
                );
            }
        }
    }

    let strat = strategies.remove(0);
    if strategies.is_empty() {
        strat
    } else {
        let mut final_strat = strat;
        for s in strategies {
            final_strat = prop_oneof![final_strat, s].boxed();
        }
        final_strat
    }
}

fn gen_app(ty: SimpleType, depth: u32, ctx: Context) -> BoxedStrategy<(TreeBuilder, usize)> {
    arb_simple_type()
        .prop_flat_map(move |arg_ty| {
            let fun_ty = SimpleType::Fun(Box::new(arg_ty.clone()), Box::new(ty.clone()));
            (
                gen_expr(fun_ty, depth, ctx.clone()),
                gen_expr(arg_ty, depth, ctx.clone()),
            )
        })
        .prop_map(|((mut b1, r1), (b2, r2))| {
            let offset = b1.push_tree(b2);
            let root = b1.push(CoreFrame::App {
                fun: r1,
                arg: r2 + offset,
            });
            (b1, root)
        })
        .boxed()
}

fn gen_lam(ty: SimpleType, depth: u32, mut ctx: Context) -> BoxedStrategy<(TreeBuilder, usize)> {
    match ty {
        SimpleType::Fun(a, b) => {
            let binder = ctx.add_var(*a);
            gen_expr(*b, depth, ctx)
                .prop_map(move |(mut builder, body)| {
                    let root = builder.push(CoreFrame::Lam { binder, body });
                    (builder, root)
                })
                .boxed()
        }
        _ => gen_leaf(ty, ctx),
    }
}

fn gen_let_non_rec(
    ty: SimpleType,
    depth: u32,
    ctx: Context,
) -> BoxedStrategy<(TreeBuilder, usize)> {
    arb_simple_type()
        .prop_flat_map(move |rhs_ty| {
            let mut ctx_body = ctx.clone();
            let binder = ctx_body.add_var(rhs_ty.clone());
            (
                gen_expr(rhs_ty, depth, ctx.clone()),
                gen_expr(ty.clone(), depth, ctx_body),
                Just(binder),
            )
        })
        .prop_map(|((mut b_rhs, r_rhs), (b_body, r_body), binder)| {
            let offset = b_rhs.push_tree(b_body);
            let root = b_rhs.push(CoreFrame::LetNonRec {
                binder,
                rhs: r_rhs,
                body: r_body + offset,
            });
            (b_rhs, root)
        })
        .boxed()
}

fn gen_let_rec(ty: SimpleType, depth: u32, ctx: Context) -> BoxedStrategy<(TreeBuilder, usize)> {
    prop::collection::vec(arb_simple_type(), 1..6)
        .prop_flat_map(move |rhs_tys| {
            let mut ctx_body = ctx.clone();
            let mut binders = Vec::new();
            for rty in &rhs_tys {
                binders.push(ctx_body.add_var(rty.clone()));
            }

            let mut rhs_vec_strat: BoxedStrategy<Vec<(TreeBuilder, usize)>> =
                Just(Vec::new()).boxed();

            for rty in rhs_tys {
                let ctx_rhs = ctx_body.clone();
                rhs_vec_strat = (rhs_vec_strat, gen_expr(rty.clone(), depth, ctx_rhs))
                    .prop_map(|(mut acc, res)| {
                        acc.push(res);
                        acc
                    })
                    .boxed();
            }

            (
                rhs_vec_strat,
                gen_expr(ty.clone(), depth, ctx_body),
                Just(binders),
            )
        })
        .prop_map(|(rhss, (b_body, r_body), binders)| {
            let mut builder = TreeBuilder::new();
            let mut bindings = Vec::new();

            for (i, (b_rhs, r_rhs)) in rhss.into_iter().enumerate() {
                let offset = builder.push_tree(b_rhs);
                bindings.push((binders[i], r_rhs + offset));
            }

            let body_offset = builder.push_tree(b_body);
            let root = builder.push(CoreFrame::LetRec {
                bindings,
                body: r_body + body_offset,
            });
            (builder, root)
        })
        .boxed()
}

fn gen_case(ty: SimpleType, depth: u32, ctx: Context) -> BoxedStrategy<(TreeBuilder, usize)> {
    let ty2 = ty.clone();
    let ty3 = ty.clone();
    let ty4 = ty.clone();
    let ty5 = ty.clone();
    let ctx2 = ctx.clone();
    let ctx3 = ctx.clone();
    let ctx4 = ctx.clone();
    let ctx5 = ctx.clone();

    prop_oneof![
        // Case on Maybe
        arb_simple_type()
            .prop_flat_map(move |inner_ty| {
                let scrut_ty = SimpleType::Maybe(Box::new(inner_ty.clone()));
                let mut ctx_alt = ctx2.clone();
                let binder = ctx_alt.add_var(scrut_ty.clone());

                let mut ctx_just = ctx_alt.clone();
                let just_binder = ctx_just.add_var(inner_ty);

                (
                    gen_expr(scrut_ty, depth, ctx2.clone()),
                    gen_expr(ty2.clone(), depth, ctx_alt),
                    gen_expr(ty2.clone(), depth, ctx_just),
                    Just((binder, just_binder)),
                )
            })
            .prop_map(
                |(
                    (mut builder, r_scrut),
                    (b_nothing, r_nothing),
                    (b_just, r_just),
                    (binder, just_binder),
                )| {
                    let off1 = builder.push_tree(b_nothing);
                    let off2 = builder.push_tree(b_just);

                    let alts = vec![
                        Alt {
                            con: AltCon::DataAlt(DataConId(0)),
                            binders: vec![],
                            body: r_nothing + off1,
                        },
                        Alt {
                            con: AltCon::DataAlt(DataConId(1)),
                            binders: vec![just_binder],
                            body: r_just + off2,
                        },
                    ];

                    let root = builder.push(CoreFrame::Case {
                        scrutinee: r_scrut,
                        binder,
                        alts,
                    });
                    (builder, root)
                }
            ),
        // Case on Bool
        Just(())
            .prop_flat_map(move |_| {
                let scrut_ty = SimpleType::Bool;
                let mut ctx_alt = ctx3.clone();
                let binder = ctx_alt.add_var(scrut_ty.clone());

                (
                    gen_expr(scrut_ty, depth, ctx3.clone()),
                    gen_expr(ty3.clone(), depth, ctx_alt.clone()),
                    gen_expr(ty3.clone(), depth, ctx_alt),
                    Just(binder),
                )
            })
            .prop_map(|((mut builder, r_scrut), (b_f, r_f), (b_t, r_t), binder)| {
                let off1 = builder.push_tree(b_f);
                let off2 = builder.push_tree(b_t);

                let alts = vec![
                    Alt {
                        con: AltCon::DataAlt(DataConId(2)),
                        binders: vec![],
                        body: r_f + off1,
                    },
                    Alt {
                        con: AltCon::DataAlt(DataConId(3)),
                        binders: vec![],
                        body: r_t + off2,
                    },
                ];

                let root = builder.push(CoreFrame::Case {
                    scrutinee: r_scrut,
                    binder,
                    alts,
                });
                (builder, root)
            }),
        // Case on Pair
        (arb_simple_type(), arb_simple_type())
            .prop_flat_map(move |(inner_a, inner_b)| {
                let scrut_ty =
                    SimpleType::Pair(Box::new(inner_a.clone()), Box::new(inner_b.clone()));
                let mut ctx_alt = ctx4.clone();
                let binder = ctx_alt.add_var(scrut_ty.clone());

                let mut ctx_body = ctx_alt.clone();
                let b1 = ctx_body.add_var(inner_a);
                let b2 = ctx_body.add_var(inner_b);

                (
                    gen_expr(scrut_ty, depth, ctx4.clone()),
                    gen_expr(ty4.clone(), depth, ctx_body),
                    Just((binder, b1, b2)),
                )
            })
            .prop_map(
                |((mut builder, r_scrut), (b_body, r_body), (binder, b1, b2))| {
                    let off = builder.push_tree(b_body);
                    let alts = vec![Alt {
                        con: AltCon::DataAlt(DataConId(4)),
                        binders: vec![b1, b2],
                        body: r_body + off,
                    }];
                    let root = builder.push(CoreFrame::Case {
                        scrutinee: r_scrut,
                        binder,
                        alts,
                    });
                    (builder, root)
                }
            ),
        // Case on Int with DEFAULT
        any::<i64>()
            .prop_flat_map(move |val| {
                let scrut_ty = SimpleType::Int;
                let mut ctx_alt = ctx5.clone();
                let binder = ctx_alt.add_var(scrut_ty.clone());

                (
                    gen_expr(scrut_ty, depth, ctx5.clone()),
                    gen_expr(ty5.clone(), depth, ctx_alt.clone()),
                    gen_expr(ty5.clone(), depth, ctx_alt),
                    Just((binder, val)),
                )
            })
            .prop_map(
                |((mut builder, r_scrut), (b_lit, r_lit), (b_def, r_def), (binder, val))| {
                    let off1 = builder.push_tree(b_lit);
                    let off2 = builder.push_tree(b_def);

                    let alts = vec![
                        Alt {
                            con: AltCon::LitAlt(Literal::LitInt(val)),
                            binders: vec![],
                            body: r_lit + off1,
                        },
                        Alt {
                            con: AltCon::Default,
                            binders: vec![],
                            body: r_def + off2,
                        },
                    ];

                    let root = builder.push(CoreFrame::Case {
                        scrutinee: r_scrut,
                        binder,
                        alts,
                    });
                    (builder, root)
                }
            ),
    ]
    .boxed()
}

fn gen_con(ty: SimpleType, depth: u32, ctx: Context) -> BoxedStrategy<(TreeBuilder, usize)> {
    match ty {
        SimpleType::Maybe(inner) => {
            prop_oneof![
                // Nothing
                Just(()).prop_map(|_| {
                    let mut builder = TreeBuilder::new();
                    let root = builder.push(CoreFrame::Con {
                        tag: DataConId(0),
                        fields: vec![],
                    });
                    (builder, root)
                }),
                // Just
                gen_expr(*inner, depth, ctx).prop_map(|(mut builder, r)| {
                    let root = builder.push(CoreFrame::Con {
                        tag: DataConId(1),
                        fields: vec![r],
                    });
                    (builder, root)
                })
            ]
            .boxed()
        }
        SimpleType::Bool => {
            prop_oneof![
                Just(DataConId(2)), // False
                Just(DataConId(3)), // True
            ]
            .prop_map(|tag| {
                let mut builder = TreeBuilder::new();
                let root = builder.push(CoreFrame::Con {
                    tag,
                    fields: vec![],
                });
                (builder, root)
            })
            .boxed()
        }
        SimpleType::Pair(a, b) => (gen_expr(*a, depth, ctx.clone()), gen_expr(*b, depth, ctx))
            .prop_map(|((mut builder, r1), (b2, r2))| {
                let off = builder.push_tree(b2);
                let root = builder.push(CoreFrame::Con {
                    tag: DataConId(4),
                    fields: vec![r1, r2 + off],
                });
                (builder, root)
            })
            .boxed(),
        _ => gen_leaf(ty, ctx),
    }
}

fn gen_join_jump(ty: SimpleType, depth: u32, ctx: Context) -> BoxedStrategy<(TreeBuilder, usize)> {
    let ty_c = ty.clone();
    let ctx_c = ctx.clone();
    prop::collection::vec(arb_simple_type(), 1..5)
        .prop_flat_map(move |arg_tys| {
            let mut ctx_rhs = ctx_c.clone();
            let mut params = Vec::new();
            for aty in &arg_tys {
                params.push(ctx_rhs.add_var(aty.clone()));
            }

            let mut ctx_body = ctx_c.clone();
            let label = ctx_body.add_join();

            (
                gen_expr(ty_c.clone(), depth, ctx_rhs),
                gen_jump(label, arg_tys, depth, ctx_body),
                Just((label, params)),
            )
        })
        .prop_map(|((mut b_rhs, r_rhs), (b_body, r_body), (label, params))| {
            let off = b_rhs.push_tree(b_body);
            let root = b_rhs.push(CoreFrame::Join {
                label,
                params,
                rhs: r_rhs,
                body: r_body + off,
            });
            (b_rhs, root)
        })
        .boxed()
}

fn gen_jump(
    label: JoinId,
    arg_tys: Vec<SimpleType>,
    depth: u32,
    ctx: Context,
) -> BoxedStrategy<(TreeBuilder, usize)> {
    let mut strat: BoxedStrategy<(TreeBuilder, Vec<usize>)> =
        Just((TreeBuilder::new(), Vec::new())).boxed();

    for aty in arg_tys {
        let aty_cloned = aty.clone();
        let ctx_cloned = ctx.clone();
        strat = (strat, gen_expr(aty_cloned, depth, ctx_cloned))
            .prop_map(move |((mut acc_builder, mut acc_args), (b, r))| {
                let off = acc_builder.push_tree(b);
                acc_args.push(r + off);
                (acc_builder, acc_args)
            })
            .boxed();
    }

    strat
        .prop_map(move |(mut builder, args)| {
            let root = builder.push(CoreFrame::Jump { label, args });
            (builder, root)
        })
        .boxed()
}

/// Represents a PrimOp with its argument type information.
#[derive(Clone, Debug)]
enum PrimOpSpec {
    /// Unary op: (op, arg_type)
    Unary(PrimOpKind, SimpleType),
    /// Binary op: (op, arg_type) — both args same type
    Binary(PrimOpKind, SimpleType),
    /// Division/remainder: (op, arg_type) — needs non-zero divisor guard
    DivOp(PrimOpKind, SimpleType),
}

fn gen_prim_op(ty: SimpleType, depth: u32, ctx: Context) -> BoxedStrategy<(TreeBuilder, usize)> {
    let mut ops: Vec<PrimOpSpec> = Vec::new();

    match &ty {
        SimpleType::Int => {
            // Same-type binary arithmetic
            for op in [
                PrimOpKind::IntAdd,
                PrimOpKind::IntSub,
                PrimOpKind::IntMul,
                PrimOpKind::IntAnd,
                PrimOpKind::IntOr,
                PrimOpKind::IntXor,
                PrimOpKind::IntShl,
                PrimOpKind::IntShra,
                PrimOpKind::IntShrl,
            ] {
                ops.push(PrimOpSpec::Binary(op, SimpleType::Int));
            }
            // Int comparisons (return Int 0/1)
            for op in [
                PrimOpKind::IntEq,
                PrimOpKind::IntNe,
                PrimOpKind::IntLt,
                PrimOpKind::IntLe,
                PrimOpKind::IntGt,
                PrimOpKind::IntGe,
            ] {
                ops.push(PrimOpSpec::Binary(op, SimpleType::Int));
            }
            // Word comparisons (return Int 0/1)
            for op in [
                PrimOpKind::WordEq,
                PrimOpKind::WordNe,
                PrimOpKind::WordLt,
                PrimOpKind::WordLe,
                PrimOpKind::WordGt,
                PrimOpKind::WordGe,
            ] {
                ops.push(PrimOpSpec::Binary(op, SimpleType::Word));
            }
            // Double comparisons (return Int 0/1)
            for op in [
                PrimOpKind::DoubleEq,
                PrimOpKind::DoubleNe,
                PrimOpKind::DoubleLt,
                PrimOpKind::DoubleLe,
                PrimOpKind::DoubleGt,
                PrimOpKind::DoubleGe,
            ] {
                ops.push(PrimOpSpec::Binary(op, SimpleType::Double));
            }
            // Float comparisons (return Int 0/1)
            for op in [
                PrimOpKind::FloatEq,
                PrimOpKind::FloatNe,
                PrimOpKind::FloatLt,
                PrimOpKind::FloatLe,
                PrimOpKind::FloatGt,
                PrimOpKind::FloatGe,
            ] {
                ops.push(PrimOpSpec::Binary(op, SimpleType::Float));
            }
            // Char comparisons (return Int 0/1)
            for op in [
                PrimOpKind::CharEq,
                PrimOpKind::CharNe,
                PrimOpKind::CharLt,
                PrimOpKind::CharLe,
                PrimOpKind::CharGt,
                PrimOpKind::CharGe,
            ] {
                ops.push(PrimOpSpec::Binary(op, SimpleType::Char));
            }
            // Conversions to Int
            ops.push(PrimOpSpec::Unary(
                PrimOpKind::Double2Int,
                SimpleType::Double,
            ));
            ops.push(PrimOpSpec::Unary(PrimOpKind::Word2Int, SimpleType::Word));
            ops.push(PrimOpSpec::Unary(PrimOpKind::Ord, SimpleType::Char));
            // Division with non-zero guard
            ops.push(PrimOpSpec::DivOp(PrimOpKind::IntQuot, SimpleType::Int));
            ops.push(PrimOpSpec::DivOp(PrimOpKind::IntRem, SimpleType::Int));
            // Unary
            ops.push(PrimOpSpec::Unary(PrimOpKind::IntNegate, SimpleType::Int));
            ops.push(PrimOpSpec::Unary(PrimOpKind::IntNot, SimpleType::Int));
        }
        SimpleType::Word => {
            for op in [
                PrimOpKind::WordAdd,
                PrimOpKind::WordSub,
                PrimOpKind::WordMul,
                PrimOpKind::WordAnd,
                PrimOpKind::WordOr,
                PrimOpKind::WordXor,
            ] {
                ops.push(PrimOpSpec::Binary(op, SimpleType::Word));
            }
            // Conversions to Word
            ops.push(PrimOpSpec::Unary(PrimOpKind::Int2Word, SimpleType::Int));
            // Division
            ops.push(PrimOpSpec::DivOp(PrimOpKind::WordQuot, SimpleType::Word));
            ops.push(PrimOpSpec::DivOp(PrimOpKind::WordRem, SimpleType::Word));
            // Unary
            ops.push(PrimOpSpec::Unary(PrimOpKind::WordNot, SimpleType::Word));
        }
        SimpleType::Double => {
            for op in [
                PrimOpKind::DoubleAdd,
                PrimOpKind::DoubleSub,
                PrimOpKind::DoubleMul,
                PrimOpKind::DoubleDiv,
            ] {
                ops.push(PrimOpSpec::Binary(op, SimpleType::Double));
            }
            // Conversions to Double
            ops.push(PrimOpSpec::Unary(PrimOpKind::Int2Double, SimpleType::Int));
            ops.push(PrimOpSpec::Unary(
                PrimOpKind::Float2Double,
                SimpleType::Float,
            ));
            // Unary
            ops.push(PrimOpSpec::Unary(
                PrimOpKind::DoubleNegate,
                SimpleType::Double,
            ));
        }
        SimpleType::Float => {
            for op in [
                PrimOpKind::FloatAdd,
                PrimOpKind::FloatSub,
                PrimOpKind::FloatMul,
                PrimOpKind::FloatDiv,
            ] {
                ops.push(PrimOpSpec::Binary(op, SimpleType::Float));
            }
            // Conversions to Float
            ops.push(PrimOpSpec::Unary(PrimOpKind::Int2Float, SimpleType::Int));
            // Unary
            ops.push(PrimOpSpec::Unary(
                PrimOpKind::FloatNegate,
                SimpleType::Float,
            ));
        }
        SimpleType::Char => {
            // No primops for Char — leaf generates valid chars directly via any::<char>().
            // Chr# from arbitrary Int mostly produces invalid codepoints (rejected by both
            // interpreter and JIT). GHC Core guarantees chr# receives valid codepoints.
            return gen_leaf(ty, ctx);
        }
        _ => return gen_leaf(ty, ctx),
    };

    if ops.is_empty() {
        return gen_leaf(ty, ctx);
    }

    prop::sample::select(ops)
        .prop_flat_map(move |spec| match spec {
            PrimOpSpec::Unary(op, arg_ty) => gen_expr(arg_ty, depth, ctx.clone())
                .prop_map(move |(mut builder, r1)| {
                    let root = builder.push(CoreFrame::PrimOp { op, args: vec![r1] });
                    (builder, root)
                })
                .boxed(),
            PrimOpSpec::Binary(op, arg_ty) => (
                gen_expr(arg_ty.clone(), depth, ctx.clone()),
                gen_expr(arg_ty, depth, ctx.clone()),
            )
                .prop_map(move |((mut builder, r1), (b2, r2))| {
                    let off = builder.push_tree(b2);
                    let root = builder.push(CoreFrame::PrimOp {
                        op,
                        args: vec![r1, r2 + off],
                    });
                    (builder, root)
                })
                .boxed(),
            PrimOpSpec::DivOp(op, arg_ty) => {
                // Generate a non-zero divisor: filter out 0
                let divisor_strat = match &arg_ty {
                    SimpleType::Int => (1i64..=i64::MAX)
                        .prop_map(|i| {
                            let mut builder = TreeBuilder::new();
                            let idx = builder.push(CoreFrame::Lit(Literal::LitInt(i)));
                            (builder, idx)
                        })
                        .boxed(),
                    SimpleType::Word => (1u64..=u64::MAX)
                        .prop_map(|w| {
                            let mut builder = TreeBuilder::new();
                            let idx = builder.push(CoreFrame::Lit(Literal::LitWord(w)));
                            (builder, idx)
                        })
                        .boxed(),
                    _ => unreachable!("DivOp only for Int/Word"),
                };
                (gen_expr(arg_ty, depth, ctx.clone()), divisor_strat)
                    .prop_map(move |((mut builder, r1), (b2, r2))| {
                        let off = builder.push_tree(b2);
                        let root = builder.push(CoreFrame::PrimOp {
                            op,
                            args: vec![r1, r2 + off],
                        });
                        (builder, root)
                    })
                    .boxed()
            }
        })
        .boxed()
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::compare;
    use proptest::strategy::ValueTree;
    use proptest::test_runner::{Config, TestRunner};
    use std::cell::Cell;
    use std::collections::HashSet;
    use tidepool_eval::{env::Env, eval::eval, heap::VecHeap};

    #[test]
    fn variant_coverage() {
        let handle = std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let mut runner = TestRunner::new(Config {
                    cases: 1000,
                    ..Config::default()
                });
                let mut seen = HashSet::new();

                for _ in 0..1000 {
                    let tree = arb_core_expr().new_tree(&mut runner).unwrap().current();
                    for node in &tree.nodes {
                        let variant = match node {
                            CoreFrame::Var(_) => "Var",
                            CoreFrame::Lit(_) => "Lit",
                            CoreFrame::App { .. } => "App",
                            CoreFrame::Lam { .. } => "Lam",
                            CoreFrame::LetNonRec { .. } => "LetNonRec",
                            CoreFrame::LetRec { .. } => "LetRec",
                            CoreFrame::Case { .. } => "Case",
                            CoreFrame::Con { .. } => "Con",
                            CoreFrame::Join { .. } => "Join",
                            CoreFrame::Jump { .. } => "Jump",
                            CoreFrame::PrimOp { .. } => "PrimOp",
                        };
                        seen.insert(variant.to_string());
                    }
                }

                let expected = [
                    "Var",
                    "Lit",
                    "App",
                    "Lam",
                    "LetNonRec",
                    "LetRec",
                    "Case",
                    "Con",
                    "Join",
                    "Jump",
                    "PrimOp",
                ];
                for exp in expected {
                    assert!(
                        seen.contains(exp),
                        "Variant {} not seen in 1000 samples",
                        exp
                    );
                }
            })
            .unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn generated_exprs_are_well_formed() {
        let handle = std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let mut runner = TestRunner::new(Config {
                    cases: 100,
                    ..Config::default()
                });
                runner
                    .run(&arb_core_expr(), |expr| {
                        assert!(!expr.nodes.is_empty());
                        for node in &expr.nodes {
                            node.clone().map_layer(|idx: usize| {
                                assert!(
                                    idx < expr.nodes.len(),
                                    "invalid index {} in tree of size {}",
                                    idx,
                                    expr.nodes.len()
                                );
                                idx
                            });
                        }
                        Ok(())
                    })
                    .unwrap();
            })
            .unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn generated_exprs_roundtrip_cbor() {
        let handle = std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let mut runner = TestRunner::new(Config {
                    cases: 100,
                    ..Config::default()
                });
                runner
                    .run(&arb_core_expr(), |expr| {
                        let bytes = tidepool_repr::serial::write_cbor(&expr).unwrap();
                        let recovered = tidepool_repr::serial::read_cbor(&bytes).unwrap();
                        assert_eq!(expr, recovered);
                        Ok(())
                    })
                    .unwrap();
            })
            .unwrap();
        handle.join().unwrap();
    }

    /// Option 1: Eval never panics.
    /// Every generated expression should either produce Ok(_) or a well-formed
    /// EvalError — never panic or crash.
    #[test]
    fn eval_never_panics() {
        let handle = std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let mut runner = TestRunner::new(Config {
                    cases: 200,
                    ..Config::default()
                });
                runner
                    .run(&arb_core_expr(), |expr| {
                        let mut heap = VecHeap::new();
                        let _ = eval(&expr, &Env::new(), &mut heap);
                        Ok(())
                    })
                    .unwrap();
            })
            .unwrap();
        handle.join().unwrap();
    }

    /// Option 6: CBOR roundtrip preserves evaluation semantics.
    /// serialize → deserialize should produce an expression that evaluates
    /// identically to the original.
    #[test]
    fn cbor_roundtrip_preserves_eval() {
        let handle = std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let mut runner = TestRunner::new(Config {
                    cases: 100,
                    ..Config::default()
                });
                let compared = Cell::new(0u64);
                let both_error = Cell::new(0u64);
                let deep_force_fail = Cell::new(0u64);

                runner
                    .run(&arb_ground_expr(), |expr| {
                        let bytes = tidepool_repr::serial::write_cbor(&expr).unwrap();
                        let recovered = tidepool_repr::serial::read_cbor(&bytes).unwrap();

                        let mut h1 = VecHeap::new();
                        let mut h2 = VecHeap::new();
                        let r1 = eval(&expr, &Env::new(), &mut h1);
                        let r2 = eval(&recovered, &Env::new(), &mut h2);

                        match (r1, r2) {
                            (Ok(v1), Ok(v2)) => {
                                let f1 = tidepool_eval::eval::deep_force(v1, &mut h1);
                                let f2 = tidepool_eval::eval::deep_force(v2, &mut h2);
                                match (f1, f2) {
                                    (Ok(fv1), Ok(fv2)) => {
                                        compare::assert_values_eq(&fv1, &fv2);
                                        compared.set(compared.get() + 1);
                                    }
                                    (Err(_), Err(_)) => { deep_force_fail.set(deep_force_fail.get() + 1); }
                                    (Ok(v), Err(e)) => {
                                        panic!(
                                            "CBOR roundtrip broke deep_force: original Ok({}) but recovered Err({:?})",
                                            v, e
                                        )
                                    }
                                    (Err(e), Ok(v)) => {
                                        panic!(
                                            "CBOR roundtrip broke deep_force: original Err({:?}) but recovered Ok({})",
                                            e, v
                                        )
                                    }
                                }
                            }
                            (Err(_), Err(_)) => { both_error.set(both_error.get() + 1); }
                            (Ok(_), Err(e)) => {
                                panic!("CBOR roundtrip broke eval: original Ok but recovered Err({:?})", e)
                            }
                            (Err(_), Ok(_)) => { both_error.set(both_error.get() + 1); }
                        }
                        Ok(())
                    })
                    .unwrap();

                let compared = compared.get();
                let both_error = both_error.get();
                let deep_force_fail = deep_force_fail.get();
                eprintln!(
                    "\nCBOR roundtrip: compared={compared}, both_error={both_error}, \
                     deep_force_fail={deep_force_fail}"
                );
                assert!(
                    compared >= 25,
                    "Only {compared} of 100 cases reached value comparison"
                );
            })
            .unwrap();
        handle.join().unwrap();
    }
}
