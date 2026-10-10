//! Compiler namespace choices and target demand are independent of custody.
//! Expectations come from fixture facts and finite-set recomputation.
use super::*;
use crate::certified_products::{
    tests::{
        original_groups_fixture, original_groups_fixture_in_unit_with_sites,
        recovered_witness_fixtures,
    },
    PendingImportOwner,
};
use crate::declaration_context::ExactDeclarationContext;
use crate::declaration_join::ExactLexicalNode;
use proptest::prelude::*;
use proptest::test_runner::{contextualize_config, Config, FileFailurePersistence, TestRunner};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use tidepool_repr::execution_schema::{CachedHomeOwner, SymbolIdentity};

const MODULES: usize = 3;
const VERSIONS: usize = 2;
const ORDINALS: [u32; 3] = [3, 11, 29];
const PRODUCER: &[u8] = b"compiler projection properties";
type RawGroup = (usize, usize, u32);
type Roles = [Option<usize>; MODULES];

struct Catalog {
    originals: Vec<Arc<ArtifactEntry>>,
    interfaces: Vec<Arc<ArtifactEntry>>,
    capture: Arc<ArtifactEntry>,
    edges: BTreeMap<RawGroup, BTreeSet<RawGroup>>,
    generations: [u64; MODULES * VERSIONS],
}

fn owner(module: usize) -> ExactModuleIdentity {
    ExactModuleIdentity {
        unit: format!("projection_unit_{}", module / 2),
        module: format!("Projection{}", module % 2),
    }
}

fn binder(module: usize, ordinal: u32) -> SymbolIdentity {
    SymbolIdentity {
        unit: owner(module).unit,
        module: owner(module).module,
        namespace: "value".into(),
        occurrence: format!("entry_{ordinal}"),
        record_parent: None,
    }
}

fn interface_bytes(module: usize) -> Vec<u8> {
    let exact = owner(module);
    format!("{}.{}", exact.unit, exact.module).into_bytes()
}

impl Catalog {
    fn new(cross_version: bool, generations: [u64; MODULES * VERSIONS]) -> Self {
        let mut edges = BTreeMap::<RawGroup, BTreeSet<RawGroup>>::new();
        for version in 0..VERSIONS {
            let helper_version = version ^ usize::from(cross_version);
            for module in 0..MODULES - 1 {
                edges.insert(
                    (module, version, 3),
                    BTreeSet::from([(module + 1, helper_version, 3)]),
                );
                edges.insert(
                    (module, version, 11),
                    BTreeSet::from([(module + 1, helper_version, 3)]),
                );
            }
            // Each source island has one helper version per module and a helper
            // SCC. Module and ordinal alone cannot distinguish the islands.
            edges.insert((2, version, 3), BTreeSet::from([(1, helper_version, 3)]));
        }
        let placeholders = (0..VERSIONS)
            .flat_map(|version| (0..MODULES).map(move |module| (module, version)))
            .map(|(module, version)| {
                original_groups_fixture_in_unit_with_sites(
                    &owner(module).unit,
                    &owner(module).module,
                    ORDINALS
                        .iter()
                        .map(|ordinal| (*ordinal, Vec::new()))
                        .collect(),
                    version as u8 + 1,
                    &BTreeMap::new(),
                    interface_bytes(module),
                    &BTreeMap::new(),
                )
            })
            .collect::<Vec<_>>();
        let build = |homes: &[CachedHomeOwner]| {
            (0..VERSIONS)
                .flat_map(|version| (0..MODULES).map(move |module| (module, version)))
                .map(|(module, version)| {
                    let groups = ORDINALS
                        .iter()
                        .map(|ordinal| {
                            let mut imports = edges
                                .get(&(module, version, *ordinal))
                                .into_iter()
                                .flatten()
                                .map(|(target, variant, required)| PendingImportOwner::Source {
                                    owner: homes[variant * MODULES + target].clone(),
                                    original_ordinal: *required,
                                    binder: binder(*target, *required),
                                })
                                .collect::<Vec<_>>();
                            if *ordinal == 11 {
                                imports.push(PendingImportOwner::Retained {
                                    identity: Self::capture_identity(module, version),
                                    generation: generations[version * MODULES + module],
                                });
                            }
                            (*ordinal, imports)
                        })
                        .collect();
                    original_groups_fixture_in_unit_with_sites(
                        &owner(module).unit,
                        &owner(module).module,
                        groups,
                        version as u8 + 1,
                        &BTreeMap::new(),
                        interface_bytes(module),
                        &BTreeMap::new(),
                    )
                })
                .collect::<Vec<_>>()
        };
        let first = build(
            &placeholders
                .iter()
                .map(|p| p.owner().clone())
                .collect::<Vec<_>>(),
        );
        let products = build(&first.iter().map(|p| p.owner().clone()).collect::<Vec<_>>());
        assert!(
            first
                .iter()
                .zip(&products)
                .all(|(a, b)| a.owner() == b.owner()),
            "fixture source owners reach a fixed point"
        );
        let producer = Sha256::digest(PRODUCER).into();
        let finalized = products
            .into_iter()
            .enumerate()
            .map(|(index, product)| {
                let module = index % MODULES;
                let required = if module < MODULES - 1 { module + 1 } else { 1 };
                // Native source dependencies must also be admitted home units
                // in the canonical fixture; distinct units expose this premise.
                let requirements = BTreeMap::from([(
                    (owner(required).unit, owner(required).module),
                    Sha256::digest(interface_bytes(required)).into(),
                )]);
                crate::certified_products::fixture_finalized_product_with_requirements(
                    product,
                    producer,
                    Some(requirements),
                )
            })
            .collect::<Vec<_>>();
        let interfaces = finalized[..MODULES]
            .iter()
            .map(|p| {
                Arc::new(ArtifactEntry::canonical(
                    p.module_interface().unwrap().clone(),
                ))
            })
            .collect::<Vec<_>>();
        for module in 0..MODULES {
            assert_eq!(
                finalized[module].module_interface(),
                finalized[MODULES + module].module_interface(),
                "variants share canonical type identity"
            );
            assert_ne!(
                finalized[module].owner(),
                finalized[MODULES + module].owner(),
                "variants retain different original bodies"
            );
        }
        let originals: Vec<_> = recovered_witness_fixtures(&finalized)
            .into_iter()
            .map(|original| Arc::new(ArtifactEntry::original(producer, original.product).unwrap()))
            .collect();
        assert_eq!(owner(0).module, owner(2).module);
        assert_ne!(owner(0).unit, owner(2).unit);
        assert_eq!(
            originals
                .iter()
                .map(|entry| entry.descriptor.id)
                .collect::<BTreeSet<_>>()
                .len(),
            originals.len(),
            "logical unit/version identities require distinct issued originals"
        );
        let capture = Arc::new(ArtifactEntry::canonical(
            crate::certified_products::fixture_module_interface(
                producer,
                "fixture",
                "ProjectionCapture",
                BTreeMap::new(),
            ),
        ));
        Self {
            originals,
            interfaces,
            capture,
            edges,
            generations,
        }
    }

