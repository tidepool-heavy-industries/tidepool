//! Serialization and deserialization for Tidepool IR using CBOR.

pub mod read;
pub mod write;

pub use read::read_cbor;
pub use read::{read_metadata, MetaWarnings};
pub use write::write_cbor;
pub use write::write_metadata;

/// Errors that can occur during CBOR deserialization of Tidepool IR.
#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    /// An error occurred in the underlying CBOR parser.
    #[error("CBOR error: {0}")]
    Cbor(String),
    /// An unexpected or unknown tag was encountered.
    #[error("Invalid tag: {0}")]
    InvalidTag(String),
    /// A literal value could not be decoded.
    #[error("Invalid literal: {0}")]
    InvalidLiteral(String),
    /// A primitive operation name was not recognized.
    #[error("Invalid primop: {0}")]
    InvalidPrimOp(String),
    /// A case alternative constructor was invalid.
    #[error("Invalid alt con: {0}")]
    InvalidAltCon(String),
    /// The structural layout of the CBOR data does not match Tidepool IR.
    #[error("Invalid structure: {0}")]
    InvalidStructure(String),
}

/// Errors that can occur during CBOR serialization of Tidepool IR.
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    /// An error occurred in the underlying CBOR serializer.
    #[error("CBOR error: {0}")]
    Cbor(String),
}

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
        assert!(
            tree.nodes.len() >= 2,
            "identity should have at least 2 nodes"
        );
        // identity = \x -> x — must contain a Lam (root may be LetNonRec wrapper in --all-closed mode)
        assert!(tree
            .nodes
            .iter()
            .any(|n| matches!(n, CoreFrame::Lam { .. })));
    }

    #[test]
    fn test_read_harness_apply_cbor() {
        let bytes = std::fs::read("../haskell/test/Identity_cbor/apply.cbor")
            .expect("apply.cbor not found — run tidepool-harness first");
        let tree = read_cbor(&bytes).expect("read_cbor failed on apply.cbor");
        assert!(tree.nodes.len() >= 5, "apply should have at least 5 nodes");
        // apply = \f x -> f x — must contain App and Lam
        assert!(tree
            .nodes
            .iter()
            .any(|n| matches!(n, CoreFrame::App { .. })));
        assert!(tree
            .nodes
            .iter()
            .any(|n| matches!(n, CoreFrame::Lam { .. })));
    }

    #[test]
    fn test_read_harness_const_prime_cbor() {
        let bytes = std::fs::read("../haskell/test/Identity_cbor/const'.cbor")
            .expect("const'.cbor not found — run tidepool-harness first");
        let tree = read_cbor(&bytes).expect("read_cbor failed on const'.cbor");
        assert!(tree.nodes.len() >= 3, "const' should have at least 3 nodes");
        // const' = \x _ -> x — must contain Lam
        assert!(tree
            .nodes
            .iter()
            .any(|n| matches!(n, CoreFrame::Lam { .. })));
    }

    #[test]
    fn test_read_harness_trmodule_cbor() {
        let bytes = std::fs::read("../haskell/test/Identity_cbor/$trModule.cbor")
            .expect("$trModule.cbor not found — run tidepool-harness first");
        let tree = read_cbor(&bytes).expect("read_cbor failed on $trModule.cbor");
        assert!(
            !tree.nodes.is_empty(),
            "$trModule should have at least 1 node"
        );
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

    #[test]
    fn test_roundtrip_metadata() {
        use crate::datacon::{DataCon, SrcBang};
        use crate::datacon_table::DataConTable;

        let mut table = DataConTable::new();
        table.insert(DataCon {
            id: DataConId(1),
            name: "Just".to_string(),
            tag: 1,
            rep_arity: 1,
            field_bangs: vec![SrcBang::SrcBang],
            qualified_name: None,
        });
        table.insert(DataCon {
            id: DataConId(2),
            name: "Nothing".to_string(),
            tag: 2,
            rep_arity: 0,
            field_bangs: vec![],
            qualified_name: None,
        });

        let bytes = write_metadata(&table).expect("write_metadata failed");
        let (recovered, warnings) = read_metadata(&bytes).expect("read_metadata failed");
        assert_eq!(table, recovered);
        assert!(!warnings.has_io);
    }

    #[test]
    fn test_roundtrip_metadata_with_qualified_names() {
        use crate::datacon::DataCon;
        use crate::datacon_table::DataConTable;

        let mut table = DataConTable::new();
        table.insert(DataCon {
            id: DataConId(100),
            name: "Bin".to_string(),
            tag: 1,
            rep_arity: 5,
            field_bangs: vec![],
            qualified_name: Some("Data.Map.Bin".to_string()),
        });
        table.insert(DataCon {
            id: DataConId(200),
            name: "Tip".to_string(),
            tag: 2,
            rep_arity: 0,
            field_bangs: vec![],
            qualified_name: Some("Data.Map.Tip".to_string()),
        });
        table.insert(DataCon {
            id: DataConId(300),
            name: "Bin".to_string(),
            tag: 1,
            rep_arity: 3,
            field_bangs: vec![],
            qualified_name: Some("Data.Set.Bin".to_string()),
        });

        let bytes = write_metadata(&table).expect("write_metadata failed");
        let (recovered, _) = read_metadata(&bytes).expect("read_metadata failed");

        // Check by-id entries survived (HashMap order may differ, so check individually)
        assert_eq!(recovered.len(), 3);
        assert_eq!(
            recovered.get(DataConId(100)).unwrap().qualified_name,
            Some("Data.Map.Bin".to_string())
        );
        assert_eq!(
            recovered.get(DataConId(200)).unwrap().qualified_name,
            Some("Data.Map.Tip".to_string())
        );
        assert_eq!(
            recovered.get(DataConId(300)).unwrap().qualified_name,
            Some("Data.Set.Bin".to_string())
        );

        // Verify qualified name index survived the round-trip
        assert_eq!(
            recovered.get_by_qualified_name("Data.Map.Bin"),
            Some(DataConId(100))
        );
        assert_eq!(
            recovered.get_by_qualified_name("Data.Set.Bin"),
            Some(DataConId(300))
        );
        assert_eq!(
            recovered.get_by_qualified_name("Data.Map.Tip"),
            Some(DataConId(200))
        );
    }

    #[test]
    fn test_roundtrip_metadata_mixed_qualified_and_none() {
        use crate::datacon::DataCon;
        use crate::datacon_table::DataConTable;

        let mut table = DataConTable::new();
        table.insert(DataCon {
            id: DataConId(1),
            name: "Just".to_string(),
            tag: 1,
            rep_arity: 1,
            field_bangs: vec![],
            qualified_name: Some("Data.Maybe.Just".to_string()),
        });
        table.insert(DataCon {
            id: DataConId(2),
            name: "Nothing".to_string(),
            tag: 2,
            rep_arity: 0,
            field_bangs: vec![],
            qualified_name: None, // legacy: no qualified name
        });

        let bytes = write_metadata(&table).expect("write_metadata failed");
        let (recovered, _) = read_metadata(&bytes).expect("read_metadata failed");

        // Check individual entries (HashMap order may differ)
        assert_eq!(recovered.len(), 2);
        assert_eq!(
            recovered.get(DataConId(1)).unwrap().qualified_name,
            Some("Data.Maybe.Just".to_string())
        );
        assert_eq!(
            recovered.get_by_qualified_name("Data.Maybe.Just"),
            Some(DataConId(1))
        );
        // Nothing had no qualified name — should not be in the index
        assert_eq!(recovered.get(DataConId(2)).unwrap().qualified_name, None);
    }
}
