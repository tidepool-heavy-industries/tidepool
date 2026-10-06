use ciborium::value::Value;
use tidepool_repr::execution_schema::{
    testing, DecodeLimits, InventoryDecodeLimits, InventoryOperation, ParseError,
    ProgramRequirements,
};

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

fn row(unit: &str, module: &str, interface: &[u8], groups: &[Vec<u8>]) -> Value {
    Value::Array(vec![
        Value::Text(unit.into()),
        Value::Text(module.into()),
        Value::Bytes(interface.into()),
        Value::Array(groups.iter().cloned().map(Value::Bytes).collect()),
    ])
}

fn inventory(rows: Vec<Value>) -> Vec<u8> {
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(
        &(
            "TPMOD",
            tidepool_repr::execution_schema::MODULE_PRODUCTS_VERSION,
            rows,
        ),
        &mut bytes,
    )
    .unwrap();
    bytes
}

fn singleton_size(row: &Value) -> usize {
    inventory(vec![row.clone()]).len()
}

fn limits(max_bytes: usize, max_module_bytes: usize) -> InventoryDecodeLimits {
    InventoryDecodeLimits {
        max_bytes,
        max_module_bytes,
        program: DecodeLimits::default(),
        max_work: 1 << 20,
    }
}

#[test]
fn several_original_owners_can_exceed_one_module_bound() {
    let first = row("unit", "First", &[1; 8], &[]);
    let second = row("unit", "Second", &[2; 8], &[]);
    let max_module_bytes = singleton_size(&first).max(singleton_size(&second));
    let bytes = inventory(vec![first, second]);
    assert!(bytes.len() > max_module_bytes);

    let products = InventoryOperation::new(limits(bytes.len(), max_module_bytes))
        .parse_module_products(&bytes, &requirements())
        .unwrap();
    assert_eq!(products.len(), 2);
    assert_eq!(products[0].module, "First");
    assert_eq!(products[1].module, "Second");
}

#[test]
fn singleton_module_limit_applies_to_plain_and_framed_decoding() {
    let owner = row("unit", "Large", &[0x42; 32], &[]);
    let bytes = inventory(vec![owner.clone()]);
    let max_module_bytes = singleton_size(&owner) - 1;
    let limits = limits(bytes.len(), max_module_bytes);

    assert!(matches!(
        InventoryOperation::new(limits).parse_module_products(&bytes, &requirements()),
        Err(ParseError::ModuleByteLimit { limit, .. }) if limit == max_module_bytes
    ));
    assert!(matches!(
        InventoryOperation::new(limits)
            .parse_module_products_with_framing(&bytes, &requirements()),
        Err(ParseError::ModuleByteLimit { limit, .. }) if limit == max_module_bytes
    ));
}

#[test]
fn inventory_group_and_operation_limits_refuse_independently() {
    let wire = testing::wire_program();
    let group = testing::projected_group(wire, 0).unwrap();
    let group_bytes = tidepool_test_data::prepared_encode::encode_projected_group(&group);
    let bytes = inventory(vec![row(
        "fixture",
        "Fixture",
        &[0x42],
        std::slice::from_ref(&group_bytes),
    )]);

    let mut group_limited = limits(bytes.len(), bytes.len());
    group_limited.program.max_bytes = group_bytes.len() - 1;
    assert!(matches!(
        InventoryOperation::new(group_limited).parse_module_products(&bytes, &requirements()),
        Err(ParseError::ByteLimit { .. })
    ));

    assert!(matches!(
        InventoryOperation::new(limits(bytes.len() - 1, bytes.len()))
            .parse_module_products(&bytes, &requirements()),
        Err(ParseError::InventoryByteLimit { .. })
    ));

    let mut operation_limited = limits(bytes.len(), bytes.len());
    operation_limited.max_work = 0;
    assert!(matches!(
        InventoryOperation::new(operation_limited).parse_module_products(&bytes, &requirements()),
        Err(ParseError::LimitExceeded("work"))
    ));
}

