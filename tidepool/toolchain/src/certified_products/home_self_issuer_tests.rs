use super::*;

#[test]
fn ghc_home_self_owner_refuses_substituted_requirements() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("HomeSelf.hs"),
        include_str!("../../tests/fixtures/home-self-issuer/HomeSelf.hs"),
    )
    .unwrap();
    let source = include_str!("../../tests/fixtures/home-self-issuer/HomeSelfCapture.hs");
    let compiled = crate::artifacts::test_support::compile_targets(
        source,
        &["result"],
        &[root.path().to_owned()],
        |_, _, _| {},
    )
    .expect("compile the independent HomeSelf through the production issuer");
    assert!(compiled.producer_identity.is_some());
    let original = compiled
        .recovery_products
        .iter()
        .find(|product| product.owner().module == "HomeSelf")
        .expect("the actual GHC HomeSelf original product");
    let owner = original.owner();
    assert_eq!(owner.unit, "main");
    let bytes = original.certification_bytes();
    let witness = decode_home_witness(bytes).expect("the production-issued Home seal");
    assert_eq!(&witness.owner, owner);
    assert!(
        !witness.groups.is_empty(),
        "the original executable groups are sealed"
    );
    assert_eq!(
        witness.sources,
        BTreeMap::from([((owner.unit.clone(), owner.module.clone()), owner.clone())]),
        "native self custody retains the full original owner"
    );
    let native = original_native_requirements(original).unwrap();
    assert!(!native.artifact_edges.is_empty());
    assert!(native
        .artifact_edges
        .iter()
        .all(|(required, _)| { required.unit == owner.unit && required.module == owner.module }));
    let canonical = original
        .module_interface()
        .expect("actual canonical GHC interface");
    validate_original_module_interface(original, canonical).unwrap();
    validate_home_certification(bytes, owner).unwrap();
    let modules = tidepool_repr::execution_schema::parse_module_products(
        original.product_bytes(),
        &crate::prepared_artifact::production_requirements().unwrap(),
        crate::module_candidates::product_decode_limits(),
    )
    .unwrap();
    assert_eq!(modules.len(), 1);
    assert_eq!(modules[0].interface, original.interface_bytes());
    assert_eq!(modules[0].unit, owner.unit);
    assert_eq!(modules[0].module, owner.module);
    assert_eq!(modules[0].groups.len(), witness.groups.len());

    // Only negative controls edit wire fields. The original native groups,
    // interface, package witnesses and owner remain the actual compiler output.
    let seal: Value = ciborium::de::from_reader(bytes).unwrap();
    for (field, replacement) in [(0, "foreign"), (1, "ForeignHome")] {
        let mut changed = seal.clone();
        let sources = changed.as_array_mut().unwrap()[4].as_array_mut().unwrap();
        assert_eq!(sources.len(), 1);
        sources[0].as_array_mut().unwrap()[field] = value_text(replacement);
        let mut invalid = Vec::new();
        ciborium::ser::into_writer(&changed, &mut invalid).unwrap();
        assert!(matches!(
            validate_home_certification(&invalid, owner),
            Err(CertificationError::Mismatch("home source witness"))
        ));
    }
    assert_eq!(original.certification_bytes(), bytes);
    validate_home_certification(bytes, owner).unwrap();
    validate_original_module_interface(original, canonical).unwrap();
}
