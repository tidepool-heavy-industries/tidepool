use ciborium::value::Value as Cbor;
use tidepool_repr::execution_schema::{
    encode_type_graph_value, parse_program, Architecture, ConstructorId, ConstructorReply,
    DecodeLimits, Endianness, ParseError, ProgramRequirements, SiteDelivery, TargetDescriptor,
    TypeNode, TypeNodeId, EXECUTION_ABI_VERSION, SCHEMA_VERSION,
};

fn int(value: u64) -> Cbor {
    Cbor::Integer(value.into())
}

fn root(signatures: Cbor) -> Cbor {
    Cbor::Array(vec![
        Cbor::Text("TPSTG".into()),
        int(SCHEMA_VERSION),
        Cbor::Text("ghc-9.12-prepared-stg".into()),
        Cbor::Text("ghc-9.12.2".into()),
        int(EXECUTION_ABI_VERSION),
        Cbor::Array(vec![
            int(0),
            int(0),
            int(64),
            int(64),
            Cbor::Text("sysv64".into()),
            Cbor::Array(vec![]),
        ]),
        signatures,
        Cbor::Array(vec![]),
        Cbor::Array(vec![]),
        Cbor::Array(vec![]),
        Cbor::Array(vec![]),
        Cbor::Array(vec![]),
        int(0),
        encode_type_graph_value(&tidepool_repr::type_graph::TypeGraph::default()),
        Cbor::Array(vec![]),
        Cbor::Array(vec![]),
        Cbor::Array(vec![int(0)]),
    ])
}

fn bytes(value: &Cbor) -> Vec<u8> {
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(value, &mut bytes).unwrap();
    bytes
}

fn requirements() -> ProgramRequirements {
    ProgramRequirements {
        schema_version: SCHEMA_VERSION,
        projection_profile: "ghc-9.12-prepared-stg".into(),
        toolchain: "ghc-9.12.2".into(),
        execution_abi_version: EXECUTION_ABI_VERSION,
        target: TargetDescriptor {
            architecture: Architecture::X86_64,
            endianness: Endianness::Little,
            pointer_width: 64,
            word_width: 64,
            abi: "sysv64".into(),
            features: vec![],
        },
    }
}

fn global_with_generation(generation: Cbor) -> Cbor {
    Cbor::Array(vec![
        Cbor::Array(vec![
            Cbor::Text("fixture".into()),
            Cbor::Text("M3.Import".into()),
            Cbor::Text("value".into()),
            Cbor::Text("retained".into()),
            Cbor::Array(vec![int(0)]),
        ]),
        Cbor::Array(vec![int(1)]),
        Cbor::Array(vec![int(0)]),
        Cbor::Bool(true),
        generation,
    ])
}

fn with_operation(mut program: Cbor, identity: Cbor, signature: u64) -> Cbor {
    let Cbor::Array(fields) = &mut program else {
        unreachable!()
    };
    fields[9] = Cbor::Array(vec![Cbor::Array(vec![identity, int(signature)])]);
    program
}

#[test]
fn valid_closed_shape_reaches_semantic_validation() {
    let result = parse_program(
        &bytes(&root(Cbor::Array(vec![]))),
        &requirements(),
        DecodeLimits::default(),
    );
    match result {
        Ok(_) => {}
        Err(ParseError::InvalidReference(detail)) => assert!(detail.contains("entry value")),
        other => panic!("valid codec shape failed before semantic validation: {other:?}"),
    }
}

#[test]
fn codec_rejects_schema_10_shape_before_enforcing_schema_11_length() {
    // A schema-10 artifact has no verb-site table: it is refused by version,
    // not misread as a malformed schema-11 program.
    let mut old = root(Cbor::Array(vec![]));
    let Cbor::Array(fields) = &mut old else {
        unreachable!()
    };
    fields[1] = int(10);
    fields.truncate(15);
    assert!(matches!(
        parse_program(&bytes(&old), &requirements(), DecodeLimits::default()),
        Err(ParseError::UnsupportedVersion(10))
    ));
}

#[test]
fn codec_rejects_schema_9_shape_before_enforcing_schema_11_length() {
    let mut old = root(Cbor::Array(vec![]));
    let Cbor::Array(fields) = &mut old else {
        unreachable!()
    };
    fields[1] = int(9);
    fields.truncate(13);
    assert!(matches!(
        parse_program(&bytes(&old), &requirements(), DecodeLimits::default()),
        Err(ParseError::UnsupportedVersion(9))
    ));
}

#[test]
fn codec_rejects_abi_v2_after_tag_abi_bump() {
    let mut old = root(Cbor::Array(vec![]));
    let Cbor::Array(fields) = &mut old else {
        unreachable!()
    };
    fields[4] = int(2);
    assert!(matches!(
        parse_program(&bytes(&old), &requirements(), DecodeLimits::default()),
        Err(ParseError::UnsupportedTarget(_))
    ));
}

#[test]
fn codec_rejects_unknown_nested_tag() {
    let signatures = Cbor::Array(vec![Cbor::Array(vec![
        Cbor::Array(vec![Cbor::Array(vec![int(99)])]),
        Cbor::Array(vec![int(0), Cbor::Array(vec![])]),
    ])]);
    assert!(matches!(
        parse_program(
            &bytes(&root(signatures)),
            &requirements(),
            DecodeLimits::default()
        ),
        Err(ParseError::InvalidTag(99))
    ));
}

