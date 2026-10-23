use super::*;
use crate::artifact_inventory::{
    ArtifactBinding, ArtifactBindingNode, ArtifactDependency, ArtifactEntry,
    ArtifactGraphSelection, ArtifactInventory, ArtifactInventoryFailure, ArtifactView,
    CanonicalProducerIdentity, CompilerInputProjection, NativeGroupKey,
};
use crate::declaration_context::{
    issue_fixture_compiler_support, ExactCompilationRequest, ExactCompileContext,
    ExactDeclarationContext, OriginalCompilerInputs,
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
    let definitions =
        crate::certified_products::original_native_group_definitions(product, ordinal)
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
    issued_demand_preservations: usize,
    partial_with_ambient_full: usize,
    custody_expansions: usize,
    captures: usize,
    duplicate_joins: usize,
    reversed_joins: usize,
    joined_preparations: usize,
    namespace_pins: usize,
    output_request_preparations: usize,
    dependent_issuances: usize,
    private_output_handoffs: usize,
    successful_demands: usize,
    ambiguous_demands: usize,
    recoveries: usize,
    produced_demands: usize,
    recovered_input_refusals: usize,
    unselected_root_ambiguities: usize,
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
    tracing::debug!(?history, "native transport history");
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
            .map(|product| Arc::new(ArtifactEntry::canonical(product.module_interface().clone())))
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
                    Arc::new(ArtifactEntry::canonical(marker.module_interface().clone())),
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
        let canonical = ArtifactEntry::canonical(products[index + 1].module_interface().clone());
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
    let dependency_interface = ArtifactEntry::canonical(products[0].module_interface().clone());
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
                let supplied = [
                    (&entries[index + 1].descriptor.owner, proofs[index].clone()),
                    (&entries[0].descriptor.owner, partial[index].clone()),
                ];
                let issuer =
                    issue_fixture_compiler_support(capture_projection, sources[index].clone())
                        .unwrap();
                let before = supplied
                    .iter()
                    .map(|(owner, proof)| issuer.issue_native_offer(owner, Some(proof.clone())))
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap();
                // Restore this source's complete selected namespace, including
                // its marker payload, before comparing custody-only additions.
                let receiving = sources[index].retain_custody(&expanded).unwrap();
                assert_eq!(
                    receiving.capture_graph_selection().namespace,
                    sources[index].capture_graph_selection().namespace
                );
                let receiving_issuer = issuer.retain_custody(&receiving).unwrap();
                let after = supplied
                    .iter()
                    .map(|(owner, proof)| {
                        receiving_issuer.issue_native_offer(owner, Some(proof.clone()))
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap();
                assert_eq!(after.len(), before.len());
                assert!(
                    after.iter().zip(&before).all(|(after, before)| {
                        after.same_recorded_facts(before)
                            && Arc::ptr_eq(after.original_acquisition(), before.original_acquisition())
                    }),
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
        // The existing role issuer selects the parent Original's exact Group
        // root. Its TYPE child is reached through the issued dependency edge.
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
        let canonical = ArtifactEntry::canonical(products[index + 1].module_interface().clone());
        for id in [canonical.descriptor.id, entries[index + 1].descriptor.id] {
            assert_eq!(binding(&selected, id), binding(&sources[index], id));
        }
        let support = issue_fixture_compiler_support(root_projection, selected).unwrap();
        for id in [canonical.descriptor.id, entries[index + 1].descriptor.id] {
            assert_eq!(
                binding(support.artifacts(), id),
                binding(&sources[index], id)
            );
        }
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
        let expected_parent = *expected[index]
            .keys()
            .find(|group| {
                group.binding.artifact() == entries[index + 1].descriptor.id
                    && group.original_ordinal == 7
            })
            .unwrap();
        assert_eq!(
            support
                .artifacts()
                .capture_graph_selection()
                .roots
                .into_iter()
                .filter_map(|root| match root {
                    ArtifactBindingNode::Group(group) => Some(group),
                    _ => None,
                })
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([expected_parent])
        );
        assert_eq!(
            support.artifacts().selected_native_groups(),
            expected[index]
                .keys()
                .map(|group| NativeGroupKey {
                    artifact: group.binding.artifact(),
                    original_ordinal: group.original_ordinal
                })
                .collect()
        );
        let input = OriginalCompilerInputs::from_issued_support(&support);
        assert_eq!(input.role_projection(), support.projection());
        assert!(input
            .native_availability()
            .same_acquisition(support.native_availability()));
        assert_eq!(input.issued_offers().len(), support.native_offers().len());
        assert!(input
            .issued_offers()
            .iter()
            .zip(support.native_offers())
            .all(|(received, issued)| received.same_issuance(issued)));
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
    // Pin the same actual selected issuer after either join order. This is
    // namespace reconciliation, not a completed source-compilation claim.
    joined_support = closed[0].merge(&joined_support).unwrap();
    let joined = OriginalCompilerInputs::from_issued_support(&joined_support);
    coverage.namespace_pins += 1;
    // This history transports the issued execution graph and its opaque owners.
    // Full availability without demand is covered by the actual captured-output
    // control full_native_choices_survive_role_and_output_projection_without_demand.
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
    let winning_indices = if same_root { vec![0] } else { vec![0, 1] };
    let expected_native = winning_indices
        .iter()
        .flat_map(|index| expected[*index].keys())
        .map(|group| NativeGroupKey {
            artifact: group.binding.artifact(),
            original_ordinal: group.original_ordinal,
        })
        .collect::<BTreeSet<_>>();
    let expected_parent_roots = winning_indices
        .iter()
        .map(|index| {
            *expected[*index]
                .keys()
                .find(|group| {
                    group.binding.artifact() == entries[*index + 1].descriptor.id
                        && group.original_ordinal == 7
                })
                .unwrap()
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        inputs.metadata().retained.selected_native_groups.as_ref(),
        &expected_native
    );
    assert_eq!(
        inputs
            .artifacts()
            .capture_graph_selection()
            .roots
            .into_iter()
            .filter_map(|root| match root {
                ArtifactBindingNode::Group(group) => Some(group),
                _ => None,
            })
            .collect::<BTreeSet<_>>(),
        expected_parent_roots
    );
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
    // The compiler census exports only selected Original owners. The TYPE
    // child's exact groups stay in executable dependency closure and private
    // availability; they are not independent compiler replay roots.
    let expected_wire = entries
        .iter()
        .map(|entry| {
            let ordinals = expected_parent_roots
                .iter()
                .filter(|group| group.binding.artifact() == entry.descriptor.id)
                .map(|group| group.original_ordinal)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            (entry.descriptor.owner.module.clone(), ordinals)
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(wire_groups(&request), expected_wire);
    assert_eq!(wire_mode(&request, "IndependentLeft"), full_mode());
    coverage.issued_demand_preservations += 1;
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
    let mut validation = PackageInterfaceValidation::default();
    let selected = crate::certified_products::CertifiedSourceSelection::from_compiler_inputs(
        inputs,
        &validation.inventory,
    )
    .unwrap();
    let bind = |validation: &mut PackageInterfaceValidation| {
        inputs.artifacts().inventory().bind_segment(
            inputs.artifacts(),
            vec![],
            &selected,
            validation,
        )
    };
    for index in 0..2 {
        let fixed = bind(&mut validation).unwrap();
        let observations = source_target_observations(&[(&products[index + 1], 7)]);
        let prepared = prepared_target_fixture(&observations);
        let observed =
            AuthenticatedTargetImports::issue(&prepared, &observations, &validation.inventory)
                .unwrap();
        let admitted = inputs.artifacts().inventory().select_native_target(
            fixed.captured_stage().physical_cohort(),
            observed,
            None,
            Some(inputs.artifacts()),
            &[],
            &BTreeMap::new(),
            &mut validation,
        );
        let admitted = admitted.unwrap();
        let groups = admitted
            .resolve()
            .map(|resolved| resolved.groups().clone())
            .unwrap();
        let expected_index = if same_root { 0 } else { index };
        assert_eq!(groups, expected[expected_index].keys().copied().collect());
        coverage.successful_demands += 1;
    }
    if same_root && shared_carrier {
        // Removing the selected Original removes its exact root choice. The
        // independently issued Full proofs still compete on the shared carrier.
        let type_support = joined_support.for_source_owners(&BTreeSet::new()).unwrap();
        assert!(
            type_support.projection().snapshot_roles()[&entries[1].descriptor.owner]
                .original()
                .is_none()
        );
        let competing = expected
            .iter()
            .map(|groups| {
                *groups
                    .keys()
                    .find(|group| {
                        group.binding.artifact() == entries[1].descriptor.id
                            && group.original_ordinal == 7
                    })
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert_ne!(competing[0], competing[1]);
        assert_eq!(expected[0][&competing[0]], expected[1][&competing[1]]);
        let type_request = ExactCompileContext::new(base_context.clone())
            .prepare_compilation_with_private_input(
                &root.path().join("unselected-competing-root"),
                PRODUCER,
                None,
                Some(OriginalCompilerInputs::from_issued_support(&type_support)),
            )
            .unwrap();
        let type_inputs = type_request.compiler_inputs();
        let type_selection =
            CertifiedSourceSelection::from_compiler_inputs(type_inputs, &validation.inventory)
                .unwrap();
        for index in 0..2 {
            let fixed = type_inputs
                .artifacts()
                .inventory()
                .bind_segment(
                    type_inputs.artifacts(),
                    vec![],
                    &type_selection,
                    &mut validation,
                )
                .unwrap();
            let observations = source_target_observations(&[(&products[index + 1], 7)]);
            let prepared = prepared_target_fixture(&observations);
            let observed =
                AuthenticatedTargetImports::issue(&prepared, &observations, &validation.inventory)
                    .unwrap();
            let error = type_inputs
                .artifacts()
                .inventory()
                .select_native_target(
                    fixed.captured_stage().physical_cohort(),
                    observed,
                    None,
                    Some(type_inputs.artifacts()),
                    &[],
                    &BTreeMap::new(),
                    &mut validation,
                )
                .err()
                .expect("TYPE-only input cannot choose between independent Full issuers");
            assert!(
                matches!(error, CompileError::ArtifactInventory(ref error)
                if matches!(error.failure, ArtifactInventoryFailure::BindingAmbiguity { artifact }
                    if artifact == entries[1].descriptor.id)),
                "{error:?}"
            );
            coverage.ambiguous_demands += 1;
            coverage.unselected_root_ambiguities += 1;
        }
    }
    let fixed = bind(&mut validation).unwrap();
    let observations = source_target_observations(&[(&products[0], 7)]);
    let prepared = prepared_target_fixture(&observations);
    let observed =
        AuthenticatedTargetImports::issue(&prepared, &observations, &validation.inventory).unwrap();
    let demanded = inputs.artifacts().inventory().select_native_target(
        fixed.captured_stage().physical_cohort(),
        observed,
        None,
        Some(inputs.artifacts()),
        &[],
        &BTreeMap::new(),
        &mut validation,
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
            .resolve()
            .map(|resolved| resolved.groups().clone())
            .unwrap();
        assert_eq!(groups, BTreeSet::from([child(0).0]));
        coverage.successful_demands += 1;
    }
    let handoff = OriginalCompilerInputs::from_issued_support(inputs.compiler_source_support());
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
            dependent.module_interface(),
            &operation,
        )
        .unwrap();
    output_selection
        .bind_produced_originals(
            &IssuedGroupCandidates::bind(Arc::from([]), std::slice::from_ref(&dependent)).unwrap(),
        )
        .unwrap();
    let produced = crate::declaration_context::CapturedCompilerStage::issue(
        &mut output_selection,
        actual_request.compiler_inputs().artifacts(),
        vec![dependent_entry.clone()],
        &mut validation,
    )
    .unwrap();
    assert_eq!(
        produced.issued_artifacts().selected_native_groups(),
        actual_request
            .compiler_inputs()
            .artifacts()
            .selected_native_groups()
    );
    coverage.dependent_issuances += 1;
    let proof = produced
        .compiler_inputs()
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
        .compiler_inputs()
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
    assert_dependent_offer(produced.compiler_inputs().native_offers());
    // The actual physical cohort owns this new RetainedCore completion. Its
    // TYPE projection does not make a later Input cohort its current producer.
    let mut observations = source_target_observations(&[(&dependent, 7)]);
    let ReceiptImportOwner::Source { module_version, .. } = &mut observations[0].owner else {
        unreachable!()
    };
    *module_version = None;
    let prepared = prepared_target_fixture(&observations);
    let observed =
        AuthenticatedTargetImports::issue(&prepared, &observations, &validation.inventory).unwrap();
    let demanded = produced
        .issued_artifacts()
        .inventory()
        .select_native_target(
            produced.physical_cohort(),
            observed,
            None,
            Some(produced.issued_artifacts()),
            &[],
            &BTreeMap::new(),
            &mut validation,
        )
        .unwrap()
        .resolve()
        .map(|resolved| resolved.groups().clone())
        .unwrap();
    assert_eq!(demanded, dependent_expected.keys().copied().collect());
    coverage.produced_demands += 1;
    coverage.successful_demands += 1;
    // Transport the actual captured prefix. No generated source owner is
    // omitted, and no source-compilation authority is needed for this carry.
    let completed_support =
        crate::artifact_inventory::CompilerSourceSupport::close_complete_output(
            produced.compiler_inputs(),
        )
        .unwrap()
        .retain_custody(&support)
        .unwrap();
    let continued = OriginalCompilerInputs::from_issued_support(&completed_support);
    coverage.private_output_handoffs += 1;
    assert!(completed_support
        .same_selected_acquisitions(produced.compiler_inputs().compiler_source_support()));
    assert_eq!(
        continued.issued_artifacts().capture_graph_selection().roots,
        produced
            .compiler_inputs()
            .issued_artifacts()
            .capture_graph_selection()
            .roots
    );
    assert_eq!(
        continued
            .issued_artifacts()
            .capture_graph_selection()
            .namespace,
        produced
            .compiler_inputs()
            .issued_artifacts()
            .capture_graph_selection()
            .namespace
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
    assert_eq!(
        continued_inputs
            .metadata()
            .retained
            .selected_native_groups
            .as_ref(),
        &produced
            .compiler_inputs()
            .issued_artifacts()
            .selected_native_groups()
    );
    assert_eq!(
        continued_inputs
            .artifacts()
            .capture_graph_selection()
            .roots
            .into_iter()
            .filter(|root| matches!(root, ArtifactBindingNode::Group(_)))
            .collect::<BTreeSet<_>>(),
        produced
            .compiler_inputs()
            .issued_artifacts()
            .capture_graph_selection()
            .roots
            .into_iter()
            .filter(|root| matches!(root, ArtifactBindingNode::Group(_)))
            .collect::<BTreeSet<_>>()
    );
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
        let fixed = view
            .inventory()
            .bind_segment(view, vec![], &continued_selection, &mut validation)
            .unwrap();
        let observations = source_target_observations(&[(&dependent, 7)]);
        let prepared = prepared_target_fixture(&observations);
        let observed =
            AuthenticatedTargetImports::issue(&prepared, &observations, &validation.inventory)
                .unwrap();
        let before = view.capture_graph_selection();
        let key = NativeGroupKey {
            artifact: dependent_entry.descriptor.id,
            original_ordinal: 7,
        };
        assert!(fixed
            .captured_stage()
            .source_selection()
            .current_product(key, &dependent_entry)
            .unwrap()
            .is_none());
        let error = view
            .inventory()
            .select_native_target(
                fixed.captured_stage().physical_cohort(),
                observed,
                None,
                Some(view),
                &[],
                &BTreeMap::new(),
                &mut validation,
            )
            .err()
            .expect("recovered TYPE+Full Input is not a new Current product");
        assert!(
            matches!(error, CompileError::ArtifactInventory(ref error)
            if error.failure == ArtifactInventoryFailure::NativeGroupUnavailable {
                artifact: key.artifact, original_ordinal: key.original_ordinal,
            } && error.native_refusal == Some(crate::artifact_inventory::NativeAdmissionRefusal::RootChoiceMissing {
                owner: dependent_entry.descriptor.owner.clone(),
            })),
            "{error:?}"
        );
        assert_eq!(view.capture_graph_selection(), before);
        coverage.recovered_input_refusals += 1;
    }
}

#[test]
fn private_transport_preserves_independent_closures_and_exact_demand() {
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
    assert_eq!(coverage.issued_demand_preservations, 24);
    assert_eq!(coverage.joined_preparations, 24);
    assert_eq!(coverage.namespace_pins, 24);
    assert_eq!(coverage.output_request_preparations, 24);
    assert_eq!(coverage.dependent_issuances, 24);
    assert_eq!(coverage.private_output_handoffs, 24);
    assert_eq!(coverage.recoveries, 48);
    assert_eq!(coverage.produced_demands, 24);
    assert_eq!(coverage.recovered_input_refusals, 48);
    assert_eq!(coverage.unselected_root_ambiguities, 8);
    assert!(coverage.successful_demands > 0 && coverage.ambiguous_demands > 0);
    tracing::debug!(?coverage, "native transport deterministic support");
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
    tracing::debug!(configured_cases = cases, coverage = ?coverage.borrow(), "native transport campaign");
    result.unwrap();
}

#[test]
#[ignore = "requires the declared rooted resident compiler"]
fn actual_source_handoff_preserves_sealed_native_dependency() {
    use crate::artifact_inventory::ArtifactPayload;
    use crate::artifacts::test_support::{
        prepare_source_publication_entry_fixture, ExactSourceFixture,
    };

    // One actual sealed publication and one actual inspection establish source
    // authority. Generated graph histories above never construct source receipts.
    let publication_fixture = prepare_source_publication_entry_fixture().unwrap();
    let root = &publication_fixture.root;
    let helper = &publication_fixture.helper;
    let publication = publication_fixture
        .entry
        .products()
        .published_source_original_selection("native-source-handoff", &root.descriptor.owner)
        .unwrap();
    let source = "module Consumer where\nimport qualified SourceOriginalSupport\nprobe :: Int\nprobe = SourceOriginalSupport.second\n__tidepool_inspect_0 = probe\n";
    let execution = ExactSourceFixture::inspect(
        Some(Arc::new(ExactCompileContext::new(Arc::clone(
            publication.context(),
        )))),
        source,
        "probe",
    )
    .unwrap();
    assert_eq!(execution.admissions().len(), 1);
    assert_eq!(execution.receipt_paths().len(), 1);
    let admission = &execution.admissions()[0];
    admission.validate_request(execution.request()).unwrap();
    assert_eq!(admission.witness().source_path(), execution.source_path());
    assert!(admission
        .compiler_dependencies()
        .adjacency()
        .values()
        .any(|imports| imports.contains(&root.descriptor.owner)));
    let root_key = NativeGroupKey {
        artifact: root.descriptor.id,
        original_ordinal: publication_fixture.root_ordinals[1],
    };
    let ArtifactPayload::Original(root_product) = &root.payload else {
        unreachable!()
    };
    let ArtifactPayload::Original(helper_product) = &helper.payload else {
        unreachable!()
    };
    let helper_ordinal = helper
        .native_group_ordinals
        .iter()
        .copied()
        .find(|ordinal| {
            original_native_group_definitions(helper_product, *ordinal)
                .unwrap()
                .export_contracts()
                .any(|contract| {
                    let contract = contract.unwrap();
                    contract.identity.unit == helper.descriptor.owner.unit
                        && contract.identity.module == helper.descriptor.owner.module
                        && contract.identity.occurrence == "second"
                })
        })
        .unwrap();
    // The authored body independently names Helper.second; neither recovery nor
    // target admission supplies the expected dependency topology.
    assert!(
        original_native_group_definitions(root_product, root_key.original_ordinal)
            .unwrap()
            .globals()
            .iter()
            .any(
                |global| global.identity.unit == helper.descriptor.owner.unit
                    && global.identity.module == helper.descriptor.owner.module
                    && global.identity.occurrence == "second"
            )
    );
    let inputs = execution.request().compiler_inputs();
    let availability = inputs.native_availability(root_key.artifact).unwrap();
    let dependency = availability
        .dependency_support(root_key, root_product.owner())
        .unwrap();
    let root_groups = dependency.groups().collect::<BTreeSet<_>>();
    assert_eq!(root_groups.len(), 1);
    let root_group = *root_groups.first().unwrap();
    let offer = dependency.offer_for_group(root_group).unwrap().clone();
    let issued = offer.issued_group_carriers().unwrap();
    let helper_groups = issued
        .keys()
        .copied()
        .filter(|group| {
            group.binding.artifact() == helper.descriptor.id
                && group.original_ordinal == helper_ordinal
        })
        .collect::<Vec<_>>();
    let [helper_group] = helper_groups.as_slice() else {
        panic!("one actual Helper.second issuance")
    };
    let expected = BTreeMap::from([
        (root_group, issued[&root_group]),
        (*helper_group, issued[helper_group]),
    ]);
    let expected_edge = (
        ArtifactBindingNode::Group(root_group),
        ArtifactBindingNode::Group(*helper_group),
        ArtifactDependency::NativeGroup {
            dependent_ordinal: root_key.original_ordinal,
            required_ordinal: helper_ordinal,
        },
    );
    assert!(offer
        .custody_snapshot()
        .binding_dependencies()
        .unwrap()
        .contains(&expected_edge));
    // The inspected input owns its existing executable graph. Selecting its
    // Original artifact roots would select their bodies, not clear demand.
    let carried_support = inputs.compiler_source_support();
    let input_groups = carried_support.artifacts().selected_native_groups();
    let group_roots = |support: &ArtifactView| {
        support
            .capture_graph_selection()
            .roots
            .into_iter()
            .filter(|root| matches!(root, ArtifactBindingNode::Group(_)))
            .collect::<BTreeSet<_>>()
    };
    let input_group_roots = group_roots(carried_support.artifacts());
    let private = OriginalCompilerInputs::from_issued_support(carried_support);
    let changed_source = private
        .for_program_continuation(
            carried_support.artifacts(),
            execution.admissions(),
            &(source.to_owned() + "-- changed\n"),
        )
        .err()
        .expect("a successful receipt does not authenticate different source bytes");
    assert!(
        matches!(&changed_source, CompileError::ExtractFailed(detail)
            if detail == "program support lacks one authenticated generated source owner"),
        "changed source must fail source authentication before graph validation: {changed_source:?}"
    );
    let graph = carried_support.artifacts().capture_graph_selection();
    let edges = carried_support.artifacts().binding_dependencies();
    let (graph, edges): (ArtifactGraphSelection, Vec<_>) =
        serde_json::from_slice(&serde_json::to_vec(&(graph, edges)).unwrap()).unwrap();
    // Recovery restores exact graph facts while this history retains the live
    // selected rows and native offer; it does not reissue authority from data.
    let fresh = ArtifactInventory::default();
    let recovered = fresh
        .recover_selection(
            &fresh.empty_view(),
            carried_support.artifacts().entries(),
            &graph,
            &edges,
        )
        .unwrap();
    assert_eq!(recovered.capture_graph_selection(), graph);
    assert_eq!(recovered.binding_dependencies(), edges);
    let scratch = tempfile::tempdir().unwrap();
    for (index, support) in [carried_support.artifacts(), &recovered]
        .into_iter()
        .enumerate()
    {
        let carried = private
            .for_program_continuation(support, execution.admissions(), execution.source())
            .unwrap();
        assert!(carried
            .compiler_source_support()
            .same_selected_acquisitions(carried_support));
        assert!(carried
            .native_offers()
            .iter()
            .any(|candidate| candidate.same_issuance(&offer)));
        assert_eq!(
            carried.issued_artifacts().selected_native_groups(),
            input_groups
        );
        assert_eq!(group_roots(carried.issued_artifacts()), input_group_roots);
        let public = Arc::new(
            ExactDeclarationContext::new(&[], &[], vec![])
                .unwrap()
                .admit_producer(
                    CanonicalProducerIdentity::from_producer_bytes(
                        &publication_fixture.producer_bytes,
                    )
                    .sha256(),
                )
                .unwrap(),
        );
        let next = ExactCompileContext::new(public)
            .prepare_compilation_with_private_input(
                &scratch.path().join(format!("next-{index}")),
                &publication_fixture.producer_bytes,
                None,
                Some(carried),
            )
            .unwrap();
        assert_eq!(
            next.compiler_inputs().artifacts().selected_native_groups(),
            input_groups
        );
        assert_eq!(
            group_roots(next.compiler_inputs().artifacts()),
            input_group_roots
        );
        let mut validation = PackageInterfaceValidation::default();
        let selection = CertifiedSourceSelection::from_compiler_inputs(
            next.compiler_inputs(),
            &validation.inventory,
        )
        .unwrap();
        let view = next.compiler_inputs().artifacts();
        let stage = view
            .inventory()
            .bind_segment(view, vec![], &selection, &mut validation)
            .unwrap();
        let observations = source_target_observations(&[(root_product, root_key.original_ordinal)]);
        let prepared = prepared_target_fixture(&observations);
        let observed =
            AuthenticatedTargetImports::issue(&prepared, &observations, &validation.inventory)
                .unwrap();
        let admitted = view
            .inventory()
            .select_native_target(
                stage.captured_stage().physical_cohort(),
                observed,
                None,
                Some(view),
                &[],
                &BTreeMap::new(),
                &mut validation,
            )
            .unwrap();
        assert_eq!(
            admitted.resolve().unwrap().groups(),
            &expected.keys().copied().collect()
        );
        assert!(admitted
            .artifacts()
            .binding_dependencies()
            .contains(&expected_edge));
        assert!(next
            .compiler_inputs()
            .compiler_source_support()
            .native_offers()
            .iter()
            .any(|candidate| candidate.same_issuance(&offer)));
    }
}