    fn capture_identity(module: usize, version: usize) -> SymbolIdentity {
        SymbolIdentity {
            unit: "fixture".into(),
            module: "ProjectionCapture".into(),
            namespace: "value".into(),
            occurrence: format!("capture_{module}_{version}"),
            record_parent: None,
        }
    }

    fn original(&self, module: usize, version: usize) -> &Arc<ArtifactEntry> {
        &self.originals[version * MODULES + module]
    }

    fn key(&self, group: RawGroup) -> NativeGroupKey {
        NativeGroupKey {
            artifact: self.original(group.0, group.1).descriptor.id,
            original_ordinal: group.2,
        }
    }

    fn closure(&self, roots: impl IntoIterator<Item = RawGroup>) -> BTreeSet<RawGroup> {
        let mut selected = roots.into_iter().collect::<BTreeSet<_>>();
        loop {
            let before = selected.clone();
            for group in &before {
                selected.extend(self.edges.get(group).into_iter().flatten().copied());
            }
            if before == selected {
                return selected;
            }
        }
    }

    fn full_view(&self) -> ArtifactView {
        let inventory = ArtifactInventory::default();
        let mut entries = self.originals.clone();
        entries.push(Arc::clone(&self.capture));
        inventory
            .admit_shared(&inventory.empty_view(), entries)
            .unwrap()
    }

    fn projection(&self, view: &ArtifactView, roles: Roles) -> CompilerInputProjection {
        let entries = roles
            .iter()
            .enumerate()
            .filter_map(|(module, version)| {
                version.map(|version| Arc::clone(self.original(module, version)))
            })
            .collect::<Vec<_>>();
        CompilerInputProjection::from_issued_entries(&entries)
            .unwrap()
            .merge(&CompilerInputProjection::from_interface_view(view).unwrap())
            .unwrap()
    }

    fn expected_ids(&self, roles: Roles) -> BTreeSet<ArtifactId> {
        (0..MODULES)
            .map(|module| {
                roles[module].map_or(self.interfaces[module].descriptor.id, |version| {
                    self.original(module, version).descriptor.id
                })
            })
            .chain(std::iter::once(self.capture.descriptor.id))
            .collect()
    }

    fn assert_projection(
        &self,
        view: &ArtifactView,
        projection: &CompilerInputProjection,
        roles: Roles,
    ) {
        let before = view.descriptors();
        let metadata = projection
            .project_metadata(view.metadata_snapshot())
            .unwrap();
        assert_eq!(
            metadata
                .entries
                .values()
                .map(|entry| entry.descriptor.id)
                .collect::<BTreeSet<_>>(),
            self.expected_ids(roles)
        );
        assert_eq!(
            metadata.artifacts.keys().copied().collect::<BTreeSet<_>>(),
            before.iter().map(|entry| entry.id).collect()
        );
        assert_eq!(
            metadata.selected_native_groups,
            view.selected_native_groups(),
            "compiler roles cannot change group custody"
        );
        assert!(metadata.ambiguous_native_owners.is_empty());
        assert_eq!(
            view.descriptors(),
            before,
            "projecting does not mutate custody"
        );
    }

    fn assert_target(&self, view: &ArtifactView, roots: &[RawGroup]) {
        let input = roots
            .iter()
            .map(|group| NativeRequirementRoot::Group {
                artifact: self.key(*group).artifact,
                original_ordinal: group.2,
            })
            .collect::<Vec<_>>();
        let imports = roots
            .iter()
            .map(|group| {
                let ArtifactPayload::Original(product) = &self.original(group.0, group.1).payload
                else {
                    unreachable!("fixture original")
                };
                PendingImportOwner::Source {
                    owner: product.owner().clone(),
                    original_ordinal: group.2,
                    binder: binder(group.0, group.2),
                }
            })
            .collect::<Vec<_>>();
        let inventory = ArtifactInventory::default();
        let available = inventory
            .admit_shared_with_demand(
                &inventory.empty_view(),
                view.entries(),
                NativeArtifactDemand::ScopeInterfaces,
            )
            .unwrap();
        let target = inventory
            .admit_shared_with_demand(
                &available,
                view.entries(),
                NativeArtifactDemand::CertifiedTargetImports(&imports),
            )
            .unwrap();
        assert_eq!(
            target.selected_native_groups(),
            self.closure(roots.iter().copied())
                .into_iter()
                .map(|group| self.key(group))
                .collect()
        );
        let demands = view
            .native_binding_requirements_from_roots(&input)
            .unwrap()
            .into_iter()
            .map(|requirement| (requirement.identity, requirement.generation))
            .collect::<BTreeMap<_, _>>();
        let expected = self
            .closure(roots.iter().copied())
            .iter()
            .filter(|group| group.2 == 11)
            .map(|group| {
                (
                    Self::capture_identity(group.0, group.1),
                    self.generations[group.1 * MODULES + group.0],
                )
            })
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            demands, expected,
            "captured generations retain exact source islands"
        );
    }

    fn context(&self, view: &ArtifactView, roles: Roles) -> ExactDeclarationContext {
        ExactDeclarationContext::from_authenticated_execution(
            Sha256::digest(PRODUCER).into(),
            view,
            (0..MODULES)
                .map(|module| ExactLexicalNode {
                    owner: owner(module),
                    imports: (module < MODULES - 1)
                        .then(|| owner(module + 1))
                        .into_iter()
                        .collect(),
                })
                .collect(),
            owner(0),
            &(0..MODULES).map(owner).collect::<Vec<_>>(),
        )
        .unwrap()
        .with_compiler_input_projection(self.projection(view, roles))
        .unwrap()
    }
}