#[test]
fn result_contract_tags_are_distinct_and_have_exact_arity() {
    for result in [
        Cbor::Array(vec![int(0), Cbor::Array(vec![])]),
        Cbor::Array(vec![int(1)]),
        Cbor::Array(vec![int(2)]),
    ] {
        let signatures = Cbor::Array(vec![Cbor::Array(vec![Cbor::Array(vec![]), result])]);
        assert!(matches!(
            parse_program(
                &bytes(&root(signatures)),
                &requirements(),
                DecodeLimits::default()
            ),
            Err(ParseError::InvalidReference(_))
        ));
    }
    for result in [
        Cbor::Array(vec![int(0)]),
        Cbor::Array(vec![int(1), Cbor::Array(vec![])]),
        Cbor::Array(vec![int(2), Cbor::Array(vec![])]),
    ] {
        let signatures = Cbor::Array(vec![Cbor::Array(vec![Cbor::Array(vec![]), result])]);
        assert!(matches!(
            parse_program(
                &bytes(&root(signatures)),
                &requirements(),
                DecodeLimits::default()
            ),
            Err(ParseError::Malformed(_))
        ));
    }
}

#[test]
fn codec_accepts_optional_generation_shape_and_rejects_unknown_tag() {
    let signatures = Cbor::Array(vec![Cbor::Array(vec![
        Cbor::Array(vec![]),
        Cbor::Array(vec![int(0), Cbor::Array(vec![])]),
    ])]);
    let mut valid = root(signatures.clone());
    let Cbor::Array(fields) = &mut valid else {
        unreachable!()
    };
    fields[7] = Cbor::Array(vec![global_with_generation(Cbor::Array(vec![
        int(1),
        int(7),
    ]))]);
    assert!(matches!(
        parse_program(&bytes(&valid), &requirements(), DecodeLimits::default()),
        Err(ParseError::InvalidReference(_))
    ));

    let mut invalid = root(signatures);
    let Cbor::Array(fields) = &mut invalid else {
        unreachable!()
    };
    fields[7] = Cbor::Array(vec![global_with_generation(Cbor::Array(vec![int(9)]))]);
    assert!(matches!(
        parse_program(&bytes(&invalid), &requirements(), DecodeLimits::default()),
        Err(ParseError::InvalidTag(9))
    ));
}

#[test]
fn codec_rejects_malformed_record_parents_and_intrinsic_identities() {
    let signatures = Cbor::Array(vec![Cbor::Array(vec![
        Cbor::Array(vec![]),
        Cbor::Array(vec![int(0), Cbor::Array(vec![])]),
    ])]);
    let mut malformed_parent = root(signatures.clone());
    let Cbor::Array(fields) = &mut malformed_parent else {
        unreachable!()
    };
    let Cbor::Array(globals) = &mut fields[7] else {
        unreachable!()
    };
    let mut global = global_with_generation(Cbor::Array(vec![int(0)]));
    let Cbor::Array(global_fields) = &mut global else {
        unreachable!()
    };
    let Cbor::Array(symbol) = &mut global_fields[0] else {
        unreachable!()
    };
    symbol[4] = Cbor::Array(vec![int(0), Cbor::Text("unexpected".into())]);
    globals.push(global);
    assert!(matches!(
        parse_program(
            &bytes(&malformed_parent),
            &requirements(),
            DecodeLimits::default()
        ),
        Err(ParseError::Malformed(detail)) if detail.contains("record parent")
    ));

    let mut unknown_parent_tag = root(signatures.clone());
    let Cbor::Array(fields) = &mut unknown_parent_tag else {
        unreachable!()
    };
    let Cbor::Array(globals) = &mut fields[7] else {
        unreachable!()
    };
    let mut global = global_with_generation(Cbor::Array(vec![int(0)]));
    let Cbor::Array(global_fields) = &mut global else {
        unreachable!()
    };
    let Cbor::Array(symbol) = &mut global_fields[0] else {
        unreachable!()
    };
    symbol[4] = Cbor::Array(vec![int(9)]);
    globals.push(global);
    assert!(matches!(
        parse_program(
            &bytes(&unknown_parent_tag),
            &requirements(),
            DecodeLimits::default()
        ),
        Err(ParseError::InvalidTag(9))
    ));

    let mut malformed_intrinsic = root(signatures);
    let Cbor::Array(fields) = &mut malformed_intrinsic else {
        unreachable!()
    };
    fields[9] = Cbor::Array(vec![Cbor::Array(vec![
        Cbor::Array(vec![
            int(1),
            Cbor::Text("rintDouble".into()),
            Cbor::Array(vec![int(0), int(1)]),
        ]),
        int(0),
    ])]);
    assert!(matches!(
        parse_program(
            &bytes(&malformed_intrinsic),
            &requirements(),
            DecodeLimits::default()
        ),
        Err(ParseError::Malformed(detail)) if detail.contains("foreign convention")
    ));
}

