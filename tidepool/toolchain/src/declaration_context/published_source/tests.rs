use super::*;
use crate::artifact_inventory::NativeGroupKey;

pub(super) struct CompiledPublicationFixture {
    pub(super) fixture: crate::artifacts::test_support::SourcePublicationEntryFixture,
    pub(super) publication: Arc<PublishedSourceOriginalSelection>,
}

impl std::ops::Deref for CompiledPublicationFixture {
    type Target = crate::artifacts::test_support::SourcePublicationEntryFixture;
    fn deref(&self) -> &Self::Target {
        &self.fixture
    }
}

pub(super) fn compiled_publication_fixture() -> CompiledPublicationFixture {
    let fixture =
        crate::artifacts::test_support::prepare_source_publication_entry_fixture().unwrap();
    let publication = fixture
        .entry
        .products()
        .published_source_original_selection("history-revision", &fixture.root.descriptor.owner)
        .unwrap();
    CompiledPublicationFixture {
        fixture,
        publication,
    }
}

#[test]
#[ignore = "requires the declared rooted compiler service"]
fn published_source_composition_preserves_sealed_selection_across_repeated_imports() {
    let fixture = compiled_publication_fixture();
    let publication = &fixture.publication;
    let source = publication.context();
    assert!(
        !source
            .compiler_input_projection()
            .snapshot_roles()
            .contains_key(&identity("main", "PublicationTarget")),
        "the public root does not retain the generated executable wrapper as a source input"
    );
    let before = source.semantic_sha256();
    let graph = source.artifact_view().capture_graph_selection();
    let role =
        source.compiler_input_projection().snapshot_roles()[&fixture.root.descriptor.owner].clone();
    assert!(role.is_published_source_original());
    check_publication_foreign_native_scopes(&fixture);
    for start_empty in [false, true] {
        let mut receiver = if start_empty {
            ExactDeclarationContext::new(&[], &[], vec![]).unwrap()
        } else {
            source.as_ref().clone()
        };
        for repetition in 0..3 {
            let next = receiver
                .clone()
                .with_published_source_originals(publication)
                .unwrap();
            assert_eq!(
                next.compiler_input_projection().snapshot_roles()[&fixture.root.descriptor.owner],
                role
            );
            assert_eq!(next.artifact_view().capture_graph_selection(), graph);
            if repetition > 0 {
                assert_eq!(next.semantic_sha256(), receiver.semantic_sha256());
            }
            receiver = next;
        }
    }
    let alternative = fixture
        .entry
        .products()
        .published_source_original_selection("other-revision", &fixture.root.descriptor.owner)
        .unwrap();
    assert!(
        source
            .as_ref()
            .clone()
            .with_published_source_originals(&alternative)
            .is_err(),
        "a distinct sealed publication policy cannot replace the selected policy"
    );
    assert_eq!(source.semantic_sha256(), before);
    assert_eq!(source.artifact_view().capture_graph_selection(), graph);
}

