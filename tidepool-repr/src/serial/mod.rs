//! CBOR serialization and deserialization for Tidepool IR.
//!
//! Provides the binary format for transferring IR from the Haskell frontend
//! to the Rust runtime. Includes serialization for both expressions and
//! constructor metadata tables.

pub mod read;
pub mod write;

pub use read::read_cbor;
pub use read::{read_metadata, MetaWarnings};
pub use write::write_cbor;
pub use write::write_metadata;

/// Errors that can occur during CBOR deserialization of Tidepool IR.
///
/// Wraps underlying `ciborium` errors and adds structural context.
#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    /// An error occurred in the underlying CBOR parser.
    #[error("CBOR decode error: {0}")]
    Cbor(#[from] ciborium::de::Error<std::io::Error>),
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
    /// Input without the mandatory `TPLR` header — a stale or foreign payload.
    #[error(
        "Missing TPLR header: not a current-format Tidepool CBOR payload \
         (stale fixtures/caches must be regenerated, not tolerated)"
    )]
    MissingHeader,
    /// Truncated or incomplete Tidepool CBOR header.
    #[error("Truncated or incomplete Tidepool CBOR header")]
    TruncatedHeader,
    /// Unsupported CBOR version.
    #[error("Unsupported CBOR version {0}.{1}")]
    UnsupportedVersion(u16, u16),
    /// Two distinct constructors in the metadata hash to the same DataConId
    /// (a `stableVarId` collision) — loud instead of a silent table overwrite.
    #[error(transparent)]
    DataConCollision(#[from] crate::datacon_table::DataConCollision),
    /// A metadata field carries a CBOR value of the wrong shape, or a value
    /// outside the range its Rust type can represent. Names the offending
    /// field and what was expected.
    #[error("malformed metadata field `{field}`: {detail}")]
    MalformedMetadataField { field: &'static str, detail: String },
    /// The same key appears more than once in the metadata warnings map.
    #[error("duplicate metadata key: {0}")]
    DuplicateMetadataKey(String),
    /// A key in the metadata warnings map is not one this reader recognizes.
    /// Every key a conforming writer at or below this build's `VERSION_MINOR`
    /// can emit is already handled below; the version gate in `strip_header`
    /// rejects any payload with a newer minor before this code ever sees it,
    /// so an unrecognized key here cannot be a legitimate forward-compat
    /// addition — it names a foreign or corrupt payload.
    #[error("unknown metadata key: {0}")]
    UnknownMetadataKey(String),
}

/// 4-byte magic: ASCII 'TPLR'
pub const HEADER_MAGIC: [u8; 4] = [0x54, 0x50, 0x4C, 0x52];
/// Wire format major version. A payload whose major version differs from
/// this build's is rejected (`ReadError::UnsupportedVersion`) — bump this
/// only on a breaking shape change, in the same commit as the Haskell
/// serializer and the regenerated fixture corpora.
///
/// `3.0` changed every metadata entry from 8 to 9 REQUIRED elements (added
/// rendered field types, in field order — `Tidepool.Translate.dcFieldTypes`),
/// so a `2.x` payload is a hard `UnsupportedVersion` reject, not a tolerated
/// short form; committed fixture corpora were regenerated in the same commit.
pub const VERSION_MAJOR: u16 = 3;
/// Wire format minor version. An older minor within the same major is
/// accepted (forward-compatible read); a newer minor than this build
/// supports is rejected.
pub const VERSION_MINOR: u16 = 0;
/// Total header length in bytes.
pub const HEADER_LEN: usize = 8;

/// Errors that can occur during CBOR serialization of Tidepool IR.
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    /// An error occurred in the underlying CBOR serializer.
    #[error("CBOR encode error: {0}")]
    Cbor(#[from] ciborium::ser::Error<std::io::Error>),
    /// Attempted to write an empty tree.
    #[error("attempted to write an empty RecursiveTree as a CoreExpr")]
    EmptyTree,
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
            type_name: "Maybe".to_string(),
        });
        table.insert(DataCon {
            id: DataConId(2),
            name: "Nothing".to_string(),
            tag: 2,
            rep_arity: 0,
            field_bangs: vec![],
            qualified_name: None,
            type_name: "Maybe".to_string(),
        });

        let bytes = write_metadata(&table, &Default::default()).expect("write_metadata failed");
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
            type_name: "Map".to_string(),
        });
        table.insert(DataCon {
            id: DataConId(200),
            name: "Tip".to_string(),
            tag: 2,
            rep_arity: 0,
            field_bangs: vec![],
            qualified_name: Some("Data.Map.Tip".to_string()),
            type_name: "Map".to_string(),
        });
        table.insert(DataCon {
            id: DataConId(300),
            name: "Bin".to_string(),
            tag: 1,
            rep_arity: 3,
            field_bangs: vec![],
            qualified_name: Some("Data.Set.Bin".to_string()),
            type_name: "Set".to_string(),
        });

        let bytes = write_metadata(&table, &Default::default()).expect("write_metadata failed");
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
    fn test_roundtrip_metadata_field_labels() {
        use crate::datacon::DataCon;
        use crate::datacon_table::DataConTable;

        let mut table = DataConTable::new();
        // Record con WITH a qualified name, field labels, and field types.
        table.insert(DataCon {
            id: DataConId(10),
            name: "Hit".to_string(),
            tag: 1,
            rep_arity: 3,
            field_bangs: vec![],
            qualified_name: Some("Tidepool.Records.Hit".to_string()),
            type_name: "Hit".to_string(),
        });
        table.set_field_labels(
            DataConId(10),
            vec!["path".to_string(), "line".to_string(), "text".to_string()],
        );
        table.set_field_types(
            DataConId(10),
            vec!["Text".to_string(), "Int".to_string(), "Text".to_string()],
        );
        // Record con WITHOUT a qualified name but WITH field labels — exercises
        // the empty-string qn placeholder path (writer emits "" → reader None).
        // Deliberately no field TYPES set here — the present/absent split
        // between the two side-tables is independent (a table missing types
        // for a con that has labels degrades gracefully, never invents one).
        table.insert(DataCon {
            id: DataConId(20),
            name: "Loc".to_string(),
            tag: 1,
            rep_arity: 1,
            field_bangs: vec![],
            qualified_name: None,
            type_name: "Loc".to_string(),
        });
        table.set_field_labels(DataConId(20), vec!["ln".to_string()]);
        // Positional con: no labels, no types.
        table.insert(DataCon {
            id: DataConId(30),
            name: "Plain".to_string(),
            tag: 1,
            rep_arity: 2,
            field_bangs: vec![],
            qualified_name: None,
            type_name: "Plain".to_string(),
        });

        let bytes = write_metadata(&table, &Default::default()).expect("write_metadata failed");
        let (recovered, _) = read_metadata(&bytes).expect("read_metadata failed");

        assert_eq!(
            recovered.field_labels_of(DataConId(10)),
            Some(["path".to_string(), "line".to_string(), "text".to_string()].as_slice())
        );
        assert_eq!(
            recovered.field_types_of(DataConId(10)),
            Some(["Text".to_string(), "Int".to_string(), "Text".to_string()].as_slice())
        );
        // qn preserved for the labeled-with-qn con
        assert_eq!(
            recovered.get(DataConId(10)).unwrap().qualified_name,
            Some("Tidepool.Records.Hit".to_string())
        );
        // labels preserved even without a qn; placeholder decodes back to None
        assert_eq!(
            recovered.field_labels_of(DataConId(20)),
            Some(["ln".to_string()].as_slice())
        );
        // absent field types (present labels, absent types) decode to None,
        // not an invented/empty-but-present entry
        assert_eq!(recovered.field_types_of(DataConId(20)), None);
        assert_eq!(recovered.get(DataConId(20)).unwrap().qualified_name, None);
        // positional con has no labels, no types
        assert_eq!(recovered.field_labels_of(DataConId(30)), None);
        assert_eq!(recovered.field_types_of(DataConId(30)), None);
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
            type_name: "Maybe".to_string(),
        });
        table.insert(DataCon {
            id: DataConId(2),
            name: "Nothing".to_string(),
            tag: 2,
            rep_arity: 0,
            field_bangs: vec![],
            qualified_name: None, // legacy: no qualified name
            type_name: "Maybe".to_string(),
        });

        let bytes = write_metadata(&table, &Default::default()).expect("write_metadata failed");
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

    // --- Negative tests: malformed CBOR → ReadError, not panic ---

    fn cbor_bytes(val: ciborium::value::Value) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&HEADER_MAGIC);
        bytes.extend_from_slice(&VERSION_MAJOR.to_be_bytes());
        bytes.extend_from_slice(&VERSION_MINOR.to_be_bytes());
        ciborium::ser::into_writer(&val, &mut bytes).unwrap();
        bytes
    }

    #[test]
    fn test_read_empty_bytes() {
        assert!(matches!(read_cbor(&[]), Err(ReadError::MissingHeader)));
    }

    #[test]
    fn test_read_wrong_root_type() {
        let bytes = cbor_bytes(ciborium::value::Value::Integer(42.into()));
        assert!(matches!(
            read_cbor(&bytes),
            Err(ReadError::InvalidStructure(_))
        ));
    }

    #[test]
    fn test_read_root_wrong_length() {
        let root = ciborium::value::Value::Array(vec![ciborium::value::Value::Array(vec![])]);
        let bytes = cbor_bytes(root);
        assert!(matches!(
            read_cbor(&bytes),
            Err(ReadError::InvalidStructure(_))
        ));
    }

    #[test]
    fn test_read_empty_nodes() {
        let root = ciborium::value::Value::Array(vec![
            ciborium::value::Value::Array(vec![]),
            ciborium::value::Value::Integer(0.into()),
        ]);
        let bytes = cbor_bytes(root);
        assert!(matches!(
            read_cbor(&bytes),
            Err(ReadError::InvalidStructure(_))
        ));
    }

    #[test]
    fn test_read_bad_frame_tag() {
        let node =
            ciborium::value::Value::Array(vec![ciborium::value::Value::Text("Bogus".to_string())]);
        let nodes = ciborium::value::Value::Array(vec![node]);
        let root =
            ciborium::value::Value::Array(vec![nodes, ciborium::value::Value::Integer(0.into())]);
        let bytes = cbor_bytes(root);
        assert!(matches!(read_cbor(&bytes), Err(ReadError::InvalidTag(_))));
    }

    #[test]
    fn test_read_bad_primop() {
        let node = ciborium::value::Value::Array(vec![
            ciborium::value::Value::Text("PrimOp".to_string()),
            ciborium::value::Value::Text("NotARealOp".to_string()),
            ciborium::value::Value::Array(vec![]),
        ]);
        let nodes = ciborium::value::Value::Array(vec![node]);
        let root =
            ciborium::value::Value::Array(vec![nodes, ciborium::value::Value::Integer(0.into())]);
        let bytes = cbor_bytes(root);
        assert!(matches!(
            read_cbor(&bytes),
            Err(ReadError::InvalidPrimOp(_))
        ));
    }

    #[test]
    fn test_read_index_out_of_bounds() {
        // App referencing index 5 in a 2-node array
        let nodes = ciborium::value::Value::Array(vec![
            ciborium::value::Value::Array(vec![
                ciborium::value::Value::Text("Var".to_string()),
                ciborium::value::Value::Integer(1.into()),
            ]),
            ciborium::value::Value::Array(vec![
                ciborium::value::Value::Text("App".to_string()),
                ciborium::value::Value::Integer(0.into()),
                ciborium::value::Value::Integer(5.into()), // out of bounds
            ]),
        ]);
        let root =
            ciborium::value::Value::Array(vec![nodes, ciborium::value::Value::Integer(1.into())]);
        let bytes = cbor_bytes(root);
        assert!(matches!(
            read_cbor(&bytes),
            Err(ReadError::InvalidStructure(_))
        ));
    }

    // ---- F1: cyclic/forward-referencing node graphs must be rejected loudly ----

    #[test]
    fn test_read_one_node_self_cycle_rejected() {
        // A single "App" node whose fun/arg both point at itself (index 0).
        // Before the F1 fix this passed `validate_indices` (0 < len == 1) and
        // would send `extract_subtree`'s Enter/Exit walk into an infinite
        // re-push loop on first use — a hang/OOM, not a loud error.
        let nodes = ciborium::value::Value::Array(vec![ciborium::value::Value::Array(vec![
            ciborium::value::Value::Text("App".to_string()),
            ciborium::value::Value::Integer(0.into()),
            ciborium::value::Value::Integer(0.into()),
        ])]);
        let root =
            ciborium::value::Value::Array(vec![nodes, ciborium::value::Value::Integer(0.into())]);
        let bytes = cbor_bytes(root);
        match read_cbor(&bytes) {
            Err(ReadError::InvalidStructure(_)) => {}
            other => panic!("expected InvalidStructure for a self-cyclic node, got {other:?}"),
        }
    }

    #[test]
    fn test_read_forward_reference_rejected() {
        // Node 0 ("App") references node 1, which does not exist yet at node
        // 0's position in the strict post-order the encoders emit — a forward
        // reference, not merely out-of-bounds (both nodes exist; the array is
        // 2 long), so the old `child >= len` bounds check would have missed it.
        let nodes = ciborium::value::Value::Array(vec![
            ciborium::value::Value::Array(vec![
                ciborium::value::Value::Text("App".to_string()),
                ciborium::value::Value::Integer(1.into()),
                ciborium::value::Value::Integer(1.into()),
            ]),
            ciborium::value::Value::Array(vec![
                ciborium::value::Value::Text("Var".to_string()),
                ciborium::value::Value::Integer(0.into()),
            ]),
        ]);
        let root =
            ciborium::value::Value::Array(vec![nodes, ciborium::value::Value::Integer(1.into())]);
        let bytes = cbor_bytes(root);
        match read_cbor(&bytes) {
            Err(ReadError::InvalidStructure(_)) => {}
            other => panic!("expected InvalidStructure for a forward reference, got {other:?}"),
        }
    }

    /// Pins the strict post-order invariant `child < parent` across a
    /// round-trip through both writer and reader — the property F1's fix
    /// relies on (verified independently against `TreeBuilder::push` and
    /// `Tidepool.CborEncode.emitNode`, which both append children before the
    /// parent that references them).
    #[test]
    fn test_post_order_invariant_round_trips() {
        let expr = RecursiveTree {
            nodes: vec![
                CoreFrame::Var(VarId(1)),            // 0
                CoreFrame::Lit(Literal::LitInt(42)), // 1
                CoreFrame::App { fun: 0, arg: 1 },   // 2
                CoreFrame::Var(VarId(2)),            // 3
                CoreFrame::LetNonRec {
                    binder: VarId(3),
                    rhs: 2,
                    body: 3,
                }, // 4
                CoreFrame::Case {
                    scrutinee: 4,
                    binder: VarId(4),
                    alts: vec![
                        Alt {
                            con: AltCon::LitAlt(Literal::LitInt(0)),
                            binders: vec![],
                            body: 3,
                        },
                        Alt {
                            con: AltCon::Default,
                            binders: vec![],
                            body: 4,
                        },
                    ],
                }, // 5
            ],
        };
        let bytes = write_cbor(&expr).expect("write failed");
        let recovered = read_cbor(&bytes).expect("read failed — post-order invariant broken");
        assert_eq!(expr, recovered);

        for (my_idx, node) in recovered.nodes.iter().enumerate() {
            for child in crate::tree::get_children(node) {
                assert!(
                    child < my_idx,
                    "node {my_idx} has child {child} which is not strictly earlier"
                );
            }
        }
    }

    #[test]
    fn test_read_metadata_not_array() {
        let bytes = cbor_bytes(ciborium::value::Value::Integer(99.into()));
        assert!(matches!(
            read_metadata(&bytes),
            Err(ReadError::InvalidStructure(_))
        ));
    }

    #[test]
    fn test_read_metadata_bad_entry() {
        // Entry with only 2 fields instead of the required 8
        let bad_entry = ciborium::value::Value::Array(vec![
            ciborium::value::Value::Integer(1.into()),
            ciborium::value::Value::Text("Bad".to_string()),
        ]);
        let root = ciborium::value::Value::Array(vec![bad_entry]);
        let bytes = cbor_bytes(root);
        assert!(matches!(
            read_metadata(&bytes),
            Err(ReadError::InvalidStructure(_))
        ));
    }
    #[test]
    fn float_ingress_rejects_high_bits_in_nodes_and_alternatives() {
        for bits in [
            1u64 << 32,
            (1u64 << 32) | u64::from(1.0f32.to_bits()),
            u64::MAX,
        ] {
            let literal = Literal::LitFloat(bits);
            let node = RecursiveTree {
                nodes: vec![CoreFrame::Lit(literal.clone())],
            };
            let alternative = RecursiveTree {
                nodes: vec![
                    CoreFrame::Lit(Literal::LitFloat(0)),
                    CoreFrame::Case {
                        scrutinee: 0,
                        binder: VarId(1),
                        alts: vec![Alt {
                            con: AltCon::LitAlt(literal),
                            binders: vec![],
                            body: 0,
                        }],
                    },
                ],
            };
            // The writer exposes the in-memory representation; the public
            // reader must reject malformed values wherever literals appear.
            for expr in [node, alternative] {
                let bytes = write_cbor(&expr).expect("encode test input");
                assert!(matches!(
                    read_cbor(&bytes),
                    Err(ReadError::InvalidLiteral(_))
                ));
            }
        }
    }

    #[test]
    fn float_bits_roundtrip_special_encodings() {
        // Serialization preserves payload bits, not IEEE numeric equality.
        // Include both zeros, subnormals, finite extrema, infinities, and
        // positive/negative quiet and signaling NaNs with distinct payloads.
        for bits in [
            0u32,
            0x8000_0000,
            1,
            0x8000_0001,
            0x007f_ffff,
            0x0080_0000,
            0x7f7f_ffff,
            0xff7f_ffff,
            0x7f80_0000,
            0xff80_0000,
            0x7fc0_0001,
            0xffc0_1234,
            0x7f80_0001,
        ] {
            roundtrip(RecursiveTree {
                nodes: vec![CoreFrame::Lit(Literal::LitFloat(u64::from(bits)))],
            });
        }
        for bits in [
            0u64,
            0x8000_0000_0000_0000,
            1,
            0x8000_0000_0000_0001,
            0x000f_ffff_ffff_ffff,
            0x0010_0000_0000_0000,
            0x7fef_ffff_ffff_ffff,
            0xffef_ffff_ffff_ffff,
            0x7ff0_0000_0000_0000,
            0xfff0_0000_0000_0000,
            0x7ff8_0000_0000_0001,
            0xfff8_0000_0000_1234,
            0x7ff0_0000_0000_0001,
        ] {
            roundtrip(RecursiveTree {
                nodes: vec![CoreFrame::Lit(Literal::LitDouble(bits))],
            });
        }
    }

    proptest::proptest! {
        #[test]
        fn float_bits_roundtrip_arbitrary_encodings(
            float_bits in proptest::prelude::any::<u32>(),
            double_bits in proptest::prelude::any::<u64>(),
        ) {
            roundtrip(RecursiveTree {
                nodes: vec![CoreFrame::Lit(Literal::LitFloat(u64::from(float_bits)))],
            });
            roundtrip(RecursiveTree {
                nodes: vec![CoreFrame::Lit(Literal::LitDouble(double_bits))],
            });
        }
    }
}