#[test]
fn codec_decodes_capability_and_wired_in_error_identities() {
    let returning = Cbor::Array(vec![Cbor::Array(vec![
        Cbor::Array(vec![]),
        Cbor::Array(vec![int(0), Cbor::Array(vec![])]),
    ])]);
    let capability = with_operation(
        root(returning),
        Cbor::Array(vec![int(2), Cbor::Text("ffi.lookup".into())]),
        0,
    );
    assert!(matches!(
        parse_program(&bytes(&capability), &requirements(), DecodeLimits::default()),
        Err(ParseError::InvalidReference(detail)) if detail.contains("entry value")
    ));

    let no_success = Cbor::Array(vec![Cbor::Array(vec![
        Cbor::Array(vec![Cbor::Array(vec![int(3)])]),
        Cbor::Array(vec![int(1)]),
    ])]);
    let wired = with_operation(root(no_success), Cbor::Array(vec![int(3), int(0)]), 0);
    assert!(matches!(
        parse_program(&bytes(&wired), &requirements(), DecodeLimits::default()),
        Err(ParseError::InvalidReference(detail)) if detail.contains("entry value")
    ));
}

#[test]
fn codec_rejects_unknown_wired_in_kind_and_malformed_identity_arity() {
    let signatures = Cbor::Array(vec![]);
    let unknown_kind = with_operation(
        root(signatures.clone()),
        Cbor::Array(vec![int(3), int(11)]),
        0,
    );
    assert_eq!(
        parse_program(
            &bytes(&unknown_kind),
            &requirements(),
            DecodeLimits::default()
        ),
        Err(ParseError::InvalidTag(11))
    );

    for identity in [
        Cbor::Array(vec![int(2)]),
        Cbor::Array(vec![int(3), int(0), int(1)]),
    ] {
        let malformed = with_operation(root(signatures.clone()), identity, 0);
        assert!(matches!(
            parse_program(
                &bytes(&malformed),
                &requirements(),
                DecodeLimits::default()
            ),
            Err(ParseError::Malformed(detail)) if detail.contains("operation identity")
        ));
    }
}

#[test]
fn codec_rejects_truncated_trailing_and_indefinite_input() {
    let mut valid = bytes(&root(Cbor::Array(vec![])));
    valid.pop();
    assert!(matches!(
        parse_program(&valid, &requirements(), DecodeLimits::default()),
        Err(ParseError::Truncated)
    ));

    let mut trailing = bytes(&root(Cbor::Array(vec![])));
    trailing.push(0);
    assert!(matches!(
        parse_program(&trailing, &requirements(), DecodeLimits::default()),
        Err(ParseError::TrailingBytes)
    ));

    assert!(matches!(
        parse_program(&[0x9f, 0xff], &requirements(), DecodeLimits::default()),
        Err(ParseError::Malformed(detail)) if detail.contains("indefinite")
    ));
}

#[test]
fn codec_rejects_maps_wrong_lengths_and_excessive_container_nesting() {
    let map = bytes(&Cbor::Map(vec![]));
    assert!(matches!(
        parse_program(&map, &requirements(), DecodeLimits::default()),
        Err(ParseError::Malformed(_))
    ));

    let short_root = bytes(&Cbor::Array(vec![Cbor::Text("TPSTG".into())]));
    assert!(matches!(
        parse_program(&short_root, &requirements(), DecodeLimits::default()),
        Err(ParseError::Malformed(_))
    ));

    let mut nested = Cbor::Null;
    for _ in 0..33 {
        nested = Cbor::Array(vec![nested]);
    }
    assert!(matches!(
        parse_program(&bytes(&nested), &requirements(), DecodeLimits::default()),
        Err(ParseError::Malformed(detail)) if detail.contains("container nesting")
    ));
}

#[test]
fn codec_enforces_byte_and_table_limits() {
    let encoded = bytes(&root(Cbor::Array(vec![])));
    let byte_limits = DecodeLimits {
        max_bytes: encoded.len() - 1,
        ..DecodeLimits::default()
    };
    assert!(matches!(
        parse_program(&encoded, &requirements(), byte_limits),
        Err(ParseError::ByteLimit { .. })
    ));

    let table_limits = DecodeLimits {
        max_table_entries: 0,
        ..DecodeLimits::default()
    };
    let one_table_entry = bytes(&root(Cbor::Array(vec![Cbor::Array(vec![
        Cbor::Array(vec![]),
        Cbor::Array(vec![int(0), Cbor::Array(vec![])]),
    ])])));
    assert!(matches!(
        parse_program(&one_table_entry, &requirements(), table_limits),
        Err(ParseError::LimitExceeded("table entries"))
    ));

    let mut schema_tables = scalar_program(Cbor::Array(vec![int(4), int(64)]));
    let Cbor::Array(fields) = &mut schema_tables else {
        unreachable!()
    };
    fields[13] = text_graph_value();
    fields[14] = Cbor::Array(vec![Cbor::Array(vec![
        int(1),
        Cbor::Text("site".into()),
        int(0),
        int(0),
        int(0),
        Cbor::Array(vec![]),
    ])]);
    let encoded = bytes(&schema_tables);
    assert_eq!(
        parse_program(
            &encoded,
            &requirements(),
            DecodeLimits {
                max_type_nodes: 0,
                ..DecodeLimits::default()
            }
        ),
        Err(ParseError::LimitExceeded("type nodes"))
    );
    assert_eq!(
        parse_program(
            &encoded,
            &requirements(),
            DecodeLimits {
                max_sites: 0,
                ..DecodeLimits::default()
            }
        ),
        Err(ParseError::LimitExceeded("sites"))
    );
}

