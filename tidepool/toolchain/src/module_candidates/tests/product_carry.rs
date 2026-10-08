use super::*;
use crate::module_candidates::product_decode_observer::DecodeCounter;
use tidepool_repr::execution_schema::{Atom, ExprFrame, Group, ScalarLiteral, testing};

#[test]
fn ordinary_selection_decodes_once_and_refuses_corrupt_products() {
    let root = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let record = candidate_fixture(root.path(), "Library");
    let include = record.include.clone();
    let bytes = record.products.clone();
    let owner = record.original_owner.owner();
    let expected = RawModuleProduct {
        unit: "u".into(),
        module: "Library".into(),
        interface: b"u:Library".to_vec(),
        groups: vec![],
    };
    let counter = DecodeCounter::new();
    let selected = select_records(
        b"endpoint",
        &include,
        scratch.path(),
        vec![(record.clone(), CandidateOrigin::Ordinary)],
    )
    .unwrap();
    assert_eq!(counter.count(), 1);
    assert_eq!(selected.by_owner.len(), 1);
    let bundle = &selected.by_owner[&("u".into(), "Library".into())];
    assert_eq!(bundle.product.bytes(), bytes);
    assert_eq!(bundle.product.decoded(), &expected);
    assert_eq!(bundle.owner, owner);

    let mut invalid = record;
    invalid.products = b"not CBOR".to_vec();
    let omitted = select_records(
        b"endpoint",
        &include,
        scratch.path(),
        vec![(invalid, CandidateOrigin::Ordinary)],
    )
    .unwrap();
    assert_eq!(counter.count(), 2);
    assert!(omitted.by_owner.is_empty());
}

fn authored_product(
    unit: String,
    module: String,
    interface: Vec<u8>,
    ordinal: u32,
    literal: i64,
    with_group: bool,
) -> RawModuleProduct {
    let groups = if with_group {
        let mut wire = testing::wire_program();
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!("minimal fixture has one nonrecursive binding")
        };
        top.identity.unit = unit.clone();
        top.identity.module = module.clone();
        wire.expressions.nodes[0] = ExprFrame::Return(vec![Atom::Scalar(ScalarLiteral::Int {
            bits: 64,
            bytes: literal.to_be_bytes().to_vec(),
        })]);
        vec![testing::projected_group(wire, ordinal).unwrap()]
    } else {
        vec![]
    };
    RawModuleProduct {
        unit,
        module,
        interface,
        groups,
    }
}

proptest! {
    #![proptest_config(catalog_config())]

    #[test]
    fn generated_candidate_products_preserve_exact_bytes_and_authored_groups(
        unit in "[a-z][a-z0-9]{0,7}",
        module in "[A-Z][A-Za-z0-9]{0,7}",
        interface in prop::collection::vec(any::<u8>(), 1..65),
        ordinal in 0u32..128,
        literal in any::<i64>(),
        with_group in any::<bool>(),
        wide_header in any::<bool>(),
    ) {
        let expected = authored_product(unit, module, interface, ordinal, literal, with_group);
        let mut bytes = tidepool_test_data::prepared_encode::encode_module_products(
            std::slice::from_ref(&expected),
        );
        if wide_header {
            prop_assert_eq!(bytes[0], 0x83);
            bytes.splice(..1, [0x98, 3]);
        }
        let counter = DecodeCounter::new();
        let carrier = CandidateProduct::decode(bytes.clone()).unwrap();
        prop_assert_eq!(counter.count(), 1);
        prop_assert_eq!(carrier.bytes(), bytes.as_slice());
        prop_assert_eq!(carrier.decoded(), &expected);

        let mut trailing = bytes;
        trailing.push(0);
        prop_assert!(CandidateProduct::decode(trailing).is_none());
        prop_assert_eq!(counter.count(), 2);

        let empty = tidepool_test_data::prepared_encode::encode_module_products(&[]);
        prop_assert!(CandidateProduct::decode(empty).is_none());
        prop_assert_eq!(counter.count(), 3);

        let other = authored_product(
            format!("{}-other", expected.unit),
            expected.module.clone(),
            expected.interface.clone(),
            ordinal,
            literal,
            with_group,
        );
        let multiple = tidepool_test_data::prepared_encode::encode_module_products(&[expected, other]);
        prop_assert!(CandidateProduct::decode(multiple).is_none());
        prop_assert_eq!(counter.count(), 4);
    }
}
