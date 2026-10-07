//! Compiler namespace choices and target demand are independent of custody.
//! Expectations come from fixture facts and finite-set recomputation.
use super::*;
use crate::certified_products::{tests::original_groups_fixture, PendingImportOwner};
use crate::declaration_context::ExactDeclarationContext;
use crate::declaration_join::ExactLexicalNode;
use proptest::prelude::*;
use proptest::test_runner::FileFailurePersistence;
use sha2::{Digest, Sha256};
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
        unit: "fixture".into(),
        module: format!("Projection{module}"),
    }
}

fn binder(module: usize, ordinal: u32) -> SymbolIdentity {
    SymbolIdentity {
        unit: "fixture".into(),
        module: owner(module).module,
        namespace: "value".into(),
        occurrence: format!("entry_{ordinal}"),
        record_parent: None,
    }
}

impl Catalog {
    fn new(cross_version: bool, generations: [u64; MODULES * VERSIONS]) -> Self {
        let mut edges = BTreeMap::<RawGroup, BTreeSet<RawGroup>>::new();
        for version in 0..VERSIONS {
            for module in 0..MODULES - 1 {
                edges.insert(
                    (module, version, 3),
                    BTreeSet::from([(module + 1, version, 3)]),
                );
                edges.insert(
                    (module, version, 11),
                    BTreeSet::from([(module + 1, version ^ usize::from(cross_version), 3)]),
                );
            }
            // Both source islands share a helper SCC, while ordinal 29 stays
            // empty. Module and ordinal alone cannot distinguish the islands.
            edges.insert((2, version, 3), BTreeSet::from([(1, version, 3)]));
        }
        let placeholders = (0..VERSIONS)
            .flat_map(|version| (0..MODULES).map(move |module| (module, version)))
            .map(|(module, version)| {
                original_groups_fixture(
                    &owner(module).module,
                    ORDINALS
                        .iter()
                        .map(|ordinal| (*ordinal, Vec::new()))
                        .collect(),
                    version as u8 + 1,
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
                    original_groups_fixture(
                        &owner(module).module,
                        groups,
                        version as u8 + 1,
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
                let requirements = (module < MODULES - 1)
                    .then(|| {
                        (
                            ("fixture".to_owned(), owner(module + 1).module),
                            Sha256::digest([0x42]).into(),
                        )
                    })
                    .into_iter()
                    .collect();
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
        let originals = finalized
            .into_iter()
            .map(|p| Arc::new(ArtifactEntry::original(producer, p).unwrap()))
            .collect();
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
        let target = view.target_native_selection(&input).unwrap();
        assert_eq!(
            *target.groups(),
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
    assert!(view
        .target_native_selection(&[])
        .unwrap()
        .groups()
        .is_empty());
    assert_eq!(
        view.target_native_selection(&[NativeRequirementRoot::AllGroups(
            catalog.original(0, 0).descriptor.id
        )])
        .unwrap()
        .groups(),
        &catalog
            .closure(ORDINALS.map(|ordinal| (0, 0, ordinal)))
            .into_iter()
            .map(|group| catalog.key(group))
            .collect()
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
        .target_native_selection(&[NativeRequirementRoot::Group {
            artifact: catalog.original(0, 0).descriptor.id,
            original_ordinal: 3
        }])
        .is_err());
    assert!(view
        .target_native_selection(&[NativeRequirementRoot::Group {
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
            .target_native_selection(&[NativeRequirementRoot::Group {
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
        view.target_native_selection(&[NativeRequirementRoot::AllGroups(original.descriptor.id)])
            .unwrap()
            .groups()
            .is_empty(),
        "genuine zero-group native proof is a known empty set"
    );
    assert!(
        view.target_native_selection(&[NativeRequirementRoot::AllGroups(canonical.descriptor.id)])
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
        .target_native_selection(&[NativeRequirementRoot::AllGroups(foreign.descriptor.id)])
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
        .unwrap();
    let second = b
        .prepare_compilation(&scratch.path().join("b"), PRODUCER)
        .unwrap();
    let metadata_b = b.compiler_metadata_snapshot().unwrap();
    let retained_b = b
        .artifact_view()
        .retained_materialization(&metadata_b)
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
                        artifact.interface.module.clone(),
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
        assert_eq!(snapshots(&first)[&owner(module).module], bytes(0));
        assert_eq!(snapshots(&second)[&owner(module).module], bytes(1));
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

fn replay(catalog: &Catalog, operations: &[Op]) {
    let view = catalog.full_view();
    let baseline = view.descriptors();
    let mut actual = std::array::from_fn::<_, 4, _>(|_| catalog.projection(&view, [None; MODULES]));
    let mut model = [[None; MODULES]; 4];
    let mut coverage = [0usize; 7];
    let mut refusals = 0;
    for operation in operations {
        match *operation {
            Op::Offer {
                domain,
                module,
                version,
            } => {
                coverage[0] += 1;
                let incoming = CompilerInputProjection::from_issued_entries(&[Arc::clone(
                    catalog.original(module, version),
                )])
                .unwrap();
                let result = actual[domain].merge(&incoming);
                if model[domain][module].is_some_and(|old| old != version) {
                    assert!(result.is_err());
                    refusals += 1;
                } else {
                    actual[domain] = result.unwrap();
                    model[domain][module] = Some(version);
                }
            }
            Op::Demote { domain } => {
                coverage[1] += 1;
                actual[domain] = actual[domain].interface_only();
                model[domain] = [None; MODULES];
            }
            Op::Merge { target, source } => {
                coverage[2] += 1;
                let conflict = (0..MODULES).any(|module| matches!((model[target][module], model[source][module]), (Some(a), Some(b)) if a != b));
                let result = actual[target].merge(&actual[source]);
                if conflict {
                    assert!(result.is_err());
                    refusals += 1;
                } else {
                    actual[target] = result.unwrap();
                    for module in 0..MODULES {
                        model[target][module] = model[target][module].or(model[source][module]);
                    }
                }
            }
            Op::SourceSurface { domain, owners } => {
                coverage[3] += 1;
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
                coverage[4] += 1;
                actual[target] = actual[source].clone();
                model[target] = model[source];
            }
            Op::Restore { domain } => {
                coverage[5] += 1;
                actual[domain] =
                    CompilerInputProjection::restore(&view, &actual[domain].roles()).unwrap();
            }
            Op::Target {
                domain,
                module,
                version,
                ordinal,
            } => {
                coverage[6] += 1;
                catalog.assert_target(&view, &[(module, version, ordinal)]);
                catalog.assert_projection(&view, &actual[domain], model[domain]);
            }
        }
        for domain in 0..4 {
            catalog.assert_projection(&view, &actual[domain], model[domain]);
        }
        assert_eq!(view.descriptors(), baseline);
    }
    assert!(coverage.iter().all(|count| *count > 0));
    assert!(
        refusals > 0,
        "histories must reach a real namespace refusal"
    );
    eprintln!(
        "compiler_projection_history coverage={coverage:?} refusals={refusals} operations={}",
        operations.len()
    );
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, max_shrink_iters: 4096, failure_persistence: option_env!("TIDEPOOL_PROPTEST_REGRESSIONS").map(|path| Box::new(FileFailurePersistence::Direct(path)) as Box<dyn proptest::test_runner::FailurePersistence>), ..ProptestConfig::default() })]
    #[test]
    fn compiler_domains_and_target_roots_match_independent_variant_histories(
        cross_version in any::<bool>(),
        generations in prop::array::uniform6(1u64..1000),
        tail in prop::collection::vec(operation(), 12..48),
    ) {
        let catalog = Catalog::new(cross_version, generations);
        let mut operations = vec![
            Op::Offer { domain: 0, module: 0, version: 0 },
            Op::Offer { domain: 0, module: 0, version: 1 },
            Op::Offer { domain: 1, module: 0, version: 1 },
            Op::Merge { target: 0, source: 1 },
            Op::Clone { target: 2, source: 0 },
            Op::Restore { domain: 2 },
            Op::Target { domain: 0, module: 0, version: 1, ordinal: 11 },
            Op::SourceSurface { domain: 2, owners: 6 },
            Op::Demote { domain: 1 },
            Op::Merge { target: 1, source: 0 },
        ];
        operations.extend(tail);
        replay(&catalog, &operations);
    }
}