fn scalar_program(result_rep: Cbor) -> Cbor {
    let a = |values| Cbor::Array(values);
    let signature = a(vec![a(vec![]), a(vec![int(0), a(vec![result_rep])])]);
    let scalar = a(vec![
        int(1),
        a(vec![
            int(0),
            int(64),
            Cbor::Bytes(42_i64.to_be_bytes().to_vec()),
        ]),
    ]);
    let body = a(vec![int(0), a(vec![scalar])]);
    let rhs = a(vec![int(0), int(0), a(vec![]), a(vec![]), int(0)]);
    let symbol = a(vec![
        Cbor::Text("fixture".into()),
        Cbor::Text("Probe".into()),
        Cbor::Text("value".into()),
        Cbor::Text("entry".into()),
        a(vec![int(0)]),
    ]);
    let top = a(vec![symbol, a(vec![int(0), rhs])]);
    let mut program = root(a(vec![signature]));
    let Cbor::Array(fields) = &mut program else {
        unreachable!()
    };
    fields[10] = a(vec![body]);
    fields[11] = a(vec![a(vec![int(0), top])]);
    program
}

fn text_graph_value() -> Cbor {
    encode_type_graph_value(&tidepool_test_data::prepared::closed_type_graph(
        tidepool_repr::execution_schema::testing::identity("Types", "Text"),
        tidepool_repr::type_graph::DeclarationForm::Text,
    ))
}

#[test]
fn codec_decodes_type_graph_and_site_rows() {
    use tidepool_repr::execution_schema::{
        testing, CheckedLayout, ConstructorDecl, FieldLayout, RuntimeRep, SiteRow,
    };
    use tidepool_repr::type_graph::{
        DeclarationForm, ParameterFlag, RootDomain, SyntaxRestriction, TypeEdge, TypeLiteral,
    };
    let mut program = testing::wire_program();
    let nominal = |name: &str| tidepool_repr::execution_schema::SymbolIdentity {
        namespace: "type".into(),
        ..testing::identity("Types", name)
    };
    let family = nominal("Phantom");
    program.constructors.push(ConstructorDecl {
        identity: testing::identity("Types", "Recursive"),
        host_id: tidepool_repr::DataConId(9001),
        family: family.clone(),
        field_reps: vec![RuntimeRep::LiftedRef],
        strict_fields: vec![false],
        layout: CheckedLayout {
            fields: vec![FieldLayout {
                rep: RuntimeRep::LiftedRef,
                offset: 0,
            }],
            alignment: 8,
            payload_size: 8,
            root_mask: vec![true],
        },
        result_rep: RuntimeRep::LiftedRef,
        tag: 1,
        family_size: 1,
    });
    let root = |rendered: &str| TypeNode::Root {
        domain: RootDomain::Closed,
        binders: vec![],
        rendered: rendered.into(),
    };
    program.types = tidepool_test_data::prepared::type_graph(
        vec![
            root("Int#"),
            root("Phantom Int#"),
            TypeNode::Declaration {
                identity: nominal("Int#"),
                parameters: vec![],
                form: DeclarationForm::Scalar(RuntimeRep::Int(64)),
                restriction: SyntaxRestriction::None,
            },
            TypeNode::NominalApplication,
            TypeNode::Declaration {
                identity: family,
                parameters: vec![ParameterFlag::NamedRequired],
                form: DeclarationForm::Data,
                restriction: SyntaxRestriction::None,
            },
            TypeNode::Literal(TypeLiteral::Symbol("Type".into())),
            TypeNode::NominalApplication,
            TypeNode::ConstructorTemplate {
                constructor: ConstructorId(0),
                identity: program.constructors[0].identity.clone(),
            },
            TypeNode::NominalApplication,
            TypeNode::Bound(0),
        ],
        &[
            (0, 3, TypeEdge::Body),
            (1, 6, TypeEdge::Body),
            (3, 2, TypeEdge::Head),
            (4, 5, TypeEdge::BinderKind(0)),
            (4, 7, TypeEdge::Constructor(1)),
            (6, 4, TypeEdge::Head),
            (6, 3, TypeEdge::Argument(0)),
            (
                7,
                8,
                TypeEdge::Field {
                    ordinal: 0,
                    source_rep: RuntimeRep::LiftedRef,
                },
            ),
            (8, 4, TypeEdge::Head),
            (8, 9, TypeEdge::Argument(0)),
        ],
        &program.constructors,
    )
    .unwrap();
    program.sites = [
        SiteDelivery::HostAnswer,
        SiteDelivery::LiveReentry,
        SiteDelivery::ExitCellFill,
        SiteDelivery::TerminalCapture,
    ]
    .into_iter()
    .enumerate()
    .map(|(index, delivery)| SiteRow {
        site: 41 + index as u64,
        origin: "Types.hs:1".into(),
        ordinal: index as u64,
        delivery,
        wire: TypeNodeId(1),
        inputs: vec![TypeNodeId(0)],
    })
    .collect();
    let encoded = tidepool_test_data::prepared_encode::encode_wire_program(&program);
    let prepared = parse_program(&encoded, &requirements(), DecodeLimits::default()).unwrap();
    assert_eq!(prepared.types(), &program.types);
    assert!(matches!(
        prepared.type_node(TypeNodeId(0)),
        Some(TypeNode::Root {
            domain: RootDomain::Closed,
            ..
        })
    ));
    let site = prepared.site(41).unwrap();
    assert_eq!(site.delivery, SiteDelivery::HostAnswer);
    assert_eq!(site.wire, TypeNodeId(1));
    assert_eq!(site.inputs, vec![TypeNodeId(0)]);
    assert_eq!(
        prepared
            .sites()
            .iter()
            .map(|site| site.delivery)
            .collect::<Vec<_>>(),
        vec![
            SiteDelivery::HostAnswer,
            SiteDelivery::LiveReentry,
            SiteDelivery::ExitCellFill,
            SiteDelivery::TerminalCapture
        ]
    );
    // A storage index must identify a Root, rather than its body/declaration.
    for invalid_root in [2, 99] {
        program.sites[0].inputs = vec![TypeNodeId(invalid_root)];
        assert!(
            matches!(parse_program(&tidepool_test_data::prepared_encode::encode_wire_program(&program), &requirements(), DecodeLimits::default()),
            Err(ParseError::InvalidReference(detail)) if detail.contains("type root"))
        );
    }
    let mut duplicate: Cbor = ciborium::de::from_reader(encoded.as_slice()).unwrap();
    let Cbor::Array(fields) = &mut duplicate else {
        unreachable!()
    };
    let Cbor::Array(graph) = &mut fields[13] else {
        unreachable!()
    };
    let Cbor::Array(nodes) = &mut graph[1] else {
        unreachable!()
    };
    nodes.push(nodes[7].clone());
    assert_eq!(
        parse_program(&bytes(&duplicate), &requirements(), DecodeLimits::default()),
        Err(ParseError::DuplicateDefinition(
            "type constructor template".into()
        ))
    );
    let mut malformed: Cbor = ciborium::de::from_reader(encoded.as_slice()).unwrap();
    let Cbor::Array(fields) = &mut malformed else {
        unreachable!()
    };
    let Cbor::Array(graph) = &mut fields[13] else {
        unreachable!()
    };
    let Cbor::Array(edges) = &mut graph[2] else {
        unreachable!()
    };
    let Cbor::Array(edge) = &mut edges[0] else {
        unreachable!()
    };
    edge[1] = int(99);
    assert!(matches!(
        parse_program(&bytes(&malformed), &requirements(), DecodeLimits::default()),
        Err(ParseError::InvalidReference(_))
    ));
}

