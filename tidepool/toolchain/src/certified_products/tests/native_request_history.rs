use super::*;
use crate::artifact_inventory::{
    ArtifactBinding, ArtifactBindingNode, ArtifactDependency, ArtifactEntry,
    ArtifactGraphSelection, ArtifactInventory, ArtifactInventoryFailure, ArtifactView,
    CanonicalProducerIdentity, CompilerInputProjection, NativeGroupKey,
};
use crate::declaration_context::{
    generated_source_admission_fixture, issue_fixture_compiler_support, ExactCompilationRequest,
    ExactCompileContext, ExactDeclarationContext, OriginalCompilerInputs,
};
use crate::recovery_artifacts::CertifiedRecoveryProduct;
use crate::CompileError;
const PRODUCER: &[u8] = b"protected-native-availability-fixture";

fn product(module: &str, version: u8) -> CertifiedRecoveryProduct {
    let finalized = fixture_finalized_product(
        original_groups_fixture_with_interface(
            module,
            vec![(7, vec![]), (9001, vec![])],
            version,
            &BTreeMap::new(),
            module.as_bytes().to_vec(),
        ),
        CanonicalProducerIdentity::from_producer_bytes(PRODUCER).sha256(),
    );
    recovered_witness_fixtures(&[finalized]).remove(0).product
}

fn dependent_product(
    module: &str,
    version: u8,
    imports: Vec<(u32, Vec<crate::certified_products::PendingImportOwner>)>,
) -> CertifiedRecoveryProduct {
    fixture_finalized_product(
        original_groups_fixture_with_interface(
            module,
            imports,
            version,
            &BTreeMap::new(),
            module.as_bytes().to_vec(),
        ),
        CanonicalProducerIdentity::from_producer_bytes(PRODUCER).sha256(),
    )
}

fn source_import(
    product: &CertifiedRecoveryProduct,
    ordinal: u32,
) -> crate::certified_products::PendingImportOwner {
    let product = crate::certified_products::finalize_original_for_inventory(
        product.clone(),
        &mut PackageInterfaceValidation::default(),
    )
    .expect("fixture native import has authenticated original groups");
    let definitions =
        crate::certified_products::original_native_group_definitions(&product, ordinal)
            .expect("fixture native import names an issued original group");
    let binding = match &definitions.bindings()[0] {
        tidepool_repr::execution_schema::Group::NonRecursive(binding) => binding,
        tidepool_repr::execution_schema::Group::Recursive(bindings) => &bindings[0],
    };
    let binder = binding.identity.clone();
    crate::certified_products::PendingImportOwner::Source {
        owner: product.owner().clone(),
        original_ordinal: ordinal,
        binder,
    }
}

// Observe the production scope writer with literal wire positions, independently
// of request selection and certificate construction.
fn wire_rows(request: &ExactCompilationRequest) -> BTreeMap<String, (Vec<u32>, Value)> {
    let bytes = std::fs::read(&request.manifest).unwrap();
    let value: Value = ciborium::de::from_reader(bytes.as_slice()).unwrap();
    let fields = value.as_array().unwrap();
    assert_eq!(fields.len(), 11);
    assert_eq!(fields[0], Value::Text("TPEXACTSCOPE".into()));
    assert_eq!(fields[1], Value::Text("16".into()));
    let mut rows = BTreeMap::new();
    for row in fields[6].as_array().unwrap() {
        let [_, module, _, _, _, _, ordinals, _, mode, _] = row.as_array().unwrap().as_slice()
        else {
            panic!("native row has ten fields")
        };
        let ordinals = ordinals
            .as_array()
            .unwrap()
            .iter()
            .map(|value| u32::try_from(value.as_integer().unwrap()).unwrap())
            .collect();
        assert!(rows
            .insert(
                module.as_text().unwrap().to_owned(),
                (ordinals, mode.clone())
            )
            .is_none());
    }
    rows
}
fn wire_groups(request: &ExactCompilationRequest) -> BTreeMap<String, Vec<u32>> {
    wire_rows(request)
        .into_iter()
        .map(|(owner, (groups, _))| (owner, groups))
        .collect()
}
fn wire_mode(request: &ExactCompilationRequest, module: &str) -> Value {
    wire_rows(request).remove(module).unwrap().1
}
fn full_mode() -> Value {
    Value::Array(vec![Value::Text("full-carrier".into())])
}
fn issued_mode(ordinals: &[u32]) -> Value {
    Value::Array(vec![
        Value::Text("issued-groups".into()),
        Value::Array(
            ordinals
                .iter()
                .map(|ordinal| Value::Integer((*ordinal).into()))
                .collect(),
        ),
    ])
}

#[derive(Clone, Debug)]
struct NativeTransportHistory {
    same_root: bool,
    overlap: bool,
    shared_carrier: bool,
    reverse: bool,
    duplicate: bool,
    custody: Vec<NativeCustodyOp>,
}

#[derive(Clone, Debug)]
enum NativeCustodyOp {
    Expand,
    Capture { right: bool },
}