#[test]
fn exact_original_variants_are_custody_not_compiler_namespace_conflicts() {
    let catalog = Catalog::new(false, [17, 23, 31, 41, 47, 59]);
    let view = catalog.full_view();
    let a = catalog.projection(&view, [Some(0); MODULES]);
    let b = catalog.projection(&view, [Some(1); MODULES]);
    catalog.assert_projection(&view, &a, [Some(0); MODULES]);
    catalog.assert_projection(&view, &b, [Some(1); MODULES]);
    assert!(
        a.merge(&b).is_err(),
        "one compiler namespace cannot select both implementations"
    );
    assert!(CompilerInputProjection::from_issued_entries(&catalog.originals).is_err());
    assert_eq!(a.merge(&a).unwrap(), a);
    catalog.assert_projection(&view, &a.interface_only(), [None; MODULES]);
    catalog.assert_target(&view, &[(0, 1, 11)]);
    let inventory = ArtifactInventory::default();
    let empty = inventory
        .admit_shared_with_demand(
            &inventory.empty_view(),
            view.entries(),
            NativeArtifactDemand::ScopeInterfaces,
        )
        .unwrap();
    assert!(empty.selected_native_groups().is_empty());
    let whole = view
        .select_roots(vec![catalog.original(0, 0).descriptor.id])
        .unwrap();
    assert_eq!(
        whole.selected_native_groups(),
        catalog
            .closure(ORDINALS.map(|ordinal| (0, 0, ordinal)))
            .into_iter()
            .map(|group| catalog.key(group))
            .collect()
    );
}

#[test]
fn compiler_projection_dependency_refusals_preserve_exact_owners() {
    let catalog = Catalog::new(false, [1, 2, 3, 4, 5, 6]);
    let view = catalog.full_view();
    let descriptors = view.descriptors();
    let complete = catalog.projection(&view, [None; MODULES]);
    complete
        .entries_from_metadata(&view.metadata_snapshot())
        .unwrap();
    let incomplete =
        CompilerInputProjection::from_issued_entries(std::slice::from_ref(&catalog.interfaces[0]))
            .unwrap();
    let missing = incomplete
        .entries_from_metadata(&view.metadata_snapshot())
        .unwrap_err();
    let mut changed_seal = view.metadata_snapshot();
    let required = Arc::make_mut(
        changed_seal
            .artifacts
            .get_mut(&catalog.interfaces[1].descriptor.id)
            .unwrap(),
    );
    required.descriptor.interface_sha256 = [99; 32];
    let mismatch = complete.entries_from_metadata(&changed_seal).unwrap_err();
    for (error, expected) in [
        (
            missing,
            ArtifactInventoryFailure::MissingDependency {
                artifact: catalog.interfaces[0].descriptor.id,
                dependent: owner(0),
                required: owner(1),
                dependency: ArtifactDependency::Interface,
            },
        ),
        (
            mismatch,
            ArtifactInventoryFailure::InterfaceSealMismatch {
                dependent: owner(0),
                required: owner(1),
            },
        ),
    ] {
        let CompileError::ArtifactInventory(error) = error else {
            panic!("projection refusals must retain exact structured dependency facts");
        };
        assert_eq!(error.failure, expected);
        let encoded = serde_json::to_vec(&error.failure).unwrap();
        let decoded: ArtifactInventoryFailure = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, expected);
    }
    assert_eq!(
        view.descriptors(),
        descriptors,
        "refusal leaves retained custody unchanged"
    );
}

#[test]
fn recovered_roles_require_exact_authenticated_original_and_type_closure() {
    let catalog = Catalog::new(false, [1, 2, 3, 4, 5, 6]);
    let view = catalog.full_view();
    let selected = catalog.projection(&view, [Some(0); MODULES]);
    assert_eq!(
        CompilerInputProjection::restore(&view, &selected.roles()).unwrap(),
        selected
    );
    for role in selected.roles() {
        for stale_id in [Some(role.interface()), role.original()]
            .into_iter()
            .flatten()
        {
            let mut stale_snapshot = view.metadata_snapshot();
            stale_snapshot.artifacts.remove(&stale_id);
            assert!(
                selected.entries_from_metadata(&stale_snapshot).is_err(),
                "stale role IDs refuse before dependent metadata indexing"
            );
        }
    }
    let mut repeated = selected.roles();
    repeated.push(repeated[0].clone());
    assert!(CompilerInputProjection::restore(&view, &repeated).is_err());
    let wrong_interface = CompilerInputRole::ReusableOriginal {
        interface: catalog.interfaces[1].descriptor.id,
        original: catalog.original(0, 0).descriptor.id,
    };
    assert!(CompilerInputProjection::restore(&view, &[wrong_interface]).is_err());
    let canonical_as_native = CompilerInputRole::ReusableOriginal {
        interface: catalog.interfaces[2].descriptor.id,
        original: catalog.interfaces[2].descriptor.id,
    };
    assert!(CompilerInputProjection::restore(&view, &[canonical_as_native]).is_err());
    let missing_type_dependency = CompilerInputRole::InterfaceOnly {
        interface: catalog.interfaces[0].descriptor.id,
    };
    assert!(CompilerInputProjection::restore(&view, &[missing_type_dependency]).is_err());
    let type_view = view
        .interface_projection(
            &(0..MODULES)
                .map(owner)
                .chain(std::iter::once(catalog.capture.descriptor.owner.clone()))
                .collect::<Vec<_>>(),
        )
        .unwrap();
    assert!(
        CompilerInputProjection::restore(&type_view, &selected.roles()).is_err(),
        "canonical custody cannot supply missing body proof"
    );
    let narrowed = selected.within_view(&type_view);
    catalog.assert_projection(&type_view, &narrowed, [None; MODULES]);
    assert!(type_view
        .native_binding_requirements_from_roots(&[NativeRequirementRoot::Group {
            artifact: catalog.original(0, 0).descriptor.id,
            original_ordinal: 3
        }])
        .is_err());
    assert!(view
        .native_binding_requirements_from_roots(&[NativeRequirementRoot::Group {
            artifact: catalog.original(0, 0).descriptor.id,
            original_ordinal: 99
        }])
        .is_err());
}