/// Equal payload custody is not the publication's exact native issuer.
fn check_publication_foreign_native_scopes(fixture: &CompiledPublicationFixture) {
    use crate::artifact_inventory::{ArtifactBindingNode, ExactArtifactSelection};
    use crate::certified_products::PendingImportOwner;
    use fvm_ipld_blockstore::Blockstore;

    let publication = &fixture.publication;
    let source = publication.context();
    let root = &fixture.root;
    let helper = &fixture.helper;
    assert!(
        source.artifact_view().selected_native_groups().is_empty(),
        "the SOURCE projection retains native custody without target demand"
    );
    let target = fixture.entry.products().native_source_products().unwrap();
    let target_groups = target
        .groups()
        .map(|row| row.binding())
        .collect::<BTreeSet<_>>();
    let genuine = source.artifact_view().capture_graph_selection();
    let genuine_groups = genuine
        .native_groups
        .iter()
        .copied()
        .filter(|group| group.binding.artifact() == root.descriptor.id)
        .collect::<BTreeSet<_>>();
    assert!(!genuine_groups.is_empty());
    assert!(genuine_groups.is_subset(&target_groups));
    let demand = target_groups
        .iter()
        .map(|group| NativeGroupKey {
            artifact: group.binding.artifact(),
            original_ordinal: group.original_ordinal,
        })
        .collect();
    let producer = root.descriptor.producer_sha256;
    let mut entries = source.artifact_view().entries();
    entries.push(Arc::new(ArtifactEntry::canonical(
        crate::certified_products::fixture_module_interface(
            producer,
            "fixture",
            "IndependentNativeDataScope",
            BTreeMap::new(),
        ),
    )));
    let independent = ArtifactInventory::default();
    // This issues native DATA from already certified payloads. It has no
    // completed output or SOURCE publication authority. Match the actual
    // issuer's group demand independently of its broader byte custody.
    let foreign_view = independent
        .admit_recovery_selection(&independent.empty_view(), entries, &demand)
        .unwrap();
    assert_eq!(
        foreign_view.selected_native_groups(),
        demand,
        "independent DATA admission preserves the completed target's whole selected closure"
    );
    let foreign_view = foreign_view.select_roots(vec![root.descriptor.id]).unwrap();
    let foreign = foreign_view.capture_graph_selection();
    let foreign_groups = foreign
        .native_groups
        .iter()
        .copied()
        .filter(|group| group.binding.artifact() == root.descriptor.id)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        genuine_groups
            .iter()
            .map(|group| group.original_ordinal)
            .collect::<BTreeSet<_>>(),
        foreign_groups
            .iter()
            .map(|group| group.original_ordinal)
            .collect()
    );
    for group in &genuine_groups {
        let other = foreign_groups
            .iter()
            .find(|other| other.original_ordinal == group.original_ordinal)
            .unwrap();
        assert_ne!(
            group.binding, other.binding,
            "equal native payload and ordinal have distinct issuing scopes"
        );
    }
    let foreign_edges = foreign_view.binding_dependencies();
    let corresponding = |group: crate::artifact_inventory::NativeGroupBinding| {
        foreign
            .native_groups
            .iter()
            .copied()
            .find(|candidate| {
                candidate.binding.artifact() == group.binding.artifact()
                    && candidate.original_ordinal == group.original_ordinal
            })
            .expect("the independently issued graph closes the actual native demand")
    };
    let mut child_edges = 0;
    for (from, to, dependency) in source.artifact_view().binding_dependencies() {
        let (ArtifactBindingNode::Group(from), ArtifactBindingNode::Group(to)) = (from, to) else {
            continue;
        };
        if genuine_groups.contains(&from)
            && matches!(
                dependency,
                crate::artifact_inventory::ArtifactDependency::NativeGroup { .. }
            )
        {
            child_edges += 1;
            assert!(
                target_groups.contains(&to),
                "the completed target selected the real native child"
            );
            assert!(
                foreign_edges.contains(&(
                    ArtifactBindingNode::Group(corresponding(from)),
                    ArtifactBindingNode::Group(corresponding(to)),
                    dependency,
                )),
                "the foreign DATA issuer retains each real selected child edge"
            );
        }
    }
    assert!(
        child_edges > 0,
        "the actual publication exercises a native child dependency"
    );
    let ArtifactPayload::Original(product) = &root.payload else {
        unreachable!()
    };
    let ordinal = fixture.root_ordinals[1];
    let binder = crate::certified_products::original_native_group_definitions(product, ordinal)
        .unwrap()
        .export_contracts()
        .map(Result::unwrap)
        .find(|contract| contract.identity.occurrence == "second")
        .expect("the actual selected group exports second")
        .identity
        .clone();
    let imports = [PendingImportOwner::Source {
        owner: product.owner().clone(),
        original_ordinal: ordinal,
        binder,
    }];
    let support = source
        .compiler_source_support()
        .retain_custody(&foreign_view)
        .unwrap();
    let held = source
        .as_ref()
        .clone()
        .rebuild(|facts| facts.compiler_support = support)
        .finish()
        .unwrap();
    assert_eq!(held.compiler_input_roles(), source.compiler_input_roles());
    assert_eq!(
        held.artifact_view().capture_graph_selection().roots,
        genuine.roots
    );
    assert!(
        foreign_groups
            .iter()
            .all(|group| held.artifact_view().contains_acquired_native_group(*group)),
        "foreign proofs are held as DATA custody, not absent from acquisition"
    );
    let assert_target_selection = |context: &ExactDeclarationContext| {
        assert_eq!(
            context
                .published_source_original_selections_for_target(&genuine_groups, &imports)
                .unwrap()
                .len(),
            1
        );
        assert!(
            context
                .published_source_original_selections_for_target(&foreign_groups, &imports)
                .unwrap()
                .is_empty(),
            "matching source spelling cannot replace the published native issuer"
        );
        let mixed = genuine_groups.union(&foreign_groups).copied().collect();
        assert!(
            context
                .published_source_original_selections_for_target(&mixed, &imports)
                .unwrap()
                .is_empty(),
            "one genuine group cannot hide a foreign group of the same payload"
        );
    };
    assert_target_selection(&held);

    // Attach DATA before the real publication narrowing and endorsement owners
    // run. The immutable birth may retain those groups without acquiring their
    // native permission from the actual completed compiler output.
    let completed = fixture
        .entry
        .products()
        .original_execution_context()
        .unwrap();
    let support = completed
        .compiler_source_support()
        .retain_custody(&foreign_view)
        .unwrap();
    let prebirth = completed
        .as_ref()
        .clone()
        .rebuild(|facts| facts.compiler_support = support)
        .finish()
        .unwrap()
        .published_context_for_root(&root.descriptor.owner, root.descriptor.id)
        .unwrap();
    assert!(foreign_groups.iter().all(|group| prebirth
        .artifact_view()
        .contains_acquired_native_group(*group)));
    let CompilerInputRole::PublishedSourceOriginal {
        original_input_identity,
        ..
    } = &source.compiler_input_projection().snapshot_roles()[&root.descriptor.owner]
    else {
        unreachable!()
    };
    let endorsement = prebirth
        .endorse_published_original(
            &root.descriptor.owner,
            "prebirth-data-revision",
            original_input_identity,
        )
        .unwrap();
    let support = prebirth
        .compiler_source_support()
        .publish_original(&endorsement)
        .unwrap();
    let prebirth = prebirth
        .rebuild(|facts| facts.compiler_support = support)
        .finish()
        .unwrap();
    let birth = prebirth
        .compiler_source_support()
        .publication_birth_support(&root.descriptor.owner)
        .unwrap();
    assert!(foreign_groups
        .iter()
        .all(|group| birth.artifacts().contains_acquired_native_group(*group)));
    assert!(
        birth.native_availability().same_recorded_facts(
            source
                .compiler_source_support()
                .publication_birth_support(&root.descriptor.owner)
                .unwrap()
                .native_availability()
        ),
        "adding DATA before birth preserves the recorded issued permission"
    );
    assert_target_selection(&prebirth);

    let (_scratch, inventory, _) =
        super::history_properties::restored_graph_roles(&held, &held.compiler_input_roles())
            .unwrap();
    let store = fvm_ipld_blockstore::MemoryBlockstore::default();
    let mut writer = crate::exact_context::Materializer::new(&store);
    let written = writer.materialize_context(None, &held.snapshot()).unwrap();
    for (cid, bytes) in writer.finish().unwrap().into_blocks() {
        store.put_keyed(&cid, &bytes).unwrap();
    }
    let decoded = crate::exact_context::hydrate_context(&store, written.root()).unwrap();
    let recovered = inventory
        .context_for_expected_snapshot(written.root(), &decoded)
        .unwrap();
    assert_eq!(
        recovered.semantic_sha256(),
        held.semantic_sha256(),
        "unchanged Context9 cold positive"
    );
    assert!(foreign_groups.iter().all(|group| recovered
        .artifact_view()
        .contains_acquired_native_group(*group)));
    assert_target_selection(&recovered);

    let published_role =
        held.compiler_input_projection().snapshot_roles()[&root.descriptor.owner].clone();
    let CompilerInputRole::PublishedSourceOriginal { selection, .. } = &published_role else {
        unreachable!()
    };
    let birth = selection.graph_selection();
    let child =
        held.compiler_input_projection().snapshot_roles()[&helper.descriptor.owner].interface();
    let mut substitutions = Vec::new();
    for (name, artifact) in [("carrier", root.descriptor.id), ("child", child)] {
        let replacement = *foreign
            .namespace
            .iter()
            .find(|binding| binding.artifact == artifact)
            .unwrap();
        let mut graph = birth.clone();
        let current = graph
            .namespace
            .iter_mut()
            .find(|binding| binding.artifact == artifact)
            .unwrap();
        assert_ne!(
            *current, replacement,
            "{name} scope substitution must change the binding"
        );
        *current = replacement;
        graph.namespace.sort();
        substitutions.push((name, graph));
    }
    let mut graph = birth.clone();
    let parent = graph
        .native_groups
        .iter_mut()
        .find(|group| {
            group.binding.artifact() == root.descriptor.id && group.original_ordinal == ordinal
        })
        .unwrap();
    let previous = *parent;
    let replacement = *foreign_groups
        .iter()
        .find(|group| group.original_ordinal == ordinal)
        .unwrap();
    assert_ne!(previous, replacement);
    *parent = replacement;
    graph.native_groups.sort();
    for node in graph.roots.iter_mut().chain(&mut graph.custody_roots) {
        if *node == ArtifactBindingNode::Group(previous) {
            *node = ArtifactBindingNode::Group(replacement);
        }
    }
    graph.roots.sort();
    graph.custody_roots.sort();
    substitutions.push(("parent", graph));
    for (name, mut graph) in substitutions {
        // Hold both real native graphs in the mutated selection. The selected
        // slot changes issuer; it must not fail merely from an absent binding
        // or a missing dependency node in the recorded DATA graph.
        graph.selections.extend(foreign.selections.iter().cloned());
        graph.selections.sort_by_key(|scope| scope.selection);
        graph.selections.dedup_by_key(|scope| scope.selection);
        graph.bindings.extend(foreign.bindings.iter().copied());
        graph.bindings.sort();
        graph.bindings.dedup();
        graph
            .native_groups
            .extend(foreign.native_groups.iter().copied());
        graph.native_groups.sort();
        graph.native_groups.dedup();
        graph.custody_roots.extend(foreign.roots.iter().copied());
        graph
            .custody_roots
            .extend(foreign.custody_roots.iter().copied());
        graph.custody_roots.sort();
        graph.custody_roots.dedup();
        let foreign_selection = ExactArtifactSelection::from_recovered_graph_selection(graph);
        held.artifact_view().select_issued(vec![root.descriptor.id], &foreign_selection)
            .unwrap_or_else(|error| panic!("{name} foreign DATA selection must authenticate before SOURCE refusal: {error:?}"));
        let mut snapshot = held.snapshot();
        snapshot.native_fact_owner = None;
        let crate::exact_context::RecordedCompilerRow::Original {
            provenance: crate::exact_context::RecordedOriginalProvenance::Published(birth),
            ..
        } = snapshot.roles.get_mut(&root.descriptor.owner).unwrap()
        else {
            unreachable!()
        };
        birth.selection = foreign_selection;
        let mut writer = crate::exact_context::Materializer::new(&store);
        let mutated = writer.materialize_context(None, &snapshot).unwrap();
        for (cid, bytes) in writer.finish().unwrap().into_blocks() {
            store.put_keyed(&cid, &bytes).unwrap();
        }
        let decoded = crate::exact_context::hydrate_context(&store, mutated.root()).unwrap();
        let error = inventory
            .context_for_expected_snapshot(mutated.root(), &decoded)
            .expect_err(
                "a retained foreign scope cannot replace the publication's issued selection",
            );
        eprintln!("cold publication {name} scope substitution refused: {error:?}");
    }
}