#[test]
fn finite_graph_codec_covers_scoped_syntax_and_declaration_metadata() {
    use tidepool_repr::execution_schema::{
        testing, CheckedLayout, ConstructorDecl, FieldLayout, RuntimeRep,
    };
    use tidepool_repr::type_graph::{
        DeclarationForm, ForAllFlag, FunctionFlag, NominalHeadKind, ParameterFlag, RootDomain,
        SourceBinderFlag, SyntaxRestriction, TypeEdge, TypeLiteral,
    };
    let mut program = testing::wire_program();
    let identity = |name: &str| tidepool_repr::execution_schema::SymbolIdentity {
        namespace: "type".into(),
        ..testing::identity("Finite", name)
    };
    let declaration = |name: &str, form| TypeNode::Declaration {
        identity: identity(name),
        parameters: vec![],
        form,
        restriction: SyntaxRestriction::None,
    };
    program.constructors.push(ConstructorDecl {
        identity: testing::identity("Finite", "Container"),
        host_id: tidepool_repr::DataConId(9002),
        family: identity("Container"),
        field_reps: vec![RuntimeRep::Int(64)],
        strict_fields: vec![false],
        layout: CheckedLayout {
            fields: vec![FieldLayout {
                rep: RuntimeRep::Int(64),
                offset: 0,
            }],
            alignment: 8,
            payload_size: 8,
            root_mask: vec![false],
        },
        result_rep: RuntimeRep::LiftedRef,
        tag: 1,
        family_size: 1,
    });
    let mut nodes = vec![
        TypeNode::Root {
            domain: RootDomain::Closed,
            binders: vec![],
            rendered: "closed application".into(),
        },
        TypeNode::Root {
            domain: RootDomain::ConstructorScheme,
            binders: vec![SourceBinderFlag::Specified, SourceBinderFlag::Inferred],
            rendered: "source scheme".into(),
        },
        TypeNode::Literal(TypeLiteral::Symbol("Type".into())),
        TypeNode::Bound(0),
        TypeNode::ForAll(ForAllFlag::Specified),
        TypeNode::Function(FunctionFlag::TypeToType),
        TypeNode::Literal(TypeLiteral::Natural("1".into())),
        TypeNode::NominalApplication,
        TypeNode::Declaration {
            identity: identity("Eff"),
            parameters: vec![],
            form: DeclarationForm::Opaque {
                head_kind: NominalHeadKind::Constructor,
                reason: "effect head".into(),
            },
            restriction: SyntaxRestriction::EffectHead,
        },
        TypeNode::Application,
        TypeNode::NominalApplication,
        TypeNode::Declaration {
            identity: identity("EtaAlias"),
            parameters: vec![
                ParameterFlag::NamedRequired,
                ParameterFlag::NamedSpecified,
                ParameterFlag::NamedInferred,
                ParameterFlag::AnonymousVisible,
            ],
            form: DeclarationForm::Newtype { eta_arity: 1 },
            restriction: SyntaxRestriction::None,
        },
        TypeNode::Literal(TypeLiteral::Character('λ')),
        TypeNode::Application,
        TypeNode::Literal(TypeLiteral::Symbol("name".into())),
        declaration("Container", DeclarationForm::Data),
        TypeNode::ConstructorTemplate {
            constructor: ConstructorId(0),
            identity: program.constructors[0].identity.clone(),
        },
        TypeNode::NominalApplication,
        TypeNode::Root {
            domain: RootDomain::Closed,
            binders: vec![],
            rendered: "Container".into(),
        },
        declaration("Text", DeclarationForm::Text),
        TypeNode::NominalApplication,
        declaration("Integer", DeclarationForm::Integer),
        TypeNode::NominalApplication,
        declaration("Natural", DeclarationForm::Natural),
        TypeNode::NominalApplication,
        declaration("Int#", DeclarationForm::Scalar(RuntimeRep::Int(64))),
        TypeNode::NominalApplication,
        declaration(
            "Family",
            DeclarationForm::Opaque {
                head_kind: NominalHeadKind::Family,
                reason: "unsupported family".into(),
            },
        ),
    ];
    let mut edges = vec![
        (0, 9, TypeEdge::Body),
        (1, 2, TypeEdge::BinderKind(0)),
        (1, 3, TypeEdge::BinderKind(1)),
        (1, 4, TypeEdge::Body),
        (4, 2, TypeEdge::Kind),
        (4, 5, TypeEdge::Body),
        (5, 6, TypeEdge::Multiplicity),
        (5, 3, TypeEdge::Domain),
        (5, 7, TypeEdge::Codomain),
        (7, 8, TypeEdge::Head),
        (9, 10, TypeEdge::Function),
        (9, 6, TypeEdge::ApplyArgument),
        (10, 11, TypeEdge::Head),
        (10, 12, TypeEdge::Argument(0)),
        (10, 6, TypeEdge::Argument(1)),
        (10, 14, TypeEdge::Argument(2)),
        (10, 7, TypeEdge::Argument(3)),
        (11, 2, TypeEdge::BinderKind(0)),
        (11, 3, TypeEdge::BinderKind(1)),
        (11, 3, TypeEdge::BinderKind(2)),
        (11, 3, TypeEdge::BinderKind(3)),
        (11, 13, TypeEdge::AliasRhs),
        (13, 3, TypeEdge::Function),
        (13, 14, TypeEdge::ApplyArgument),
        (15, 16, TypeEdge::Constructor(1)),
        (
            16,
            26,
            TypeEdge::Field {
                ordinal: 0,
                source_rep: RuntimeRep::Int(64),
            },
        ),
        (17, 15, TypeEdge::Head),
        (18, 17, TypeEdge::Body),
        (20, 19, TypeEdge::Head),
        (22, 21, TypeEdge::Head),
        (24, 23, TypeEdge::Head),
        (26, 25, TypeEdge::Head),
    ];
    for flag in [
        FunctionFlag::TypeToConstraint,
        FunctionFlag::ConstraintToType,
        FunctionFlag::ConstraintToConstraint,
    ] {
        let index = nodes.len() as u32;
        nodes.push(TypeNode::Function(flag));
        edges.extend([
            (index, 6, TypeEdge::Multiplicity),
            (index, 7, TypeEdge::Domain),
            (index, 7, TypeEdge::Codomain),
        ]);
    }
    for flag in [ForAllFlag::Required, ForAllFlag::Inferred] {
        let index = nodes.len() as u32;
        nodes.push(TypeNode::ForAll(flag));
        edges.extend([(index, 2, TypeEdge::Kind), (index, 3, TypeEdge::Body)]);
    }
    program.types =
        tidepool_test_data::prepared::type_graph(nodes, &edges, &program.constructors).unwrap();
    let prepared = parse_program(
        &tidepool_test_data::prepared_encode::encode_wire_program(&program),
        &requirements(),
        DecodeLimits::default(),
    )
    .unwrap();
    assert_eq!(prepared.types(), &program.types);
}

