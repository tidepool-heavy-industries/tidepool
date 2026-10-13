use ciborium::value::Value;
use tidepool_repr::execution_schema::*;
use tidepool_repr::SessionVarId;

fn fixture() -> (Vec<u8>, ProgramRequirements, SymbolIdentity) {
    let wire = testing::wire_program();
    let entry = match &wire.bindings[0] {
        Group::NonRecursive(top) => top.identity.clone(),
        Group::Recursive(_) => unreachable!(),
    };
    let group = testing::projected_group(wire, 7).unwrap();
    let encoded = tidepool_test_data::prepared_encode::encode_projected_group(&group);
    (encoded, requirements(), entry)
}

fn requirements() -> ProgramRequirements {
    let envelope = testing::envelope();
    ProgramRequirements {
        schema_version: envelope.schema_version,
        projection_profile: envelope.projection_profile,
        toolchain: envelope.toolchain,
        execution_abi_version: envelope.execution_abi_version,
        target: envelope.target,
    }
}

fn symbol_value(symbol: &SymbolIdentity) -> Value {
    Value::Array(vec![
        Value::Text(symbol.unit.clone()),
        Value::Text(symbol.module.clone()),
        Value::Text(symbol.namespace.clone()),
        Value::Text(symbol.occurrence.clone()),
        match &symbol.record_parent {
            None => Value::Array(vec![Value::Integer(0.into())]),
            Some(parent) => {
                Value::Array(vec![Value::Integer(1.into()), Value::Text(parent.clone())])
            }
        },
    ])
}

#[test]
fn module_framing_normalizes_outer_widths_and_preserves_opaque_group_bytes() {
    let (group, requirements, entry) = fixture();
    // The group itself has a valid nonminimal definite array width. TPMOD
    // normalization must preserve it as an opaque byte string.
    assert_eq!(group[0], 0x93);
    let mut opaque = vec![0x98, 19];
    opaque.extend_from_slice(&group[1..]);
    let row = Value::Array(vec![
        Value::Text(entry.unit),
        Value::Text(entry.module),
        Value::Bytes(vec![0x42, 0x00, 0xff]),
        Value::Array(vec![Value::Bytes(opaque.clone())]),
    ]);
    let mut singleton = Vec::new();
    ciborium::ser::into_writer(&("TPMOD", 1u64, [&row]), &mut singleton).unwrap();
    let mut widened = vec![0x98, 3, 0x78, 5];
    widened.extend_from_slice(b"TPMOD");
    widened.extend_from_slice(&[0x18, 1, 0x98, 1, 0x98, 4]);
    // Widen the product's unit-text length without changing its identity.
    let Value::Array(fields) = &row else {
        unreachable!()
    };
    let unit = fields[0].as_text().unwrap();
    assert!(unit.len() < 256);
    widened.extend_from_slice(&[0x78, unit.len() as u8]);
    widened.extend_from_slice(unit.as_bytes());
    for value in &fields[1..] {
        ciborium::ser::into_writer(value, &mut widened).unwrap();
    }
    let (products, frames) = parse_module_products_with_framing(
        &widened,
        &requirements,
        InventoryDecodeLimits::default(),
    )
    .unwrap();
    assert_eq!(products[0].interface, [0x42, 0x00, 0xff]);
    assert_eq!(frames, [singleton.clone()]);
    let Value::Array(header) = ciborium::de::from_reader(frames[0].as_slice()).unwrap() else {
        unreachable!()
    };
    assert_eq!(
        header[2].as_array().unwrap()[0].as_array().unwrap()[3],
        Value::Array(vec![Value::Bytes(opaque)])
    );
    let unrelated = Value::Array(vec![
        Value::Text("other".into()),
        Value::Text("Unrelated".into()),
        Value::Bytes(vec![0x43]),
        Value::Array(vec![]),
    ]);
    let mut combined = Vec::new();
    ciborium::ser::into_writer(&("TPMOD", 1u64, [&row, &unrelated]), &mut combined).unwrap();
    let (_, frames) = parse_module_products_with_framing(
        &combined,
        &requirements,
        InventoryDecodeLimits::default(),
    )
    .unwrap();
    assert_eq!(frames[0], singleton);
    let mut limits = InventoryDecodeLimits::default();
    limits.max_bytes = widened.len() - 1;
    assert!(matches!(
        parse_module_products_with_framing(&widened, &requirements, limits),
        Err(ParseError::InventoryByteLimit { .. })
    ));
    let mut trailing = widened;
    trailing.push(0);
    assert_eq!(
        parse_module_products_with_framing(
            &trailing,
            &requirements,
            InventoryDecodeLimits::default(),
        ),
        Err(ParseError::TrailingBytes)
    );
}

