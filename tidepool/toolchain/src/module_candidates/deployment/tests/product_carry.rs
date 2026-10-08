use super::*;
use crate::module_candidates::product_decode_observer::DecodeCounter;
use tidepool_repr::execution_schema::RawModuleProduct;

#[test]
fn deployment_hydration_decodes_once_and_selection_carries_exact_products() {
    let names = ["A", "B"];
    let fixture = Fixture::with_modules(&names);
    let counter = DecodeCounter::new();
    let package = fixture.load().unwrap();
    assert_eq!(counter.count(), names.len());

    let expected = names
        .iter()
        .map(|name| {
            let product = RawModuleProduct {
                unit: "u".into(),
                module: (*name).into(),
                interface: name.as_bytes().to_vec(),
                groups: vec![],
            };
            let record = package
                .records
                .iter()
                .find(|record| record.record().module == *name)
                .unwrap();
            assert_eq!(record.product, product);
            assert_eq!(
                record.record().products,
                product_bytes("u", name, name.as_bytes())
            );
            (
                product,
                record.record().products.clone(),
                record.record().original_owner.owner(),
            )
        })
        .collect::<Vec<_>>();

    let scratch = tempfile::tempdir().unwrap();
    let mut include = vec![scratch.path().to_path_buf()];
    include.extend(package.source_selection().include_roots());
    let candidates = package.into_candidates(&[3; 32]).unwrap();
    assert_eq!(counter.count(), names.len());
    let selected = super::super::super::select_records_inner(
        &[3; 32],
        &include,
        scratch.path(),
        candidates,
        None,
    )
    .unwrap();
    assert_eq!(counter.count(), names.len());
    assert_eq!(selected.by_owner.len(), names.len());
    for (product, bytes, owner) in expected {
        let bundle = &selected.by_owner[&(product.unit.clone(), product.module.clone())];
        assert_eq!(bundle.product.bytes(), bytes);
        assert_eq!(bundle.product.decoded(), &product);
        assert_eq!(bundle.owner, owner);
    }
}

#[test]
fn fresh_deployment_offers_redecode_and_refuse_changed_product_files() {
    let fixture = Fixture::new();
    let counter = DecodeCounter::new();
    let first = fixture.load().unwrap();
    assert_eq!(counter.count(), 1);
    let original = first.records[0].record().products.clone();
    let first_owner = first.records[0].record().original_owner.owner();
    drop(first);

    let second = fixture.load().unwrap();
    assert_eq!(counter.count(), 2);
    assert_eq!(second.records[0].record().products, original);
    assert_eq!(
        second.records[0].record().original_owner.owner(),
        first_owner
    );
    drop(second);

    let reference = fixture.catalog().modules[0].products.clone();
    let path = fixture.output.join(reference.path);
    let mut corrupt = original.clone();
    corrupt.push(0);
    fs::write(&path, corrupt).unwrap();
    assert!(matches!(
        fixture.load(),
        Err(ModulePackageError::ArtifactChanged(changed)) if changed == path
    ));
    assert_eq!(
        counter.count(),
        2,
        "changed file is refused before product decoding"
    );

    fs::write(path, &original).unwrap();
    let restored = fixture.load().unwrap();
    assert_eq!(counter.count(), 3);
    assert_eq!(restored.records[0].record().products, original);
    assert_eq!(
        restored.records[0].record().original_owner.owner(),
        first_owner
    );
}

#[test]
fn deployment_singleton_decode_refuses_invalid_framing() {
    let fixture = Fixture::new();
    let package = fixture.load().unwrap();
    let record = package.records[0].record().clone();
    let product = package.records[0].product.clone();
    let mut trailing = record.products.clone();
    trailing.push(0);
    let mut other = product.clone();
    other.module = "Other".into();
    let cases = [
        b"not CBOR".to_vec(),
        trailing,
        tidepool_test_data::prepared_encode::encode_module_products(&[]),
        tidepool_test_data::prepared_encode::encode_module_products(&[product, other]),
    ];
    let counter = DecodeCounter::new();
    for (index, products) in cases.into_iter().enumerate() {
        let mut invalid = record.clone();
        invalid.data.products = products;
        assert!(matches!(
            DecodedDeploymentRecord::decode(invalid, &InventoryOperation::new(Default::default()),),
            Err(ModulePackageError::Format(_))
        ));
        assert_eq!(counter.count(), index + 1);
    }
}