#[derive(Debug, Default)]
struct NativeTransportCoverage {
    callbacks: usize,
    completed: usize,
    equal_payload_scopes: usize,
    shared_carriers: usize,
    distinct_carriers: usize,
    full_without_demand: usize,
    partial_with_ambient_full: usize,
    custody_expansions: usize,
    captures: usize,
    duplicate_joins: usize,
    reversed_joins: usize,
    joined_preparations: usize,
    namespace_pin_continuations: usize,
    output_request_preparations: usize,
    dependent_issuances: usize,
    continuations: usize,
    successful_demands: usize,
    ambiguous_demands: usize,
    recoveries: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct LogicalNativeGroup {
    issuance: usize,
    payload: usize,
    ordinal: u32,
}

#[derive(Default)]
struct NativeTransportModel {
    groups: BTreeMap<LogicalNativeGroup, crate::artifact_inventory::NativeGroupBinding>,
    carriers: BTreeMap<LogicalNativeGroup, ArtifactBinding>,
    edges: BTreeMap<LogicalNativeGroup, BTreeSet<LogicalNativeGroup>>,
}

impl NativeTransportModel {
    fn observe_issuance(
        &mut self,
        issuance: usize,
        root_payload: usize,
        dependency_ordinal: u32,
        entries: &[Arc<ArtifactEntry>; 3],
        source: &ArtifactView,
    ) -> LogicalNativeGroup {
        let root = LogicalNativeGroup {
            issuance,
            payload: root_payload,
            ordinal: 7,
        };
        let dependency = LogicalNativeGroup {
            issuance,
            payload: 0,
            ordinal: dependency_ordinal,
        };
        let graph = source.capture_graph_selection();
        assert_eq!(
            graph.native_groups.len(),
            2,
            "fixture issues exactly the authored pair"
        );
        for logical in [root, dependency] {
            let id = entries[logical.payload].descriptor.id;
            let handles = graph
                .native_groups
                .iter()
                .copied()
                .filter(|group| {
                    group.binding.artifact() == id && group.original_ordinal == logical.ordinal
                })
                .collect::<Vec<_>>();
            let [handle] = handles.as_slice() else {
                panic!("one issued identity per logical group")
            };
            assert!(
                !self.groups.values().any(|prior| prior == handle),
                "independent group identities cannot alias"
            );
            let carriers = graph
                .namespace
                .iter()
                .copied()
                .filter(|binding| binding.artifact == id)
                .collect::<Vec<_>>();
            let [carrier] = carriers.as_slice() else {
                panic!("one issued carrier per logical payload")
            };
            self.groups.insert(logical, *handle);
            self.carriers.insert(logical, *carrier);
        }
        // Expected topology comes from the authored imports, not captured edges.
        self.edges.insert(root, BTreeSet::from([dependency]));
        self.edges.insert(dependency, BTreeSet::new());
        let native_edges = source
            .binding_dependencies()
            .into_iter()
            .filter_map(|(from, to, label)| match (from, to, label) {
                (
                    ArtifactBindingNode::Group(from),
                    ArtifactBindingNode::Group(to),
                    ArtifactDependency::NativeGroup { .. },
                ) => Some((from, to)),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(
            native_edges,
            BTreeSet::from([(self.groups[&root], self.groups[&dependency])])
        );
        root
    }

    fn closure(
        &self,
        root: LogicalNativeGroup,
    ) -> BTreeMap<crate::artifact_inventory::NativeGroupBinding, ArtifactBinding> {
        let mut reachable = BTreeSet::from([root]);
        loop {
            let next = reachable
                .iter()
                .flat_map(|group| self.edges[group].iter().copied())
                .chain(reachable.iter().copied())
                .collect::<BTreeSet<_>>();
            if next == reachable {
                break;
            }
            reachable = next;
        }
        reachable
            .into_iter()
            .map(|group| (self.groups[&group], self.carriers[&group]))
            .collect()
    }
}

fn replay_native_transport_history(
    history: &NativeTransportHistory,
    coverage: &mut NativeTransportCoverage,
) {
    use crate::artifact_inventory::{
        IssuedNativeOffer, NativeArtifactDemand, NativeOriginalRequirement,
    };
    let NativeTransportHistory {
        same_root,
        overlap,
        shared_carrier,
        reverse,
        duplicate,
        custody,
    } = history;
    let (same_root, overlap, shared_carrier, reverse, duplicate) =
        (*same_root, *overlap, *shared_carrier, *reverse, *duplicate);
    eprintln!("native transport history: {history:?}");
    assert!(
        !same_root || overlap,
        "one root has one authored dependency"
    );
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let root = tempfile::tempdir().unwrap();
    let dependency = product("OverlappingDependency", 80);
    let left = dependent_product(
        "IndependentLeft",
        81,
        vec![(7, vec![source_import(&dependency, 7)])],
    );
    let right = dependent_product(
        "IndependentRight",
        82,
        vec![(
            7,
            vec![source_import(&dependency, if overlap { 7 } else { 9001 })],
        )],
    );
    let recovered = recovered_witness_fixtures(&[dependency, left, right]);
    let get = |module: &str| {
        recovered
            .iter()
            .find(|row| row.product.owner().module == module)
            .unwrap()
            .product
            .clone()
    };
    let products = [
        get("OverlappingDependency"),
        get("IndependentLeft"),
        get(if same_root {
            "IndependentLeft"
        } else {
            "IndependentRight"
        }),
    ];
    let entries = products.each_ref().map(|product| {
        Arc::new(ArtifactEntry::original(producer.sha256(), product.clone()).unwrap())
    });
    let interfaces = CompilerInputProjection::from_issued_entries(&entries)
        .unwrap()
        .interface_only();
    let inventory = ArtifactInventory::default();
    let base_entries = if shared_carrier {
        entries.to_vec()
    } else {
        products
            .iter()
            .map(|product| {
                Arc::new(ArtifactEntry::canonical(
                    product.module_interface().unwrap().clone(),
                ))
            })
            .collect()
    };
    let base = inventory
        .admit_shared_with_demand(
            &inventory.empty_view(),
            base_entries,
            NativeArtifactDemand::ScopeInterfaces,
        )
        .unwrap();
    // Extra canonical owners distinguish the issuance scopes even
    // when both sides offer exactly the same original root.
    let issue = |index: usize, marker: &str, version| {
        let marker = product(marker, version);
        inventory
            .admit_recovery_selection(
                &base,
                vec![
                    entries[0].clone(),
                    entries[index].clone(),
                    Arc::new(ArtifactEntry::canonical(
                        marker.module_interface().unwrap().clone(),
                    )),
                ],
                &BTreeSet::from([
                    NativeGroupKey {
                        artifact: entries[index].descriptor.id,
                        original_ordinal: 7,
                    },
                    NativeGroupKey {
                        artifact: entries[0].descriptor.id,
                        original_ordinal: if index == 1 || overlap { 7 } else { 9001 },
                    },
                ]),
            )
            .unwrap()
    };
    let sources = [issue(1, "LeftScope", 83), issue(2, "RightScope", 84)];
    let proofs = std::array::from_fn::<_, 2, _>(|index| {
        let canonical =
            ArtifactEntry::canonical(products[index + 1].module_interface().unwrap().clone());
        NativeOriginalRequirement::from_issued_group_custody(
            entries[index + 1].descriptor.id,
            canonical.descriptor.id,
            &sources[index],
        )
        .unwrap()
        .unwrap()
    });
    let mut model = NativeTransportModel::default();
    let logical_roots = std::array::from_fn::<_, 2, _>(|index| {
        model.observe_issuance(
            index,
            if same_root { 1 } else { index + 1 },
            if index == 0 || overlap { 7 } else { 9001 },
            &entries,
            &sources[index],
        )
    });
    let expected = logical_roots.map(|root| model.closure(root));
    for index in 0..2 {
        assert_eq!(
            proofs[index]
                .issued_group_carriers(&sources[index])
                .unwrap(),
            expected[index]
        );
    }
    let child = |index: usize| {
        expected[index]
            .iter()
            .find(|(group, _)| group.binding.artifact() == entries[0].descriptor.id)
            .map(|(group, carrier)| (*group, *carrier))
            .unwrap()
    };
    assert_eq!(child(0).1 == child(1).1, shared_carrier);
    coverage.shared_carriers += usize::from(shared_carrier);
    coverage.distinct_carriers += usize::from(!shared_carrier);
    if overlap {
        assert_eq!(child(0).0.original_ordinal, child(1).0.original_ordinal);
        assert_ne!(child(0).0, child(1).0);
        coverage.equal_payload_scopes += 1;
    }
    let partial = std::array::from_fn::<_, 2, _>(|index| {
        NativeOriginalRequirement::from_selected_view(entries[0].descriptor.id, &sources[index])
            .unwrap()
            .unwrap()
    });
    let ambient = inventory
        .admit_recovery_selection(
            &sources[0],
            vec![],
            &BTreeSet::from([7, 9001].map(|original_ordinal| NativeGroupKey {
                artifact: entries[0].descriptor.id,
                original_ordinal,
            })),
        )
        .unwrap();
    let dependency_interface =
        ArtifactEntry::canonical(products[0].module_interface().unwrap().clone());
    let ambient_proof = NativeOriginalRequirement::from_issued_group_custody(
        entries[0].descriptor.id,
        dependency_interface.descriptor.id,
        &ambient,
    )
    .unwrap()
    .unwrap();
    let mut expanded = sources[0].clone();
    let mut has_ambient_full = false;
    for op in custody {
        match *op {
            NativeCustodyOp::Expand => {
                let before = expanded.capture_graph_selection();
                expanded = expanded
                    .retain_custody(&sources[1])
                    .unwrap()
                    .retain_custody(&ambient)
                    .unwrap();
                let after = expanded.capture_graph_selection();
                assert_eq!(before.namespace, after.namespace);
                assert_eq!(before.roots, after.roots);
                has_ambient_full = true;
                coverage.custody_expansions += 1;
            }
            NativeCustodyOp::Capture { right } => {
                let index = usize::from(right);
                // Namespace restoration below must not repair lost proof nodes.
                // Observe exact custody before rejoining any source payloads.
                for required in 0..if has_ambient_full { 2 } else { 1 } {
                    assert_eq!(
                        proofs[required].issued_group_carriers(&expanded).unwrap(),
                        expected[required]
                    );
                }
                if has_ambient_full {
                    assert!(ambient_proof
                        .qualify_availability(&expanded)
                        .unwrap()
                        .is_some());
                }
                let capture_projection = CompilerInputProjection::from_issued_entries(&[
                    entries[0].clone(),
                    entries[index + 1].clone(),
                ])
                .unwrap();
                let supplied = [proofs[index].clone(), partial[index].clone()];
                let before = crate::artifact_inventory::IssuedNativeOffer::issue_context_offers(
                    &capture_projection,
                    &sources[index],
                    &supplied,
                )
                .unwrap();
                // Restore this source's complete selected namespace, including
                // its marker payload, before comparing custody-only additions.
                let receiving = sources[index].retain_custody(&expanded).unwrap();
                assert_eq!(
                    receiving.capture_graph_selection().namespace,
                    sources[index].capture_graph_selection().namespace
                );
                let after = crate::artifact_inventory::IssuedNativeOffer::issue_context_offers(
                    &capture_projection,
                    &receiving,
                    &supplied,
                )
                .unwrap();
                assert_eq!(
                    after, before,
                    "custody expansion cannot change supplied evidence with fixed canonical bindings"
                );
                assert!(after
                    .iter()
                    .any(|offer| offer.is_full() && offer.proof() == Some(&proofs[index])));
                assert!(after
                    .iter()
                    .any(|offer| !offer.is_full() && offer.proof() == Some(&partial[index])));
                if index == 0 && has_ambient_full {
                    assert!(ambient_proof
                        .qualify_availability(&receiving)
                        .unwrap()
                        .is_some());
                    coverage.partial_with_ambient_full += 1;
                }
                coverage.captures += 1;
            }
        }
    }
    let context = |view: &ArtifactView| {
        let view = inventory
            .empty_view()
            .merge_fixture_compiler_inputs(&CompilerInputProjection::default(), view, &interfaces)
            .unwrap()
            .0;
        let support = issue_fixture_compiler_support(interfaces.clone(), view).unwrap();
        Arc::new(
            ExactDeclarationContext::new(&[], &[], vec![])
                .unwrap()
                .extend_issued_compiler_support(&support)
                .unwrap(),
        )
    };
    let private = std::array::from_fn::<_, 2, _>(|index| {
        // This is a selected existing Input root, independently of its empty
        // executable demand. Dependency bodies remain private type support.
        let root_projection = interfaces
            .merge(
                &CompilerInputProjection::from_issued_entries(&[entries[index + 1].clone()])
                    .unwrap(),
            )
            .unwrap();
        let selected = inventory
            .empty_view()
            .merge_fixture_compiler_inputs(
                &CompilerInputProjection::default(),
                &sources[index],
                &root_projection,
            )
            .unwrap()
            .0;
        let binding = |view: &ArtifactView, id| {
            view.capture_graph_selection()
                .namespace
                .into_iter()
                .find(|binding| binding.artifact == id)
                .unwrap()
        };
        let canonical =
            ArtifactEntry::canonical(products[index + 1].module_interface().unwrap().clone());
        for id in [canonical.descriptor.id, entries[index + 1].descriptor.id] {
            assert_eq!(binding(&selected, id), binding(&sources[index], id));
        }
        let complete_input =
            issue_fixture_compiler_support(root_projection.clone(), selected).unwrap();
        let issued = complete_input.artifacts();
        let mut graph = issued.capture_graph_selection();
        let namespace = graph.namespace.clone();
        let execution = graph
            .roots
            .iter()
            .filter(|root| matches!(root, ArtifactBindingNode::Group(_)))
            .copied()
            .collect::<Vec<_>>();
        graph.roots.retain(|root| !execution.contains(root));
        graph.custody_roots.extend(execution);
        graph.custody_roots.sort();
        graph.custody_roots.dedup();
        let chosen = issued
            .select_issued(
                namespace.iter().map(|binding| binding.artifact).collect(),
                &crate::artifact_inventory::ExactArtifactSelection::from_recovered_graph_selection(
                    graph,
                ),
            )
            .unwrap();
        for id in [canonical.descriptor.id, entries[index + 1].descriptor.id] {
            assert_eq!(binding(&chosen, id), binding(&sources[index], id));
        }
        let support = issue_fixture_compiler_support(root_projection, chosen)
            .unwrap()
            .retain_offers(complete_input.native_offers().iter().cloned())
            .unwrap()
            .retain_normalized_availability(complete_input.native_availability())
            .unwrap();
        let roles = support.projection().snapshot_roles();
        assert_eq!(
            roles[&entries[index + 1].descriptor.owner].original(),
            Some(entries[index + 1].descriptor.id)
        );
        assert!(roles[&entries[0].descriptor.owner].original().is_none());
        let root_row = support
            .native_availability()
            .rows()
            .find(|row| row.original_owner() == products[index + 1].owner())
            .unwrap();
        assert!(root_row.permits(7));
        assert_eq!(
            support.native_availability().snapshot_facts().unwrap()
                [&entries[index + 1].descriptor.id]
                .mode,
            crate::declaration_context::NativeTransportMode::Full
        );
        let dependency_row = support
            .native_availability()
            .rows()
            .find(|row| row.original_owner() == products[0].owner())
            .unwrap();
        assert!(dependency_row.permits(if index == 0 || overlap { 7 } else { 9001 }));
        assert!(!dependency_row.permits(if index == 0 || overlap { 9001 } else { 7 }));
        assert!(support.artifacts().selected_native_groups().is_empty());
        let input = OriginalCompilerInputs::from_issued_support(&support);
        assert_eq!(input.role_projection(), support.projection());
        assert_eq!(input.native_availability(), support.native_availability());
        assert_eq!(input.issued_offers(), support.native_offers());
        input
    });
    let base_context = context(&base);
    let first = usize::from(reverse);
    let closed = private.each_ref().map(|input| {
        crate::artifact_inventory::CompilerSourceSupport::close_complete_output(input).unwrap()
    });
    let mut joined_support = closed[first].merge(&closed[1 - first]).unwrap();
    coverage.reversed_joins += usize::from(reverse);
    if duplicate {
        joined_support = joined_support.merge(&closed[first]).unwrap();
        coverage.duplicate_joins += 1;
    }
    let mut joined = OriginalCompilerInputs::from_issued_support(&joined_support);
    // Join order must not change demand when the caller pins the same namespace.
    let pinned = private[0]
        .issued_artifacts()
        .retain_custody(joined.issued_artifacts())
        .unwrap();
    let pin_request = ExactCompileContext::new(base_context.clone())
        .prepare_compilation(&root.path().join("namespace-pin-input"), PRODUCER)
        .unwrap();
    let (pin_admission, pin_source) = generated_source_admission_fixture(
        &root.path().join("namespace-pin-receipt"),
        &pin_request,
    );
    joined = joined
        .for_program_continuation(&pinned, &[pin_admission], &pin_source)
        .unwrap();
    coverage.namespace_pin_continuations += 1;
    // The private product constructor deliberately has no full issuer view.
    // Eager-fold equivalence is covered by the independently issued history
    // above; this history follows the real opaque-owner consumer path.
    let before_retention = base_context.artifact_view().capture_graph_selection();
    let retained = base_context
        .artifact_view()
        .retain_issued_offer_custody(joined.native_offers().iter().cloned())
        .unwrap();
    let after_retention = retained.capture_graph_selection();
    assert_eq!(after_retention.roots, before_retention.roots);
    assert_eq!(after_retention.namespace, before_retention.namespace);
    let request = ExactCompileContext::new(base_context.clone())
        .prepare_compilation_with_private_input(
            &root.path().join("request"),
            PRODUCER,
            None,
            Some(joined),
        )
        .unwrap();
    coverage.joined_preparations += 1;
    let inputs = request.compiler_inputs();
    assert!(inputs.metadata.retained.selected_native_groups.is_empty());
    assert_eq!(
        inputs.projection().snapshot_roles()[&entries[1].descriptor.owner].original(),
        Some(entries[1].descriptor.id)
    );
    let root_binding = |view: &ArtifactView| {
        view.capture_graph_selection()
            .namespace
            .into_iter()
            .find(|binding| binding.artifact == entries[1].descriptor.id)
            .unwrap()
    };
    assert_eq!(
        root_binding(inputs.artifacts()),
        root_binding(&sources[0]),
        "the exact selected Root0 carrier disambiguates distinct-carrier historical offers"
    );
    let roles = inputs.projection().snapshot_roles();
    let role = &roles[&entries[0].descriptor.owner];
    assert!(
        role.original().is_none(),
        "a dependency receives no native role"
    );
    assert_eq!(
        wire_mode(&request, "OverlappingDependency"),
        issued_mode(if overlap { &[7] } else { &[7, 9001] }),
        "a union of issued dependency groups never issues Full"
    );
    assert!(wire_groups(&request).values().all(Vec::is_empty));
    assert_eq!(wire_mode(&request, "IndependentLeft"), full_mode());
    coverage.full_without_demand += 1;
    for index in 0..2 {
        let availability = inputs.native_availability(proofs[index].original).unwrap();
        assert!(availability
            .offers()
            .any(|offer| offer.original() == proofs[index].original
                && offer.proof().is_some_and(|proof| proof
                    .issued_group_carriers(inputs.artifacts())
                    .unwrap()
                    == expected[index])));
    }
    let validation = PackageInterfaceValidation::default();
    let selected = crate::certified_products::CertifiedSourceSelection::from_compiler_inputs(
        inputs,
        &validation.inventory,
    )
    .unwrap();
    let bind = || {
        inputs
            .artifacts()
            .inventory()
            .bind_segment(inputs.artifacts(), vec![], &selected)
    };
    for index in 0..2 {
        let mut fixed = bind().unwrap();
        let imports = [source_import(&products[index + 1], 7)];
        let admitted = inputs.artifacts().inventory().admit_bound_target(
            &mut fixed,
            inputs.artifacts(),
            NativeArtifactDemand::CertifiedTargetImports(&imports),
            vec![],
            &selected,
        );
        if same_root && shared_carrier {
            assert!(
                matches!(admitted, Err(CompileError::ArtifactInventory(error))
                if matches!(error.failure, ArtifactInventoryFailure::BindingAmbiguity { artifact }
                    if artifact == entries[1].descriptor.id))
            );
            coverage.ambiguous_demands += 1;
        } else {
            let admitted = admitted.unwrap();
            let groups = admitted
                .clone()
                .bind_target(None, (&imports).to_vec())
                .resolve()
                .map(|resolved| resolved.groups().clone())
                .unwrap();
            let expected_index = if same_root { 0 } else { index };
            assert_eq!(groups, expected[expected_index].keys().copied().collect());
            coverage.successful_demands += 1;
        }
    }
    let mut fixed = bind().unwrap();
    let imports = [source_import(&products[0], 7)];
    let demanded = inputs.artifacts().inventory().admit_bound_target(
        &mut fixed,
        inputs.artifacts(),
        NativeArtifactDemand::CertifiedTargetImports(&imports),
        vec![],
        &selected,
    );
    if overlap {
        assert!(
            matches!(demanded, Err(CompileError::ArtifactInventory(error))
            if matches!(error.failure, ArtifactInventoryFailure::BindingAmbiguity { artifact }
                if artifact == entries[0].descriptor.id))
        );
        coverage.ambiguous_demands += 1;
    } else {
        let demanded = demanded.unwrap();
        let groups = demanded
            .clone()
            .bind_target(None, (&imports).to_vec())
            .resolve()
            .map(|resolved| resolved.groups().clone())
            .unwrap();
        assert_eq!(groups, BTreeSet::from([child(0).0]));
        coverage.successful_demands += 1;
    }
    let handoff = OriginalCompilerInputs::from_selection(&selected, inputs.artifacts()).unwrap();
    let next = ExactCompileContext::new(base_context.clone())
        .prepare_compilation_with_private_input(
            &root.path().join("handoff"),
            PRODUCER,
            None,
            Some(handoff),
        )
        .unwrap();
    let encoded = serde_json::to_vec(&(
        next.compiler_inputs().artifacts().capture_graph_selection(),
        next.compiler_inputs().artifacts().binding_dependencies(),
    ))
    .unwrap();
    let (graph, edges): (ArtifactGraphSelection, Vec<_>) =
        serde_json::from_slice(&encoded).unwrap();
    let recovery = ArtifactInventory::default();
    assert_eq!(recovery.node_count(), 0);
    let restored = recovery
        .recover_selection(
            &recovery.empty_view(),
            next.compiler_inputs().artifacts().entries(),
            &graph,
            &edges,
        )
        .unwrap();
    coverage.recoveries += 1;
    for index in 0..2 {
        assert_eq!(
            proofs[index].issued_group_carriers(&restored).unwrap(),
            expected[index]
        );
    }
    assert_eq!(restored.capture_graph_selection(), graph);
    assert_eq!(restored.binding_dependencies(), edges);

    // A dependent product earns its new graph before continuation returns to
    // older support that owns its bytes but none of its issued group nodes.
    let dependent = dependent_product(
        "LaterDependent",
        85,
        vec![(7, vec![source_import(&products[1], 7)])],
    );
    let dependent =
        recovered_witness_fixtures(&[products[0].clone(), products[1].clone(), dependent])
            .into_iter()
            .find(|row| row.product.owner().module == "LaterDependent")
            .unwrap()
            .product;
    let dependent_entry =
        Arc::new(ArtifactEntry::original(producer.sha256(), dependent.clone()).unwrap());
    let support = private[0]
        .issued_artifacts()
        .inventory()
        .admit_shared_with_demand(
            private[0].issued_artifacts(),
            vec![dependent_entry.clone()],
            NativeArtifactDemand::ScopeInterfaces,
        )
        .unwrap();
    let actual_context = context(&base);
    let actual_request = ExactCompileContext::new(actual_context)
        .prepare_compilation_with_private_input(
            &root.path().join("new-output-input"),
            PRODUCER,
            None,
            Some(private[0].clone()),
        )
        .unwrap();
    coverage.output_request_preparations += 1;
    let operation = InventoryOperation::new(Default::default());
    let mut output_selection = CertifiedSourceSelection::from_compiler_inputs(
        actual_request.compiler_inputs(),
        &operation,
    )
    .unwrap();
    output_selection
        .admit_current(
            dependent.owner(),
            ProductOrigin::RetainedCore,
            dependent.module_interface().unwrap(),
            &operation,
        )
        .unwrap();
    let physical = actual_request
        .compiler_inputs()
        .artifacts()
        .inventory()
        .admit_shared_with_demand(
            actual_request.compiler_inputs().artifacts(),
            vec![dependent_entry.clone()],
            NativeArtifactDemand::ScopeInterfaces,
        )
        .unwrap();
    let produced = OriginalCompilerInputs::from_selection(&output_selection, &physical).unwrap();
    assert_eq!(
        produced.issued_artifacts().selected_native_groups(),
        actual_request
            .compiler_inputs()
            .artifacts()
            .selected_native_groups()
    );
    coverage.dependent_issuances += 1;
    let proof = produced
        .native_offers()
        .iter()
        .filter(|offer| offer.original() == dependent_entry.descriptor.id)
        .find_map(|offer| offer.proof())
        .unwrap()
        .clone();
    assert!(
        proof.issued_groups(&support).is_err(),
        "older support cannot already contain the newly issued group"
    );
    let issued_graph = proof.selection.graph_selection();
    let dependent_groups = issued_graph
        .native_groups
        .iter()
        .copied()
        .filter(|group| group.binding.artifact() == dependent_entry.descriptor.id)
        .collect::<Vec<_>>();
    let [dependent_group] = dependent_groups.as_slice() else {
        panic!("one new dependent group")
    };
    assert_eq!(dependent_group.original_ordinal, 7);
    let dependent_carrier = issued_graph
        .namespace
        .iter()
        .copied()
        .find(|binding| binding.artifact == dependent_entry.descriptor.id)
        .unwrap();
    let mut dependent_expected = expected[0].clone();
    assert!(dependent_expected
        .insert(*dependent_group, dependent_carrier)
        .is_none());
    let dependent_offer = produced
        .native_offers()
        .iter()
        .find(|offer| offer.original() == dependent_entry.descriptor.id)
        .unwrap()
        .clone();
    assert!(dependent_offer.is_full());
    assert_eq!(dependent_offer.proof(), Some(&proof));
    assert!(
        proof
            .issued_group_carriers(produced.issued_artifacts())
            .is_err(),
        "carrier-only output cannot authenticate the opaque dependent Group proof"
    );
    let expected_child = *expected[0]
        .keys()
        .find(|group| {
            group.binding.artifact() == entries[1].descriptor.id && group.original_ordinal == 7
        })
        .unwrap();
    let expected_dependency = (
        ArtifactBindingNode::Group(*dependent_group),
        ArtifactBindingNode::Group(expected_child),
        crate::artifact_inventory::ArtifactDependency::NativeGroup {
            dependent_ordinal: 7,
            required_ordinal: 7,
        },
    );
    let assert_dependent_offer = |offers: &[IssuedNativeOffer]| {
        let retained = offers
            .iter()
            .find(|offer| offer.same_issuance(&dependent_offer))
            .expect("the exact dependent issuance retains its historical child");
        assert!(retained.is_full());
        assert_eq!(retained.proof(), Some(&proof));
        assert_eq!(
            retained.issued_group_carriers().unwrap(),
            dependent_expected
        );
        let edges = retained.custody_snapshot().binding_dependencies().unwrap();
        assert!(edges.contains(&(
            ArtifactBindingNode::Group(*dependent_group),
            ArtifactBindingNode::Artifact(dependent_carrier),
            crate::artifact_inventory::ArtifactDependency::Interface,
        )));
        assert!(edges.contains(&expected_dependency));
    };
    assert_dependent_offer(produced.native_offers());
    let (admission, source) =
        generated_source_admission_fixture(&root.path().join("receipt"), &request);
    let continued = produced
        .for_program_continuation(&support, &[admission], &source)
        .unwrap();
    coverage.continuations += 1;
    assert_eq!(
        continued.issued_artifacts().capture_graph_selection().roots,
        support.capture_graph_selection().roots
    );
    assert_eq!(
        continued
            .issued_artifacts()
            .capture_graph_selection()
            .namespace,
        support.capture_graph_selection().namespace
    );
    assert!(proof
        .issued_group_carriers(continued.issued_artifacts())
        .is_err());
    assert_dependent_offer(continued.native_offers());
    let continued_request = ExactCompileContext::new(base_context)
        .prepare_compilation_with_private_input(
            &root.path().join("continued"),
            PRODUCER,
            None,
            Some(continued),
        )
        .unwrap();
    let continued_inputs = continued_request.compiler_inputs();
    assert!(continued_inputs
        .metadata
        .retained
        .selected_native_groups
        .is_empty());
    let continued_selection =
        crate::certified_products::CertifiedSourceSelection::from_compiler_inputs(
            continued_inputs,
            &validation.inventory,
        )
        .unwrap();
    let (continued_graph, continued_edges): (ArtifactGraphSelection, Vec<_>) =
        serde_json::from_slice(
            &serde_json::to_vec(&(
                continued_inputs.artifacts().capture_graph_selection(),
                continued_inputs.artifacts().binding_dependencies(),
            ))
            .unwrap(),
        )
        .unwrap();
    let fresh = ArtifactInventory::default();
    assert_eq!(fresh.node_count(), 0);
    let reopened = fresh
        .recover_selection(
            &fresh.empty_view(),
            continued_inputs.artifacts().entries(),
            &continued_graph,
            &continued_edges,
        )
        .unwrap();
    coverage.recoveries += 1;
    assert_dependent_offer(continued_inputs.compiler_source_support().native_offers());
    for view in [continued_inputs.artifacts(), &reopened] {
        assert!(
            proof.issued_group_carriers(view).is_err(),
            "selected carrier snapshots do not reconstruct opaque Group availability"
        );
        let mut fixed = view
            .inventory()
            .bind_segment(view, vec![], &continued_selection)
            .unwrap();
        let imports = [source_import(&dependent, 7)];
        let admitted = view
            .inventory()
            .admit_bound_target(
                &mut fixed,
                view,
                NativeArtifactDemand::CertifiedTargetImports(&imports),
                vec![],
                &continued_selection,
            )
            .unwrap();
        let demanded = admitted
            .clone()
            .bind_target(None, (&imports).to_vec())
            .resolve()
            .map(|resolved| resolved.groups().clone())
            .unwrap();
        assert_eq!(demanded, dependent_expected.keys().copied().collect());
        coverage.successful_demands += 1;
    }
}

#[test]
fn private_transport_preserves_independent_closures_until_exact_demand() {
    let mut coverage = NativeTransportCoverage::default();
    for same_root in [false, true] {
        for overlap in [false, true] {
            if same_root && !overlap {
                continue;
            }
            for shared_carrier in [false, true] {
                for reverse in [false, true] {
                    for duplicate in [false, true] {
                        replay_native_transport_history(
                            &NativeTransportHistory {
                                same_root,
                                overlap,
                                shared_carrier,
                                reverse,
                                duplicate,
                                custody: vec![
                                    NativeCustodyOp::Capture { right: false },
                                    NativeCustodyOp::Expand,
                                    NativeCustodyOp::Capture { right: false },
                                    NativeCustodyOp::Capture { right: true },
                                ],
                            },
                            &mut coverage,
                        );
                        coverage.completed += 1;
                    }
                }
            }
        }
    }
    assert_eq!(coverage.completed, 24);
    assert!(coverage.shared_carriers > 0 && coverage.distinct_carriers > 0);
    assert_eq!(coverage.equal_payload_scopes, 16);
    assert_eq!(coverage.captures, 72);
    assert_eq!(coverage.partial_with_ambient_full, 24);
    assert_eq!(coverage.full_without_demand, 24);
    assert_eq!(coverage.joined_preparations, 24);
    assert_eq!(coverage.namespace_pin_continuations, 24);
    assert_eq!(coverage.output_request_preparations, 24);
    assert_eq!(coverage.dependent_issuances, 24);
    assert_eq!(coverage.continuations, 24);
    assert_eq!(coverage.recoveries, 48);
    assert!(coverage.successful_demands > 0 && coverage.ambiguous_demands > 0);
    eprintln!("native transport deterministic support: {coverage:?}");
}

#[test]
fn generated_private_transport_histories_preserve_issued_graphs() {
    use proptest::prelude::*;
    use proptest::test_runner::{
        contextualize_config, Config, FileFailurePersistence, TestCaseError, TestRunner,
    };

    let mut config = Config::default();
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = 32;
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
        "::generated_private_transport_histories_preserve_issued_graphs"
    ));
    let cases = config.cases;
    let strategy = (
        0u8..3,
        any::<[bool; 3]>(),
        prop::collection::vec(
            prop_oneof![
                Just(NativeCustodyOp::Expand),
                any::<bool>().prop_map(|right| NativeCustodyOp::Capture { right })
            ],
            0..9,
        ),
    )
        .prop_map(|(shape, [shared_carrier, reverse, duplicate], tail)| {
            let mut custody = vec![
                NativeCustodyOp::Capture { right: false },
                NativeCustodyOp::Expand,
                NativeCustodyOp::Capture { right: false },
                NativeCustodyOp::Capture { right: true },
            ];
            custody.extend(tail);
            NativeTransportHistory {
                same_root: shape == 2,
                overlap: shape != 0,
                shared_carrier,
                reverse,
                duplicate,
                custody,
            }
        });
    let coverage = std::cell::RefCell::new(NativeTransportCoverage::default());
    let result = TestRunner::new(config).run(&strategy, |history| {
        coverage.borrow_mut().callbacks += 1;
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            replay_native_transport_history(&history, &mut coverage.borrow_mut());
        }));
        if let Err(panic) = outcome {
            let reason = panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| {
                    panic
                        .downcast_ref::<&str>()
                        .map(|message| (*message).to_owned())
                })
                .unwrap_or_else(|| "non-string panic".into());
            return Err(TestCaseError::fail(format!(
                "history={history:?}: {reason}"
            )));
        }
        coverage.borrow_mut().completed += 1;
        Ok(())
    });
    eprintln!(
        "native transport campaign configured_cases={cases} coverage={:?}",
        coverage.borrow()
    );
    result.unwrap();
}