#[test]
fn codec_rejects_retired_prepared_schema_and_graph_versions() {
    let program = tidepool_repr::execution_schema::testing::wire_program();
    let encoded = tidepool_test_data::prepared_encode::encode_wire_program(&program);
    for version in [14, 15] {
        let mut value: Cbor = ciborium::de::from_reader(encoded.as_slice()).unwrap();
        let Cbor::Array(fields) = &mut value else {
            unreachable!()
        };
        fields[1] = int(version);
        assert_eq!(
            parse_program(&bytes(&value), &requirements(), DecodeLimits::default()),
            Err(ParseError::UnsupportedVersion(version))
        );
    }
    let mut value: Cbor = ciborium::de::from_reader(encoded.as_slice()).unwrap();
    let Cbor::Array(fields) = &mut value else {
        unreachable!()
    };
    let Cbor::Array(graph) = &mut fields[13] else {
        unreachable!()
    };
    graph[0] = int(0);
    assert!(matches!(
        parse_program(&bytes(&value), &requirements(), DecodeLimits::default()),
        Err(ParseError::Malformed(_))
    ));
}

#[test]
fn codec_decodes_constructor_static_reply_without_sites() {
    let mut program = scalar_program(Cbor::Array(vec![int(4), int(64)]));
    let Cbor::Array(fields) = &mut program else {
        unreachable!()
    };
    let symbol = |occurrence: &str| {
        Cbor::Array(vec![
            Cbor::Text("fixture".into()),
            Cbor::Text("Effects".into()),
            Cbor::Text("value".into()),
            Cbor::Text(occurrence.into()),
            Cbor::Array(vec![int(0)]),
        ])
    };
    fields[8] = Cbor::Array(vec![Cbor::Array(vec![
        symbol("Print"),
        symbol("Console"),
        Cbor::Array(vec![]),
        Cbor::Array(vec![]),
        Cbor::Array(vec![
            Cbor::Array(vec![]),
            int(1),
            int(0),
            Cbor::Array(vec![]),
        ]),
        Cbor::Array(vec![int(1)]),
        int(1),
        int(1),
        int(9001),
    ])]);
    fields[13] = text_graph_value();
    fields[15] = Cbor::Array(vec![Cbor::Array(vec![
        int(0),
        Cbor::Array(vec![int(0), int(0)]),
    ])]);
    let prepared =
        parse_program(&bytes(&program), &requirements(), DecodeLimits::default()).unwrap();
    assert_eq!(
        prepared.constructor_replies(),
        &[(ConstructorId(0), ConstructorReply::Static(TypeNodeId(0)))]
    );
    assert!(prepared.sites().is_empty());

    // The table is bounded by the site limit and its entries are pairs.
    let limits = DecodeLimits {
        max_sites: 0,
        ..DecodeLimits::default()
    };
    assert!(parse_program(&bytes(&program), &requirements(), limits).is_err());
    let Cbor::Array(fields) = &mut program else {
        unreachable!()
    };
    fields[15] = Cbor::Array(vec![Cbor::Array(vec![int(0)])]);
    assert!(matches!(
        parse_program(&bytes(&program), &requirements(), DecodeLimits::default()),
        Err(ParseError::Malformed(_))
    ));
    // The retired integer-only routing entry is not the new evidence grammar.
    let Cbor::Array(fields) = &mut program else {
        unreachable!()
    };
    fields[15] = Cbor::Array(vec![Cbor::Array(vec![int(0), int(7)])]);
    assert!(matches!(
        parse_program(&bytes(&program), &requirements(), DecodeLimits::default()),
        Err(ParseError::Malformed(_))
    ));
}