#[test]
fn exact_group_availability_survives_projection_clone_and_cross_inventory_merge() {
    let catalog = Catalog::new(false, [5, 7, 11, 13, 17, 19]);
    let view = catalog.full_view();
    let mut islands = Vec::new();
    for version in 0..VERSIONS {
        let island = view
            .select_roots(vec![catalog.original(0, version).descriptor.id])
            .unwrap();
        let expected = catalog
            .closure(ORDINALS.map(|ordinal| (0, version, ordinal)))
            .into_iter()
            .map(|group| catalog.key(group))
            .collect::<BTreeSet<_>>();
        assert_eq!(island.selected_native_groups(), expected);
        catalog.assert_projection(
            &island,
            &catalog.projection(&island, [Some(version); MODULES]),
            [Some(version); MODULES],
        );
        assert!(catalog
            .projection(&view, [Some(1 - version); MODULES])
            .validate(&island)
            .is_err());
        assert!(island
            .native_binding_requirements_from_roots(&[NativeRequirementRoot::Group {
                artifact: catalog.original(0, 1 - version).descriptor.id,
                original_ordinal: 29
            }])
            .is_err());
        let separate = ArtifactInventory::default();
        islands.push(
            separate
                .admit_recovery_selection(&separate.empty_view(), island.entries(), &expected)
                .unwrap(),
        );
    }
    let expected = islands[0]
        .selected_native_groups()
        .union(&islands[1].selected_native_groups())
        .copied()
        .collect::<BTreeSet<_>>();
    let old = islands[0].clone();
    let merged = islands[0].merge(&islands[1]).unwrap();
    assert_eq!(merged.selected_native_groups(), expected);
    assert_ne!(
        old.selected_native_groups(),
        expected,
        "cross-inventory merge cannot widen the prior view"
    );
    for version in 0..VERSIONS {
        catalog.assert_target(&merged, &[(0, version, 11)]);
    }
    catalog.assert_projection(
        &merged,
        &catalog.projection(&merged, [Some(0); MODULES]),
        [Some(0); MODULES],
    );
    catalog.assert_projection(
        &merged,
        &catalog.projection(&merged, [Some(1); MODULES]),
        [Some(1); MODULES],
    );
}

#[test]
fn selected_source_islands_require_the_exact_helper_variant() {
    let catalog = Catalog::new(true, [61, 67, 71, 73, 79, 83]);
    let available = catalog
        .originals
        .iter()
        .map(|entry| match &entry.payload {
            ArtifactPayload::Original(product) => (entry.descriptor.id, product),
            _ => unreachable!(),
        })
        .collect::<BTreeMap<_, _>>();
    let selected = catalog
        .closure([(0, 0, 11)])
        .into_iter()
        .map(|group| catalog.key(group))
        .collect::<BTreeSet<_>>();
    let certify =
        |available: &BTreeMap<ArtifactId, &crate::recovery_artifacts::CertifiedRecoveryProduct>,
         selected: &BTreeSet<NativeGroupKey>| {
            crate::certified_products::certify_selected_owned_products_in_context_with_validation(
                available,
                &[],
                selected,
                &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
            )
        };
    assert_eq!(
        certify(&available, &selected).unwrap().len(),
        selected.len(),
        "one selected source island uses its exact helper variant"
    );
    let both_islands = catalog
        .closure([(0, 0, 11), (0, 1, 11)])
        .into_iter()
        .map(|group| catalog.key(group))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        certify(&available, &both_islands).unwrap().len(),
        both_islands.len(),
        "both same-named helper variants may be retained across exact domains"
    );
    let mut wrong_selection = selected.clone();
    wrong_selection.remove(&catalog.key((1, 1, 3)));
    wrong_selection.extend(
        catalog
            .closure([(1, 0, 3)])
            .into_iter()
            .map(|group| catalog.key(group)),
    );
    assert!(
        certify(&available, &wrong_selection).is_err(),
        "a same module/ordinal/binder with another original version cannot satisfy the source edge"
    );
    let mut missing_helper = available.clone();
    missing_helper.remove(&catalog.original(1, 1).descriptor.id);
    assert!(certify(&missing_helper, &selected).is_err());
    assert_ne!(
        catalog.expected_ids([Some(0); MODULES]),
        catalog.expected_ids([Some(1); MODULES]),
        "reference role observations distinguish availability from the active implementation"
    );
    assert_ne!(
        catalog.closure([(0, 0, 11)]),
        catalog.closure([(0, 0, 29)]),
        "reference graph observations distinguish an actual dependency from zero-edge groups"
    );
}