#[test]
fn sparse_partial_support_cannot_promote_unissued_later_group() {
    retained_composition_histories(true, false);
}

#[test]
fn retained_support_preserves_selected_issuer_over_historical_custody() {
    retained_composition_histories(false, false);
}

#[test]
fn interface_adapter_preserves_issued_canonical_scope_and_refuses_unselected_attachment() {
    retained_composition_histories(false, true);
}

fn check_interface_adapter(
    issued: &ExactDeclarationContext,
    selected_issuer: &ArtifactView,
    original: ArtifactId,
    child: ArtifactId,
) {
    use crate::artifact_inventory::{
        ArtifactBindingNode, ArtifactDependency, ExactArtifactSelection,
    };
    let root_owner = identity("fixture", "Root");
    let graph = issued.artifact_view().capture_graph_selection();
    let original_binding = *graph
        .namespace
        .iter()
        .find(|binding| binding.artifact == original)
        .unwrap();
    let canonical = issued.compiler_input_projection().snapshot_roles()[&root_owner].interface();
    let canonical_binding = issued
        .artifact_view()
        .binding_dependencies()
        .into_iter()
        .find_map(|(from, to, dependency)| match (from, to, dependency) {
            (
                ArtifactBindingNode::Artifact(from),
                ArtifactBindingNode::Artifact(to),
                ArtifactDependency::Interface,
            ) if from == original_binding && to.artifact == canonical => Some(to),
            _ => None,
        })
        .expect("the selected original owns its exact canonical attachment");
    let types = issued
        .compiler_source_support()
        .selected_interfaces()
        .unwrap()
        .support_for_owners(vec![root_owner.clone()])
        .unwrap();
    let interfaces = types.artifacts();
    assert!(interfaces
        .capture_graph_selection()
        .namespace
        .contains(&canonical_binding));
    assert!(interfaces.selected_native_groups().is_empty());
    let expected_edges = interfaces.binding_dependencies();
    let empty = ExactDeclarationContext::new(&[], &[], vec![]).unwrap();
    let extended = empty
        .clone()
        .extend_issued_compiler_support(&types)
        .expect("the checked-cell interface projection preserves its issuing scope");
    let roles = extended.compiler_input_projection().snapshot_roles();
    assert_eq!(roles[&root_owner].interface(), canonical);
    assert_eq!(roles[&identity("fixture", "Child")].interface(), child);
    assert_eq!(roles.len(), 2);
    assert!(roles.values().all(|role| role.original().is_none()));
    assert!(extended
        .artifact_view()
        .capture_graph_selection()
        .namespace
        .contains(&canonical_binding));
    assert_eq!(
        extended.artifact_view().binding_dependencies(),
        expected_edges
    );
    assert!(extended.artifact_view().selected_native_groups().is_empty());
    assert!(extended.lexical_graph().is_empty());

    // Recovery may transport a narrower namespace without changing any
    // authenticated binding, edge, root or custody fact. An attachment in
    // custody does not make it a selected interface input.
    assert_eq!(
        selected_issuer
            .capture_graph_selection()
            .bindings
            .iter()
            .filter(|binding| binding.artifact == canonical)
            .count(),
        1,
        "the clean issuer negative cannot refuse from canonical binding ambiguity"
    );
    for source in [selected_issuer, issued.artifact_view()] {
        let graph = source.capture_graph_selection();
        let mut original_only = graph.clone();
        original_only
            .namespace
            .retain(|binding| *binding == original_binding);
        let original_only = source
            .select_issued(
                vec![original],
                &ExactArtifactSelection::from_recovered_graph_selection(original_only),
            )
            .expect("the owner authenticates the unchanged issued graph");
        let narrowed = original_only.capture_graph_selection();
        assert_eq!(narrowed.namespace, vec![original_binding]);
        assert_eq!(narrowed.bindings, graph.bindings);
        assert_eq!(narrowed.roots, graph.roots);
        assert_eq!(narrowed.custody_roots, graph.custody_roots);
        assert_eq!(narrowed.native_groups, graph.native_groups);
        assert_eq!(
            original_only.binding_dependencies(),
            source.binding_dependencies()
        );
        assert!(narrowed.bindings.contains(&canonical_binding));
        assert!(
            matches!(crate::declaration_context::recover_fixture_compiler_support(
                types.projection().clone(), original_only, [],
            ), Err(CompileError::ExtractFailed(detail))
                if detail == "exact declaration context: bound recovery namespace differs from recorded compiler roles"),
            "recorded type roles must refuse a namespace that omits their selected canonical attachment"
        );
    }
}