#[test]
fn inventory_operation_budget_is_cumulative_and_fresh_operations_reset_it() {
    let bytes = inventory(vec![row("unit", "Small", &[0x42], &[])]);
    let requirements = requirements();
    let mut low = 0;
    let mut high = 1 << 20;
    InventoryOperation::new(InventoryDecodeLimits {
        max_bytes: bytes.len(),
        max_module_bytes: bytes.len(),
        max_work: high,
        ..InventoryDecodeLimits::default()
    })
    .parse_module_products(&bytes, &requirements)
    .expect("generous bounded fixture budget must admit");
    while low + 1 < high {
        let middle = low + (high - low) / 2;
        match InventoryOperation::new(InventoryDecodeLimits {
            max_bytes: bytes.len(),
            max_module_bytes: bytes.len(),
            program: DecodeLimits::default(),
            max_work: middle,
        })
        .parse_module_products(&bytes, &requirements)
        {
            Ok(_) => high = middle,
            Err(ParseError::LimitExceeded("work")) => low = middle,
            Err(error) => panic!("unexpected threshold refusal: {error}"),
        }
    }

    let limits = InventoryDecodeLimits {
        max_bytes: bytes.len(),
        max_module_bytes: bytes.len(),
        program: DecodeLimits::default(),
        max_work: high,
    };
    let operation = InventoryOperation::new(limits);
    operation
        .parse_module_products(&bytes, &requirements)
        .unwrap();
    assert!(matches!(
        operation.parse_module_products(&bytes, &requirements),
        Err(ParseError::LimitExceeded("work"))
    ));
    InventoryOperation::new(limits)
        .parse_module_products(&bytes, &requirements)
        .unwrap();
}

#[test]
fn many_real_rows_validate_and_duplicate_owners_or_group_ordinals_fail() {
    let rows: Vec<_> = (0..129)
        .map(|index| {
            let module = format!("Module{index}");
            let mut wire = testing::wire_program();
            if let tidepool_repr::execution_schema::Group::NonRecursive(top) = &mut wire.bindings[0]
            {
                top.identity.unit = "unit".into();
                top.identity.module = module.clone();
            }
            let group = testing::projected_group(wire, index).unwrap();
            let group_bytes = tidepool_test_data::prepared_encode::encode_projected_group(&group);
            row(
                "unit",
                &module,
                &[index as u8],
                std::slice::from_ref(&group_bytes),
            )
        })
        .collect();
    for count in [127, 128, 129] {
        let bytes = inventory(rows[..count].to_vec());
        let mut policy = limits(bytes.len(), bytes.len());
        // This fixture probes owner cardinality rather than cumulative work.
        // Keep a generous finite budget for all three complete inventories.
        policy.max_work = 32 << 20;
        let products = InventoryOperation::new(policy)
            .parse_module_products(&bytes, &requirements())
            .unwrap();
        assert_eq!(products.len(), count);
    }

    let duplicate_owner = inventory(vec![
        row("unit", "Same", &[1], &[]),
        row("unit", "Same", &[2], &[]),
    ]);
    assert!(matches!(
        InventoryOperation::new(limits(duplicate_owner.len(), duplicate_owner.len()))
            .parse_module_products(&duplicate_owner, &requirements()),
        Err(ParseError::DuplicateDefinition(_))
    ));

    let group = testing::projected_group(testing::wire_program(), 7).unwrap();
    let group_bytes = tidepool_test_data::prepared_encode::encode_projected_group(&group);
    let duplicate_ordinal = inventory(vec![row(
        "fixture",
        "Fixture",
        &[1],
        &[group_bytes.clone(), group_bytes],
    )]);
    assert!(matches!(
        InventoryOperation::new(limits(duplicate_ordinal.len(), duplicate_ordinal.len()))
            .parse_module_products(&duplicate_ordinal, &requirements()),
        Err(ParseError::DuplicateDefinition(reason)) if reason == "original group ordinal"
    ));
}