#[test]
fn codec_admits_zero_at_site_with_valid_carrier_and_reply_graph() {
    let mut program = scalar_program(Cbor::Array(vec![int(4), int(64)]));
    let Cbor::Array(fields) = &mut program else {
        unreachable!()
    };
    let symbol = |occurrence: &str| {
        Cbor::Array(vec![
            Cbor::Text("fixture".into()),
            Cbor::Text("Effects".into()),
            Cbor::Text("value".into()),
            Cbor::Text(occurrence.into()),
            Cbor::Array(vec![int(0)]),
        ])
    };
    let carrier = Cbor::Array(vec![int(4), int(64)]);
    fields[8] = Cbor::Array(vec![Cbor::Array(vec![
        symbol("Request"),
        symbol("Effect"),
        Cbor::Array(vec![carrier.clone()]),
        Cbor::Array(vec![Cbor::Bool(true)]),
        Cbor::Array(vec![
            Cbor::Array(vec![Cbor::Array(vec![carrier, int(0)])]),
            int(8),
            int(8),
            Cbor::Array(vec![Cbor::Bool(false)]),
        ]),
        Cbor::Array(vec![int(1)]),
        int(1),
        int(1),
        int(9001),
    ])]);
    fields[13] = text_graph_value();
    fields[14] = Cbor::Array(vec![Cbor::Array(vec![
        int(0),
        Cbor::Text("Effects.request".into()),
        int(0),
        int(0),
        int(0),
        Cbor::Array(vec![]),
    ])]);
    fields[15] = Cbor::Array(vec![Cbor::Array(vec![int(0), Cbor::Array(vec![int(1)])])]);
    let prepared =
        parse_program(&bytes(&program), &requirements(), DecodeLimits::default()).unwrap();
    assert_eq!(
        prepared.constructor_replies(),
        &[(ConstructorId(0), ConstructorReply::AtSite)]
    );
    let row = prepared
        .site(0)
        .expect("zero is an ordinary declared site ID");
    assert_eq!(row.delivery, SiteDelivery::HostAnswer);
    assert!(matches!(
        prepared.type_node(row.wire),
        Some(TypeNode::Root { .. })
    ));

    let Cbor::Array(fields) = &mut program else {
        unreachable!()
    };
    let Cbor::Array(rows) = &mut fields[14] else {
        unreachable!()
    };
    rows.push(rows[0].clone());
    assert_eq!(
        parse_program(&bytes(&program), &requirements(), DecodeLimits::default()),
        Err(ParseError::DuplicateDefinition("site".into()))
    );
}