#[test]
fn large_module_product_fits_default_work_ceiling_within_admitted_bytes() {
    let (group, requirements, entry) = fixture();
    let interface_len = 20 << 20;
    let row = Value::Array(vec![
        Value::Text(entry.unit),
        Value::Text(entry.module),
        Value::Bytes(vec![0x5a; interface_len]),
        Value::Array(vec![Value::Bytes(group)]),
    ]);
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(&("TPMOD", 1u64, [&row]), &mut bytes).unwrap();
    let limits = InventoryDecodeLimits {
        max_module_bytes: 64 << 20,
        ..InventoryDecodeLimits::default()
    };
    let (products, frames) =
        parse_module_products_with_framing(&bytes, &requirements, limits).unwrap();
    assert_eq!(products.len(), 1);
    assert_eq!(products[0].groups.len(), 1);
    assert_eq!(products[0].interface.len(), interface_len);
    assert!(products[0].interface.iter().all(|byte| *byte == 0x5a));
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0], bytes);
    assert!(matches!(
        parse_module_products_with_framing(
            &bytes,
            &requirements,
            InventoryDecodeLimits {
                max_bytes: bytes.len() - 1,
                ..limits
            },
        ),
        Err(ParseError::InventoryByteLimit { .. }),
    ));
}

#[test]
fn entry_free_group_preserves_ordinal_without_executable_admission() {
    let (bytes, requirements, entry) = fixture();
    let group = parse_projected_group(&bytes, &requirements, DecodeLimits::default()).unwrap();
    assert_eq!(group.original_ordinal(), 7);
    assert_eq!(group.binders(), &[entry.clone()]);
    assert!(!group.definitions().bindings().is_empty());
}

#[test]
fn certified_group_requires_exact_binder_owner_and_aligned_imports() {
    let group = testing::projected_group(testing::wire_program(), 3).unwrap();
    let owner = CachedHomeOwner {
        unit: "fixture".into(),
        module: "Fixture".into(),
        module_version: ModuleVersion([1; 32]),
        skinny_iface_sha256: [2; 32],
        product_sha256: [3; 32],
    };
    let certified = CertifiedGroup::admit(owner.clone(), group.clone(), vec![]).unwrap();
    assert_eq!(certified.original_ordinal(), 3);
    assert_eq!(certified.binders(), group.binders());
    assert_eq!(certified.definitions().bindings().len(), 1);
    assert!(CertifiedGroup::admit(
        CachedHomeOwner {
            module: "Other".into(),
            ..owner.clone()
        },
        group.clone(),
        vec![],
    )
    .is_err());
    assert!(CertifiedGroup::admit(
        owner.clone(),
        group.clone(),
        vec![ImportOwner::Source {
            version: ModuleVersion([4; 32]),
            binder: testing::identity("Fixture", "entry"),
        }],
    )
    .is_err());
    // Inconsistent binder inventories are checked by the encoded tampering
    // control below; external tests cannot mutate a validated group.
}