fn assert_sparse_partial_transport(context: &ExactDeclarationContext, original: ArtifactId) {
    let offers = context.compiler_source_support().native_offers();
    let partials = offers
        .iter()
        .filter(|offer| offer.original() == original)
        .filter_map(|offer| match offer.snapshot_facts().body {
            crate::exact_context::NativeOfferSnapshotBody::Partial(selection) => Some(selection),
            crate::exact_context::NativeOfferSnapshotBody::Full(_) => {
                panic!("sparse issuance became Full")
            }
            crate::exact_context::NativeOfferSnapshotBody::Empty(_) => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(partials.len(), 1);
    assert_eq!(
        partials[0]
            .graph_selection()
            .native_groups
            .iter()
            .filter(|group| group.binding.artifact() == original)
            .map(|group| group.original_ordinal)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([7])
    );
    assert!(context
        .artifact_view()
        .capture_graph_selection()
        .native_groups
        .iter()
        .all(|group| group.binding.artifact() != original || group.original_ordinal == 7));
    let transport = NativeInputSelection::issue_checked(
        context.compiler_source_support().compiler_roles(),
        &context.compiler_metadata_snapshot().unwrap(),
        context.artifact_view(),
        offers,
    )
    .unwrap();
    let availability = &transport.availability.0[&original];
    assert!(availability.permits(7));
    assert!(!availability.permits(11));
    assert!(
        matches!(&availability.mode, NativeTransportMode::IssuedGroups)
            && availability.issued_ordinals() == BTreeSet::from([7])
    );
}

fn retained_composition_histories(sparse: bool, interface_adapter_only: bool) {
    use crate::artifact_inventory::ArtifactBindingNode;
    let producer = [2; 32];
    let child_interface = crate::certified_products::fixture_module_interface(
        producer,
        "fixture",
        "Child",
        BTreeMap::new(),
    );
    let child_owner = identity("fixture", "Child");
    let root_owner = identity("fixture", "Root");
    let target = Arc::new(ArtifactEntry::canonical(
        crate::certified_products::fixture_module_interface(
            producer,
            "fixture",
            "Target",
            BTreeMap::new(),
        ),
    ));
    let ordinals = if sparse { [7, 11] } else { [3, 29] };
    let root = |version| {
        let product = crate::certified_products::fixture_finalized_product_with_requirements(
            crate::certified_products::tests::original_groups_fixture_with_interface(
                "Root",
                ordinals
                    .into_iter()
                    .map(|ordinal| (ordinal, vec![]))
                    .collect(),
                version,
                &BTreeMap::new(),
                b"published root interface".to_vec(),
            ),
            producer,
            Some(BTreeMap::from([(
                ("fixture".into(), "Child".into()),
                child_interface.interface_sha256(),
            )])),
        );
        let product = crate::certified_products::tests::recovered_witness_fixtures(&[product])
            .remove(0)
            .product;
        Arc::new(ArtifactEntry::original(producer, product).unwrap())
    };
    let native = root(1);
    let lexical = vec![
        ExactLexicalNode {
            owner: target.descriptor.owner.clone(),
            imports: vec![root_owner.clone()],
        },
        ExactLexicalNode {
            owner: root_owner.clone(),
            imports: vec![child_owner.clone()],
        },
        ExactLexicalNode {
            owner: child_owner.clone(),
            imports: vec![],
        },
    ];
    let mut completed = 0;
    for changed_child in [false, true] {
        let children = [
            Arc::new(ArtifactEntry::canonical(child_interface.clone())),
            Arc::new(ArtifactEntry::canonical(if changed_child {
                crate::certified_products::fixture_module_core(
                    &child_interface,
                    b"next Child Core".to_vec(),
                )
            } else {
                child_interface.clone()
            })),
        ];
        assert_eq!(
            children[0].descriptor.interface_sha256,
            children[1].descriptor.interface_sha256
        );
        assert_eq!(
            children[0].descriptor.id != children[1].descriptor.id,
            changed_child
        );
        for reverse in [false, true] {
            let views = children
                .iter()
                .enumerate()
                .map(|(index, child)| {
                    let inventory = ArtifactInventory::default();
                    let sibling = Arc::new(ArtifactEntry::canonical(
                        crate::certified_products::fixture_module_interface(
                            producer,
                            "fixture",
                            ["Left", "Right"][index],
                            BTreeMap::new(),
                        ),
                    ));
                    let mut entries = vec![native.clone(), child.clone(), target.clone(), sibling];
                    if reverse {
                        entries.reverse();
                    }
                    if sparse {
                        inventory
                            .admit_recovery_selection(
                                &inventory.empty_view(),
                                entries,
                                &BTreeSet::from([NativeGroupKey {
                                    artifact: native.descriptor.id,
                                    original_ordinal: 7,
                                }]),
                            )
                            .unwrap()
                    } else {
                        inventory
                            .admit_shared(&inventory.empty_view(), entries)
                            .unwrap()
                    }
                })
                .collect::<Vec<_>>();
            let binding = |index: usize| {
                *views[index]
                    .capture_graph_selection()
                    .namespace
                    .iter()
                    .find(|binding| binding.artifact == native.descriptor.id)
                    .unwrap()
            };
            assert_ne!(
                binding(0),
                binding(1),
                "equal native payloads retain distinct issuers"
            );
            let edges = views
                .iter()
                .flat_map(|view| view.binding_dependencies())
                .filter(|(from, to, _)| {
                    matches!(
                        (from, to),
                        (
                            ArtifactBindingNode::Artifact(_),
                            ArtifactBindingNode::Artifact(_)
                        )
                    )
                })
                .collect::<BTreeSet<_>>();
            for selected in 0..2 {
                let projection = CompilerInputProjection::from_issued_entries(&[
                    native.clone(),
                    children[selected].clone(),
                    target.clone(),
                ])
                .unwrap();
                let historical = issue_fixture_compiler_support(
                    CompilerInputProjection::from_issued_entries(&[
                        native.clone(),
                        children[1 - selected].clone(),
                        target.clone(),
                    ])
                    .unwrap(),
                    views[1 - selected].clone(),
                )
                .unwrap();
                let old = historical
                    .issue_native_offer(&native.descriptor.owner, None)
                    .unwrap();
                let support = crate::declaration_context::recover_fixture_compiler_support(
                    projection.clone(),
                    views[selected].clone(),
                    [old.clone(), old],
                )
                .unwrap();
                let issued = ExactDeclarationContext::retained_execution_fixture(
                    producer,
                    &views[selected],
                    projection,
                    lexical.clone(),
                    target.descriptor.owner.clone(),
                    &[
                        target.descriptor.owner.clone(),
                        root_owner.clone(),
                        child_owner.clone(),
                    ],
                )
                .unwrap()
                .extend_issued_compiler_support(&support)
                .unwrap();
                if interface_adapter_only {
                    check_interface_adapter(
                        &issued,
                        &views[selected],
                        native.descriptor.id,
                        children[selected].descriptor.id,
                    );
                    completed += 1;
                    continue;
                }
                // These authenticated graph and support facts carry no completion
                // claim. Partial transport cannot issue a new SOURCE publication.
                if sparse {
                    assert_sparse_partial_transport(&issued, native.descriptor.id);
                }
                assert_eq!(
                    issued.compiler_input_projection().snapshot_roles()[&child_owner].interface(),
                    children[selected].descriptor.id,
                );
                assert!(edges
                    .iter()
                    .all(|edge| issued.artifact_view().binding_dependencies().contains(edge)));
                let expected_graph = issued.artifact_view().capture_graph_selection();
                let (_, _, restored) = super::history_properties::restored_graph_roles(
                    &issued,
                    &issued.compiler_input_roles(),
                )
                .unwrap();
                let support = restored
                    .compiler_source_support()
                    .restore_snapshot_evidence(
                        &issued.snapshot(),
                        &BTreeMap::new(),
                        &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
                    )
                    .unwrap();
                let restored = restored
                    .rebuild(|facts| facts.compiler_support = support)
                    .finish()
                    .unwrap();
                assert_eq!(
                    restored.artifact_view().capture_graph_selection(),
                    expected_graph
                );
                assert_eq!(
                    restored.compiler_input_roles(),
                    issued.compiler_input_roles()
                );
                if sparse {
                    assert_sparse_partial_transport(&restored, native.descriptor.id);
                }
                let different_body = root(2);
                assert_eq!(
                    different_body.descriptor.interface_sha256,
                    native.descriptor.interface_sha256
                );
                assert_ne!(different_body.descriptor.id, native.descriptor.id);
                let ArtifactPayload::Original(product) = &different_body.payload else {
                    unreachable!()
                };
                let crossed = restored
                    .compiler_source_support()
                    .checked_native_input_decision(
                        different_body.descriptor.id,
                        ArtifactEntry::canonical_artifact_id(product.module_interface()),
                        product.owner(),
                    )
                    .map(|_| ());
                assert!(
                    matches!(crossed,
                    Err(crate::certified_products::InputDecisionRefusal::ArtifactIdentityMismatch(ref mismatch))
                        if mismatch.stage == crate::certified_products::ArtifactIdentityStage::RequestedIdentityDiffers
                            && mismatch.expected_original == Some(different_body.descriptor.id)
                            && mismatch.observed_original == Some(native.descriptor.id)),
                    "an equal canonical interface cannot substitute another body: {crossed:?}"
                );
                completed += 1;
            }
        }
    }
    assert_eq!(completed, 8);
}

#[test]
#[ignore = "requires the declared rooted compiler service"]
fn authored_certifier_preserves_published_policy_with_complete_demand() {
    let fixture = compiled_publication_fixture();
    let producer = crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(
        &fixture.producer_bytes,
    )
    .sha256();
    let root = &fixture.root;
    let published = fixture.publication.context();
    let before = published.artifact_view().capture_graph_selection();
    let before_sha = published.semantic_sha256();
    let complete = fixture
        .entry
        .products()
        .original_execution_context()
        .unwrap();
    let projection = complete.compiler_input_projection().clone();
    let selection = crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
        &projection,
        &complete.artifact_view().metadata_snapshot(),
        &tidepool_repr::execution_schema::InventoryOperation::new(Default::default()),
    )
    .unwrap();
    let originals = selection
        .selected_original_closure(complete.artifact_view())
        .unwrap();
    let original_graph = originals.native_closure().capture_graph_selection();
    let role =
        published.compiler_input_projection().snapshot_roles()[&root.descriptor.owner].clone();
    let policy =
        crate::artifact_inventory::NativeOriginalRequirement::from_role(role.clone()).unwrap();
    assert!(policy
        .selection
        .graph_selection()
        .native_groups
        .iter()
        .any(|group| group.binding.artifact() == root.descriptor.id));
    let support = certified_artifact_view(&originals, published).unwrap();
    let demanded = (**published)
        .clone()
        .rebuild(|facts| facts.compiler_support = support.clone());
    let extended = demanded
        .clone()
        .extend_type_interfaces(&SelectedTypeInterfaces {
            support: demanded
                .compiler_source_support()
                .selected_interfaces()
                .unwrap()
                .type_interfaces_for_owners(
                    demanded
                        .compiler_input_projection()
                        .snapshot_roles()
                        .keys()
                        .cloned()
                        .collect(),
                )
                .unwrap(),
            producer: demanded.toolchain_identity_sha256(),
            identity: OnceLock::new(),
        })
        .unwrap();
    assert_eq!(
        extended.artifact_view().capture_graph_selection().roots,
        demanded.artifact_view().capture_graph_selection().roots,
        "type evidence must preserve the existing complete demand independently of published policy"
    );

    assert_eq!(
        support.projection().snapshot_roles()[&root.descriptor.owner],
        role
    );
    let selected_originals = published
        .compiler_input_roles()
        .iter()
        .filter_map(CompilerInputRole::original)
        .collect::<BTreeSet<_>>();
    let expected_groups = original_graph
        .roots
        .iter()
        .filter_map(|node| match node {
            crate::artifact_inventory::ArtifactBindingNode::Group(group)
                if selected_originals.contains(&group.binding.artifact()) =>
            {
                Some(*group)
            }
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    assert!(!expected_groups.is_empty());
    let certified_graph = support.artifacts().capture_graph_selection();
    assert_eq!(support.projection(), published.compiler_input_projection());
    assert_eq!(certified_graph.namespace, before.namespace);
    assert_eq!(
        certified_graph
            .roots
            .iter()
            .filter_map(|node| match node {
                crate::artifact_inventory::ArtifactBindingNode::Group(group) => Some(*group),
                _ => None,
            })
            .collect::<BTreeSet<_>>(),
        expected_groups,
        "certification retains complete demand for the selected compiler originals"
    );
    let private_groups = original_graph
        .roots
        .iter()
        .filter_map(|node| match node {
            crate::artifact_inventory::ArtifactBindingNode::Group(group)
                if !selected_originals.contains(&group.binding.artifact()) =>
            {
                Some(*group)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        !private_groups.is_empty(),
        "the completed output also holds the private executable wrapper"
    );
    assert!(
        private_groups
            .iter()
            .all(|group| support.artifacts().contains_acquired_native_group(*group)),
        "unselected completed bodies retain their exact issued custody"
    );
    assert!(originals
        .native_closure()
        .binding_dependencies()
        .iter()
        .all(|edge| support.artifacts().binding_dependencies().contains(edge)));
    assert_eq!(published.artifact_view().capture_graph_selection(), before);
    assert_eq!(
        published.semantic_sha256(),
        before_sha,
        "joining custody must not mutate the immutable publication"
    );
    for shared_type in [false, true] {
        for fresh_group_count in [1, 2, 3] {
            let fresh = crate::certified_products::tests::recovered_witness_fixtures(&[
                crate::certified_products::fixture_finalized_product_with_requirements(
                    crate::certified_products::tests::original_groups_fixture_with_interface(
                        "UnrelatedAuthored",
                        (0..fresh_group_count)
                            .map(|index| (7 + index * 10, vec![]))
                            .collect(),
                        2 + fresh_group_count as u8,
                        &BTreeMap::new(),
                        b"fresh authored interface".to_vec(),
                    ),
                    producer,
                    Some(if shared_type {
                        BTreeMap::from([(
                            (
                                root.descriptor.owner.unit.clone(),
                                root.descriptor.owner.module.clone(),
                            ),
                            root.descriptor.interface_sha256,
                        )])
                    } else {
                        BTreeMap::new()
                    }),
                ),
            ])
            .remove(0)
            .product;
            let fresh_entry = Arc::new(ArtifactEntry::original(producer, fresh).unwrap());
            let types = published
                .compiler_source_support()
                .selected_interfaces()
                .unwrap()
                .support_for_owners(vec![root.descriptor.owner.clone()])
                .unwrap();
            let fresh_view = types
                .artifacts()
                .inventory()
                .admit_shared(types.artifacts(), vec![fresh_entry.clone()])
                .unwrap();
            let mut fresh_projection = types.projection().clone();
            let incoming = CompilerInputProjection::from_issued_entries(&[fresh_entry]).unwrap();
            for (owner, role) in incoming.snapshot_roles() {
                fresh_projection
                    .admit_role(owner.clone(), role.clone())
                    .unwrap();
            }
            let fresh_selection =
                crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                    &fresh_projection,
                    &fresh_view.metadata_snapshot(),
                    &tidepool_repr::execution_schema::InventoryOperation::new(Default::default()),
                )
                .unwrap();
            let fresh_originals = fresh_selection
                .selected_original_closure(&fresh_view)
                .unwrap();
            let baseline = published
                .as_ref()
                .clone()
                .extend_issued_compiler_support(
                    &crate::declaration_context::issue_fixture_compiler_support(
                        fresh_projection,
                        fresh_view,
                    )
                    .unwrap(),
                )
                .unwrap();
            assert!(
                baseline
                    .compiler_input_projection()
                    .within_view(fresh_originals.native_closure())
                    .validate(fresh_originals.native_closure())
                    .is_err(),
                "within_view must not silently erase missing published policy"
            );
            let certified = certified_artifact_view(&fresh_originals, &baseline).unwrap();
            assert_eq!(
                certified.artifacts().selected_native_groups().len(),
                fresh_group_count as usize
                    + published.artifact_view().selected_native_groups().len()
            );
            for selected in fresh_originals
                .native_closure()
                .capture_graph_selection()
                .roots
                .iter()
                .chain(&before.roots)
            {
                assert!(certified
                    .artifacts()
                    .capture_graph_selection()
                    .roots
                    .contains(selected));
            }
            assert_eq!(
                certified.projection().snapshot_roles()[&root.descriptor.owner],
                role
            );
        }
    }
}

#[test]
#[ignore = "requires the declared rooted compiler service"]
fn published_native_custody_does_not_require_dependency_namespace_originals() {
    let fixture = compiled_publication_fixture();
    let root = &fixture.root;
    let helper = &fixture.helper;
    let publication = &fixture.publication;
    let published = publication.context();
    let birth = published
        .compiler_source_support()
        .publication_birth_support(&root.descriptor.owner)
        .unwrap();
    let expected = birth.artifacts().capture_graph_selection();
    assert!(
        expected
            .native_groups
            .iter()
            .any(|group| group.binding.artifact() == helper.descriptor.id),
        "the real dependent root retains Helper's native body"
    );
    let narrowed = published
        .compiler_source_support()
        .source_support(&BTreeSet::from([root.descriptor.owner.clone()]))
        .unwrap();
    assert!(
        narrowed.projection().snapshot_roles()[&helper.descriptor.owner]
            .original()
            .is_none()
    );
    assert!(narrowed
        .artifacts()
        .artifact_ids()
        .contains(&helper.descriptor.id));
    let continued = published
        .as_ref()
        .clone()
        .rebuild(|facts| facts.compiler_support = narrowed)
        .finish()
        .unwrap();
    let reopened = continued.published_source_original_selections().unwrap();
    assert_eq!(reopened.len(), 1);
    assert_eq!(
        reopened[0]
            .context()
            .compiler_source_support()
            .publication_birth_support(&root.descriptor.owner)
            .unwrap()
            .artifacts()
            .capture_graph_selection(),
        expected
    );

    let (scratch, inventory, _) = super::history_properties::restored_graph_roles(
        &continued,
        &continued.compiler_input_roles(),
    )
    .unwrap();
    let snapshot = continued.snapshot();
    let store = fvm_ipld_blockstore::MemoryBlockstore::default();
    let mut writer = crate::exact_context::Materializer::new(&store);
    let written = writer.materialize_context(None, &snapshot).unwrap();
    use fvm_ipld_blockstore::Blockstore;
    for (cid, bytes) in writer.finish().unwrap().into_blocks() {
        store.put_keyed(&cid, &bytes).unwrap();
    }
    let decoded = crate::exact_context::hydrate_context(&store, written.root()).unwrap();
    let recovered = inventory
        .context_for_expected_snapshot(written.root(), &decoded)
        .unwrap();
    drop(scratch);
    assert_eq!(recovered.artifact_view(), continued.artifact_view());
    assert_eq!(
        recovered.compiler_input_roles(),
        continued.compiler_input_roles()
    );
    let reopened = recovered.published_source_original_selections().unwrap();
    assert_eq!(
        reopened[0]
            .context()
            .compiler_source_support()
            .publication_birth_support(&root.descriptor.owner)
            .unwrap()
            .artifacts()
            .capture_graph_selection(),
        expected
    );

    let mut missing = inventory;
    missing.entries.remove(&helper.descriptor.id);
    assert!(
        missing
            .context_for_expected_snapshot(written.root(), &decoded)
            .is_err(),
        "canonical Helper custody cannot replace the exact required native child"
    );
}

#[test]
#[ignore = "requires the declared rooted compiler service"]
fn nested_publication_digest_cannot_change_recovered_source_or_native_selection() {
    use crate::exact_context::{
        hydrate_context, ExactContextSnapshot, Materializer, PublicationNativeFacts,
        PublicationNativeMaps, RecordedCompilerRow, RecordedOriginalProvenance,
    };
    use fvm_ipld_blockstore::{Blockstore, MemoryBlockstore};

    fn publication(
        row: &mut RecordedCompilerRow,
    ) -> &mut crate::exact_context::RecordedPublicationBirth {
        let RecordedCompilerRow::Original {
            provenance: RecordedOriginalProvenance::Published(birth),
            ..
        } = row
        else {
            panic!("actual nested publication row")
        };
        birth
    }
    fn encode(snapshot: &ExactContextSnapshot) -> crate::exact_context::DecodedContext {
        let store = MemoryBlockstore::new();
        let mut writer = Materializer::new(&store);
        let receipt = writer.materialize_context(None, snapshot).unwrap();
        for (cid, bytes) in writer.finish().unwrap().into_blocks() {
            store.put_keyed(&cid, &bytes).unwrap();
        }
        hydrate_context(&store, receipt.root()).unwrap()
    }

    let fixture =
        crate::artifacts::test_support::prepare_source_publication_entry_fixture().unwrap();
    let root = fixture.root.descriptor.owner.clone();
    let helper = fixture.helper.descriptor.owner.clone();
    let helper_publication = fixture
        .entry
        .products()
        .published_source_original_selection("nested-helper", &helper)
        .unwrap();
    let source = fixture
        .entry
        .products()
        .original_execution_context()
        .unwrap();
    let before_root = source
        .published_context_for_root(&root, fixture.root.descriptor.id)
        .unwrap()
        .with_published_source_originals(&helper_publication)
        .unwrap();
    let input = fixture
        .entry
        .products()
        .original_compile_input
        .as_ref()
        .unwrap();
    let endorsement = before_root
        .endorse_published_original(&root, "nested-root", input.original_input_identity())
        .unwrap();
    let support = before_root
        .compiler_source_support()
        .publish_original(&endorsement)
        .unwrap();
    let context = before_root
        .clone()
        .rebuild(|facts| facts.compiler_support = support)
        .finish()
        .unwrap();
    let snapshot = context.snapshot();
    let birth = snapshot.roles[&root].publication().unwrap();
    let original_nested_digest = birth.native_facts.roles[&helper]
        .publication()
        .unwrap()
        .selection_sha256;
    assert_eq!(birth.native_facts.roles[&root].publication(), None);
    let view = context.artifact_view();
    let graph = view.capture_graph_selection();
    let inventory = RecoveredArtifactInventory {
        producer: context.toolchain_identity_sha256(),
        entries: view
            .entries()
            .into_iter()
            .map(|entry| (entry.descriptor.id, entry))
            .collect(),
        recorded_inventory: true,
        bindings: graph.bindings.iter().copied().collect(),
        binding_edges: view.binding_dependencies(),
        interfaces: view.interface_dependencies(),
    };
    let recorded = encode(&snapshot);
    let positive = inventory
        .context_for_expected_snapshot(recorded.root(), &recorded)
        .unwrap();
    positive.published_source_original_selections().unwrap();
    let positive_birth = positive
        .compiler_source_support()
        .publication_birth_support(&root)
        .unwrap();
    before_root
        .clone()
        .rebuild(|facts| facts.compiler_support = positive_birth.clone())
        .finish()
        .unwrap();

    // Deliberate raw-record corruption: the nested historical digest changes,
    // while all acquisitions, native permissions, lexical rows and source rows
    // remain those of the actual issuer. Rebuild content addressing honestly.
    let mut altered = recorded.snapshot().clone();
    let outer = publication(altered.roles.get_mut(&root).unwrap());
    let mut nested_rows = outer.native_facts.roles.clone();
    publication(nested_rows.get_mut(&helper).unwrap()).selection_sha256[0] ^= 1;
    outer.native_facts = PublicationNativeFacts::maintain(
        None,
        PublicationNativeMaps {
            roles: nested_rows,
            offers: outer.native_facts.offers.clone(),
            native_availability: outer.native_facts.native_availability.clone(),
        },
    )
    .unwrap();
    altered.native_fact_owner = None;
    let unframed = encode(&altered);
    assert!(
        inventory
            .context_for_expected_snapshot(unframed.root(), &unframed)
            .is_err(),
        "the original outer commitment must detect changed historical bytes"
    );

    // Recompute only the outer digest with the owning algorithm, so failure
    // cannot be attributed to stale framing. This is not a successful issuer:
    // the intermediate support is never presented as a finished context.
    let support = crate::artifact_inventory::CompilerSourceSupport::authenticate_recorded_rows(
        context.artifact_view().clone(),
        &altered.roles,
        &altered.offers,
        &altered.native_availability,
    )
    .unwrap();
    let unfinished = context
        .clone()
        .rebuild(|facts| facts.compiler_support = support);
    let roles = unfinished.compiler_input_projection().snapshot_roles();
    let facts = PublishedSelectionFacts::from_role(&roles[&root]).unwrap();
    let digest = unfinished
        .published_selection_sha256(&root, &facts)
        .unwrap();
    publication(altered.roles.get_mut(&root).unwrap()).selection_sha256 = digest;
    let reframed = encode(&altered);
    let restored = inventory
        .context_for_expected_snapshot(reframed.root(), &reframed)
        .unwrap();
    let altered_birth = restored
        .compiler_source_support()
        .publication_birth_support(&root)
        .unwrap();
    assert_ne!(
        altered_birth.recorded_rows()[&helper]
            .publication()
            .unwrap()
            .selection_sha256,
        original_nested_digest
    );

    // The nested digest is retained metadata in the outer native domain. It
    // does not select a different occurrence, issue SOURCE, or change demand.
    assert_eq!(
        restored.snapshot().source_scope,
        positive.snapshot().source_scope
    );
    assert_eq!(restored.snapshot().lexical, positive.snapshot().lexical);
    assert_eq!(
        restored.artifact_view().capture_graph_selection(),
        positive.artifact_view().capture_graph_selection()
    );
    assert_eq!(
        altered_birth.artifacts().capture_graph_selection(),
        positive_birth.artifacts().capture_graph_selection()
    );
    assert_eq!(
        altered_birth
            .native_availability()
            .snapshot_facts()
            .unwrap(),
        positive_birth
            .native_availability()
            .snapshot_facts()
            .unwrap()
    );
    let selected = |context: &ExactDeclarationContext| {
        context
            .published_source_original_selections()
            .unwrap()
            .into_iter()
            .map(|selection| {
                (
                    selection.public_root().clone(),
                    selection.revision().to_owned(),
                    selection
                        .context()
                        .artifact_view()
                        .capture_graph_selection(),
                    selection
                        .context()
                        .compiler_source_support()
                        .native_availability()
                        .snapshot_facts()
                        .unwrap(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(selected(&restored), selected(&positive));
    let groups = positive
        .artifact_view()
        .selected_native_binding_rows()
        .1
        .into_iter()
        .collect::<BTreeSet<_>>();
    assert!(
        !groups.is_empty(),
        "actual producer supplies executable demand"
    );
    let target_selections = |context: &ExactDeclarationContext| {
        context
            .published_source_original_selections_for_target(&groups, &[])
            .unwrap()
            .into_iter()
            .map(|selection| selection.public_root().clone())
            .collect::<Vec<_>>()
    };
    let expected = target_selections(&positive);
    assert!(
        !expected.is_empty(),
        "positive target consumes actual publication authority"
    );
    assert_eq!(target_selections(&restored), expected);

    // Reusing that birth as a source context must validate its own historical
    // lexical context. The retained pre-endorsement context is the independent
    // oracle here; the receiving context is deliberately not substituted.
    assert!(
        before_root
            .rebuild(|facts| facts.compiler_support = altered_birth)
            .finish()
            .is_err(),
        "corrupt historical digest cannot become a usable source context"
    );
}

impl ExactDeclarationContext {
    /// Continue an actual completed compilation with its selected acquisition
    /// crossed against equal-fact transport custody. This helper asserts the
    /// production transitions; it issues no stand-alone test authority.
    pub(crate) fn assert_selected_acquisition_publication_history(
        &self,
        support: &crate::artifact_inventory::CompilerSourceSupport,
        incoming: &crate::artifact_inventory::CompilerSourceSupport,
        owner: &ExactModuleIdentity,
        expected: &Arc<ArtifactEntry>,
        input_identity: &str,
        selection: &crate::certified_products::CertifiedSourceSelection,
    ) {
        let assert_acquisition = |support: &crate::artifact_inventory::CompilerSourceSupport| {
            assert!(
                Arc::ptr_eq(
                    support
                        .compiler_roles()
                        .original_acquisition(owner)
                        .unwrap(),
                    expected,
                ),
                "an equal-fact custody acquisition replaced the selected compiler owner"
            );
            assert!(matches!(
                support.compiler_roles().native_decision(owner),
                crate::artifact_inventory::InputNativeDecision::UnissuedPolicy,
            ), "publication and continuation cannot issue a native choice from retained availability");
        };
        let context = self
            .published_context_for_root(owner, expected.descriptor.id)
            .unwrap()
            .rebuild(|facts| facts.compiler_support = support.clone())
            .finish()
            .unwrap();
        assert_acquisition(context.compiler_source_support());
        let endorsement = context
            .endorse_published_original(owner, "crossed-acquisition-history", input_identity)
            .unwrap();
        let published = support.publish_original(&endorsement).unwrap();
        assert_acquisition(&published);
        assert_acquisition(&published.publication_birth_support(owner).unwrap());

        // An empty source delta still traverses the real source continuation
        // owner with a genuine completed source selection. It must retain the
        // receiving published row and its immutable pre-endorsement birth.
        let extended = published
            .extend_program_source(
                selection,
                incoming.artifacts(),
                incoming.projection(),
                &CompilerDependencies::merge(std::iter::empty()).unwrap(),
            )
            .unwrap();
        assert_acquisition(&extended);
        assert_acquisition(&extended.publication_birth_support(owner).unwrap());
        let context = Arc::new(
            context
                .rebuild(|facts| facts.compiler_support = extended)
                .finish()
                .unwrap(),
        );

        // Exercise the same retained input owner used by compiler World
        // continuations, including its actual native-selection consumer.
        let world = context
            .capture_world_primary_inputs(context.toolchain_identity_sha256())
            .unwrap();
        assert_acquisition(world.inputs().compiler_source_support());
        let additions = incoming
            .compiler_roles()
            .keys()
            .filter(|owner| !context.compiler_roles().keys().any(|held| held == *owner))
            .cloned()
            .collect::<BTreeSet<_>>();
        assert!(
            !additions.is_empty(),
            "the real completed source cohort adds new World roles"
        );
        let continued = world.advance_program_support(&context, incoming).unwrap();
        assert!(
            additions
                .iter()
                .all(|owner| continued.changed_owners.contains(owner)),
            "the continuation must actually install the nonempty source delta"
        );
        for added in &additions {
            if let Some(incoming_acquisition) =
                incoming.compiler_roles().original_acquisition(added)
            {
                assert!(
                    Arc::ptr_eq(
                        continued
                            .primary
                            .inputs()
                            .compiler_source_support()
                            .compiler_roles()
                            .original_acquisition(added)
                            .unwrap(),
                        incoming_acquisition,
                    ),
                    "a newly installed source row retains the actual incoming acquisition"
                );
            }
        }
        assert_acquisition(continued.primary.context().compiler_source_support());
        assert_acquisition(continued.primary.inputs().compiler_source_support());
        assert_acquisition(
            &continued
                .primary
                .context()
                .compiler_source_support()
                .publication_birth_support(owner)
                .unwrap(),
        );
    }
}
