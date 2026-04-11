//! Pretty-printing for Tidepool IR.

use crate::{types::*, AltCon, CoreExpr, CoreFrame};

/// Pretty-print a CoreExpr to a human-readable string.
pub fn pretty_print(expr: &CoreExpr) -> String {
    if expr.nodes.is_empty() {
        return String::new();
    }
    pp_at(expr, expr.nodes.len() - 1)
}

impl std::fmt::Display for crate::tree::RecursiveTree<CoreFrame<usize>> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", pretty_print(self))
    }
}

fn pp_at(expr: &CoreExpr, idx: usize) -> String {
    match &expr.nodes[idx] {
        CoreFrame::Var(id) => format_var(id),
        CoreFrame::Lit(lit) => format_lit(lit),
        CoreFrame::App { fun, arg } => {
            let fun_frame = &expr.nodes[*fun];
            let arg_frame = &expr.nodes[*arg];

            let mut fun_str = pp_at(expr, *fun);
            if needs_parens_in_app_fun(fun_frame) {
                fun_str = format!("({})", fun_str);
            }

            let mut arg_str = pp_at(expr, *arg);
            if needs_parens_in_app_arg(arg_frame) {
                arg_str = format!("({})", arg_str);
            }

            format!("{} {}", fun_str, arg_str)
        }
        CoreFrame::Lam { binder, body } => {
            let mut binders = vec![*binder];
            let mut current_body = *body;
            while let CoreFrame::Lam {
                binder: next_binder,
                body: next_body,
            } = &expr.nodes[current_body]
            {
                binders.push(*next_binder);
                current_body = *next_body;
            }
            let binders_str = binders.iter().map(format_var).collect::<Vec<_>>().join(" ");
            format!("\\{} -> {}", binders_str, pp_at(expr, current_body))
        }
        CoreFrame::LetNonRec { binder, rhs, body } => {
            let rhs_str = pp_at(expr, *rhs);
            let body_str = pp_at(expr, *body);
            format!("let {} = {}\nin {}", format_var(binder), rhs_str, body_str)
        }
        CoreFrame::LetRec { bindings, body } => {
            let mut s = String::from("let rec\n");
            for (binder, rhs) in bindings {
                s.push_str(&format!(
                    "  {} = {}\n",
                    format_var(binder),
                    pp_at(expr, *rhs)
                ));
            }
            s.push_str(&format!("in {}", pp_at(expr, *body)));
            s
        }
        CoreFrame::Case {
            scrutinee,
            binder,
            alts,
        } => {
            let mut s = format!(
                "case {} of {} {{\n",
                pp_at(expr, *scrutinee),
                format_var(binder)
            );
            for alt in alts {
                s.push_str(&format!(
                    "  {} -> {}\n",
                    format_alt_con(&alt.con, &alt.binders),
                    pp_at(expr, alt.body)
                ));
            }
            s.push('}');
            s
        }
        CoreFrame::Con { tag, fields } => {
            let fields_str = fields
                .iter()
                .map(|&f| pp_at(expr, f))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{}({})", tag, fields_str)
        }
        CoreFrame::Join {
            label,
            params,
            rhs,
            body,
        } => {
            let params_str = params.iter().map(format_var).collect::<Vec<_>>().join(" ");
            format!(
                "join {} ({}) = {}\nin {}",
                label,
                params_str,
                pp_at(expr, *rhs),
                pp_at(expr, *body)
            )
        }
        CoreFrame::Jump { label, args } => {
            let args_str = args
                .iter()
                .map(|&a| pp_at(expr, a))
                .collect::<Vec<_>>()
                .join(", ");
            format!("jump {}({})", label, args_str)
        }
        CoreFrame::PrimOp { op, args } => {
            let args_str = args
                .iter()
                .map(|&a| pp_at(expr, a))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{}({})", primop_name(op), args_str)
        }
    }
}

fn format_var(id: &VarId) -> String {
    id.to_string()
}

fn format_lit(lit: &Literal) -> String {
    lit.to_string()
}

fn primop_name(op: &PrimOpKind) -> String {
    op.to_string()
}