#[test]
fn public_parse_rejects_wrong_declared_result_representation() {
    let program = scalar_program(Cbor::Array(vec![int(1)]));
    assert!(matches!(
        parse_program(&bytes(&program), &requirements(), DecodeLimits::default()),
        Err(ParseError::InvalidSignature(_))
    ));
}

#[test]
fn valid_artifact_truncations_and_single_bit_mutations_never_panic() {
    // IntRep is tag 4 in the wire format.
    let program = scalar_program(Cbor::Array(vec![int(4), int(64)]));
    let encoded = bytes(&program);
    let limits = DecodeLimits {
        max_bytes: 4096,
        max_nodes: 128,
        max_table_entries: 128,
        max_string_bytes: 1024,
        max_work: 4096,
        max_type_nodes: 128,
        max_sites: 128,
    };
    parse_program(&encoded, &requirements(), limits).expect("mutation seed is valid");
    let decoded: Cbor = ciborium::de::from_reader(encoded.as_slice()).unwrap();
    assert_eq!(bytes(&decoded), encoded);
    for end in 0..encoded.len() {
        assert!(
            parse_program(&encoded[..end], &requirements(), limits).is_err(),
            "prefix {end}"
        );
    }
    for offset in 0..encoded.len() {
        for bit in 0..8 {
            let mut mutant = encoded.clone();
            mutant[offset] ^= 1 << bit;
            // Some mutations remain valid programs. Both a checked program and
            // a typed refusal are acceptable; panic is not.
            parse_program(&mutant, &requirements(), limits).ok();
        }
    }
}

#[test]
fn codec_keeps_closed_reply_and_independent_original_site_fields() {
    let mut program = scalar_program(Cbor::Array(vec![int(4), int(64)]));
    let Cbor::Array(fields) = &mut program else {
        unreachable!()
    };
    let symbol = |occurrence: &str| {
        Cbor::Array(vec![
            Cbor::Text("fixture".into()),
            Cbor::Text("Effects".into()),
            Cbor::Text("constructor".into()),
            Cbor::Text(occurrence.into()),
            Cbor::Array(vec![int(0)]),
        ])
    };
    let carrier = Cbor::Array(vec![int(4), int(64)]);
    let managed = Cbor::Array(vec![int(1)]);
    fields[8] = Cbor::Array(vec![Cbor::Array(vec![
        symbol("Request"),
        symbol("Effect"),
        Cbor::Array(vec![carrier.clone(), carrier.clone(), managed.clone()]),
        Cbor::Array(vec![Cbor::Bool(true), Cbor::Bool(true), Cbor::Bool(false)]),
        Cbor::Array(vec![
            Cbor::Array(vec![
                Cbor::Array(vec![carrier.clone(), int(0)]),
                Cbor::Array(vec![carrier, int(8)]),
                Cbor::Array(vec![managed, int(16)]),
            ]),
            int(8),
            int(24),
            Cbor::Array(vec![Cbor::Bool(false), Cbor::Bool(false), Cbor::Bool(true)]),
        ]),
        Cbor::Array(vec![int(1)]),
        int(1),
        int(1),
        int(9001),
    ])]);
    fields[13] = text_graph_value();
    let reply = |field, payload, capture| {
        Cbor::Array(vec![Cbor::Array(vec![
            int(0),
            Cbor::Array(vec![int(2), int(0), int(field), int(payload), capture]),
        ])])
    };
    fields[15] = reply(1, 2, int(1));
    let prepared =
        parse_program(&bytes(&program), &requirements(), DecodeLimits::default()).unwrap();
    assert_eq!(
        prepared.constructor_replies(),
        &[(
            ConstructorId(0),
            ConstructorReply::StaticWithSite {
                reply: TypeNodeId(0),
                field: 1,
                payload_field: 2,
                capture_input: Some(1),
            }
        )]
    );
    for (field, payload) in [(0, 2), (5, 2), (1, 1), (1, 5)] {
        let Cbor::Array(fields) = &mut program else {
            unreachable!()
        };
        fields[15] = reply(field, payload, Cbor::Null);
        assert!(matches!(
            parse_program(&bytes(&program), &requirements(), DecodeLimits::default()),
            Err(ParseError::InvalidLayout(_))
        ));
    }
    let Cbor::Array(fields) = &mut program else {
        unreachable!()
    };
    fields[15] = reply(1, 2, int(u64::from(u32::MAX) + 1));
    assert!(parse_program(&bytes(&program), &requirements(), DecodeLimits::default()).is_err());
    let Cbor::Array(fields) = &mut program else {
        unreachable!()
    };
    fields[1] = int(16);
    assert!(matches!(
        parse_program(&bytes(&program), &requirements(), DecodeLimits::default()),
        Err(ParseError::UnsupportedVersion(16))
    ));
}
