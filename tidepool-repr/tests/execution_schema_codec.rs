use ciborium::value::Value as Cbor;
use tidepool_repr::execution_schema::{
    parse_program, Architecture, DecodeLimits, Endianness, ParseError, ProgramRequirements,
    SiteDelivery, TargetDescriptor, TypeNode, TypeNodeId, EXECUTION_ABI_VERSION, SCHEMA_VERSION,
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
        Cbor::Array(vec![]),
        Cbor::Array(vec![]),
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
fn codec_rejects_schema_9_shape_before_enforcing_schema_10_length() {
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
    fields[13] = Cbor::Array(vec![Cbor::Array(vec![int(1)])]);
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

#[test]
fn codec_decodes_type_graph_and_site_rows() {
    let mut program = scalar_program(Cbor::Array(vec![int(4), int(64)]));
    let Cbor::Array(fields) = &mut program else {
        unreachable!()
    };
    let family = Cbor::Array(vec![
        Cbor::Text("fixture".into()),
        Cbor::Text("Types".into()),
        Cbor::Text("type".into()),
        Cbor::Text("Phantom".into()),
        Cbor::Array(vec![int(0)]),
    ]);
    let constructor_identity = Cbor::Array(vec![
        Cbor::Text("fixture".into()),
        Cbor::Text("Types".into()),
        Cbor::Text("value".into()),
        Cbor::Text("Recursive".into()),
        Cbor::Array(vec![int(0)]),
    ]);
    fields[8] = Cbor::Array(vec![Cbor::Array(vec![
        constructor_identity,
        family.clone(),
        Cbor::Array(vec![Cbor::Array(vec![int(1)])]),
        Cbor::Array(vec![Cbor::Bool(false)]),
        Cbor::Array(vec![
            Cbor::Array(vec![Cbor::Array(vec![Cbor::Array(vec![int(1)]), int(0)])]),
            int(8),
            int(8),
            Cbor::Array(vec![Cbor::Bool(true)]),
        ]),
        Cbor::Array(vec![int(1)]),
        int(1),
        int(1),
        int(9001),
    ])]);
    fields[13] = Cbor::Array(vec![
        Cbor::Array(vec![int(4), Cbor::Array(vec![int(4), int(64)])]),
        Cbor::Array(vec![
            int(0),
            family,
            Cbor::Array(vec![int(0)]),
            Cbor::Array(vec![Cbor::Array(vec![int(0), Cbor::Array(vec![int(1)])])]),
        ]),
    ]);
    fields[14] = Cbor::Array(
        (0..4)
            .map(|delivery| {
                Cbor::Array(vec![
                    int(41 + delivery),
                    Cbor::Text("Types.hs:1".into()),
                    int(delivery),
                    int(delivery),
                    int(1),
                    Cbor::Array(vec![int(0)]),
                ])
            })
            .collect(),
    );

    let prepared =
        parse_program(&bytes(&program), &requirements(), DecodeLimits::default()).unwrap();
    assert!(matches!(
        prepared.type_node(TypeNodeId(0)),
        Some(TypeNode::Scalar(
            tidepool_repr::execution_schema::RuntimeRep::Int(64)
        ))
    ));
    let TypeNode::Data { arguments, .. } = &prepared.types()[1] else {
        panic!("expected data type node")
    };
    assert_eq!(arguments, &[TypeNodeId(0)]);
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
            SiteDelivery::TerminalCapture,
        ]
    );

    let Cbor::Array(fields) = &mut program else {
        unreachable!()
    };
    let Cbor::Array(sites) = &mut fields[14] else {
        unreachable!()
    };
    let Cbor::Array(site) = &mut sites[0] else {
        unreachable!()
    };
    site[5] = Cbor::Array(vec![int(99)]);
    assert!(matches!(
        parse_program(&bytes(&program), &requirements(), DecodeLimits::default()),
        Err(ParseError::InvalidReference(detail)) if detail.contains("type node")
    ));
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
            let _ = parse_program(&mutant, &requirements(), limits);
        }
    }
}
