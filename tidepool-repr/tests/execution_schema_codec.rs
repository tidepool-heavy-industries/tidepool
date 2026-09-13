use ciborium::value::Value as Cbor;
use tidepool_repr::execution_schema::{
    parse_program, Architecture, DecodeLimits, Endianness, ParseError, ProgramRequirements,
    TargetDescriptor, EXECUTION_ABI_VERSION, SCHEMA_VERSION,
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
        ]),
        Cbor::Array(vec![int(1)]),
        Cbor::Array(vec![int(0)]),
        Cbor::Bool(true),
        generation,
    ])
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
fn codec_rejects_unknown_nested_tag() {
    let signatures = Cbor::Array(vec![Cbor::Array(vec![
        Cbor::Array(vec![Cbor::Array(vec![int(99)])]),
        Cbor::Array(vec![]),
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
fn codec_accepts_optional_generation_shape_and_rejects_unknown_tag() {
    let signatures = Cbor::Array(vec![Cbor::Array(vec![
        Cbor::Array(vec![]),
        Cbor::Array(vec![]),
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
        Cbor::Array(vec![]),
    ])])));
    assert!(matches!(
        parse_program(&one_table_entry, &requirements(), table_limits),
        Err(ParseError::LimitExceeded("table entries"))
    ));
}

#[test]
fn public_parse_rejects_wrong_declared_result_representation() {
    let a = |values| Cbor::Array(values);
    let signature = a(vec![a(vec![]), a(vec![a(vec![int(1)])])]);
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
    ]);
    let top = a(vec![symbol, a(vec![int(0), rhs])]);
    let mut program = root(a(vec![signature]));
    let Cbor::Array(fields) = &mut program else {
        unreachable!()
    };
    fields[10] = a(vec![body]);
    fields[11] = a(vec![a(vec![int(0), top])]);
    assert!(matches!(
        parse_program(&bytes(&program), &requirements(), DecodeLimits::default()),
        Err(ParseError::InvalidSignature(_))
    ));
}