#[test]
fn all_groups_distinguishes_native_zero_groups_from_non_native_custody() {
    let producer = Sha256::digest(PRODUCER).into();
    let zero = crate::certified_products::fixture_finalized_product(
        original_groups_fixture("ProjectionZero", Vec::new(), 1, &BTreeMap::new()),
        producer,
    );
    let canonical = Arc::new(ArtifactEntry::canonical(
        zero.module_interface().unwrap().clone(),
    ));
    let original = Arc::new(ArtifactEntry::original(producer, zero).unwrap());
    let inventory = ArtifactInventory::default();
    let view = inventory
        .admit_shared(&inventory.empty_view(), vec![Arc::clone(&original)])
        .unwrap();
    assert!(
        view.native_binding_requirements_from_roots(&[NativeRequirementRoot::AllGroups(
            original.descriptor.id
        )])
        .unwrap()
        .is_empty(),
        "genuine zero-group native proof is a known empty set"
    );
    assert!(
        view.native_binding_requirements_from_roots(&[NativeRequirementRoot::AllGroups(
            canonical.descriptor.id
        )])
        .is_err(),
        "type custody is not native proof"
    );
    let foreign = ArtifactEntry::canonical(crate::certified_products::fixture_module_interface(
        producer,
        "fixture",
        "OutsideProjection",
        BTreeMap::new(),
    ));
    assert!(view
        .native_binding_requirements_from_roots(&[NativeRequirementRoot::AllGroups(
            foreign.descriptor.id
        )])
        .is_err());
}

#[test]
fn same_view_a_b_a_materialization_reuses_only_the_exact_projection() {
    let catalog = Catalog::new(true, [101, 103, 107, 109, 113, 127]);
    let view = catalog.full_view();
    let a = Arc::new(catalog.context(&view, [Some(0); MODULES]));
    let b = Arc::new(
        a.as_ref()
            .clone()
            .with_compiler_input_projection(
                catalog.projection(a.artifact_view(), [Some(1); MODULES]),
            )
            .unwrap(),
    );
    let scratch = tempfile::tempdir().unwrap();
    let first = a
        .prepare_compilation(&scratch.path().join("a1"), PRODUCER)
        .unwrap();
    let metadata_a = a.compiler_metadata_snapshot().unwrap();
    let retained_a = a
        .artifact_view()
        .retained_materialization(&metadata_a)
        .unwrap()
        .unwrap();
    let second = b
        .prepare_compilation(&scratch.path().join("b"), PRODUCER)
        .unwrap();
    let metadata_b = b.compiler_metadata_snapshot().unwrap();
    let retained_b = b
        .artifact_view()
        .retained_materialization(&metadata_b)
        .unwrap()
        .unwrap();
    assert!(!Arc::ptr_eq(&retained_a, &retained_b));
    assert_ne!(
        metadata_a.materialization_key(),
        metadata_b.materialization_key()
    );
    assert_ne!(first.semantic_sha256, second.semantic_sha256);
    let snapshots = |request: &crate::declaration_context::ExactCompilationRequest| {
        request
            .artifacts
            .iter()
            .filter_map(|artifact| {
                artifact.product.as_ref().map(|product| {
                    (
                        (
                            artifact.interface.unit.clone(),
                            artifact.interface.module.clone(),
                        ),
                        std::fs::read(&product.path).unwrap(),
                    )
                })
            })
            .collect::<BTreeMap<_, _>>()
    };
    for module in 0..MODULES {
        let bytes = |version| match &catalog.original(module, version).payload {
            ArtifactPayload::Original(product) => product.product_bytes().to_vec(),
            _ => unreachable!(),
        };
        assert_eq!(
            snapshots(&first)[&(owner(module).unit, owner(module).module)],
            bytes(0)
        );
        assert_eq!(
            snapshots(&second)[&(owner(module).unit, owner(module).module)],
            bytes(1)
        );
    }
    let again = a
        .prepare_compilation(&scratch.path().join("a2"), PRODUCER)
        .unwrap();
    assert_eq!(first.artifacts, again.artifacts);
    assert_eq!(first.semantic_sha256, again.semantic_sha256);
    assert!(Arc::ptr_eq(
        &retained_a,
        &a.artifact_view()
            .retained_materialization(&metadata_a)
            .unwrap()
            .unwrap()
    ));
    assert_eq!(
        first.groups.len(),
        MODULES * VERSIONS * ORDINALS.len(),
        "the compiler projection does not discard other exact source islands"
    );
    assert_eq!(second.groups.len(), first.groups.len());
    catalog.assert_target(a.artifact_view(), &[(0, 1, 11)]);
    catalog.assert_target(b.artifact_view(), &[(0, 0, 3)]);
    let owned_paths = first
        .artifacts
        .iter()
        .chain(&second.artifacts)
        .map(|artifact| artifact.interface.path.clone())
        .collect::<Vec<_>>();
    let weak_a = Arc::downgrade(&retained_a);
    let weak_b = Arc::downgrade(&retained_b);
    drop(retained_a);
    drop(retained_b);
    drop(first);
    drop(second);
    drop(again);
    drop(a);
    drop(b);
    drop(view);
    assert!(weak_a.upgrade().is_none());
    assert!(weak_b.upgrade().is_none());
    assert!(
        owned_paths.iter().all(|path| !path.exists()),
        "last owner release removes both keyed private materializations"
    );
}

#[derive(Clone, Debug)]
enum Op {
    Offer {
        domain: usize,
        module: usize,
        version: usize,
    },
    Demote {
        domain: usize,
    },
    Merge {
        target: usize,
        source: usize,
    },
    SourceSurface {
        domain: usize,
        owners: u8,
    },
    Clone {
        target: usize,
        source: usize,
    },
    Restore {
        domain: usize,
    },
    Target {
        domain: usize,
        module: usize,
        version: usize,
        ordinal: u32,
    },
    IssueSegment {
        domain: usize,
    },
    PublishPrefix {
        domain: usize,
    },
    Demand {
        domain: usize,
        module: usize,
        version: usize,
        ordinal: u32,
        whole: bool,
    },
    RecoverDemand {
        domain: usize,
        mutation: u8,
    },
    ReleaseSegment {
        domain: usize,
    },
    DeclarationOriginal {
        domain: usize,
        module: usize,
        version: usize,
    },
}

