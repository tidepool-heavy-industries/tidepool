pub mod read;
pub mod write;

pub use read::read_cbor;
pub use read::read_metadata;
pub use write::write_cbor;

#[derive(Debug)]
pub enum ReadError {
    Cbor(String),
    InvalidTag(String),
    InvalidLiteral(String),
    InvalidPrimOp(String),
    InvalidAltCon(String),
    InvalidStructure(String),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Cbor(e) => write!(f, "CBOR error: {}", e),
            ReadError::InvalidTag(s) => write!(f, "Invalid tag: {}", s),
            ReadError::InvalidLiteral(s) => write!(f, "Invalid literal: {}", s),
            ReadError::InvalidPrimOp(s) => write!(f, "Invalid primop: {}", s),
            ReadError::InvalidAltCon(s) => write!(f, "Invalid alt con: {}", s),
            ReadError::InvalidStructure(s) => write!(f, "Invalid structure: {}", s),
        }
    }
}

impl std::error::Error for ReadError {}

#[derive(Debug)]
pub enum WriteError {
    Cbor(String),
}

impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WriteError::Cbor(e) => write!(f, "CBOR error: {}", e),
        }
    }
}

impl std::error::Error for WriteError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::CoreFrame;
    use crate::tree::RecursiveTree;
    use crate::types::*;

    fn roundtrip(expr: RecursiveTree<CoreFrame<usize>>) {
        let bytes = write_cbor(&expr).expect("write failed");
        let recovered = read_cbor(&bytes).expect("read failed");
        assert_eq!(expr, recovered);
    }

    #[test]
    fn test_roundtrip_var() {
        roundtrip(RecursiveTree {
            nodes: vec![CoreFrame::Var(VarId(42))],
        });
    }

    #[test]
    fn test_roundtrip_lit() {
        let lits = vec![
            Literal::LitInt(-123),
            Literal::LitWord(456),
            Literal::LitChar('a'),
            Literal::LitString(b"hello".to_vec()),
            Literal::LitFloat(1.0f32.to_bits() as u64),
            Literal::LitDouble(2.0f64.to_bits()),
        ];
        for lit in lits {
            roundtrip(RecursiveTree {
                nodes: vec![CoreFrame::Lit(lit)],
            });
        }
    }

    #[test]
    fn test_roundtrip_app() {
        roundtrip(RecursiveTree {
            nodes: vec![
                CoreFrame::Var(VarId(1)),
                CoreFrame::Var(VarId(2)),
                CoreFrame::App { fun: 0, arg: 1 },
            ],
        });
    }

    #[test]
    fn test_roundtrip_lam() {
        roundtrip(RecursiveTree {
            nodes: vec![
                CoreFrame::Var(VarId(1)),
                CoreFrame::Lam {
                    binder: VarId(2),
                    body: 0,
                },
            ],
        });
    }

    #[test]
    fn test_roundtrip_let_non_rec() {
        roundtrip(RecursiveTree {
            nodes: vec![
                CoreFrame::Var(VarId(1)),
                CoreFrame::Var(VarId(2)),
                CoreFrame::LetNonRec {
                    binder: VarId(3),
                    rhs: 0,
                    body: 1,
                },
            ],
        });
    }

    #[test]
    fn test_roundtrip_let_rec() {
        roundtrip(RecursiveTree {
            nodes: vec![
                CoreFrame::Var(VarId(1)),
                CoreFrame::Var(VarId(2)),
                CoreFrame::LetRec {
                    bindings: vec![(VarId(3), 0), (VarId(4), 1)],
                    body: 1,
                },
            ],
        });
    }

    #[test]
    fn test_roundtrip_case() {
        roundtrip(RecursiveTree {
            nodes: vec![
                CoreFrame::Var(VarId(1)), // 0
                CoreFrame::Var(VarId(2)), // 1
                CoreFrame::Case {
                    scrutinee: 0,
                    binder: VarId(3),
                    alts: vec![
                        Alt {
                            con: AltCon::DataAlt(DataConId(4)),
                            binders: vec![VarId(5)],
                            body: 1,
                        },
                        Alt {
                            con: AltCon::LitAlt(Literal::LitInt(42)),
                            binders: vec![],
                            body: 1,
                        },
                        Alt {
                            con: AltCon::Default,
                            binders: vec![],
                            body: 1,
                        },
                    ],
                },
            ],
        });
    }

    #[test]
    fn test_roundtrip_con() {
        roundtrip(RecursiveTree {
            nodes: vec![
                CoreFrame::Var(VarId(1)),
                CoreFrame::Var(VarId(2)),
                CoreFrame::Con {
                    tag: DataConId(3),
                    fields: vec![0, 1],
                },
            ],
        });
    }

    #[test]
    fn test_roundtrip_join_jump() {
        roundtrip(RecursiveTree {
            nodes: vec![
                CoreFrame::Var(VarId(1)), // 0
                CoreFrame::Jump {
                    label: JoinId(2),
                    args: vec![0],
                }, // 1
                CoreFrame::Join {
                    label: JoinId(2),
                    params: vec![VarId(3)],
                    rhs: 1,
                    body: 0,
                },
            ],
        });
    }

    #[test]
    fn test_roundtrip_primop() {
        use PrimOpKind::*;
        let ops = vec![
            IntAdd, IntSub, IntMul, IntNegate, IntEq, IntNe, IntLt, IntLe, IntGt, IntGe, WordAdd,
            WordSub, WordMul, WordEq, WordNe, WordLt, WordLe, WordGt, WordGe, DoubleAdd, DoubleSub,
            DoubleMul, DoubleDiv, DoubleEq, DoubleNe, DoubleLt, DoubleLe, DoubleGt, DoubleGe,
            CharEq, CharNe, CharLt, CharLe, CharGt, CharGe, IndexArray, SeqOp, TagToEnum,
            DataToTag, IntQuot, IntRem, Chr, Ord,
        ];
        for op in ops {
            roundtrip(RecursiveTree {
                nodes: vec![
                    CoreFrame::Var(VarId(1)),
                    CoreFrame::PrimOp { op, args: vec![0] },
                ],
            });
        }
    }

    #[test]
    fn test_read_harness_identity_cbor() {
        let bytes = std::fs::read("../haskell/test/Identity_cbor/identity.cbor")
            .expect("identity.cbor not found — run tidepool-harness first");
        let tree = read_cbor(&bytes).expect("read_cbor failed on identity.cbor");
        assert_eq!(tree.nodes.len(), 2);
        // identity = \x -> x: [Var(x), Lam(x, 0)]
        assert!(matches!(tree.nodes[0], CoreFrame::Var(_)));
        assert!(matches!(tree.nodes[1], CoreFrame::Lam { .. }));
    }

    #[test]
    fn test_read_harness_apply_cbor() {
        let bytes = std::fs::read("../haskell/test/Identity_cbor/apply.cbor")
            .expect("apply.cbor not found — run tidepool-harness first");
        let tree = read_cbor(&bytes).expect("read_cbor failed on apply.cbor");
        assert_eq!(tree.nodes.len(), 5);
        // apply = \f -> \x -> f x: [Var(f), Var(x), App(0,1), Lam(x,2), Lam(f,3)]
        assert!(matches!(tree.nodes[0], CoreFrame::Var(_)));
        assert!(matches!(tree.nodes[1], CoreFrame::Var(_)));
        assert!(matches!(tree.nodes[2], CoreFrame::App { .. }));
        assert!(matches!(tree.nodes[3], CoreFrame::Lam { .. }));
        assert!(matches!(tree.nodes[4], CoreFrame::Lam { .. }));
    }

    #[test]
    fn test_read_harness_const_prime_cbor() {
        let bytes = std::fs::read("../haskell/test/Identity_cbor/const'.cbor")
            .expect("const'.cbor not found — run tidepool-harness first");
        let tree = read_cbor(&bytes).expect("read_cbor failed on const'.cbor");
        assert_eq!(tree.nodes.len(), 3);
        // const' = \x _ -> x: [Var(x), Lam(_, 0), Lam(x, 1)]
        assert!(matches!(tree.nodes[0], CoreFrame::Var(_)));
        assert!(matches!(tree.nodes[1], CoreFrame::Lam { .. }));
        assert!(matches!(tree.nodes[2], CoreFrame::Lam { .. }));
    }

    #[test]
    fn test_read_harness_trmodule_cbor() {
        let bytes = std::fs::read("../haskell/test/Identity_cbor/$trModule.cbor")
            .expect("$trModule.cbor not found — run tidepool-harness first");
        let tree = read_cbor(&bytes).expect("read_cbor failed on $trModule.cbor");
        assert_eq!(tree.nodes.len(), 3);
        // GHC module metadata: contains a Con node
        assert!(matches!(tree.nodes[2], CoreFrame::Con { .. }));
    }

    // End-to-end: .cbor → read_cbor → pretty_print
    #[test]
    fn test_e2e_identity_pretty() {
        let bytes = std::fs::read("../haskell/test/Identity_cbor/identity.cbor")
            .expect("identity.cbor not found");
        let tree = read_cbor(&bytes).expect("read_cbor failed");
        let output = crate::pretty::pretty_print(&tree);
        assert!(!output.is_empty());
        // identity = \x -> x, should contain a lambda
        assert!(output.contains('\\'), "expected lambda in: {}", output);
    }

    #[test]
    fn test_e2e_apply_pretty() {
        let bytes = std::fs::read("../haskell/test/Identity_cbor/apply.cbor")
            .expect("apply.cbor not found");
        let tree = read_cbor(&bytes).expect("read_cbor failed");
        let output = crate::pretty::pretty_print(&tree);
        assert!(!output.is_empty());
        // apply = \f x -> f x, should contain lambda and application
        assert!(output.contains('\\'), "expected lambda in: {}", output);
    }

    #[test]
    fn test_e2e_const_prime_pretty() {
        let bytes = std::fs::read("../haskell/test/Identity_cbor/const'.cbor")
            .expect("const'.cbor not found");
        let tree = read_cbor(&bytes).expect("read_cbor failed");
        let output = crate::pretty::pretty_print(&tree);
        assert!(!output.is_empty());
        // const' = \x _ -> x, two chained lambdas
        assert!(output.contains('\\'), "expected lambda in: {}", output);
    }

    #[test]
    fn test_e2e_trmodule_pretty() {
        let bytes = std::fs::read("../haskell/test/Identity_cbor/$trModule.cbor")
            .expect("$trModule.cbor not found");
        let tree = read_cbor(&bytes).expect("read_cbor failed");
        let output = crate::pretty::pretty_print(&tree);
        assert!(!output.is_empty());
        // Module metadata: Con node
        assert!(output.contains("Con_"), "expected Con in: {}", output);
    }

    #[test]
    fn test_complex_nested() {
        roundtrip(RecursiveTree {
            nodes: vec![
                CoreFrame::Var(VarId(1)), // 0
                CoreFrame::Lam {
                    binder: VarId(1),
                    body: 0,
                }, // 1
                CoreFrame::Lit(Literal::LitInt(42)), // 2
                CoreFrame::App { fun: 1, arg: 2 }, // 3
            ],
        });
    }
}
