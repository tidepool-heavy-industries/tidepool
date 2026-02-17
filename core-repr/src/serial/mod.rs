pub mod read;
pub mod write;

pub use read::read_cbor;
pub use write::write_cbor;

#[derive(Debug)]
pub enum ReadError {
    Cbor(ciborium::de::Error<std::io::Error>),
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

impl std::error::Error for ReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ReadError::Cbor(e) => Some(e),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub enum WriteError {
    Cbor(ciborium::ser::Error<std::io::Error>),
}

impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WriteError::Cbor(e) => write!(f, "CBOR error: {}", e),
        }
    }
}

impl std::error::Error for WriteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            WriteError::Cbor(e) => Some(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;
    use crate::frame::CoreFrame;
    use crate::tree::RecursiveTree;

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
                CoreFrame::Lam { binder: VarId(2), body: 0 },
            ],
        });
    }

    #[test]
    fn test_roundtrip_let_non_rec() {
        roundtrip(RecursiveTree {
            nodes: vec![
                CoreFrame::Var(VarId(1)),
                CoreFrame::Var(VarId(2)),
                CoreFrame::LetNonRec { binder: VarId(3), rhs: 0, body: 1 },
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
                CoreFrame::Jump { label: JoinId(2), args: vec![0] }, // 1
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
            IntAdd, IntSub, IntMul, IntNegate, IntEq, IntNe, IntLt, IntLe, IntGt, IntGe,
            WordAdd, WordSub, WordMul, WordEq, WordNe, WordLt, WordLe, WordGt, WordGe,
            DoubleAdd, DoubleSub, DoubleMul, DoubleDiv, DoubleEq, DoubleNe, DoubleLt, DoubleLe, DoubleGt, DoubleGe,
            CharEq, CharNe, CharLt, CharLe, CharGt, CharGe,
            IndexArray, SeqOp, TagToEnum, DataToTag,
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
    fn test_complex_nested() {
        roundtrip(RecursiveTree {
            nodes: vec![
                CoreFrame::Var(VarId(1)), // 0
                CoreFrame::Lam { binder: VarId(1), body: 0 }, // 1
                CoreFrame::Lit(Literal::LitInt(42)), // 2
                CoreFrame::App { fun: 1, arg: 2 }, // 3
            ],
        });
    }
}