fn operation() -> impl Strategy<Value = Op> {
    prop_oneof![
        (0usize..4, 0usize..MODULES, 0usize..VERSIONS).prop_map(|(domain, module, version)| {
            Op::Offer {
                domain,
                module,
                version,
            }
        }),
        (0usize..4).prop_map(|domain| Op::Demote { domain }),
        (0usize..4, 0usize..4).prop_map(|(target, source)| Op::Merge { target, source }),
        (0usize..4, 0u8..8).prop_map(|(domain, owners)| Op::SourceSurface { domain, owners }),
        (0usize..4, 0usize..4).prop_map(|(target, source)| Op::Clone { target, source }),
        (0usize..4).prop_map(|domain| Op::Restore { domain }),
        (0usize..4).prop_map(|domain| Op::IssueSegment { domain }),
        (0usize..4).prop_map(|domain| Op::PublishPrefix { domain }),
        (
            0usize..4,
            0usize..MODULES,
            0usize..VERSIONS,
            prop::sample::select(ORDINALS.to_vec()),
            any::<bool>()
        )
            .prop_map(|(domain, module, version, ordinal, whole)| Op::Demand {
                domain,
                module,
                version,
                ordinal,
                whole,
            }),
        (0usize..4, 0u8..3).prop_map(|(domain, mutation)| Op::RecoverDemand { domain, mutation }),
        (0usize..4).prop_map(|domain| Op::ReleaseSegment { domain }),
        (0usize..4, 0usize..MODULES, 0usize..VERSIONS).prop_map(|(domain, module, version)| {
            Op::DeclarationOriginal {
                domain,
                module,
                version,
            }
        }),
        (
            0usize..4,
            0usize..MODULES,
            0usize..VERSIONS,
            prop::sample::select(ORDINALS.to_vec())
        )
            .prop_map(|(domain, module, version, ordinal)| Op::Target {
                domain,
                module,
                version,
                ordinal
            }),
    ]
}