#[test]
fn native_code_identity_excludes_live_owners_and_preserves_definition_contracts() {
    let mut wire = testing::wire_program();
    let binder = testing::identity("Imports", "retained");
    wire.globals.push(GlobalDecl {
        identity: binder.clone(),
        rep: RuntimeRep::LiftedRef,
        entry_signature: None,
        required_evaluated: false,
        required_generation: Some(1),
    });
    let group = testing::projected_group(wire.clone(), 3).unwrap();
    let owner = CachedHomeOwner {
        unit: "fixture".into(),
        module: "Fixture".into(),
        module_version: ModuleVersion([1; 32]),
        skinny_iface_sha256: [2; 32],
        product_sha256: [3; 32],
    };
    let retained = |id| {
        vec![ImportOwner::Retained {
            id: SessionVarId::from_extract(id),
            generation: 1,
        }]
    };
    let first = CertifiedGroup::admit(owner.clone(), group.clone(), retained(1)).unwrap();
    let later = CertifiedGroup::admit(owner.clone(), group.clone(), retained(2)).unwrap();
    let exported = CertifiedGroup::admit(
        owner.clone(),
        group.clone(),
        vec![ImportOwner::CodeExport {
            binder: binder.clone(),
            generation: 1,
            root_id: 99,
            interface_digest: None,
        }],
    )
    .unwrap();
    assert_ne!(first, later);
    assert_ne!(first, exported);
    assert_eq!(first.code_identity(), later.code_identity());
    assert_eq!(first.code_identity(), exported.code_identity());
    let mut foreign = owner.clone();
    foreign.product_sha256[0] ^= 1;
    let different = CertifiedGroup::admit(foreign, group.clone(), retained(1)).unwrap();
    assert_ne!(first.code_identity(), different.code_identity());
    let ordinal = CertifiedGroup::admit(
        owner.clone(),
        testing::projected_group(wire.clone(), 4).unwrap(),
        retained(1),
    )
    .unwrap();
    assert_ne!(first.code_identity(), ordinal.code_identity());
    wire.globals[0].required_evaluated = true;
    let contract = CertifiedGroup::admit(
        owner,
        testing::projected_group(wire, 3).unwrap(),
        retained(1),
    )
    .unwrap();
    assert_ne!(first.code_identity(), contract.code_identity());
    assert!(CertifiedGroup::admit(
        first.owner().clone(),
        group,
        vec![ImportOwner::Retained {
            id: SessionVarId::from_extract(1),
            generation: 2,
        }]
    )
    .is_err());
}

#[test]
fn group_header_and_binder_tampering_fail_before_publication() {
    let (bytes, requirements, _) = fixture();
    let Value::Array(mut fields) = ciborium::de::from_reader(bytes.as_slice()).unwrap() else {
        unreachable!()
    };
    fields[0] = Value::Text("TPSTG".into());
    let mut changed = Vec::new();
    ciborium::ser::into_writer(&Value::Array(fields.clone()), &mut changed).unwrap();
    assert!(parse_projected_group(&changed, &requirements, DecodeLimits::default()).is_err());
    fields[0] = Value::Text("TPGRP".into());
    fields[3] = Value::Array(vec![symbol_value(&testing::identity("Other", "missing"))]);
    changed.clear();
    ciborium::ser::into_writer(&Value::Array(fields), &mut changed).unwrap();
    assert!(parse_projected_group(&changed, &requirements, DecodeLimits::default()).is_err());
}

#[test]
fn large_projected_group_membership_preserves_identity_duplicates_and_limits() {
    const TOPS: usize = 4096;
    let mut wire = tidepool_repr::execution_schema::testing::wire_program();
    wire.expressions.nodes.clear();
    wire.bindings = vec![Group::Recursive(
        (0..TOPS)
            .map(|index| TopBinding {
                identity: tidepool_repr::execution_schema::testing::identity(
                    "Large",
                    &format!("top{index:04}"),
                ),
                binding: HeapBinding {
                    id: ValueId(index as u32),
                    rhs: HeapRhs::Bytes(vec![]),
                },
            })
            .collect(),
    )];
    let typed = tidepool_repr::execution_schema::testing::projected_group(wire, 0).unwrap();
    let bytes = tidepool_test_data::prepared_encode::encode_projected_group(&typed);
    let Value::Array(mut group) = ciborium::de::from_reader(bytes.as_slice()).unwrap() else {
        unreachable!()
    };
    let encode = |fields: &[Value]| {
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&Value::Array(fields.to_vec()), &mut bytes).unwrap();
        bytes
    };
    let envelope = tidepool_repr::execution_schema::testing::envelope();
    let requirements = ProgramRequirements {
        schema_version: envelope.schema_version,
        projection_profile: envelope.projection_profile,
        toolchain: envelope.toolchain,
        execution_abi_version: envelope.execution_abi_version,
        target: envelope.target,
    };
    let encoded = encode(&group);
    let parsed = parse_projected_group(&encoded, &requirements, DecodeLimits::default()).unwrap();
    assert_eq!(parsed.binders().len(), TOPS);
    assert!(
        matches!(&parsed.definitions().bindings()[0], Group::Recursive(tops) if tops.len() == TOPS)
    );
    assert!(matches!(
        parse_projected_group(
            &encoded,
            &requirements,
            DecodeLimits {
                max_table_entries: TOPS - 1,
                ..DecodeLimits::default()
            }
        ),
        Err(ParseError::LimitExceeded("table entries"))
    ));
    assert!(matches!(
        parse_projected_group(
            &encoded,
            &requirements,
            DecodeLimits {
                max_work: 0,
                ..DecodeLimits::default()
            }
        ),
        Err(ParseError::LimitExceeded("work"))
    ));

    let Value::Array(binders) = &mut group[3] else {
        unreachable!()
    };
    binders[TOPS - 1] = binders[0].clone();
    assert!(
        matches!(parse_projected_group(&encode(&group), &requirements, DecodeLimits::default()), Err(ParseError::DuplicateDefinition(detail)) if detail == "projected group binder")
    );
    let Value::Array(binders) = &mut group[3] else {
        unreachable!()
    };
    let Value::Array(identity) = &mut binders[TOPS - 1] else {
        unreachable!()
    };
    identity[0] = Value::Text("another-unit".into());
    assert!(
        matches!(parse_projected_group(&encode(&group), &requirements, DecodeLimits::default()), Err(ParseError::InvalidReference(detail)) if detail.ends_with("is absent"))
    );
}

