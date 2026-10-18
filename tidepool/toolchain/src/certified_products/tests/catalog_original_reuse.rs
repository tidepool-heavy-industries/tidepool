use super::*;

#[test]
fn catalog_original_binding_compares_exact_import_versions_and_complete_census() {
    let child = inherited_owner("Child");
    let original = full_native_fixture(
        "Parent",
        vec![
            (
                7,
                vec![PendingImportOwner::Source {
                    owner: child.clone(),
                    original_ordinal: 11,
                    binder: testing::identity("Child", "entry_11"),
                }],
            ),
            (13, vec![]),
        ],
        17,
    );
    let native = original.original_native().unwrap();
    let groups = native
        .groups
        .iter()
        .map(AuthenticatedOriginalGroup::admitted)
        .collect::<Vec<_>>();
    let compare = |groups: &[PendingCertifiedGroup]| {
        compare_catalog_original_binding(
            &original,
            original.owner(),
            original.interface_bytes(),
            original.product_bytes(),
            original.package_imports_bytes(),
            groups,
            original.module_interface().unwrap(),
            original.execution_source(),
            &native.interface_requirements,
            &native.packages,
        )
    };
    assert!(compare(&groups).is_ok());
    let mut rebound = groups.clone();
    let PendingImportOwner::Source { owner, .. } = &mut Arc::make_mut(&mut rebound[0].imports)[0]
    else {
        panic!("source edge control");
    };
    owner.module_version = ModuleVersion([99; 32]);
    assert!(matches!(
        compare(&rebound),
        Err(CatalogBindingDifference::Groups)
    ));
    assert!(matches!(
        compare(&groups[..1]),
        Err(CatalogBindingDifference::Groups)
    ));
    let duplicate = vec![groups[0].clone(), groups[0].clone()];
    assert!(matches!(
        compare(&duplicate),
        Err(CatalogBindingDifference::Groups)
    ));
}