fn replay(catalog: &Catalog, operations: &[Op]) -> ([usize; 13], [usize; 13], [usize; 13]) {
    let view = catalog.full_view();
    let baseline = view.descriptors();
    let mut actual = std::array::from_fn::<_, 4, _>(|_| catalog.projection(&view, [None; MODULES]));
    let mut model = [[None; MODULES]; 4];
    let mut segments: [Option<ArtifactView>; 4] = std::array::from_fn(|_| None);
    let mut published = [false; 4];
    let mut selected: [BTreeSet<RawGroup>; 4] = std::array::from_fn(|_| BTreeSet::new());
    #[derive(Clone, Copy)]
    enum Outcome {
        Success,
        Refusal,
        Absent,
    }
    let mut attempted = [0usize; 13];
    let mut successes = [0usize; 13];
    let mut refusals = [0usize; 13];
    let mut absent = [0usize; 13];
    for operation in operations {
        let kind = match operation {
            Op::Offer { .. } => 0,
            Op::Demote { .. } => 1,
            Op::Merge { .. } => 2,
            Op::SourceSurface { .. } => 3,
            Op::Clone { .. } => 4,
            Op::Restore { .. } => 5,
            Op::Target { .. } => 6,
            Op::IssueSegment { .. } => 7,
            Op::PublishPrefix { .. } => 8,
            Op::Demand { .. } => 9,
            Op::RecoverDemand { .. } => 10,
            Op::ReleaseSegment { .. } => 11,
            Op::DeclarationOriginal { .. } => 12,
        };
        attempted[kind] += 1;
        let mut outcome = Outcome::Success;
        match *operation {
            Op::Offer {
                domain,
                module,
                version,
            } => {
                let incoming = CompilerInputProjection::from_issued_entries(&[Arc::clone(
                    catalog.original(module, version),
                )])
                .unwrap();
                let result = actual[domain].merge(&incoming);
                if model[domain][module].is_some_and(|old| old != version) {
                    assert!(result.is_err());
                    outcome = Outcome::Refusal;
                } else {
                    actual[domain] = result.unwrap();
                    model[domain][module] = Some(version);
                }
            }
            Op::Demote { domain } => {
                actual[domain] = actual[domain].interface_only();
                model[domain] = [None; MODULES];
            }
            Op::Merge { target, source } => {
                let conflict = (0..MODULES).any(|module| matches!((model[target][module], model[source][module]), (Some(a), Some(b)) if a != b));
                let result = actual[target].merge(&actual[source]);
                if conflict {
                    assert!(result.is_err());
                    outcome = Outcome::Refusal;
                } else {
                    actual[target] = result.unwrap();
                    for module in 0..MODULES {
                        model[target][module] = model[target][module].or(model[source][module]);
                    }
                }
            }
            Op::SourceSurface { domain, owners } => {
                let selected = (0..MODULES)
                    .filter(|module| owners & (1 << module) != 0)
                    .map(owner)
                    .collect();
                actual[domain] = actual[domain].for_source_owners(&selected);
                for module in 0..MODULES {
                    if owners & (1 << module) == 0 {
                        model[domain][module] = None;
                    }
                }
            }
            Op::Clone { target, source } => {
                actual[target] = actual[source].clone();
                model[target] = model[source];
            }
            Op::Restore { domain } => {
                actual[domain] =
                    CompilerInputProjection::restore(&view, &actual[domain].roles()).unwrap();
            }
            Op::Target {
                domain,
                module,
                version,
                ordinal,
            } => {
                catalog.assert_target(&view, &[(module, version, ordinal)]);
                catalog.assert_projection(&view, &actual[domain], model[domain]);
            }
            Op::IssueSegment { domain } => {
                let inventory = ArtifactInventory::default();
                segments[domain] = Some(
                    inventory
                        .admit_shared_with_demand(
                            &inventory.empty_view(),
                            catalog.originals.clone(),
                            NativeArtifactDemand::ScopeInterfaces,
                        )
                        .unwrap(),
                );
                published[domain] = false;
                selected[domain].clear();
            }
            Op::PublishPrefix { domain } => {
                if let Some(previous) = &segments[domain] {
                    let next = previous
                        .inventory()
                        .admit_shared_with_demand(
                            previous,
                            vec![Arc::clone(&catalog.capture)],
                            NativeArtifactDemand::ScopeInterfaces,
                        )
                        .unwrap();
                    assert_eq!(
                        next.selected_native_groups(),
                        previous.selected_native_groups()
                    );
                    segments[domain] = Some(next);
                    published[domain] = true;
                } else {
                    outcome = Outcome::Absent;
                }
            }
            Op::Demand {
                domain,
                module,
                version,
                ordinal,
                whole,
            } => {
                if let Some(available) = &segments[domain] {
                    let roots = if whole {
                        ORDINALS
                            .into_iter()
                            .map(|ordinal| (module, version, ordinal))
                            .collect::<Vec<_>>()
                    } else {
                        vec![(module, version, ordinal)]
                    };
                    let wanted = catalog.closure(roots);
                    let requires_capture = wanted.iter().any(|group| group.2 == 11);
                    let expected_refusal = requires_capture && !published[domain];
                    let result = if whole {
                        available.inventory().admit_shared(
                            available,
                            vec![Arc::clone(catalog.original(module, version))],
                        )
                    } else {
                        let ArtifactPayload::Original(product) =
                            &catalog.original(module, version).payload
                        else {
                            unreachable!()
                        };
                        let imports = [PendingImportOwner::Source {
                            owner: product.owner().clone(),
                            original_ordinal: ordinal,
                            binder: binder(module, ordinal),
                        }];
                        available.inventory().admit_shared_with_demand(
                            available,
                            available.entries(),
                            NativeArtifactDemand::CertifiedTargetImports(&imports),
                        )
                    };
                    if expected_refusal {
                        assert!(
                            result.is_err(),
                            "withheld prefix capture must refuse only demanded groups"
                        );
                        outcome = Outcome::Refusal;
                    } else {
                        let next = result.unwrap();
                        selected[domain].extend(wanted);
                        assert_eq!(
                            next.selected_native_groups(),
                            selected[domain]
                                .iter()
                                .map(|group| catalog.key(*group))
                                .collect(),
                            "target admission must match both missing and extra oracle groups"
                        );
                        segments[domain] = Some(next);
                    }
                } else {
                    outcome = Outcome::Absent;
                }
            }
            Op::RecoverDemand { domain, mutation } => {
                if let Some(previous) = &segments[domain] {
                    let mut offered = selected[domain].clone();
                    if mutation == 1 {
                        if let Some(group) = offered.iter().next().copied() {
                            offered.remove(&group);
                        }
                    } else if mutation == 2 {
                        offered.insert((0, 0, 29));
                    }
                    let closed = catalog.closure(offered.iter().copied());
                    let keys = offered.iter().map(|group| catalog.key(*group)).collect();
                    let inventory = ArtifactInventory::default();
                    let result = inventory.admit_recovery_selection(
                        &inventory.empty_view(),
                        previous.entries(),
                        &keys,
                    );
                    if closed != offered
                        || (!published[domain] && closed.iter().any(|group| group.2 == 11))
                    {
                        assert!(
                            result.is_err(),
                            "recovery must refuse incomplete exact closure"
                        );
                        outcome = Outcome::Refusal;
                    } else {
                        let recovered = result.unwrap();
                        assert_eq!(recovered.selected_native_groups(), keys);
                        assert_eq!(
                            previous.selected_native_groups(),
                            selected[domain]
                                .iter()
                                .map(|group| catalog.key(*group))
                                .collect(),
                            "recovery cannot mutate the live source view"
                        );
                        selected[domain] = offered;
                        segments[domain] = Some(recovered);
                    }
                } else {
                    outcome = Outcome::Absent;
                }
            }
            Op::DeclarationOriginal {
                domain,
                module,
                version,
            } => {
                if let Some(available) = &segments[domain] {
                    let projection = CompilerInputProjection::from_issued_entries(&[Arc::clone(
                        catalog.original(module, version),
                    )])
                    .unwrap()
                    .merge(&CompilerInputProjection::from_interface_view(available).unwrap())
                    .unwrap();
                    let selection = crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                        &projection, &available.metadata_snapshot(),
                        &tidepool_repr::execution_schema::InventoryOperation::new(Default::default()),
                    ).unwrap();
                    let result = selection.selected_original_closure(available);
                    if published[domain] {
                        let original = result.unwrap();
                        assert_eq!(original.products().len(), 1);
                        let ArtifactPayload::Original(expected) =
                            &catalog.original(module, version).payload
                        else {
                            unreachable!()
                        };
                        assert_eq!(original.products()[0].owner(), expected.owner(),
                            "whole-original consumers preserve issued roles rather than custody variants");
                    } else {
                        assert!(
                            result.is_err(),
                            "whole-original consumers require unavailable captured groups"
                        );
                        outcome = Outcome::Refusal;
                    }
                } else {
                    outcome = Outcome::Absent;
                }
            }
            Op::ReleaseSegment { domain } => {
                if let Some(previous) = segments[domain].take() {
                    let inventory = previous.inventory().clone();
                    drop(previous);
                    assert_eq!(
                        inventory.node_count(),
                        0,
                        "last segment owner must release all retained vertices"
                    );
                } else {
                    outcome = Outcome::Absent;
                }
                selected[domain].clear();
                published[domain] = false;
            }
        }
        match outcome {
            Outcome::Success => successes[kind] += 1,
            Outcome::Refusal => refusals[kind] += 1,
            Outcome::Absent => absent[kind] += 1,
        }
        for domain in 0..4 {
            catalog.assert_projection(&view, &actual[domain], model[domain]);
        }
        assert_eq!(view.descriptors(), baseline);
    }
    assert!(
        (0..13).all(|kind| successes[kind] + refusals[kind] > 0),
        "every operation must reach its production boundary"
    );
    assert!(
        refusals.iter().sum::<usize>() > 0,
        "histories must reach a real namespace refusal"
    );
    assert!(
        successes[11] > 0,
        "release must drop an actual segment owner"
    );
    for kind in [8, 9, 10, 11, 12] {
        assert!(
            absent[kind] > 0,
            "the guided absent prerequisite cohort is reached"
        );
    }
    for kind in 0..13 {
        assert_eq!(
            attempted[kind],
            successes[kind] + refusals[kind] + absent[kind]
        );
    }
    (successes, refusals, absent)
}