#[test]
fn module_product_groups_share_one_operation_work_budget() {
    let row = |module: &str| {
        let mut wire = testing::wire_program();
        match &mut wire.bindings[0] {
            Group::NonRecursive(top) => top.identity.module = module.into(),
            Group::Recursive(_) => unreachable!(),
        }
        let group = testing::projected_group(wire, 0).unwrap();
        Value::Array(vec![
            Value::Text("fixture".into()),
            Value::Text(module.into()),
            Value::Bytes(vec![1]),
            Value::Array(vec![Value::Bytes(
                tidepool_test_data::prepared_encode::encode_projected_group(&group),
            )]),
        ])
    };
    let encode = |rows: &[Value]| {
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&("TPMOD", 1u64, rows), &mut bytes).unwrap();
        bytes
    };
    let required = requirements();
    let first = row("First");
    let second = row("Second");
    let first_bytes = encode(std::slice::from_ref(&first));
    let second_bytes = encode(std::slice::from_ref(&second));
    let combined = encode(&[first, second]);
    let minimum = |bytes: &[u8]| {
        const SEARCH_CAP: usize = 1 << 20;
        let limits = |max_work| InventoryDecodeLimits {
            max_work,
            ..InventoryDecodeLimits::default()
        };
        assert!(
            parse_module_products(bytes, &required, limits(SEARCH_CAP)).is_ok(),
            "fixture should fit the bounded work search"
        );
        let mut low = 0;
        let mut high = SEARCH_CAP;
        while low + 1 < high {
            let middle = low + (high - low) / 2;
            match parse_module_products(bytes, &required, limits(middle)) {
                Ok(_) => high = middle,
                Err(ParseError::LimitExceeded("work")) => low = middle,
                Err(error) => panic!("budget control failed for another reason: {error}"),
            }
        }
        assert!(parse_module_products(bytes, &required, limits(high)).is_ok());
        assert!(matches!(
            parse_module_products(bytes, &required, limits(low)),
            Err(ParseError::LimitExceeded("work"))
        ));
        high
    };
    let first_limit = minimum(&first_bytes);
    let second_limit = minimum(&second_bytes);
    let limit = first_limit.max(second_limit);
    let limits = InventoryDecodeLimits {
        max_work: limit,
        ..InventoryDecodeLimits::default()
    };
    parse_module_products(&first_bytes, &required, limits).unwrap();
    parse_module_products(&second_bytes, &required, limits).unwrap();
    assert_eq!(
        parse_module_products(&combined, &required, limits),
        Err(ParseError::LimitExceeded("work"))
    );
    let combined_limit = minimum(&combined);
    assert!(combined_limit > limit);
    let products = parse_module_products(
        &combined,
        &required,
        InventoryDecodeLimits {
            max_work: combined_limit,
            ..InventoryDecodeLimits::default()
        },
    )
    .unwrap();
    assert_eq!(
        parse_module_products_with_framing(
            &combined,
            &required,
            InventoryDecodeLimits {
                max_work: combined_limit,
                ..InventoryDecodeLimits::default()
            }
        ),
        Err(ParseError::LimitExceeded("work")),
    );
    let (_, frames) =
        parse_module_products_with_framing(&combined, &required, InventoryDecodeLimits::default())
            .unwrap();
    assert_eq!(frames.len(), 2);
    assert_eq!(products.len(), 2);
    assert!(products.iter().all(|product| product.groups.len() == 1));
}