fn format_alt_con(con: &AltCon, binders: &[VarId]) -> String {
    let binders_str = binders.iter().map(format_var).collect::<Vec<_>>().join(" ");
    let mut s = con.to_string();
    if !binders_str.is_empty() {
        s.push(' ');
        s.push_str(&binders_str);
    }
    s
}

fn needs_parens_in_app_fun(frame: &CoreFrame<usize>) -> bool {
    match frame {
        CoreFrame::Var(_)
        | CoreFrame::Lit(_)
        | CoreFrame::App { .. }
        | CoreFrame::Con { .. }
        | CoreFrame::Jump { .. }
        | CoreFrame::PrimOp { .. } => false,
        CoreFrame::Lam { .. }
        | CoreFrame::LetNonRec { .. }
        | CoreFrame::LetRec { .. }
        | CoreFrame::Case { .. }
        | CoreFrame::Join { .. } => true,
    }
}

fn needs_parens_in_app_arg(frame: &CoreFrame<usize>) -> bool {
    match frame {
        CoreFrame::Var(_)
        | CoreFrame::Lit(_)
        | CoreFrame::Con { .. }
        | CoreFrame::Jump { .. }
        | CoreFrame::PrimOp { .. } => false,
        CoreFrame::App { .. }
        | CoreFrame::Lam { .. }
        | CoreFrame::LetNonRec { .. }
        | CoreFrame::LetRec { .. }
        | CoreFrame::Case { .. }
        | CoreFrame::Join { .. } => true,
    }
}

#[cfg(test)]
#[allow(clippy::approx_constant)] // tests use 3.14 as a round-trip float literal
mod tests {
    use super::*;
    use crate::frame::CoreFrame;
    use crate::tree::RecursiveTree;
    use crate::types::*;

    fn var(id: u64) -> CoreFrame<usize> {
        CoreFrame::Var(VarId(id))
    }
    fn lit_int(n: i64) -> CoreFrame<usize> {
        CoreFrame::Lit(Literal::LitInt(n))
    }

    #[test]
    fn test_all_variants_coverage() {
        let nodes = vec![
            var(1),                            // 0: v_1
            lit_int(42),                       // 1: 42#
            CoreFrame::App { fun: 0, arg: 1 }, // 2: v_1 42#
            CoreFrame::Lam {
                binder: VarId(2),
                body: 2,
            }, // 3: \v_2 -> v_1 42#
            CoreFrame::LetNonRec {
                binder: VarId(3),
                rhs: 1,
                body: 0,
            }, // 4: let v_3 = 42# in v_1
            CoreFrame::LetRec {
                bindings: vec![(VarId(4), 1)],
                body: 0,
            }, // 5: let rec v_4 = 42# in v_1
            CoreFrame::Case {
                scrutinee: 1,
                binder: VarId(5),
                alts: vec![Alt {
                    con: AltCon::Default,
                    binders: vec![],
                    body: 0,
                }],
            }, // 6: case 42# of v_5 { _ -> v_1 }
            CoreFrame::Con {
                tag: DataConId(7),
                fields: vec![1],
            }, // 7: Con_7(42#)
            CoreFrame::Join {
                label: JoinId(8),
                params: vec![VarId(9)],
                rhs: 1,
                body: 0,
            }, // 8: join j_8 (v_9) = 42# in v_1
            CoreFrame::Jump {
                label: JoinId(10),
                args: vec![1],
            }, // 9: jump j_10(42#)
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![1, 1],
            }, // 10: +#(42#, 42#)
        ];