#[test]
fn compiler_domains_and_target_roots_match_independent_variant_histories() {
    let mut config = Config::default();
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = 64;
    }
    if std::env::var_os("PROPTEST_MAX_SHRINK_ITERS").is_none() {
        config.max_shrink_iters = 4096;
    }
    if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
        config.failure_persistence = Some(Box::new(FileFailurePersistence::Direct(path)));
    }
    let mut config = contextualize_config(config);
    config.source_file = Some(file!());
    config.test_name = Some(concat!(
        module_path!(),
        "::compiler_domains_and_target_roots_match_independent_variant_histories"
    ));
    let configured_cases = config.cases;
    let callbacks = RefCell::new(0usize);
    let completed = RefCell::new(0usize);
    let initial_failure = RefCell::new(None);
    let observed = RefCell::new([[0usize; 13]; 3]);
    let strategy = (
        any::<bool>(),
        prop::array::uniform6(1u64..1000),
        prop::collection::vec(operation(), 12..48),
    );
    let result = TestRunner::new(config).run(&strategy, |(cross_version, generations, tail)| {
        *callbacks.borrow_mut() += 1;
        let input = (cross_version, generations, tail.clone());
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let catalog = Catalog::new(cross_version, generations);
            let mut operations = vec![
                Op::PublishPrefix { domain: 0 },
                Op::Demand {
                    domain: 0,
                    module: 0,
                    version: 0,
                    ordinal: 3,
                    whole: false,
                },
                Op::RecoverDemand {
                    domain: 0,
                    mutation: 1,
                },
                Op::ReleaseSegment { domain: 0 },
                Op::DeclarationOriginal {
                    domain: 0,
                    module: 0,
                    version: 0,
                },
                Op::Offer {
                    domain: 0,
                    module: 0,
                    version: 0,
                },
                Op::Offer {
                    domain: 0,
                    module: 0,
                    version: 1,
                },
                Op::Offer {
                    domain: 1,
                    module: 0,
                    version: 1,
                },
                Op::Merge {
                    target: 0,
                    source: 1,
                },
                Op::Clone {
                    target: 2,
                    source: 0,
                },
                Op::Restore { domain: 2 },
                Op::Offer {
                    domain: 1,
                    module: 2,
                    version: 0,
                },
                Op::Merge {
                    target: 2,
                    source: 1,
                },
                Op::Target {
                    domain: 2,
                    module: 2,
                    version: 0,
                    ordinal: 3,
                },
                Op::Target {
                    domain: 0,
                    module: 0,
                    version: 1,
                    ordinal: 11,
                },
                Op::SourceSurface {
                    domain: 2,
                    owners: 6,
                },
                Op::Demote { domain: 1 },
                Op::Merge {
                    target: 1,
                    source: 0,
                },
                Op::IssueSegment { domain: 3 },
                Op::DeclarationOriginal {
                    domain: 3,
                    module: 0,
                    version: 0,
                },
                Op::Demand {
                    domain: 3,
                    module: 0,
                    version: 0,
                    ordinal: 3,
                    whole: false,
                },
                Op::Demand {
                    domain: 3,
                    module: 0,
                    version: 0,
                    ordinal: 11,
                    whole: true,
                },
                Op::RecoverDemand {
                    domain: 3,
                    mutation: 1,
                },
                Op::PublishPrefix { domain: 3 },
                Op::DeclarationOriginal {
                    domain: 3,
                    module: 0,
                    version: 1,
                },
                Op::Demand {
                    domain: 3,
                    module: 0,
                    version: 0,
                    ordinal: 11,
                    whole: false,
                },
                Op::RecoverDemand {
                    domain: 3,
                    mutation: 2,
                },
                Op::ReleaseSegment { domain: 3 },
            ];
            operations.extend(tail);
            let outcomes = replay(&catalog, &operations);
            for (totals, values) in observed
                .borrow_mut()
                .iter_mut()
                .zip([outcomes.0, outcomes.1, outcomes.2])
            {
                for (total, value) in totals.iter_mut().zip(values) {
                    *total += value;
                }
            }
            *completed.borrow_mut() += 1;
        }));
        match outcome {
            Ok(()) => Ok(()),
            Err(panic) => {
                if initial_failure.borrow().is_none() {
                    *initial_failure.borrow_mut() = Some(input);
                }
                let message = panic
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                    .unwrap_or_else(|| "projection history panicked".to_owned());
                Err(proptest::test_runner::TestCaseError::fail(message))
            }
        }
    });
    eprintln!("compiler_projection_campaign configured_cases={configured_cases} actual_callbacks={} completed_histories={} observed_success_refusal_absent={:?}", callbacks.borrow(), completed.borrow(), observed.borrow());
    eprintln!(
        "compiler_projection_initial_failure={:?} minimized={:?}",
        initial_failure.borrow(),
        result
    );
    result.unwrap();
}