        for i in 0..nodes.len() {
            let expr = RecursiveTree {
                nodes: nodes[0..=i].to_vec(),
            };
            let s = pretty_print(&expr);
            assert!(!s.is_empty(), "Variant {} produced empty string", i);
        }
    }

    #[test]
    fn test_app_associativity() {
        // App(App(f, x), y) -> f x y
        let nodes = vec![
            var(1),                            // 0: f
            var(2),                            // 1: x
            var(3),                            // 2: y
            CoreFrame::App { fun: 0, arg: 1 }, // 3: f x
            CoreFrame::App { fun: 3, arg: 2 }, // 4: f x y
        ];
        let expr = RecursiveTree { nodes };
        assert_eq!(pretty_print(&expr), "v_1 v_2 v_3");
    }

    #[test]
    fn test_app_arg_parens() {
        // App(f, App(g, x)) -> f (g x)
        let nodes = vec![
            var(1),                            // 0: f
            var(2),                            // 1: g
            var(3),                            // 2: x
            CoreFrame::App { fun: 1, arg: 2 }, // 3: g x
            CoreFrame::App { fun: 0, arg: 3 }, // 4: f (g x)
        ];
        let expr = RecursiveTree { nodes };
        assert_eq!(pretty_print(&expr), "v_1 (v_2 v_3)");
    }

    #[test]
    fn test_lambda_chaining() {
        // Lam(x, Lam(y, body)) -> \v_x v_y -> body
        let nodes = vec![
            var(3), // 0: body
            CoreFrame::Lam {
                binder: VarId(2),
                body: 0,
            }, // 1: \v_2 -> v_3
            CoreFrame::Lam {
                binder: VarId(1),
                body: 1,
            }, // 2: \v_1 v_2 -> v_3
        ];
        let expr = RecursiveTree { nodes };
        assert_eq!(pretty_print(&expr), r"\v_1 v_2 -> v_3");
    }

    #[test]
    fn test_let_rec() {
        let nodes = vec![
            lit_int(1), // 0
            lit_int(2), // 1
            var(3),     // 2
            CoreFrame::LetRec {
                bindings: vec![(VarId(4), 0), (VarId(5), 1)],
                body: 2,
            },
        ];
        let expr = RecursiveTree { nodes };
        let expected = "let rec\n  v_4 = 1#\n  v_5 = 2#\nin v_3";
        assert_eq!(pretty_print(&expr), expected);
    }

    #[test]
    fn test_case() {
        let nodes = vec![
            var(1), // 0: scrut
            var(2), // 1: body
            CoreFrame::Case {
                scrutinee: 0,
                binder: VarId(3),
                alts: vec![
                    Alt {
                        con: AltCon::Default,
                        binders: vec![],
                        body: 1,
                    },
                    Alt {
                        con: AltCon::DataAlt(DataConId(4)),
                        binders: vec![VarId(5), VarId(6)],
                        body: 1,
                    },
                    Alt {
                        con: AltCon::LitAlt(Literal::LitInt(42)),
                        binders: vec![],
                        body: 1,
                    },
                ],
            },
        ];
        let expr = RecursiveTree { nodes };
        let s = pretty_print(&expr);
        assert!(s.contains("_ -> v_2"));
        assert!(s.contains("Con_4 v_5 v_6 -> v_2"));
        assert!(s.contains("42# -> v_2"));
    }

    #[test]
    fn test_primop() {
        let nodes = vec![
            lit_int(1),
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![0, 0],
            },
        ];

        let expr = RecursiveTree { nodes };

        assert_eq!(pretty_print(&expr), "+#(1#, 1#)");
    }

    #[test]
    fn test_literals() {
        let nodes = [
            CoreFrame::Lit(Literal::LitWord(42)),
            CoreFrame::Lit(Literal::LitChar('x')),
            CoreFrame::Lit(Literal::LitString(b"hello".to_vec())),
            CoreFrame::Lit(Literal::LitFloat(3.14f32.to_bits() as u64)),
            CoreFrame::Lit(Literal::LitDouble(3.14f64.to_bits())),
        ];

        let expr_word = RecursiveTree {
            nodes: vec![nodes[0].clone()],
        };

        assert_eq!(pretty_print(&expr_word), "42##");

        let expr_char = RecursiveTree {
            nodes: vec![nodes[1].clone()],
        };

        assert_eq!(pretty_print(&expr_char), "'x'#");

        let expr_string = RecursiveTree {
            nodes: vec![nodes[2].clone()],
        };

        assert_eq!(pretty_print(&expr_string), "\"hello\"#");

        let expr_float = RecursiveTree {
            nodes: vec![nodes[3].clone()],
        };

        assert_eq!(pretty_print(&expr_float), "3.14#");

        let expr_double = RecursiveTree {
            nodes: vec![nodes[4].clone()],
        };

        assert_eq!(pretty_print(&expr_double), "3.14##");
    }
}
