impl ExactDeclarationContext {
    /// The fixture adapter exits before its Haskell consumer runs. Its packet
    /// directory therefore owns the complete issued resource until consumption
    /// and cleanup, rather than borrowing the adapter process's temporary files.
    pub(crate) fn prepare_fixture_compilation(
        self: &Arc<Self>,
        root: &Path,
        producer: &[u8],
    ) -> Result<ExactCompilationRequest, CompileError> {
        let scaffold = ProtectedScaffoldRequirements::default();
        let inputs = scaffold.compiler_inputs(self, None)?;
        let metadata = &inputs.native_transport.metadata;
        if !root.is_absolute()
            || self
                .owner
                .facts
                .compiler_support
                .artifacts()
                .retained_materialization(&metadata)?
                .is_some()
            || self.owner.facts.producer
                != crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(
                    producer,
                )
                .sha256()
        {
            return Err(failure(
                "fixture delivery requires a fresh matched artifact owner",
            ));
        }
        std::fs::create_dir_all(root)?;

        self.owner
            .facts
            .compiler_support
            .artifacts()
            .retain_materialization(&metadata, |parents| {
                if !parents.is_empty() {
                    return Err(failure(
                        "fixture delivery cannot borrow process-local artifact owners",
                    ));
                }
                let mut directory = tempfile::Builder::new()
                    .prefix("tidepool-exact-artifacts-")
                    .tempdir_in(root)?;
                let cleanup_on_failure = directory.path().to_path_buf();
                // These paths already belong to the packet's scoped directory. Its
                // owner releases them after the downstream process consumes them.
                directory.disable_cleanup(true);
                let result = self.materialize_retained_artifacts(&metadata, parents, directory);
                if result.is_err() {
                    let _ = std::fs::remove_dir_all(cleanup_on_failure);
                }
                result
            })?;
        self.prepare_compilation_from_metadata(
            root,
            producer,
            None,
            inputs,
            &scaffold,
            None,
            RequestInputTransport::DurableFiles,
        )
    }
}

fn assert_context_observation_wire(context: &ExactDeclarationContext) {
    use sha2::Digest;
    let retained = context.artifact_view().metadata_snapshot();
    let original = context.original_semantic_bytes_oracle(&retained);
    let observation = context.observation();
    assert_eq!(observation.semantics.as_ref(), original);
    assert_eq!(
        context.semantic_sha256(),
        <[u8; 32]>::from(sha2::Sha256::digest(&original))
    );
    // Reproduce the old owned JSON DTO independently of the borrowed encoder.
    #[derive(serde::Serialize)]
    struct Wire {
        semantics: Vec<u8>,
        descriptors: Vec<crate::artifact_inventory::ArtifactDescriptor>,
        dependencies: Vec<(ArtifactId, ArtifactId, ArtifactDependency)>,
        native_groups: BTreeSet<NativeGroupKey>,
    }
    let encoded = serde_json::to_vec(&Wire {
        semantics: original,
        descriptors: retained.descriptors().into_iter().cloned().collect(),
        dependencies: retained.dependencies().to_vec(),
        native_groups: retained.selected_native_groups.clone(),
    })
    .unwrap();
    for witness in [&b""[..], &b"original-input-type-witness"[..]] {
        let mut hash = sha2::Sha256::new();
        hash.update(b"TPEXACTDISPLAYCONTEXT1");
        hash.update((encoded.len() as u64).to_le_bytes());
        hash.update(&encoded);
        hash.update(witness);
        assert_eq!(
            context.display_identity(witness),
            <[u8; 32]>::from(hash.finalize())
        );
    }
}

#[test]
fn immutable_context_clones_share_one_semantic_and_display_encoding() {
    use std::sync::atomic::Ordering;
    let context = Arc::new(published_original_fixture());
    let same = Arc::clone(&context);
    let raw = context.as_ref().clone();
    assert_eq!(context.as_ref(), &raw);
    assert!(context.owner.observations.get().is_none());
    assert_context_observation_wire(&context);
    assert_context_observation_wire(&same);
    assert_eq!(context.as_ref(), &raw);
    assert!(raw.owner.observations.get().is_some());
    assert_eq!(context.owner.semantic_issuances.load(Ordering::Relaxed), 1);
    assert_eq!(context.owner.display_issuances.load(Ordering::Relaxed), 1);
    assert!(std::ptr::eq(context.observation(), same.observation()));
    assert_context_observation_wire(&raw);
    assert_eq!(raw.owner.semantic_issuances.load(Ordering::Relaxed), 1);
    assert_eq!(raw.owner.display_issuances.load(Ordering::Relaxed), 1);
    assert!(std::ptr::eq(context.observation(), raw.observation()));
    assert!(context.same_authenticated_context(&raw));
    assert!(context.same_display_observations(&raw));
    for _ in 0..8 {
        let clone = context
            .as_ref()
            .clone()
            .admit_producer(context.toolchain_identity_sha256())
            .unwrap();
        assert_context_observation_wire(&clone);
        assert_eq!(clone.owner.semantic_issuances.load(Ordering::Relaxed), 1);
        assert_eq!(clone.owner.display_issuances.load(Ordering::Relaxed), 1);
        assert!(Arc::ptr_eq(&context.owner, &clone.owner));
    }
}

#[test]
fn same_owner_context_comparisons_do_not_observe_inventory() {
    let context = published_original_fixture();
    let cloned = context.clone();
    let compare = || {
        assert_eq!(context, cloned);
        assert!(context.same_authenticated_context(&cloned));
        assert!(context.same_display_observations(&cloned));
    };
    let before = context.artifact_view().inventory().metrics();
    let encodings = CONTEXT_OBSERVATION_WORK.with(std::cell::Cell::get);
    let comparisons = CONTEXT_FACT_COMPARISONS.with(std::cell::Cell::get);
    for _ in 0..8 {
        compare();
    }
    assert_eq!(context.artifact_view().inventory().metrics(), before);
    assert_eq!(
        CONTEXT_OBSERVATION_WORK.with(std::cell::Cell::get),
        encodings
    );
    assert_eq!(
        CONTEXT_FACT_COMPARISONS.with(std::cell::Cell::get),
        comparisons
    );
    assert!(context.owner.observations.get().is_none());

    assert_context_observation_wire(&context);
    let before = context.artifact_view().inventory().metrics();
    let encodings = CONTEXT_OBSERVATION_WORK.with(std::cell::Cell::get);
    for _ in 0..8 {
        compare();
    }
    assert_eq!(context.artifact_view().inventory().metrics(), before);
    assert_eq!(
        CONTEXT_OBSERVATION_WORK.with(std::cell::Cell::get),
        encodings
    );
    assert_eq!(
        CONTEXT_FACT_COMPARISONS.with(std::cell::Cell::get),
        comparisons
    );
}

#[test]
fn independently_issued_equal_contexts_compare_authenticated_facts() {
    use std::sync::atomic::Ordering;
    let context = published_original_fixture();
    let independent = context.clone().rebuild(|_| {});
    let comparisons = CONTEXT_FACT_COMPARISONS.with(std::cell::Cell::get);
    assert_eq!(context, independent);
    assert!(context.same_authenticated_context(&independent));
    assert!(CONTEXT_FACT_COMPARISONS.with(std::cell::Cell::get) > comparisons);
    assert!(context.same_display_observations(&independent));
    assert_eq!(context.owner.semantic_issuances.load(Ordering::Relaxed), 1);
    assert_eq!(
        independent.owner.semantic_issuances.load(Ordering::Relaxed),
        1
    );
    assert_eq!(context.semantic_sha256(), independent.semantic_sha256());
}

#[test]
fn changed_context_facts_cannot_reuse_initialized_observations() {
    let mut context = published_original_fixture();
    assert_context_observation_wire(&context);
    let before = context.clone();
    let digest = context.semantic_sha256();
    context = context.rebuild(|facts| {
        facts.original_instance_environment =
            OriginalInstanceEnvironment::MissingOriginalOwners(vec![identity("u", "Missing")])
    });
    assert!(context.owner.observations.get().is_none());
    assert_ne!(context, before);
    assert_ne!(context.semantic_sha256(), digest);
    assert!(!context.same_authenticated_context(&before));
    assert_context_observation_wire(&context);
    assert_context_observation_wire(&before);
}

#[test]
fn context_observation_wire_preserves_v8_v9_and_missing_owner_order() {
    let base = ExactDeclarationContext::new(&[], &[], vec![]).unwrap();
    assert_context_observation_wire(&base);
    assert_eq!(
        hex(&base.semantic_sha256()),
        "38c954e31cc17c0b3ea10155e2490d5b3d8155009a7374ba19ac49b53bef0c91"
    );
    let mut complete = base.clone();
    complete = complete.rebuild(|facts| {
        facts.original_instance_environment = OriginalInstanceEnvironment::Complete {
            target: identity("unit", "Target"),
        }
    });
    assert_context_observation_wire(&complete);
    assert_eq!(
        hex(&complete.semantic_sha256()),
        "79cefd269cead0be57b9b3d3b4369ee4965a3fdd56479c79f67f11c406e3e497"
    );
    let mut missing = base.clone();
    missing = missing.rebuild(|facts| {
        facts.original_instance_environment =
            OriginalInstanceEnvironment::MissingOriginalOwners(vec![
                identity("unit", "Z"),
                identity("unit", "A"),
            ])
    });
    assert_context_observation_wire(&missing);
    assert_eq!(
        hex(&missing.semantic_sha256()),
        "bdb20f1087c6e7392613622f3e4860c8e3a4a317e1feaa4a2c916298db958f8d"
    );
    let reversed = {
        let mut context = missing.clone();
        context = context.rebuild(|facts| {
            if let OriginalInstanceEnvironment::MissingOriginalOwners(owners) =
                &mut facts.original_instance_environment
            {
                owners.reverse();
            }
        });
        context
    };
    assert_context_observation_wire(&reversed);
    assert_ne!(missing.semantic_sha256(), reversed.semantic_sha256());
}
use super::*;
use crate::artifact_inventory::ArtifactKind;

include!("program_source_support_history.rs");
include!("materialization_capture_history.rs");

fn current_scope_fields(manifest: &Value) -> &[Value] {
    let fields = manifest.as_array().expect("issued scope is an array");
    assert_eq!(fields.len(), EXACT_SCOPE_SCHEMA_FIELDS);
    assert_eq!(fields[0], text("TPEXACTSCOPE"));
    assert_eq!(fields[1], text(EXACT_SCOPE_SCHEMA_VERSION));
    fields
}

fn bound_recovery_pair_fixture() -> BoundRecoveryPair {
    use crate::certified_products::{fixture_module_core, fixture_module_interface};
    use crate::recovery_artifacts::materialize_module_interface;
    let child_old = fixture_module_interface([2; 32], "unit", "Child", BTreeMap::new());
    let child_new = fixture_module_core(&child_old, b"independently issued child Core".to_vec());
    assert_eq!(child_old.interface_sha256(), child_new.interface_sha256());
    assert_ne!(child_old.certificate_bytes(), child_new.certificate_bytes());
    let root = fixture_module_interface(
        [2; 32],
        "unit",
        "Root",
        BTreeMap::from([(
            ("unit".into(), "Child".into()),
            child_old.interface_sha256(),
        )]),
    );
    let root_id = ArtifactEntry::canonical(root.clone()).descriptor.id;
    let old_id = ArtifactEntry::canonical(child_old.clone()).descriptor.id;
    let new_id = ArtifactEntry::canonical(child_new.clone()).descriptor.id;
    let issuer = ArtifactInventory::default();
    let old = issuer
        .admit(
            &issuer.empty_view(),
            vec![
                ArtifactEntry::canonical(root.clone()),
                ArtifactEntry::canonical(child_old.clone()),
            ],
        )
        .unwrap();
    let new = issuer
        .admit(
            &issuer.empty_view(),
            vec![
                ArtifactEntry::canonical(root.clone()),
                ArtifactEntry::canonical(child_new.clone()),
            ],
        )
        .unwrap();
    let root_binding = |selection: &ArtifactGraphSelection| {
        *selection
            .bindings
            .iter()
            .find(|binding| binding.artifact == root_id)
            .unwrap()
    };
    let selections = [old.capture_graph_selection(), new.capture_graph_selection()];
    assert_ne!(root_binding(&selections[0]), root_binding(&selections[1]));
    let edges = old
        .binding_dependencies()
        .into_iter()
        .chain(new.binding_dependencies())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let bindings = selections
        .iter()
        .flat_map(|selection| selection.bindings.iter().copied())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let directory = tempfile::tempdir().unwrap();
    let mut validation = PackageInterfaceValidation::default();
    let references = [&root, &child_old, &child_new]
        .into_iter()
        .map(|interface| {
            materialize_module_interface(
                directory.path(),
                interface,
                &mut validation,
                MaterializationMode::Durable,
            )
        })
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let descriptors = references
        .iter()
        .map(crate::artifact_inventory::ArtifactDescriptor::from_recovery_module_interface)
        .collect::<Vec<_>>();
    let encoded =
        serde_json::to_vec(&(references, descriptors, bindings, edges, selections)).unwrap();
    drop((old, new, issuer, root, child_old, child_new));
    let (references, descriptors, bindings, edges, selections): (
        Vec<recovery_artifacts::RecoveryModuleInterfaceRef>,
        Vec<crate::artifact_inventory::ArtifactDescriptor>,
        Vec<ArtifactBinding>,
        Vec<(ArtifactBindingNode, ArtifactBindingNode, ArtifactDependency)>,
        [ArtifactGraphSelection; 2],
    ) = serde_json::from_slice(&encoded).unwrap();
    let recovered = RecoveredArtifactInventory::capture_bound(
        directory.path(),
        &[],
        &references,
        &[],
        &[],
        &descriptors,
        &bindings,
        &edges,
    )
    .unwrap();
    BoundRecoveryPair {
        recovered,
        selections,
        root_id,
        old_id,
        new_id,
        references,
        descriptors,
        bindings,
        edges,
        directory,
    }
}

struct BoundRecoveryPair {
    recovered: RecoveredArtifactInventory,
    selections: [ArtifactGraphSelection; 2],
    root_id: ArtifactId,
    old_id: ArtifactId,
    new_id: ArtifactId,
    references: Vec<recovery_artifacts::RecoveryModuleInterfaceRef>,
    descriptors: Vec<crate::artifact_inventory::ArtifactDescriptor>,
    bindings: Vec<ArtifactBinding>,
    edges: Vec<(ArtifactBindingNode, ArtifactBindingNode, ArtifactDependency)>,
    directory: tempfile::TempDir,
}
struct BoundRecoveryInputs<'a> {
    selection: &'a ArtifactGraphSelection,
    references: Vec<recovery_artifacts::RecoveryModuleInterfaceRef>,
    descriptors: Vec<crate::artifact_inventory::ArtifactDescriptor>,
    edges: Vec<(ArtifactBindingNode, ArtifactBindingNode, ArtifactDependency)>,
}
impl BoundRecoveryInputs<'_> {
    fn capture(
        &self,
        acquisition: &mut RecoveredArtifactAcquisition,
    ) -> Result<RecoveredArtifactInventory, RecoveryInventoryError> {
        acquisition.capture_bound(
            &[],
            &self.references,
            &[],
            &[],
            &self.descriptors,
            &self.selection.bindings,
            &self.edges,
        )
    }
}
impl BoundRecoveryPair {
    fn inputs(&self, index: usize) -> BoundRecoveryInputs<'_> {
        let selection = &self.selections[index];
        let ids = selection
            .bindings
            .iter()
            .map(|binding| binding.artifact)
            .collect::<BTreeSet<_>>();
        let references = self
            .references
            .iter()
            .filter(|reference| {
                ids.contains(
                    &crate::artifact_inventory::ArtifactDescriptor::from_recovery_module_interface(
                        reference,
                    )
                    .id,
                )
            })
            .cloned()
            .collect();
        let descriptors = self
            .descriptors
            .iter()
            .filter(|descriptor| ids.contains(&descriptor.id))
            .cloned()
            .collect();
        let bindings = selection.bindings.iter().copied().collect::<BTreeSet<_>>();
        let groups = selection
            .native_groups
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let edges = self
            .edges
            .iter()
            .filter(|(from, _, _)| from.is_retained_in(&bindings, &groups))
            .cloned()
            .collect();
        BoundRecoveryInputs {
            selection,
            references,
            descriptors,
            edges,
        }
    }
    fn restore_acquired(
        &self,
        acquisition: &mut RecoveredArtifactAcquisition,
        index: usize,
        selection: &ArtifactGraphSelection,
    ) -> Result<ExactDeclarationContext, RecoveryInventoryError> {
        let inputs = self.inputs(index);
        let mut stage = acquisition.stage_inputs(&[], &inputs.references, &[], &[])?;
        let captured = stage.capture_bound(
            &[],
            &inputs.references,
            &[],
            &[],
            &inputs.descriptors,
            &selection.bindings,
            &inputs.edges,
        )?;
        let roles = inputs
            .selection
            .namespace
            .iter()
            .map(|binding| CompilerInputRole::InterfaceOnly {
                interface: binding.artifact,
            })
            .collect::<Vec<_>>();
        let context = captured.context_with_graph_selection(selection, &roles, vec![])?;
        acquisition.append(stage)?;
        Ok(context)
    }
    fn restore(
        &self,
        selection: &ArtifactGraphSelection,
    ) -> Result<ExactDeclarationContext, CompileError> {
        let roles = selection
            .namespace
            .iter()
            .map(|binding| CompilerInputRole::InterfaceOnly {
                interface: binding.artifact,
            })
            .collect::<Vec<_>>();
        self.recovered
            .context_with_graph_selection(selection, &roles, vec![])
    }
}

#[test]
fn recovery_payload_acquisition_measures_overlapping_exact_contexts() {
    let pair = bound_recovery_pair_fixture();
    let capture = |index: usize, work: &mut recovery_artifacts::RecoveryArtifactWork| {
        let inputs = pair.inputs(index);
        recovery_artifacts::with_artifact_work(work, |validation| {
            let captured = RecoveredArtifactInventory::capture_inputs_with_validation(
                pair.directory.path(),
                &[],
                &inputs.references,
                &[],
                &[],
                None,
                validation,
                &BTreeMap::new(),
            )?;
            RecoveredArtifactInventory::bind_captured(
                captured,
                &inputs.descriptors,
                &inputs.selection.bindings,
                &inputs.edges,
            )
        })
        .unwrap()
    };
    let mut work_a = recovery_artifacts::RecoveryArtifactWork::default();
    let a = capture(0, &mut work_a);
    let mut work_b = recovery_artifacts::RecoveryArtifactWork::default();
    let b = capture(1, &mut work_b);
    let restore = |inventory: &RecoveredArtifactInventory, index: usize| {
        let selection = &pair.selections[index];
        let roles = selection
            .namespace
            .iter()
            .map(|binding| CompilerInputRole::InterfaceOnly {
                interface: binding.artifact,
            })
            .collect::<Vec<_>>();
        inventory
            .context_with_graph_selection(selection, &roles, vec![])
            .unwrap()
    };
    let old = restore(&a, 0);
    let new = restore(&b, 1);
    let old_again = restore(&a, 0);
    assert_eq!(
        old.artifact_view().capture_graph_selection(),
        pair.selections[0]
    );
    assert_eq!(
        new.artifact_view().capture_graph_selection(),
        pair.selections[1]
    );
    assert_eq!(old_again.semantic_sha256(), old.semantic_sha256());
    assert_ne!(old.semantic_sha256(), new.semantic_sha256());
    assert!(work_a.read_bytes > 0 && work_b.read_bytes > 0);
    eprintln!("recovery acquisition overlap: A={work_a:?} B={work_b:?}");
    let mut acquisition = RecoveredArtifactAcquisition::new(pair.directory.path());
    let mut observations = Vec::new();
    let mut root_payload = None;
    for index in [0, 1, 0] {
        let inputs = pair.inputs(index);
        let mut stage = acquisition
            .stage_inputs(&[], &inputs.references, &[], &[])
            .unwrap();
        let captured = inputs.capture(&mut stage).unwrap();
        let context = restore(&captured, index);
        assert_eq!(
            context.artifact_view().capture_graph_selection(),
            *inputs.selection
        );
        if let Some(root) = &root_payload {
            assert!(Arc::ptr_eq(root, &captured.entries[&pair.root_id]));
        } else {
            root_payload = Some(captured.entries[&pair.root_id].clone());
        }
        observations.push(stage.work());
        acquisition.append(stage).unwrap();
    }
    assert_eq!(observations[0], work_a);
    assert!(observations[1].read_bytes < work_b.read_bytes);
    assert!(observations[1].decoded_bytes < work_b.decoded_bytes);
    assert_eq!(
        observations[2],
        recovery_artifacts::RecoveryArtifactWork::default()
    );
    assert_eq!(acquisition.payloads.len(), 3);
    eprintln!("recovery acquisition delta: {observations:?}");
}

#[test]
fn recovery_payload_acquisition_refuses_new_corruption_and_reauthenticates_on_reopen() {
    let pair = bound_recovery_pair_fixture();
    let a = pair.inputs(0);
    let b = pair.inputs(1);
    let mut acquisition = RecoveredArtifactAcquisition::new(pair.directory.path());
    let held_a = a.capture(&mut acquisition).unwrap();
    assert_eq!(acquisition.payloads.len(), 2);
    let new_child = b
        .references
        .iter()
        .find(|reference| {
            crate::artifact_inventory::ArtifactDescriptor::from_recovery_module_interface(reference)
                .id
                == pair.new_id
        })
        .unwrap();
    let path = pair.directory.path().join(&new_child.certificate_path);
    let bytes = std::fs::read(&path).unwrap();
    std::fs::write(&path, b"corrupt new child").unwrap();
    let mut refused_stage = acquisition
        .stage_inputs(&[], &b.references, &[], &[])
        .unwrap();
    assert!(b.capture(&mut refused_stage).is_err());
    assert!(refused_stage.work().read_bytes > 0);
    assert_eq!(refused_stage.payloads.len(), 1);
    assert_eq!(acquisition.payloads.len(), 2);
    assert_eq!(held_a.entries[&pair.root_id].descriptor.id, pair.root_id);
    std::fs::write(&path, bytes).unwrap();
    let mut retry = acquisition
        .stage_inputs(&[], &b.references, &[], &[])
        .unwrap();
    let held_b = b.capture(&mut retry).unwrap();
    acquisition.append(retry).unwrap();
    assert_eq!(acquisition.payloads.len(), 3);
    assert!(Arc::ptr_eq(
        &held_a.entries[&pair.root_id],
        &held_b.entries[&pair.root_id]
    ));

    let root = a
        .references
        .iter()
        .find(|reference| {
            crate::artifact_inventory::ArtifactDescriptor::from_recovery_module_interface(reference)
                .id
                == pair.root_id
        })
        .unwrap();
    let root_path = pair.directory.path().join(&root.certificate_path);
    std::fs::write(&root_path, b"corrupt already held root").unwrap();
    let before = acquisition.work();
    let held_again = a.capture(&mut acquisition).unwrap();
    assert!(Arc::ptr_eq(
        &held_a.entries[&pair.root_id],
        &held_again.entries[&pair.root_id]
    ));
    assert_eq!(acquisition.work(), before);
    let mut independent = RecoveredArtifactAcquisition::new(pair.directory.path());
    assert!(a.capture(&mut independent).is_err());
    assert!(independent.payloads.is_empty());
    assert!(independent.work().read_bytes > 0);
}

#[test]
fn recovery_payload_acquisition_preserves_exact_reference_and_selection_refusals() {
    let pair = bound_recovery_pair_fixture();
    let mut a = pair.inputs(0);
    let b = pair.inputs(1);
    let mut acquisition = RecoveredArtifactAcquisition::new(pair.directory.path());
    let held = a.capture(&mut acquisition).unwrap();
    b.capture(&mut acquisition).unwrap();
    let before = acquisition.work();
    let mut changed = a.references.clone();
    changed[0].certificate_path = PathBuf::from("another-untrusted-path");
    assert!(acquisition.stage_inputs(&[], &changed, &[], &[]).is_err());
    assert!(acquisition
        .capture_bound(
            &[],
            &changed,
            &[],
            &[],
            &a.descriptors,
            &a.selection.bindings,
            &a.edges
        )
        .is_err());
    assert_eq!(acquisition.work(), before);

    let incoming = *b
        .selection
        .bindings
        .iter()
        .find(|binding| binding.artifact == pair.new_id)
        .unwrap();
    a.edges.iter_mut().find(|(_, to, _)| matches!(to, ArtifactBindingNode::Artifact(binding) if binding.artifact == pair.old_id)).unwrap().1 = ArtifactBindingNode::Artifact(incoming);
    assert!(a.capture(&mut acquisition).is_err());
    assert_eq!(acquisition.work(), before);
    let wrong_roles = b
        .selection
        .namespace
        .iter()
        .map(|binding| CompilerInputRole::InterfaceOnly {
            interface: binding.artifact,
        })
        .collect::<Vec<_>>();
    assert!(held
        .context_with_graph_selection(a.selection, &wrong_roles, vec![])
        .is_err());
    assert_eq!(acquisition.payloads.len(), 3);

    let mut altered_stage = acquisition
        .stage_inputs(&[], &b.references, &[], &[])
        .unwrap();
    altered_stage
        .payloads
        .get_mut(&pair.root_id)
        .unwrap()
        .reference = RecoveryPayloadReference::Canonical(changed[0].clone());
    assert!(acquisition.append(altered_stage).is_err());
    assert_eq!(acquisition.work(), before);
}

#[test]
fn recovery_payload_acquisition_keeps_lexical_requirements_local_to_exact_bindings() {
    let pair = bound_recovery_pair_fixture();
    let a = pair.inputs(0);
    let bytes = b"retained lexical join".to_vec();
    let mut packages = Vec::new();
    ciborium::ser::into_writer(
        &Value::Array(vec![
            text("TPPKGROOTS"),
            text("2"),
            Value::Array(vec![text("unit"), text("Joined"), text(sha256(&bytes))]),
            Value::Array(vec![]),
            Value::Array(vec![]),
        ]),
        &mut packages,
    )
    .unwrap();
    let joined = CertifiedJoinedInterface::from_certification(
        [2; 32],
        "unit".into(),
        "Joined".into(),
        bytes,
        packages,
    )
    .unwrap();
    let reference = joined.materialize(pair.directory.path()).unwrap();
    let join_id = crate::artifact_inventory::ArtifactDescriptor::from_recovery_join(&reference).id;
    let mut views = Vec::new();
    let mut roles = Vec::new();
    for required in [pair.root_id, pair.old_id] {
        let mut entries = a
            .descriptors
            .iter()
            .map(|descriptor| pair.recovered.entries[&descriptor.id].clone())
            .collect::<Vec<_>>();
        entries.push(Arc::new(ArtifactEntry::interface(
            joined.clone(),
            JoinedInterfaceRole::LexicalJoin,
            vec![pair.recovered.entries[&required].descriptor.owner.clone()],
        )));
        roles.push(
            CompilerInputProjection::from_issued_entries(&entries)
                .unwrap()
                .roles(),
        );
        let inventory = ArtifactInventory::default();
        views.push(
            inventory
                .admit_shared(&inventory.empty_view(), entries)
                .unwrap(),
        );
    }
    let mut acquisition = RecoveredArtifactAcquisition::new(pair.directory.path());
    let mut selections = Vec::new();
    let mut all_bindings = BTreeSet::new();
    let mut all_edges = BTreeSet::new();
    for (index, view) in views.iter().enumerate() {
        let selection = view.capture_graph_selection();
        let edges = view.binding_dependencies();
        let held = acquisition
            .capture_bound(
                &[],
                &a.references,
                std::slice::from_ref(&reference),
                &[],
                &view.descriptors(),
                &selection.bindings,
                &edges,
            )
            .unwrap();
        let context = held
            .context_with_graph_selection(&selection, &roles[index], vec![])
            .unwrap();
        assert_eq!(context.artifact_view().capture_graph_selection(), selection);
        assert_eq!(
            held.entries[&join_id].requirements,
            vec![pair.recovered.entries[&[pair.root_id, pair.old_id][index]]
                .descriptor
                .owner
                .clone()]
        );
        assert!(acquisition.payloads[&join_id].entry.requirements.is_empty());
        all_bindings.extend(selection.bindings.iter().copied());
        all_edges.extend(edges);
        selections.push(selection);
    }
    assert_ne!(selections[0], selections[1]);
    let before = acquisition.work();
    assert!(acquisition
        .capture_bound(
            &[],
            &a.references,
            std::slice::from_ref(&reference),
            &[],
            &views[0].descriptors(),
            &all_bindings.into_iter().collect::<Vec<_>>(),
            &all_edges.into_iter().collect::<Vec<_>>()
        )
        .is_err());
    assert_eq!(acquisition.work(), before);
    assert!(acquisition.payloads[&join_id].entry.requirements.is_empty());
}

#[test]
fn recovery_payload_acquisition_preserves_batch_original_owner_uniqueness() {
    let raw = support_product_in_unit("main", "Tidepool.Session.Lib.G11");
    let products = [
        crate::certified_products::fixture_finalized_product(raw.clone(), [2; 32]),
        crate::certified_products::fixture_native_declaration_product(raw, [2; 32], 11),
    ];
    assert_eq!(products[0].owner(), products[1].owner());
    assert_ne!(
        products[0].certification_bytes(),
        products[1].certification_bytes()
    );
    let directory = tempfile::tempdir().unwrap();
    let references = products.each_ref().map(|product| {
        recovery_artifacts::materialize_certified_products(
            directory.path(),
            [2; 32],
            std::slice::from_ref(product),
        )
        .unwrap()
        .remove(0)
    });
    let views = products
        .into_iter()
        .map(|product| {
            let inventory = ArtifactInventory::default();
            inventory
                .admit(
                    &inventory.empty_view(),
                    vec![ArtifactEntry::original([2; 32], product).unwrap()],
                )
                .unwrap()
        })
        .collect::<Vec<_>>();
    let selections = views
        .iter()
        .map(ArtifactView::capture_graph_selection)
        .collect::<Vec<_>>();
    let descriptors = views
        .iter()
        .flat_map(ArtifactView::descriptors)
        .map(|descriptor| (descriptor.id, descriptor))
        .collect::<BTreeMap<_, _>>()
        .into_values()
        .collect::<Vec<_>>();
    let bindings = selections
        .iter()
        .flat_map(|selection| selection.bindings.iter().copied())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let edges = views
        .iter()
        .flat_map(ArtifactView::binding_dependencies)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let interfaces = references
        .iter()
        .map(|reference| reference.module_interface.clone().unwrap())
        .collect::<Vec<_>>();
    assert!(RecoveredArtifactInventory::capture_bound(
        directory.path(),
        &references,
        &interfaces,
        &[],
        &[],
        &descriptors,
        &bindings,
        &edges
    )
    .is_err());
    let mut acquisition = RecoveredArtifactAcquisition::new(directory.path());
    acquisition
        .capture_bound(
            std::slice::from_ref(&references[0]),
            std::slice::from_ref(&interfaces[0]),
            &[],
            &[],
            &views[0].descriptors(),
            &selections[0].bindings,
            &views[0].binding_dependencies(),
        )
        .unwrap();
    let before = acquisition.payloads.len();
    assert!(acquisition
        .capture_bound(
            &references,
            &interfaces,
            &[],
            &[],
            &descriptors,
            &bindings,
            &edges
        )
        .is_err());
    assert_eq!(acquisition.payloads.len(), before);
    acquisition
        .capture_bound(
            std::slice::from_ref(&references[1]),
            std::slice::from_ref(&interfaces[1]),
            &[],
            &[],
            &views[1].descriptors(),
            &selections[1].bindings,
            &views[1].binding_dependencies(),
        )
        .unwrap();
    assert!(acquisition.payloads.len() > before);
    let all_held = acquisition.work();
    assert!(acquisition
        .capture_bound(
            &references,
            &interfaces,
            &[],
            &[],
            &descriptors,
            &bindings,
            &edges
        )
        .is_err());
    assert_eq!(acquisition.work(), all_held);
    acquisition
        .capture_bound(
            std::slice::from_ref(&references[0]),
            std::slice::from_ref(&interfaces[0]),
            &[],
            &[],
            &views[0].descriptors(),
            &selections[0].bindings,
            &views[0].binding_dependencies(),
        )
        .unwrap();
    assert_eq!(acquisition.work(), all_held);
}

#[test]
fn bound_recovery_preserves_same_root_payload_with_distinct_child_certificates() {
    let pair = bound_recovery_pair_fixture();
    let BoundRecoveryPair {
        recovered,
        selections,
        root_id,
        old_id,
        new_id,
        references,
        descriptors,
        bindings,
        edges,
        directory,
    } = pair;
    let root_binding = |selection: &ArtifactGraphSelection| {
        *selection
            .bindings
            .iter()
            .find(|binding| binding.artifact == root_id)
            .unwrap()
    };
    let restore = |selection: &ArtifactGraphSelection| {
        let roles = selection
            .namespace
            .iter()
            .map(|binding| CompilerInputRole::InterfaceOnly {
                interface: binding.artifact,
            })
            .collect::<Vec<_>>();
        recovered.context_with_graph_selection(selection, &roles, vec![])
    };
    let old = restore(&selections[0]).unwrap();
    let new = restore(&selections[1]).unwrap();
    assert_ne!(old, new);
    assert!(!old.same_authenticated_context(&new));
    assert!(!old.same_display_observations(&new));
    assert_context_observation_wire(&old);
    assert_context_observation_wire(&new);
    assert_ne!(old.semantic_sha256(), new.semantic_sha256());
    let before_clones = CONTEXT_OBSERVATION_WORK.with(std::cell::Cell::get);
    for context in [&old, &new] {
        let cloned = context.clone();
        assert_context_observation_wire(&cloned);
    }
    assert_eq!(
        CONTEXT_OBSERVATION_WORK.with(std::cell::Cell::get),
        before_clones
    );
    // Reopening is a new authenticated owner, even for the same wire facts.
    let reopened = restore(&selections[0]).unwrap();
    assert_context_observation_wire(&reopened);
    assert_eq!(reopened.semantic_sha256(), old.semantic_sha256());
    assert_eq!(
        CONTEXT_OBSERVATION_WORK.with(std::cell::Cell::get).0,
        before_clones.0 + 1
    );
    for _ in 0..4 {
        assert_eq!(
            old.artifact_view()
                .select_roots(vec![root_id])
                .unwrap()
                .artifact_ids()
                .into_iter()
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([root_id, old_id])
        );
        assert_eq!(
            new.artifact_view()
                .select_roots(vec![root_id])
                .unwrap()
                .artifact_ids()
                .into_iter()
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([root_id, new_id])
        );
    }
    let before = old.artifact_view().binding_dependencies();
    let mut missing = selections[0].clone();
    missing
        .bindings
        .retain(|binding| binding.artifact != old_id);
    assert!(restore(&missing).is_err());
    let mut wrong_scope = selections[0].clone();
    wrong_scope.namespace[0] = root_binding(&selections[1]);
    assert!(restore(&wrong_scope).is_err());
    let mut missing_witness = selections[0].clone();
    missing_witness.selections.clear();
    assert!(restore(&missing_witness).is_err());
    let mut altered_witness = selections[0].clone();
    Arc::make_mut(&mut altered_witness.selections[0])
        .dependencies
        .clear();
    assert!(restore(&altered_witness).is_err());
    let mut missing_binding = bindings.clone();
    missing_binding.pop();
    assert!(RecoveredArtifactInventory::capture_bound(
        directory.path(),
        &[],
        &references,
        &[],
        &[],
        &descriptors,
        &missing_binding,
        &edges
    )
    .is_err());
    let mut corrupted = edges.clone();
    let old_root = root_binding(&selections[0]);
    corrupted
        .iter_mut()
        .find(|(from, _, _)| *from == ArtifactBindingNode::Artifact(old_root))
        .unwrap()
        .1 = ArtifactBindingNode::Artifact(
        *selections[1]
            .bindings
            .iter()
            .find(|binding| binding.artifact == new_id)
            .unwrap(),
    );
    let refused = RecoveredArtifactInventory::capture_bound(
        directory.path(),
        &[],
        &references,
        &[],
        &[],
        &descriptors,
        &bindings,
        &corrupted,
    )
    .unwrap();
    let roles = selections[0]
        .namespace
        .iter()
        .map(|binding| CompilerInputRole::InterfaceOnly {
            interface: binding.artifact,
        })
        .collect::<Vec<_>>();
    assert!(refused
        .context_with_graph_selection(&selections[0], &roles, vec![])
        .is_err());
    assert_eq!(
        old.artifact_view().binding_dependencies(),
        before,
        "refusal cannot mutate an already recovered graph"
    );
    assert!(restore(&selections[0]).is_ok());
    assert!(restore(&selections[1]).is_ok());
}

#[test]
fn bound_recovery_later_group_retains_inherited_carrier_binding() {
    use crate::artifact_inventory::{NativeArtifactDemand, NativeRequirementRoot};
    use crate::certified_products::tests::{original_groups_fixture, recovered_witness_fixtures};
    let product = original_groups_fixture(
        "Later",
        vec![(3, Vec::new()), (7, Vec::new())],
        1,
        &BTreeMap::new(),
    );
    let product = crate::certified_products::fixture_finalized_product_with_requirements(
        product,
        [2; 32],
        Some(BTreeMap::new()),
    );
    let product = recovered_witness_fixtures(&[product]).remove(0).product;
    let native = Arc::new(ArtifactEntry::original([2; 32], product.clone()).unwrap());
    let native_id = native.descriptor.id;
    let import = |ordinal| crate::certified_products::PendingImportOwner::Source {
        owner: product.owner().clone(),
        original_ordinal: ordinal,
        binder: tidepool_repr::execution_schema::SymbolIdentity {
            unit: product.owner().unit.clone(),
            module: product.owner().module.clone(),
            namespace: "value".into(),
            occurrence: format!("entry_{ordinal}"),
            record_parent: None,
        },
    };
    let issuer = ArtifactInventory::default();
    let carrier = issuer
        .admit_shared_with_demand(
            &issuer.empty_view(),
            vec![native.clone()],
            NativeArtifactDemand::ScopeInterfaces,
        )
        .unwrap();
    let initial = issuer
        .admit_shared_with_demand(
            &carrier,
            vec![native.clone()],
            NativeArtifactDemand::CertifiedTargetImports(&[import(3)]),
        )
        .unwrap();
    let later = issuer
        .admit_shared_with_demand(
            &initial,
            vec![native.clone()],
            NativeArtifactDemand::CertifiedTargetImports(&[import(7)]),
        )
        .unwrap();
    let selection = later.capture_graph_selection();
    let edges = later.binding_dependencies();
    let group = *selection
        .native_groups
        .iter()
        .find(|group| group.original_ordinal == 7)
        .unwrap();
    assert!(!selection.bindings.iter().any(|binding| {
        (binding.artifact, binding.selection)
            == (group.binding.artifact(), group.binding.selection())
    }));
    let roles = CompilerInputProjection::from_issued_entries(&later.entries())
        .unwrap()
        .roles();
    let directory = tempfile::tempdir().unwrap();
    let products = recovery_artifacts::materialize_certified_products(
        directory.path(),
        [2; 32],
        &[product.clone()],
    )
    .unwrap();
    let interfaces = products
        .iter()
        .map(|product| product.module_interface.clone().unwrap())
        .collect::<Vec<_>>();
    let descriptors = later.descriptors();
    let encoded = serde_json::to_vec(&(selection, edges, roles)).unwrap();
    drop((carrier, initial, later, issuer, native, product));
    let (selection, edges, roles): (
        ArtifactGraphSelection,
        Vec<(ArtifactBindingNode, ArtifactBindingNode, ArtifactDependency)>,
        Vec<CompilerInputRole>,
    ) = serde_json::from_slice(&encoded).unwrap();
    let recovered = RecoveredArtifactInventory::capture_bound(
        directory.path(),
        &products,
        &interfaces,
        &[],
        &[],
        &descriptors,
        &selection.bindings,
        &edges,
    )
    .unwrap();
    let ArtifactPayload::Original(recovered_product) = &recovered.entries[&native_id].payload
    else {
        unreachable!()
    };
    let canonical = Arc::new(ArtifactEntry::canonical(
        recovered_product.module_interface().unwrap().clone(),
    ));
    let canonical_issuer = ArtifactInventory::default();
    let canonical_view = canonical_issuer
        .admit_shared(&canonical_issuer.empty_view(), vec![canonical])
        .unwrap();
    let canonical_selection = canonical_view.capture_graph_selection();
    let canonical_reference = products[0].module_interface.as_ref().unwrap();
    let mut acquisition = RecoveredArtifactAcquisition::new(directory.path());
    acquisition
        .capture_bound(
            &[],
            std::slice::from_ref(canonical_reference),
            &[],
            &[],
            &canonical_view.descriptors(),
            &canonical_selection.bindings,
            &canonical_view.binding_dependencies(),
        )
        .unwrap();
    let canonical_path = directory.path().join(&canonical_reference.certificate_path);
    let canonical_bytes = std::fs::read(&canonical_path).unwrap();
    std::fs::write(
        &canonical_path,
        b"held canonical certificate later corrupted",
    )
    .unwrap();
    let acquired_native = acquisition
        .capture_bound(
            &products,
            &interfaces,
            &[],
            &[],
            &descriptors,
            &selection.bindings,
            &edges,
        )
        .unwrap();
    assert_eq!(
        acquired_native
            .context_with_graph_selection(&selection, &roles, vec![])
            .unwrap()
            .artifact_view()
            .capture_graph_selection(),
        selection
    );
    assert!(RecoveredArtifactInventory::capture_bound(
        directory.path(),
        &products,
        &interfaces,
        &[],
        &[],
        &descriptors,
        &selection.bindings,
        &edges
    )
    .is_err());
    std::fs::write(&canonical_path, canonical_bytes).unwrap();
    let restore = || {
        recovered
            .context_with_graph_selection(&selection, &roles, vec![])
            .unwrap()
    };
    let restored = restore();
    assert_eq!(
        restored.artifact_view().capture_graph_selection(),
        selection
    );
    restored
        .artifact_view()
        .native_requirements_from_roots(&[NativeRequirementRoot::Group {
            artifact: native_id,
            original_ordinal: 7,
        }])
        .unwrap();
    let before = restored.artifact_view().binding_dependencies();
    let mut missing = selection.clone();
    let carrier = edges
        .iter()
        .find_map(|(from, to, dependency)| {
            (*from == ArtifactBindingNode::Group(group)
                && *dependency == ArtifactDependency::Interface)
                .then_some(*to)
        })
        .unwrap();
    let ArtifactBindingNode::Artifact(carrier) = carrier else {
        unreachable!()
    };
    missing.bindings.retain(|binding| *binding != carrier);
    assert!(recovered
        .context_with_graph_selection(&missing, &roles, vec![])
        .is_err());
    let mut wrong = selection.clone();
    let selected = wrong
        .native_groups
        .iter_mut()
        .find(|selected| **selected == group)
        .unwrap();
    let mut decoded = serde_json::to_value(*selected).unwrap();
    decoded["binding"]["selection"] = serde_json::to_value(carrier.selection).unwrap();
    *selected = serde_json::from_value(decoded).unwrap();
    assert!(recovered
        .context_with_graph_selection(&wrong, &roles, vec![])
        .is_err());
    let mut altered_edges = edges.clone();
    let edge = altered_edges
        .iter_mut()
        .find(|(from, _, _)| *from == ArtifactBindingNode::Group(group))
        .unwrap();
    let ArtifactBindingNode::Group(altered) = &mut edge.0 else {
        unreachable!()
    };
    altered.original_ordinal = u32::MAX;
    assert!(RecoveredArtifactInventory::capture_bound(
        directory.path(),
        &products,
        &interfaces,
        &[],
        &[],
        &descriptors,
        &selection.bindings,
        &altered_edges,
    )
    .is_err());
    assert_eq!(restored.artifact_view().binding_dependencies(), before);
    drop(restored);
    assert_eq!(
        restore().artifact_view().capture_graph_selection(),
        selection
    );
}

proptest::proptest! {
    #![proptest_config({
        let mut config = proptest::test_runner::Config::default();
        config.cases = 64;
        if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
            config.failure_persistence = Some(Box::new(proptest::test_runner::FileFailurePersistence::Direct(path)));
        }
        config
    })]
    #[test]
    fn bound_recovery_history_retains_exact_closure_and_refusal_atomicity(
        history in proptest::collection::vec((proptest::bool::ANY, 0u8..7), 1..33)
    ) {
        let pair = bound_recovery_pair_fixture();
        let mut acquisition = RecoveredArtifactAcquisition::new(pair.directory.path());
        let mut held = [Some(pair.restore_acquired(&mut acquisition, 0, &pair.selections[0]).unwrap()),
            Some(pair.restore_acquired(&mut acquisition, 1, &pair.selections[1]).unwrap())];
        let mut seen = [true; 2];
        let mut actions = [0usize; 7];
        let mut scopes = [0usize; 2];
        for (new, action) in &history { actions[usize::from(*action)] += 1; scopes[usize::from(*new)] += 1; }
        eprintln!("bound-recovery-history steps={} scopes={scopes:?} actions={actions:?}", history.len());
        for (new, action) in history {
            let index = usize::from(new);
            match action {
                0 => held[index] = None,
                1 => {
                    let mut missing = pair.selections[index].clone();
                    missing.bindings.pop();
                    proptest::prop_assert!(pair.restore_acquired(&mut acquisition, index, &missing).is_err());
                }
                2 => {
                    let mut wrong = pair.selections[index].clone();
                    wrong.namespace[0] = pair.selections[1-index].bindings[0];
                    proptest::prop_assert!(pair.restore_acquired(&mut acquisition, index, &wrong).is_err());
                }
                3 => {
                    let mut missing = pair.selections[index].clone();
                    missing.selections.clear();
                    proptest::prop_assert!(pair.restore_acquired(&mut acquisition, index, &missing).is_err());
                }
                4 => {
                    let mut changed = pair.selections[index].clone();
                    Arc::make_mut(&mut changed.selections[0]).selection.0[0] ^= 1;
                    proptest::prop_assert!(pair.restore_acquired(&mut acquisition, index, &changed).is_err());
                }
                5 => {
                    held[index] = Some(pair.restore_acquired(&mut acquisition, index, &pair.selections[index]).unwrap());
                    seen[index] = true;
                }
                _ => {
                    acquisition = RecoveredArtifactAcquisition::new(pair.directory.path());
                    seen = [false; 2];
                    held[index] = Some(pair.restore_acquired(&mut acquisition, index, &pair.selections[index]).unwrap());
                    seen[index] = true;
                    proptest::prop_assert!(acquisition.work().read_bytes > 0);
                }
            }
            proptest::prop_assert_eq!(acquisition.payloads.len(), 1 + seen.into_iter().filter(|value| *value).count());
            for (view_index, context) in held.iter().enumerate() {
                if let Some(context) = context {
                    let closure = context.artifact_view().select_roots(vec![pair.root_id]).unwrap()
                        .artifact_ids().into_iter().collect::<BTreeSet<_>>();
                    let child = if view_index == 0 { pair.old_id } else { pair.new_id };
                    proptest::prop_assert_eq!(closure, BTreeSet::from([pair.root_id, child]));
                    proptest::prop_assert_eq!(context.artifact_view().capture_graph_selection(),
                        pair.selections[view_index].clone());
                }
            }
        }
    }
}

#[test]
fn lexical_composition_retains_shared_owners_idempotently() {
    let node = ExactLexicalNode {
        owner: identity("fixture", "Consumer"),
        imports: vec![identity("fixture", "Second"), identity("fixture", "First")],
    };
    let mut reordered = node.clone();
    reordered.imports.reverse();
    let merged = compose_lexical_nodes([&node, &reordered]).unwrap();
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].owner, node.owner);
    assert_eq!(merged[0].imports, reordered.imports);
    assert_eq!(
        compose_lexical_nodes(merged.iter().chain(merged.iter())).unwrap(),
        merged
    );
}

#[test]
fn lexical_composition_refuses_conflicting_or_duplicate_edges() {
    let node = ExactLexicalNode {
        owner: identity("fixture", "Consumer"),
        imports: vec![identity("fixture", "First")],
    };
    let mut conflicting = node.clone();
    conflicting.imports.push(identity("fixture", "Second"));
    assert!(compose_lexical_nodes([&node, &conflicting]).is_err());
    assert!(compose_lexical_nodes([&conflicting, &node]).is_err());
    let mut duplicate = node.clone();
    duplicate.imports.push(duplicate.imports[0].clone());
    assert!(compose_lexical_nodes([&duplicate]).is_err());
}

#[test]
fn pure_preview_refuses_explicit_heap_inputs_and_omits_unrelated_certified_tags() {
    let retained = crate::certified_products::PendingImportOwner::Retained {
        identity: tidepool_repr::execution_schema::SymbolIdentity {
            unit: "main".into(),
            module: "Tidepool.Session.Val.G7".into(),
            namespace: "value".into(),
            occurrence: "groupStore".into(),
            record_parent: None,
        },
        generation: 7,
    };
    let native = retained_generation_inputs(
        RetainedGenerationPolicy::PreserveCertifiedDemand,
        BTreeMap::new(),
        [&retained].into_iter(),
    )
    .unwrap();
    assert_eq!(native.len(), 1, "ordinary native demand is preserved");
    assert!(retained_generation_inputs(
        RetainedGenerationPolicy::PureActivationPreview,
        BTreeMap::new(),
        [&retained].into_iter(),
    )
    .unwrap()
    .is_empty());
    assert!(
        retained_generation_inputs(
            RetainedGenerationPolicy::PureActivationPreview,
            native,
            [&retained].into_iter(),
        )
        .is_err(),
        "an explicitly requested heap input cannot be silently removed"
    );
}

type PolicyKey = (u8, u8, u8, u8, Option<u8>);
type PolicyRow = (PolicyKey, u64, u8);
type PolicySemanticKey = (String, String, String, String, Option<String>);

fn policy_key(key: PolicyKey) -> PolicySemanticKey {
    (
        format!("unit{}", key.0),
        format!("Module{}", key.1),
        if key.2 == 0 { "value" } else { "constructor" }.into(),
        format!("binding{}", key.3),
        key.4.map(|parent| format!("Record{parent}")),
    )
}

fn policy_symbol(key: PolicyKey) -> tidepool_extract_cmd::SymbolIdentity {
    let (unit, module, namespace, occurrence, record_parent) = policy_key(key);
    tidepool_extract_cmd::SymbolIdentity {
        unit,
        module,
        namespace,
        occurrence,
        record_parent,
    }
}

fn policy_import(row: PolicyRow) -> crate::certified_products::PendingImportOwner {
    use crate::certified_products::PendingImportOwner;
    let (unit, module, namespace, occurrence, record_parent) = policy_key(row.0);
    let binder = tidepool_repr::execution_schema::SymbolIdentity {
        unit: unit.clone(),
        module: module.clone(),
        namespace,
        occurrence,
        record_parent,
    };
    match row.2 {
        0 => PendingImportOwner::Retained {
            identity: binder,
            generation: row.1,
        },
        1 => PendingImportOwner::RetainedPackage {
            unit,
            module,
            binder,
            generation: row.1,
            interface_digest: [1; 32],
        },
        2 => PendingImportOwner::Source {
            owner: tidepool_repr::execution_schema::CachedHomeOwner {
                unit,
                module,
                module_version: tidepool_repr::execution_schema::ModuleVersion([2; 32]),
                skinny_iface_sha256: [3; 32],
                product_sha256: [4; 32],
            },
            original_ordinal: 0,
            binder,
        },
        3 => PendingImportOwner::Package {
            unit,
            module,
            binder,
            interface_digest: [5; 32],
        },
        _ => unreachable!(),
    }
}

fn policy_property_config() -> proptest::test_runner::Config {
    let mut config = proptest::test_runner::Config::default();
    if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
        config.failure_persistence = Some(Box::new(
            proptest::test_runner::FileFailurePersistence::Direct(path),
        ));
    }
    config
}

proptest::proptest! {
    #![proptest_config(policy_property_config())]
    #[test]
    fn retained_purpose_matrix_matches_list_oracle_under_unrelated_and_repeated_imports(
        tail in proptest::collection::vec(
            ((0u8..3, 0u8..3, 0u8..2, 0u8..3, proptest::option::of(0u8..3)), 1u64..4, 0u8..4),
            0..20,
        ),
        explicit in proptest::collection::vec(
            ((0u8..3, 0u8..3, 0u8..2, 0u8..3, proptest::option::of(0u8..3)), 1u64..4),
            0..8,
        ),
    ) {
        // Representation-only policy rows issue no certificate or site authority.
        // Every case includes both retained roles, ignored conflicting rows,
        // exact duplication, and each identity field distinguished independently.
        let first = (0, 0, 0, 0, None);
        let package = (1, 1, 0, 1, None);
        let mut rows = vec![
            (first, 1, 0), (package, 2, 1), (first, 3, 2),
            (package, 3, 3), (first, 1, 0), ((0, 0, 0, 0, Some(0)), 2, 0),
            ((1, 0, 0, 0, None), 2, 1),
            ((0, 1, 0, 0, None), 3, 0),
            ((0, 0, 1, 0, None), 2, 0),
            ((0, 0, 0, 1, None), 3, 1),
        ];
        rows.extend(tail);
        let explicit: BTreeMap<_, _> = explicit.into_iter()
            .map(|(key, generation)| (policy_symbol(key), generation)).collect();
        for request in [
            BTreeMap::new(), explicit,
            BTreeMap::from([(policy_symbol(first), 3)]),
            BTreeMap::from([(policy_symbol(package), 1)]),
        ] {
            // Independent oracle scans semantic pairs; it never calls the
            // production union routine or constructs admitted native evidence.
            let mut pairs = request.iter().map(|(key, generation)| (
                (key.unit.clone(), key.module.clone(), key.namespace.clone(),
                    key.occurrence.clone(), key.record_parent.clone()), *generation,
            )).collect::<Vec<_>>();
            pairs.extend(rows.iter().filter(|row| row.2 < 2)
                .map(|row| (policy_key(row.0), row.1)));
            let conflicts = pairs.iter().any(|(key, generation)| pairs.iter()
                .any(|(other, value)| key == other && generation != value));
            pairs.sort();
            pairs.dedup();
            for order in [rows.clone(), rows.iter().copied().rev().collect(), rows.repeat(2)] {
                let imports = order.into_iter().map(policy_import).collect::<Vec<_>>();
                let native = retained_generation_inputs(
                    RetainedGenerationPolicy::PreserveCertifiedDemand,
                    request.clone(), imports.iter(),
                );
                proptest::prop_assert_eq!(native.is_err(), conflicts);
                if let Ok(native) = native {
                    let mut actual = native.into_iter().map(|(key, generation)| (
                        (key.unit, key.module, key.namespace, key.occurrence, key.record_parent), generation,
                    )).collect::<Vec<_>>();
                    actual.sort();
                    proptest::prop_assert_eq!(&actual, &pairs);
                }
                let preview = retained_generation_inputs(
                    RetainedGenerationPolicy::PureActivationPreview,
                    request.clone(), imports.iter(),
                );
                proptest::prop_assert_eq!(preview.is_err(), !request.is_empty());
                if let Ok(preview) = preview {
                    proptest::prop_assert!(preview.is_empty());
                }
            }
        }
    }
}

#[test]
fn certified_demand_tags_preserve_full_identity_and_refuse_generation_conflicts() {
    use crate::certified_products::PendingImportOwner;
    let identity = tidepool_repr::execution_schema::SymbolIdentity {
        unit: "main".into(),
        module: "Tidepool.Session.Val.G1".into(),
        namespace: "value".into(),
        occurrence: "x".into(),
        record_parent: None,
    };
    let retained = PendingImportOwner::Retained {
        identity: identity.clone(),
        generation: 1,
    };
    let mut package_identity = identity.clone();
    package_identity.unit = "package".into();
    package_identity.module = "PackageModule".into();
    let package = PendingImportOwner::RetainedPackage {
        unit: package_identity.unit.clone(),
        module: package_identity.module.clone(),
        binder: package_identity,
        generation: 3,
        interface_digest: [4; 32],
    };
    let tags =
        certified_retained_generation_tags(BTreeMap::new(), [&retained, &package].into_iter())
            .unwrap();
    assert_eq!(tags.len(), 2);
    assert!(certified_retained_generation_tags(tags.clone(), [&retained].into_iter()).is_ok());
    let conflict = PendingImportOwner::Retained {
        identity: identity.clone(),
        generation: 2,
    };
    assert!(certified_retained_generation_tags(tags, [&conflict].into_iter()).is_err());
    assert!(certified_retained_generation_tags(
        BTreeMap::new(),
        [&retained, &conflict].into_iter(),
    )
    .is_err());
    let mut distinct = identity;
    distinct.record_parent = Some("Record".into());
    let distinct = PendingImportOwner::Retained {
        identity: distinct,
        generation: 2,
    };
    assert_eq!(
        certified_retained_generation_tags(BTreeMap::new(), [&retained, &distinct].into_iter(),)
            .unwrap()
            .len(),
        2,
    );
}
use sha2::{Digest, Sha256};
use tidepool_repr::execution_schema::{CachedHomeOwner, ModuleVersion};

fn support_product(module: &str) -> CertifiedRecoveryProduct {
    support_product_in_unit("fixture", module)
}

fn support_product_in_unit(unit: &str, module: &str) -> CertifiedRecoveryProduct {
    support_product_with_interface(unit, module, format!("{module} interface").into_bytes())
}

fn scaffold_native_fixture(
    producer: [u8; 32],
    unit: &str,
    module: &str,
    version: u8,
) -> CertifiedRecoveryProduct {
    let product = support_product_in_unit(unit, module);
    let mut owner = product.owner().clone();
    owner.module_version = ModuleVersion([version; 32]);
    let certification =
        crate::certified_products::encode_home_certification(&owner, &[], &BTreeMap::new())
            .unwrap();
    let product = CertifiedRecoveryProduct::from_certification(
        owner,
        product.interface_bytes().to_vec(),
        product.product_bytes().to_vec(),
        product.package_imports_bytes().to_vec(),
        certification,
    );
    crate::certified_products::tests::recovered_witness_fixtures(&[
        crate::certified_products::fixture_finalized_product(product, producer),
    ])
    .remove(0)
    .product
}

fn support_product_with_interface(
    unit: &str,
    module: &str,
    interface: Vec<u8>,
) -> CertifiedRecoveryProduct {
    let product = tidepool_test_data::prepared_encode::encode_module_products(&[
        tidepool_repr::execution_schema::RawModuleProduct {
            unit: unit.into(),
            module: module.into(),
            interface: interface.clone(),
            groups: Vec::new(),
        },
    ]);
    let owner = CachedHomeOwner {
        unit: unit.into(),
        module: module.into(),
        module_version: ModuleVersion([1; 32]),
        skinny_iface_sha256: Sha256::digest(&interface).into(),
        product_sha256: Sha256::digest(&product).into(),
    };
    let certification =
        crate::certified_products::encode_home_certification(&owner, &[], &BTreeMap::new())
            .unwrap();
    let mut packages = Vec::new();
    ciborium::ser::into_writer(
        &Value::Array(vec![
            text("TPPKGROOTS"),
            text("2"),
            Value::Array(vec![
                text(&owner.unit),
                text(&owner.module),
                text(sha256(&interface)),
            ]),
            Value::Array(vec![]),
            Value::Array(vec![]),
        ]),
        &mut packages,
    )
    .unwrap();
    crate::certified_products::tests::recovered_witness_fixtures(&[
        crate::certified_products::fixture_finalized_product(
            CertifiedRecoveryProduct::from_certification(
                owner,
                interface,
                product,
                packages,
                certification,
            ),
            [2; 32],
        ),
    ])
    .remove(0)
    .product
}

// Retain the old identity encoder to prove selection-aware keys invalidate it.
fn legacy_semantic_sha256(context: &ExactDeclarationContext) -> [u8; 32] {
    use sha2::Digest;
    let mut lexical = context.owner.facts.lexical.iter().collect::<Vec<_>>();
    lexical.sort_by_key(|node| &node.owner);
    let value = Value::Array(vec![
        text("TPEXACTCONTEXT"),
        text("2"),
        text(hex(&sha2::Sha256::digest(
            serde_json::to_vec(&(
                context
                    .owner
                    .facts
                    .compiler_support
                    .artifacts()
                    .descriptors(),
                context
                    .owner
                    .facts
                    .compiler_support
                    .artifacts()
                    .dependencies(),
            ))
            .expect("inventory encoding"),
        )
        .into())),
        text(hex(&context.owner.facts.producer)),
        Value::Array(
            context
                .recovery_products()
                .iter()
                .map(|product| {
                    let owner = product.owner();
                    Value::Array(vec![
                        text(&owner.unit),
                        text(&owner.module),
                        text(hex(&owner.module_version.0)),
                        text(hex(&owner.skinny_iface_sha256)),
                        text(hex(&owner.product_sha256)),
                        text(sha256(product.package_imports_bytes())),
                        text(sha256(product.certification_bytes())),
                    ])
                })
                .collect(),
        ),
        Value::Array(
            context
                .joined_interfaces()
                .iter()
                .map(|join| {
                    Value::Array(vec![
                        text(join.unit()),
                        text(join.module()),
                        text(sha256(join.interface_bytes())),
                        text(sha256(join.package_imports_bytes())),
                    ])
                })
                .collect(),
        ),
        Value::Array(
            lexical
                .into_iter()
                .map(|node| {
                    let mut imports = node.imports.iter().collect::<Vec<_>>();
                    imports.sort();
                    Value::Array(vec![
                        module_value(&node.owner),
                        Value::Array(imports.into_iter().map(module_value).collect()),
                    ])
                })
                .collect(),
        ),
    ]);
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(&value, &mut bytes).expect("owned value encoding");
    sha2::Sha256::digest(bytes).into()
}

#[test]
fn metadata_identity_invalidates_preselection_encoding_and_normalizes_lexical_order() {
    let (context, _) = metadata_fixture();
    assert_ne!(context.semantic_sha256(), legacy_semantic_sha256(&context));
    let mut reordered = context.as_ref().clone();
    edit_lexical(&mut reordered, |nodes| nodes.reverse());
    assert_eq!(reordered.semantic_sha256(), context.semantic_sha256());
    assert_ne!(
        reordered.semantic_sha256(),
        legacy_semantic_sha256(&reordered)
    );
    let extended = context
        .as_ref()
        .clone()
        .extend_checked_original_products(
            context.owner.facts.producer,
            &[crate::certified_products::fixture_finalized_product(
                support_product("Later"),
                context.owner.facts.producer,
            )],
        )
        .unwrap();
    assert_ne!(extended.semantic_sha256(), context.semantic_sha256());
    assert_ne!(
        extended.semantic_sha256(),
        legacy_semantic_sha256(&extended)
    );
}

fn support_view(products: &[CertifiedRecoveryProduct]) -> ArtifactView {
    let producer = products
        .first()
        .and_then(|product| product.module_interface())
        .map_or([2; 32], |interface| interface.producer_sha256());
    certified_product_artifact_view(producer, products, &[], None).unwrap()
}

struct CompilerSupportFixture {
    artifacts: ArtifactView,
    projection: CompilerInputProjection,
}

fn support_offer(products: &[CertifiedRecoveryProduct]) -> CompilerSupportFixture {
    let producer = products
        .first()
        .and_then(|product| product.module_interface())
        .map_or([2; 32], |interface| interface.producer_sha256());
    let entries = products
        .iter()
        .map(|product| Arc::new(ArtifactEntry::original(producer, product.clone()).unwrap()))
        .collect::<Vec<_>>();
    let projection = CompilerInputProjection::from_issued_entries(&entries).unwrap();
    CompilerSupportFixture {
        artifacts: support_view(products),
        projection,
    }
}

impl ExactCompilationRequest {
    fn admit_fixture_support(
        &mut self,
        context: Arc<ExactDeclarationContext>,
        fixture: &CompilerSupportFixture,
        admissions: &[ExactSourceAdmission],
        produced_types: Option<&crate::checked_cell::ProducedValueTypeInterfaces>,
    ) -> Result<Arc<ExactDeclarationContext>, CompileError> {
        let selection =
            crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                &fixture.projection,
                &fixture.artifacts.metadata_snapshot(),
                &tidepool_repr::execution_schema::InventoryOperation::new(Default::default()),
            )
            .map_err(compiler_evidence_failure)?;
        self.admit_program_support_with_selection(
            context,
            &fixture.artifacts,
            admissions,
            produced_types,
            &selection,
        )
    }

    fn admit_fixture_segment_support(
        &mut self,
        context: Arc<ExactDeclarationContext>,
        fixture: &CompilerSupportFixture,
        segment: &ExactProgramSegmentAdmission,
        produced_types: Option<&crate::checked_cell::ProducedValueTypeInterfaces>,
    ) -> Result<Arc<ExactDeclarationContext>, CompileError> {
        let selection =
            crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                &fixture.projection,
                &fixture.artifacts.metadata_snapshot(),
                &tidepool_repr::execution_schema::InventoryOperation::new(Default::default()),
            )
            .map_err(compiler_evidence_failure)?;
        self.admit_program_segment_support_with_selection(
            context,
            &fixture.artifacts,
            segment,
            produced_types,
            &selection,
        )
    }
}

#[test]
fn program_support_requires_explicit_authenticated_original_offer() {
    let directory = tempfile::tempdir().unwrap();
    let empty = Arc::new(ExactDeclarationContext::new(&[], &[], vec![]).unwrap());
    let product = support_product("Hidden");
    let offer = support_offer(std::slice::from_ref(&product));
    let mut request = program_request(directory.path(), empty.clone());
    let native = request
        .admit_fixture_support(empty.clone(), &offer, &[], None)
        .unwrap();
    assert_eq!(native.recovery_products().len(), 1);
    assert!(native.compiler_original_products().unwrap().is_empty());
    assert!(native
        .compiler_input_roles()
        .iter()
        .all(|role| matches!(role, CompilerInputRole::InterfaceOnly { .. })));
    assert!(native.lexical_graph().is_empty());
    assert!(request.program_support.is_none());
    assert!(request.program_source_lexical().is_empty());

    let types = CompilerSupportFixture {
        projection: CompilerInputProjection::from_issued_entries(&[Arc::new(
            ArtifactEntry::canonical(product.module_interface().unwrap().clone()),
        )])
        .unwrap(),
        artifacts: offer.artifacts.clone(),
    };
    let mut request = program_request(directory.path(), empty.clone());
    let retained = request
        .admit_fixture_support(empty.clone(), &types, &[], None)
        .unwrap();
    assert_eq!(retained.recovery_products().len(), 1);
    assert!(retained.compiler_original_products().unwrap().is_empty());
    assert!(request.program_support.is_none());
    assert!(retained.lexical_graph().is_empty());

    // Authenticated source admissions select implementations independently
    // of the otherwise identical native custody-only support operation.
    let source_offer = support_offer(&[
        support_product("InstanceOwner"),
        support_product("InstanceRelay"),
    ]);
    let mut request = program_request(directory.path(), empty.clone());
    let admitted = request
        .admit_fixture_support(
            empty.clone(),
            &source_offer,
            &[support_admission(directory.path())],
            None,
        )
        .unwrap();
    assert_eq!(admitted.compiler_original_products().unwrap().len(), 2);
    assert_eq!(
        admitted.compiler_input_roles(),
        source_offer.projection.roles()
    );
    assert!(request.program_support.is_some());
    assert_eq!(request.program_source_lexical().len(), 2);
    assert!(empty.artifact_view().is_empty());

    let uncertified = CertifiedRecoveryProduct::from_certification(
        product.owner().clone(),
        product.interface_bytes().to_vec(),
        product.product_bytes().to_vec(),
        product.package_imports_bytes().to_vec(),
        product.certification_bytes().to_vec(),
    )
    .with_module_interface(product.module_interface().unwrap().clone())
    .unwrap();
    let mut request = program_request(directory.path(), empty.clone());
    assert!(uncertified.original_native().is_none());
    let retained = request
        .admit_fixture_support(empty.clone(), &support_offer(&[uncertified]), &[], None)
        .unwrap();
    assert!(retained.compiler_original_products().unwrap().is_empty());
    assert!(retained
        .compiler_input_roles()
        .iter()
        .all(|role| matches!(role, CompilerInputRole::InterfaceOnly { .. })));
    assert!(request.program_support.is_none());
    assert!(empty.artifact_view().is_empty());
}

#[test]
fn program_support_omits_only_authenticated_generated_original() {
    let directory = tempfile::tempdir().unwrap();
    let empty = Arc::new(ExactDeclarationContext::new(&[], &[], vec![]).unwrap());
    let before = empty.semantic_sha256();
    let selection = |fixture: &CompilerSupportFixture| {
        crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
            &fixture.projection,
            &fixture.artifacts.metadata_snapshot(),
            &tidepool_repr::execution_schema::InventoryOperation::new(Default::default()),
        )
        .unwrap()
    };
    let missing = support_offer(&[support_product("Hidden")]);
    let mut request = program_request(directory.path(), empty.clone());
    let refusal = request
        .admit_program_support_with_selection(
            empty.clone(),
            &support_view(&[]),
            &[],
            None,
            &selection(&missing),
        )
        .unwrap_err();
    assert!(matches!(refusal, CompileError::CompilerEvidence(_)));
    assert!(request.program_support.is_none());
    assert!(request.program_source_lexical().is_empty());
    assert_eq!(empty.semantic_sha256(), before);
    assert!(empty.artifact_view().is_empty());

    let receipt = import_receipt(directory.path(), &request, "Unadmitted");
    let mut value = read_receipt(&receipt);
    value.as_array_mut().unwrap()[8].as_array_mut().unwrap()[0]
        .as_array_mut()
        .unwrap()[3] = Value::Array(vec![]);
    write_receipt(&receipt, &value);
    let admission = request.validate_receipt(&receipt, None, &empty).unwrap();
    assert_eq!(
        admission.generated_source_owner().unwrap(),
        identity("fixture", "Consumer")
    );
    let source = std::fs::read(directory.path().join("Consumer.hs")).unwrap();
    let generated = crate::certified_products::fixture_source_finalized_product(
        support_product("Consumer").with_source_sha256(Sha256::digest(&source).into()),
        [2; 32],
        vec![],
    );
    let generated = crate::certified_products::tests::recovered_witness_fixtures(&[generated])
        .remove(0)
        .product;
    let omitted = request
        .admit_program_support_with_selection(
            empty.clone(),
            &support_view(&[]),
            std::slice::from_ref(&admission),
            None,
            &selection(&support_offer(&[generated.clone()])),
        )
        .unwrap();
    assert_eq!(omitted.owner.facts.producer, request.producer_sha256);
    assert!(omitted.artifact_view().is_empty());
    assert!(omitted.compiler_original_products().unwrap().is_empty());
    assert_eq!(empty.semantic_sha256(), before);
    assert!(request.program_support.is_none());
    assert!(request.program_source_lexical().is_empty());

    let source_text = std::str::from_utf8(&source).unwrap();
    let hidden = support_product("Hidden");
    let issued = support_offer(&[generated.clone(), hidden.clone()]);
    let private =
        OriginalCompilerInputs::from_selection(&selection(&issued), &issued.artifacts).unwrap();
    let support = support_view(std::slice::from_ref(&hidden));
    let continued = private
        .for_program_continuation(&support, std::slice::from_ref(&admission), source_text)
        .unwrap();
    assert!(continued
        .artifacts
        .entries()
        .iter()
        .all(|entry| entry.descriptor.owner != admission.generated_source_owner().unwrap()));
    assert_eq!(
        continued.artifacts.capture_graph_selection().roots,
        support.capture_graph_selection().roots
    );
    assert_eq!(
        continued.artifacts.capture_graph_selection().namespace,
        support.capture_graph_selection().namespace
    );
    assert_eq!(
        continued.projection.roles(),
        support_offer(std::slice::from_ref(&hidden))
            .projection
            .roles()
    );
    assert!(
        private
            .for_program_continuation(&support, &[], source_text)
            .is_err(),
        "source spelling alone cannot identify the generated original for exclusion"
    );
    assert!(
        private
            .for_program_continuation(
                &support_view(&[]),
                std::slice::from_ref(&admission),
                source_text
            )
            .is_err(),
        "continuation must retain every issued non-generated original role"
    );
    let hidden_issuance = support_offer(std::slice::from_ref(&hidden));
    let hidden_support =
        recover_fixture_compiler_support(hidden_issuance.projection, support.clone(), []).unwrap();
    let types = hidden_support
        .selected_interfaces()
        .unwrap()
        .select_owners(vec![identity("fixture", "Hidden")])
        .unwrap();
    assert!(
        private
            .for_program_continuation(&types, std::slice::from_ref(&admission), source_text)
            .is_err(),
        "retaining an issued interface does not preserve its native role"
    );
    let variant = scaffold_native_fixture([2; 32], "fixture", "Hidden", 99);
    assert!(
        private
            .for_program_continuation(
                &support_view(&[variant]),
                std::slice::from_ref(&admission),
                source_text
            )
            .is_err(),
        "same-named other native version cannot satisfy the exact issued role"
    );
    let foreign = support_product_with_interface(
        "foreign",
        "Consumer",
        b"foreign:Consumer interface".to_vec(),
    );
    assert_ne!(
        generated.owner().skinny_iface_sha256,
        foreign.owner().skinny_iface_sha256,
    );
    let issued = support_offer(&[generated, hidden.clone(), foreign.clone()]);
    let private =
        OriginalCompilerInputs::from_selection(&selection(&issued), &issued.artifacts).unwrap();
    assert!(
        private
            .for_program_continuation(&support, std::slice::from_ref(&admission), source_text)
            .is_err(),
        "the authenticated generated owner's module name cannot omit a foreign-unit role"
    );
    let preserved = private
        .for_program_continuation(
            &support_view(&[hidden, foreign]),
            std::slice::from_ref(&admission),
            source_text,
        )
        .unwrap();
    assert_eq!(preserved.projection.roles().len(), 2);
    assert!(
        issued
            .artifacts
            .descriptors()
            .iter()
            .any(|entry| entry.owner == identity("fixture", "Consumer")),
        "continuation refusal and exclusion do not mutate original custody"
    );

    for (unit, module) in [("foreign", "Consumer"), ("fixture", "AnotherConsumer")] {
        let offer = support_offer(&[support_product_in_unit(unit, module)]);
        let mut request = program_request(directory.path(), empty.clone());
        assert!(request
            .admit_program_support_with_selection(
                empty.clone(),
                &support_view(&[]),
                std::slice::from_ref(&admission),
                None,
                &selection(&offer),
            )
            .is_err());
        assert!(request.program_support.is_none());
        assert!(request.program_source_lexical().is_empty());
        assert_eq!(request.context().semantic_sha256(), before);
        assert_eq!(empty.semantic_sha256(), before);
        assert!(empty.artifact_view().is_empty());
    }
}

fn support_admission(root: &Path) -> ExactSourceAdmission {
    let context = Arc::new(ExactDeclarationContext::new(&[], &[], vec![]).unwrap());
    let request = program_request(root, context);
    let receipt = support_receipt(root, &request);
    request
        .validate_receipt(&receipt, None, request.context())
        .unwrap()
}

fn support_receipt(root: &Path, request: &ExactCompilationRequest) -> PathBuf {
    let modules = [
        ("InstanceOwner", None),
        ("InstanceRelay", Some("InstanceOwner")),
    ];
    let mut sources = Vec::new();
    let mut nodes = Vec::new();
    for (module, dependency) in modules {
        let path = root.join(format!("{module}.hs"));
        let source = format!("module {module} where\n");
        std::fs::write(&path, &source).unwrap();
        sources.push(crate::cache::SourceEvidence {
            path: path.clone(),
            sha256: sha256(source.as_bytes()),
        });
        nodes.push(crate::cache::ModuleEvidence {
            unit: "fixture".into(),
            module: module.into(),
            boot: false,
            source: path,
            imports: dependency
                .into_iter()
                .map(|dependency| crate::cache::ModuleImportEvidence {
                    qualifier: crate::cache::ImportQualifier::Unqualified,
                    module: dependency.into(),
                    boot: false,
                    selected: Some(root.join(format!("{dependency}.hs"))),
                })
                .collect(),
            product: crate::cache::ProductAvailability::Ready,
        });
    }
    let source = "module Target where\n";
    let target = root.join("Target.hs");
    std::fs::write(&target, source).unwrap();
    sources.push(crate::cache::SourceEvidence {
        path: target.clone(),
        sha256: sha256(source.as_bytes()),
    });
    let evidence = crate::cache::DependencyEvidence {
        version: 4,
        cache_safe: true,
        selection_complete: true,
        sources,
        resolutions: vec![crate::cache::ResolutionEvidence {
            qualifier: crate::cache::ImportQualifier::Unqualified,
            module: "InstanceOwner".into(),
            boot: false,
            selected: Some(root.join("InstanceOwner.hs")),
            candidates: vec![root.join("InstanceOwner.hs")],
        }],
        packages: vec![],
        modules: nodes,
    };
    let directory = root.join("support-receipt");
    std::fs::create_dir_all(&directory).unwrap();
    let snapshot = directory.join("source.hs");
    std::fs::write(&snapshot, source).unwrap();
    let receipt = directory.join("receipt.cbor");
    let edges = evidence
        .modules
        .iter()
        .map(|module| {
            Value::Array(vec![
                text(&module.unit),
                text(&module.module),
                Value::Bool(module.boot),
                Value::Array(vec![]),
            ])
        })
        .collect();
    let value = Value::Array(vec![
        text("TPEXACTCOMPILE"),
        text("3"),
        text(&request.request_sha256),
        text(hex(&request.semantic_sha256)),
        path_value(&target).unwrap(),
        text(sha256(source.as_bytes())),
        path_value(&snapshot).unwrap(),
        text(serde_json::to_string(&evidence).unwrap()),
        Value::Array(edges),
        Value::Array(vec![Value::Array(vec![]), Value::Null]),
    ]);
    write_receipt(&receipt, &value);
    receipt
}

fn edit_lexical(
    context: &mut ExactDeclarationContext,
    edit: impl FnOnce(&mut Vec<ExactLexicalNode>),
) {
    let mut nodes = context.owner.facts.lexical.to_vec();
    edit(&mut nodes);
    {
        let updated = context.owner.facts.lexical.replace(nodes).unwrap();
        *context = context.clone().rebuild(|facts| facts.lexical = updated);
    }
}

fn metadata_fixture() -> (Arc<ExactDeclarationContext>, Vec<u8>) {
    let producer = b"metadata producer".to_vec();
    let producer_sha256 = Sha256::digest(&producer).into();
    let interface = |module: &str| {
        let bytes = format!("{module} interface").into_bytes();
        let mut packages = Vec::new();
        ciborium::ser::into_writer(
            &Value::Array(vec![
                text("TPPKGROOTS"),
                text("2"),
                Value::Array(vec![text("fixture"), text(module), text(sha256(&bytes))]),
                Value::Array(Vec::new()),
                Value::Array(vec![]),
            ]),
            &mut packages,
        )
        .unwrap();
        CertifiedJoinedInterface::from_certification(
            producer_sha256,
            "fixture".into(),
            module.into(),
            bytes,
            packages,
        )
        .unwrap()
    };
    let inventory = ArtifactInventory::default();
    let entries = vec![
        ArtifactEntry::original(
            producer_sha256,
            crate::certified_products::fixture_finalized_product(
                support_product("Alpha"),
                producer_sha256,
            ),
        )
        .unwrap(),
        ArtifactEntry::original(
            producer_sha256,
            crate::certified_products::fixture_finalized_product(
                support_product("Beta"),
                producer_sha256,
            ),
        )
        .unwrap(),
        ArtifactEntry::interface(
            interface("Joined"),
            JoinedInterfaceRole::LexicalJoin,
            vec![identity("fixture", "Alpha"), identity("fixture", "Beta")],
        ),
        ArtifactEntry::interface(
            interface("Value"),
            JoinedInterfaceRole::ValueInterface,
            vec![identity("fixture", "Alpha")],
        ),
    ]
    .into_iter()
    .map(Arc::new)
    .collect::<Vec<_>>();
    let compiler_projection = CompilerInputProjection::from_issued_entries(&entries).unwrap();
    let view = inventory
        .admit_shared(&inventory.empty_view(), entries)
        .unwrap();
    let context = ExactDeclarationContext::from_facts(DeclarationContextFacts {
        compiler_support: issue_fixture_compiler_support(compiler_projection, view).unwrap(),

        producer: producer_sha256,
        original_instance_environment: OriginalInstanceEnvironment::Unknown,
        template_imports: None,

        lexical: crate::exact_context::ExactLexicalSnapshot::default()
            .replace(vec![
                ExactLexicalNode {
                    owner: identity("fixture", "Joined"),
                    imports: vec![identity("fixture", "Alpha")],
                },
                ExactLexicalNode {
                    owner: identity("fixture", "Alpha"),
                    imports: Vec::new(),
                },
            ])
            .unwrap(),
    });
    context.normalize().unwrap();
    (Arc::new(context), producer)
}

#[test]
fn recovery_inspection_projection_refuses_interface_and_package_mutation() {
    let (context, _) = metadata_fixture();
    let inputs = context.recovery_inspection_inputs().unwrap();
    assert_eq!(inputs.0.artifacts.len(), 4);
    assert!(inputs
        .0
        .artifacts
        .iter()
        .all(|artifact| artifact.product.is_none()));
    let artifact = &inputs.0.artifacts[0];
    let path = &artifact.interface.path;
    let original = std::fs::read(path).unwrap();
    std::fs::write(path, b"corrupt interface").unwrap();
    assert!(inputs.0.validate().is_err());
    std::fs::write(path, original).unwrap();
    inputs.0.validate().unwrap();
    let mut package_path = path.as_os_str().to_os_string();
    package_path.push(".packages");
    std::fs::write(PathBuf::from(package_path), b"corrupt package witness").unwrap();
    assert!(inputs.0.validate().is_err());
}

#[test]
fn authenticated_execution_uses_issued_roles_not_available_interfaces() {
    let producer = [7; 32];
    let entries = ["Target", "A", "B", "C"].map(|module| {
        Arc::new(ArtifactEntry::canonical(
            crate::certified_products::fixture_module_interface(
                producer,
                "fixture",
                module,
                BTreeMap::new(),
            ),
        ))
    });
    let inventory = ArtifactInventory::default();
    let view = inventory
        .admit_shared(&inventory.empty_view(), entries.to_vec())
        .unwrap();
    let target = entries[0].descriptor.owner.clone();
    let dependencies = entries[1..]
        .iter()
        .map(|entry| entry.descriptor.owner.clone())
        .collect::<Vec<_>>();
    for mask in 0u8..8 {
        let issued_entries = std::iter::once(entries[0].clone())
            .chain(
                entries[1..]
                    .iter()
                    .enumerate()
                    .filter_map(|(index, entry)| (mask & (1 << index) != 0).then(|| entry.clone())),
            )
            .collect::<Vec<_>>();
        let projection = CompilerInputProjection::from_issued_entries(&issued_entries).unwrap();
        let lexical = std::iter::once(ExactLexicalNode {
            owner: target.clone(),
            imports: dependencies.clone(),
        })
        .chain(dependencies.iter().cloned().map(|owner| ExactLexicalNode {
            owner,
            imports: Vec::new(),
        }))
        .collect();
        let context = ExactDeclarationContext::from_authenticated_execution(
            producer,
            &view,
            projection.clone(),
            lexical,
            target.clone(),
            &dependencies,
        )
        .unwrap();
        assert_eq!(context.compiler_input_roles(), projection.roles());
        assert_eq!(context.artifact_view().artifact_ids(), view.artifact_ids());
        assert_eq!(
            context
                .artifact_view()
                .capture_graph_selection()
                .namespace
                .into_iter()
                .map(|binding| binding.artifact)
                .collect::<BTreeSet<_>>(),
            issued_entries
                .iter()
                .map(|entry| entry.descriptor.id)
                .collect(),
        );
        let missing = dependencies
            .iter()
            .enumerate()
            .filter_map(|(index, owner)| (mask & (1 << index) == 0).then(|| owner.clone()))
            .collect::<Vec<_>>();
        let expected = if missing.is_empty() {
            OriginalInstanceEnvironment::Complete {
                target: target.clone(),
            }
        } else {
            OriginalInstanceEnvironment::MissingOriginalOwners(missing)
        };
        assert_eq!(context.original_instance_environment(), &expected);
        assert_eq!(context.lexical_graph().len(), issued_entries.len());
    }
}

fn published_original_fixture() -> ExactDeclarationContext {
    let (base, _) = metadata_fixture();
    let target = identity("fixture", "PrivateSourceTarget");
    let canonical = ArtifactEntry::canonical(crate::certified_products::fixture_module_interface(
        base.owner.facts.producer,
        &target.unit,
        &target.module,
        BTreeMap::new(),
    ));
    let view = base
        .owner
        .facts
        .compiler_support
        .artifacts()
        .inventory()
        .admit(base.artifact_view(), vec![canonical])
        .unwrap();
    let projection = CompilerInputProjection::from_issued_entries(&view.entries()).unwrap();
    ExactDeclarationContext::from_authenticated_execution(
        base.owner.facts.producer,
        &view,
        projection,
        vec![
            ExactLexicalNode {
                owner: target.clone(),
                imports: vec![identity("fixture", "Alpha")],
            },
            ExactLexicalNode {
                owner: identity("fixture", "Alpha"),
                imports: vec![],
            },
        ],
        target.clone(),
        &[target, identity("fixture", "Alpha")],
    )
    .unwrap()
}

/// Share immutable authenticated artifacts while generated history remains case-local.
fn published_original_history_fixture() -> &'static ExactDeclarationContext {
    static FIXTURE: std::sync::OnceLock<ExactDeclarationContext> = std::sync::OnceLock::new();
    FIXTURE.get_or_init(published_original_fixture)
}

#[test]
fn context_role_observations_are_compact_and_bind_publication_policy() {
    let source = published_original_history_fixture();
    let root = identity("fixture", "Alpha");
    let selected = source
        .issue_published_source_original("revision", "input", &root)
        .unwrap();
    let context = selected.context();
    let roles = context.compiler_input_roles();
    let CompilerInputRole::PublishedSourceOriginal {
        interface,
        original,
        source_revision,
        original_input_identity,
        selection,
        selection_sha256,
    } = &roles[0]
    else {
        panic!("issuer must retain published policy")
    };
    let observations = context
        .owner
        .facts
        .compiler_support
        .projection()
        .semantic_observations()
        .unwrap();
    let compact = serde_json::to_vec(&observations).unwrap();
    let proof_transport = serde_json::to_vec(&roles).unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&compact).unwrap(),
        serde_json::json!([{
            "kind": "published_source_original",
            "interface": interface,
            "original": original,
            "source_revision": source_revision,
            "original_input_identity": original_input_identity,
            "selection": selection.observation_digest().unwrap(),
            "selection_sha256": selection_sha256,
        }])
    );
    assert!(
        compact.len() < 1024,
        "one role contains fixed identities and policy strings"
    );
    assert!(proof_transport.len() > compact.len());
    assert_eq!(
        compact,
        serde_json::to_vec(
            &context
                .owner
                .facts
                .compiler_support
                .projection()
                .semantic_observations()
                .unwrap()
        )
        .unwrap()
    );
    let digest = context.semantic_sha256();
    for (revision, input) in [("other-revision", "input"), ("revision", "other-input")] {
        let changed = source
            .issue_published_source_original(revision, input, &root)
            .unwrap();
        assert_ne!(changed.context().semantic_sha256(), digest);
    }
    let interface_only = context
        .as_ref()
        .clone()
        .recover_fixture_projection(
            context
                .owner
                .facts
                .compiler_support
                .projection()
                .interface_only(),
        )
        .unwrap();
    assert_ne!(interface_only.semantic_sha256(), digest);
    eprintln!(
        "context role observation: compact={} proof_transport={}",
        compact.len(),
        proof_transport.len()
    );
}

#[test]
fn selected_raw_materialization_skips_retained_bytes_and_refuses_missing_custody() {
    let source = published_original_history_fixture();
    let entries = source.artifact_view().entries();
    let original = |module: &str| {
        entries
            .iter()
            .find(|entry| {
                entry.descriptor.owner.module == module
                    && matches!(entry.payload, ArtifactPayload::Original(_))
            })
            .unwrap()
            .descriptor
            .id
    };
    let canonical = |module: &str| {
        entries
            .iter()
            .find(|entry| {
                entry.descriptor.owner.module == module
                    && matches!(entry.payload, ArtifactPayload::Canonical(_))
            })
            .unwrap()
            .descriptor
            .id
    };
    let scratch = tempfile::tempdir().unwrap();
    let absent = scratch.path().join("absent");
    assert_eq!(
        source
            .materialize_recovery_products_and_interfaces_for_artifacts(&absent, &BTreeSet::new())
            .unwrap(),
        (vec![], vec![])
    );
    assert!(
        !absent.exists(),
        "empty materialization does not create directories"
    );
    let (retained, _) = source
        .materialize_recovery_products_and_interfaces_for_artifacts(
            scratch.path(),
            &BTreeSet::from([original("Beta")]),
        )
        .unwrap();
    assert_eq!(retained.len(), 1);
    let retained_path = scratch.path().join(&retained[0].product_path);
    std::fs::write(&retained_path, b"damaged retained product").unwrap();
    let (new_products, new_interfaces) = source
        .materialize_recovery_products_and_interfaces_for_artifacts(
            scratch.path(),
            &BTreeSet::from([original("Alpha"), canonical("Alpha")]),
        )
        .unwrap();
    assert_eq!(new_products.len(), 1);
    assert_eq!(new_products[0].module, "Alpha");
    assert_eq!(
        new_interfaces,
        vec![new_products[0].module_interface.clone().unwrap()]
    );
    assert_eq!(
        std::fs::read(&retained_path).unwrap(),
        b"damaged retained product"
    );
    assert!(
        source
            .materialize_recovery_products_and_interfaces_for_artifacts(
                scratch.path(),
                &BTreeSet::from([original("Beta")])
            )
            .is_err(),
        "fresh materialization verifies its requested existing destination"
    );
    let selected = source
        .issue_published_source_original("revision", "input", &identity("fixture", "Alpha"))
        .unwrap();
    assert!(
        selected
            .context()
            .materialize_recovery_products_and_interfaces_for_artifacts(
                scratch.path(),
                &BTreeSet::from([original("Beta")])
            )
            .is_err(),
        "ambient inventory payload cannot supply a selected durable reference"
    );
    assert!(source
        .materialize_recovery_products_and_interfaces_for_artifacts(
            scratch.path(),
            &BTreeSet::from([ArtifactId([0xee; 32])])
        )
        .is_err());
}

#[test]
fn selected_interface_materialization_skips_retained_join_and_preserves_value_identity() {
    let source = published_original_history_fixture();
    let entries = source.artifact_view().entries();
    let joined = entries
        .iter()
        .find(|entry| entry.descriptor.owner.module == "Joined")
        .unwrap();
    let value = entries
        .iter()
        .find(|entry| entry.descriptor.owner.module == "Value")
        .unwrap();
    let ArtifactPayload::Interface(joined_interface, JoinedInterfaceRole::LexicalJoin) =
        &joined.payload
    else {
        panic!("joined fixture")
    };
    assert_eq!(joined_interface.artifact_id(), joined.descriptor.id);
    let scratch = tempfile::tempdir().unwrap();
    let absent = scratch.path().join("absent");
    assert_eq!(
        source
            .materialize_joined_and_value_interfaces_for_artifacts(&absent, &BTreeSet::new())
            .unwrap(),
        (vec![], vec![])
    );
    assert!(!absent.exists());
    let (joins, values) = source
        .materialize_joined_and_value_interfaces_for_artifacts(
            scratch.path(),
            &BTreeSet::from([joined.descriptor.id]),
        )
        .unwrap();
    assert_eq!(joins.len(), 1);
    assert!(values.is_empty());
    assert_eq!(
        crate::artifact_inventory::ArtifactDescriptor::from_recovery_join(&joins[0]).id,
        joined.descriptor.id
    );
    let retained_path = scratch.path().join(&joins[0].interface_path);
    std::fs::write(&retained_path, b"damaged retained join").unwrap();
    let (joins, values) = source
        .materialize_joined_and_value_interfaces_for_artifacts(
            scratch.path(),
            &BTreeSet::from([value.descriptor.id]),
        )
        .unwrap();
    assert!(joins.is_empty());
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].artifact_id, value.descriptor.id);
    assert_eq!(
        crate::artifact_inventory::ArtifactDescriptor::from_recovery_value_interface(&values[0]).id,
        value.descriptor.id
    );
    assert_eq!(values[0].requirements, value.requirements);
    assert_eq!(
        std::fs::read(&retained_path).unwrap(),
        b"damaged retained join"
    );
    assert!(source
        .materialize_joined_and_value_interfaces_for_artifacts(
            scratch.path(),
            &BTreeSet::from([joined.descriptor.id])
        )
        .is_err());
    assert!(source
        .materialize_joined_and_value_interfaces_for_artifacts(
            scratch.path(),
            &BTreeSet::from([ArtifactId([0xee; 32])])
        )
        .is_err());
}

#[test]
fn role_snapshot_equality_preserves_proof_availability_and_refuses_false_baselines() {
    use crate::exact_context::{hydrate_context, Materializer};
    use fvm_ipld_blockstore::{Blockstore, MemoryBlockstore};
    let source = published_original_history_fixture();
    let root = identity("fixture", "Alpha");
    let published = source
        .issue_published_source_original("revision", "input", &root)
        .unwrap();
    let context = published.context();
    let original_role = context.compiler_input_roles().remove(0);
    let CompilerInputRole::PublishedSourceOriginal { selection, .. } = &original_role else {
        panic!("published issuer role")
    };
    let mut graph = selection.graph_selection().clone();
    let additional = source
        .artifact_view()
        .capture_graph_selection()
        .selections
        .into_iter()
        .find(|proof| {
            !graph
                .selections
                .iter()
                .any(|existing| existing.selection == proof.selection)
        })
        .expect("private target proof is absent from the public selection");
    graph.selections.push(additional);
    let mut changed_role = original_role.clone();
    let CompilerInputRole::PublishedSourceOriginal {
        selection: changed_selection,
        ..
    } = &mut changed_role
    else {
        unreachable!()
    };
    *changed_selection =
        crate::artifact_inventory::ExactArtifactSelection::from_recovered_graph_selection(graph);
    assert_eq!(
        selection, changed_selection,
        "selected semantic observation ignores available proof transport"
    );
    assert_ne!(
        original_role, changed_role,
        "durable role facts include complete proof availability"
    );
    let mut next = context.snapshot();
    next.roles.insert(root, changed_role);
    let store = MemoryBlockstore::new();
    let mut materializer = Materializer::new(&store);
    let base = materializer
        .materialize_context(None, &context.snapshot())
        .unwrap();
    for (cid, bytes) in materializer.finish().unwrap().into_blocks() {
        store.put_keyed(&cid, &bytes).unwrap();
    }
    let mut materializer = Materializer::new(&store);
    let changed = materializer
        .materialize_context(Some(&base), &next)
        .unwrap();
    assert_ne!(base.root(), changed.root());
    let blocks = materializer.finish().unwrap();
    assert!(blocks.metrics().changed_entries > 0);
    for (cid, bytes) in blocks.into_blocks() {
        store.put_keyed(&cid, &bytes).unwrap();
    }
    let mut canonical = Materializer::new(&store);
    assert_eq!(
        canonical.materialize_context(None, &next).unwrap().root(),
        changed.root()
    );
    let cold = hydrate_context(&store, changed.root()).unwrap();
    assert!(
        context
            .as_ref()
            .clone()
            .bind_snapshot_baseline(&cold)
            .is_err(),
        "equal semantic observations cannot attach different durable role facts"
    );

    let proof = selection.graph_selection().selections[0].clone();
    let mut altered = proof.as_ref().clone();
    altered.nodes.pop().expect("nonempty issued scope");
    assert_ne!(
        crate::exact_context::ImmutableScopeProofRef::from_issued(proof),
        crate::exact_context::ImmutableScopeProofRef::from_issued(Arc::new(altered)),
        "claimed scope identity alone is insufficient for decoded fact equality"
    );
}

fn published_original_choice_history_fixture() -> &'static ExactDeclarationContext {
    static FIXTURE: std::sync::OnceLock<ExactDeclarationContext> = std::sync::OnceLock::new();
    FIXTURE.get_or_init(|| {
        let producer = [2; 32];
        let products = ["Alpha", "Beta"].map(|module| {
            crate::certified_products::fixture_source_finalized_product(
                crate::certified_products::tests::original_groups_fixture_with_interface(
                    module,
                    vec![(7, vec![])],
                    1,
                    &BTreeMap::new(),
                    format!("{module} interface").into_bytes(),
                ),
                producer,
                vec![],
            )
        });
        let products = crate::certified_products::tests::recovered_witness_fixtures(&products);
        let target = identity("fixture", "PrivateSourceTarget");
        let mut entries = products
            .into_iter()
            .map(|product| Arc::new(ArtifactEntry::original(producer, product.product).unwrap()))
            .collect::<Vec<_>>();
        entries.push(Arc::new(ArtifactEntry::canonical(
            crate::certified_products::fixture_module_interface(
                producer,
                &target.unit,
                &target.module,
                BTreeMap::new(),
            ),
        )));
        let projection = CompilerInputProjection::from_issued_entries(&entries).unwrap();
        let inventory = ArtifactInventory::default();
        let view = inventory
            .admit_shared(&inventory.empty_view(), entries)
            .unwrap();
        ExactDeclarationContext::from_authenticated_execution(
            producer,
            &view,
            projection,
            vec![
                ExactLexicalNode {
                    owner: target.clone(),
                    imports: vec![identity("fixture", "Alpha")],
                },
                ExactLexicalNode {
                    owner: identity("fixture", "Alpha"),
                    imports: vec![],
                },
            ],
            target,
            &[],
        )
        .unwrap()
    })
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(24))]
    #[test]
    fn merkle_context_history_matches_canonical_and_cold_facts(
        operations in proptest::collection::vec((proptest::prelude::any::<u16>(), 0u8..7), 1..28),
    ) {
        use fvm_ipld_blockstore::{Blockstore,MemoryBlockstore};
        use crate::exact_context::{Materializer,hydrate_context};
        let issued = published_original_choice_history_fixture();
        let mut lexical = issued.lexical_graph().to_vec();
        lexical.iter_mut().find(|node| node.owner.module == "PrivateSourceTarget").unwrap().imports.push(identity("fixture","Beta"));
        lexical.push(ExactLexicalNode { owner: identity("fixture","Beta"), imports: vec![] });
        let source = ExactDeclarationContext::from_authenticated_execution(
            issued.owner.facts.producer, issued.artifact_view(), issued.owner.facts.compiler_support.projection().clone(), lexical,
            identity("fixture","PrivateSourceTarget"), &[identity("fixture","PrivateSourceTarget"),identity("fixture","Alpha"),identity("fixture","Beta")],
        ).unwrap();
        let alpha = source.issue_published_source_original("revision","input",&identity("fixture","Alpha")).unwrap();
        let beta = source.issue_published_source_original("revision","input",&identity("fixture","Beta")).unwrap();
        let empty = ExactDeclarationContext::new(&[],&[],vec![]).unwrap();
        let left = empty.clone().with_published_source_originals(&alpha).unwrap();
        let right = empty.clone().with_published_source_originals(&beta).unwrap();
        let joined = left.clone().with_published_source_originals(&beta).unwrap();
        assert!(!left.compiler_source_support().recorded_input_decisions().is_empty(),
            "published fixture must carry a real issued decision domain");
        assert!(left.compiler_source_support().recorded_input_decisions().values()
            .any(|fact| !fact.groups.is_empty()),
            "withdrawal history starts with an actually chosen native Group");
        let interface_roots = left.compiler_input_projection().roles().into_iter()
            .map(|role| role.interface()).collect();
        let withdrawn_support = left.compiler_source_support().select_support_roots(interface_roots).unwrap();
        let withdrawn = left.clone().rebuild(|facts| facts.compiler_support = withdrawn_support).finish().unwrap();
        assert!(!withdrawn.compiler_source_support().recorded_input_decisions().is_empty());
        assert!(withdrawn.compiler_source_support().recorded_input_decisions().values()
            .all(|fact| fact.groups.is_empty()), "withdrawal preserves domains but no chosen Group");
        let store = MemoryBlockstore::new();
        let mut materializer = Materializer::new(&store);
        let initial = materializer.materialize_context(None,&left.snapshot()).unwrap();
        for (cid,bytes) in materializer.finish().unwrap().into_blocks() { store.put_keyed(&cid,&bytes).unwrap(); }
        let mut history = vec![(left.clone(),initial)];
        for (branch,operation) in operations {
            let (previous,base) = &history[usize::from(branch) % history.len()];
            assert_context_observation_wire(previous);
            let next = match operation {
                0 => previous.clone(),
                1 => left.clone(),
                2 => {
                    previous.clone().recover_fixture_projection(previous.compiler_input_projection().interface_only()).unwrap()
                },
                3 => empty.clone(),
                4 => right.clone(),
                5 => joined.clone(),
                _ => withdrawn.clone(),
            };
            assert_context_observation_wire(&next);
            let snapshot = next.snapshot();
            let previous_snapshot = previous.snapshot();
            let introduced = snapshot.selection().artifact_ids_introduced(Some(previous_snapshot.selection())).unwrap();
            let previous_ids = previous.artifact_view().artifact_ids().into_iter().collect::<BTreeSet<_>>();
            let next_ids = next.artifact_view().artifact_ids().into_iter().collect::<BTreeSet<_>>();
            proptest::prop_assert!(next_ids.difference(&previous_ids).all(|id| introduced.contains(id)));
            proptest::prop_assert!(introduced.is_subset(&next_ids));
            proptest::prop_assert_eq!(snapshot.selection().artifact_ids_introduced(None).unwrap(), next_ids);
            if operation == 0 || operation == 3 { proptest::prop_assert!(introduced.is_empty()); }
            let mut materializer = Materializer::new(&store);
            let result = materializer.materialize_context(Some(base),&snapshot).unwrap();
            let packet = materializer.finish().unwrap();
            if operation == 0 { proptest::prop_assert_eq!(packet.metrics(),crate::exact_context::MaterializationMetrics::default()); }
            for (cid,bytes) in packet.into_blocks() { store.put_keyed(&cid,&bytes).unwrap(); }
            let mut canonical = Materializer::new(&store);
            let rebuilt = canonical.materialize_context(None,&snapshot).unwrap();
            proptest::prop_assert_eq!(result.root(),rebuilt.root());
            let restored = hydrate_context(&store,result.root()).unwrap();
            proptest::prop_assert_eq!(restored.snapshot().graph_selection().unwrap(),snapshot.graph_selection().unwrap());
            proptest::prop_assert_eq!(restored.snapshot().compiler_roles(),snapshot.compiler_roles());
            proptest::prop_assert_eq!(
                &restored.snapshot().input_decisions,
                next.compiler_source_support().recorded_input_decisions(),
                "the real incremental writer/reader must preserve issued empty domains and exact Group references",
            );
            proptest::prop_assert_eq!(restored.snapshot().lexical(),snapshot.lexical());
            proptest::prop_assert_eq!(restored.snapshot().producer_sha256(),snapshot.producer_sha256());
            let graph = restored.snapshot().graph_selection().unwrap();
            let view = next.artifact_view();
            let inventory = RecoveredArtifactInventory {
                producer: snapshot.producer_sha256(),
                entries: view.entries().into_iter().map(|entry| (entry.descriptor.id,entry)).collect(),
                recorded_inventory: true,
                bindings: graph.bindings.iter().copied().collect(),
                binding_edges: restored.snapshot().binding_dependencies().unwrap(),
                interfaces: view.interface_dependencies(),
            };
            let mut recovered = inventory.context_with_snapshot(restored.snapshot()).unwrap();
            proptest::prop_assert_eq!(
                recovered.compiler_source_support().recorded_input_decisions(),
                next.compiler_source_support().recorded_input_decisions(),
                "authenticated context recovery must preserve the same issued choices without policy resurrection",
            );
            proptest::prop_assert_eq!(recovered.original_instance_environment(),&OriginalInstanceEnvironment::Unknown);
            assert_context_observation_wire(&recovered);
            recovered.bind_snapshot_baseline(&restored).unwrap();
            assert_context_observation_wire(&recovered);
            let mut same = Materializer::new(&store);
            let rebound = same.materialize_context(Some(&restored),&recovered.snapshot()).unwrap();
            proptest::prop_assert_eq!(rebound.root(),result.root());
            proptest::prop_assert_eq!(same.finish().unwrap().metrics(),crate::exact_context::MaterializationMetrics::default());
            history.push((next,result));
        }
        for (context,receipt) in history {
            let restored = hydrate_context(&store,receipt.root()).unwrap();
            proptest::prop_assert_eq!(restored.snapshot().lexical(),context.lexical_graph());
            proptest::prop_assert_eq!(
                &restored.snapshot().input_decisions,
                context.compiler_source_support().recorded_input_decisions(),
            );
            proptest::prop_assert_eq!(restored.snapshot().graph_selection().unwrap(),context.snapshot().graph_selection().unwrap());
        }
    }
}

#[test]
fn small_lexical_edit_materializes_only_changed_fact_paths() {
    use crate::exact_context::{hydrate_context, Materializer};
    use fvm_ipld_blockstore::{Blockstore, MemoryBlockstore};
    let inventory = ArtifactInventory::default();
    let entries = (0..512)
        .map(|index| {
            Arc::new(ArtifactEntry::canonical(
                crate::certified_products::fixture_module_interface(
                    [7; 32],
                    "fixture",
                    &format!("Owner{index:04}"),
                    BTreeMap::new(),
                ),
            ))
        })
        .collect::<Vec<_>>();
    let projection = CompilerInputProjection::from_issued_entries(&entries).unwrap();
    let view = inventory
        .admit_shared(&inventory.empty_view(), entries)
        .unwrap();
    let lexical = (0..512)
        .map(|index| ExactLexicalNode {
            owner: identity("fixture", &format!("Owner{index:04}")),
            imports: vec![],
        })
        .collect::<Vec<_>>();
    let context = ExactDeclarationContext::from_authenticated_execution(
        [7; 32],
        &view,
        projection,
        lexical.clone(),
        identity("fixture", "Owner0000"),
        &[],
    )
    .unwrap();
    let store = MemoryBlockstore::new();
    let mut materializer = Materializer::new(&store);
    let base = materializer
        .materialize_context(None, &context.snapshot())
        .unwrap();
    let initial = materializer.finish().unwrap();
    let baseline_bytes = initial.metrics().staged_bytes;
    for (cid, bytes) in initial.into_blocks() {
        store.put_keyed(&cid, &bytes).unwrap();
    }
    let mut changed = lexical;
    changed[256].imports.push(identity("fixture", "Owner0000"));
    let next = context.extend(&[], &[], changed).unwrap();
    let mut materializer = Materializer::new(&store);
    let result = materializer
        .materialize_context(Some(&base), &next.snapshot())
        .unwrap();
    let packet = materializer.finish().unwrap();
    assert_eq!(packet.metrics().changed_entries, 1);
    assert!(packet.metrics().staged_bytes < baseline_bytes / 8);
    assert!(packet.metrics().blocks < 12);
    eprintln!(
        "512 lexical/role facts: baseline_staged_bytes={baseline_bytes}, edit={:?}",
        packet.metrics()
    );
    for (cid, bytes) in packet.into_blocks() {
        store.put_keyed(&cid, &bytes).unwrap();
    }
    let restored = hydrate_context(&store, result.root()).unwrap();
    assert_eq!(restored.snapshot().lexical(), next.lexical_graph());
}

#[test]
fn held_context_receipt_reuses_authenticated_blocks_without_reads() {
    use crate::exact_context::{hydrate_context, Materializer};
    use fvm_ipld_blockstore::{Blockstore, MemoryBlockstore};
    struct Reads {
        store: MemoryBlockstore,
        count: std::cell::Cell<usize>,
    }
    impl Blockstore for Reads {
        fn get(&self, cid: &cid::Cid) -> anyhow::Result<Option<Vec<u8>>> {
            self.count.set(self.count.get() + 1);
            self.store.get(cid)
        }
        fn put_keyed(&self, cid: &cid::Cid, bytes: &[u8]) -> anyhow::Result<()> {
            self.store.put_keyed(cid, bytes)
        }
    }
    let source = published_original_history_fixture();
    let selection = source
        .issue_published_source_original("revision", "input", &identity("fixture", "Alpha"))
        .unwrap();
    let context = ExactDeclarationContext::new(&[], &[], vec![])
        .unwrap()
        .with_published_source_originals(&selection)
        .unwrap();
    let store = Reads {
        store: MemoryBlockstore::new(),
        count: std::cell::Cell::new(0),
    };
    let mut materializer = Materializer::new(&store);
    let receipt = materializer
        .materialize_context(None, &context.snapshot())
        .unwrap();
    for (cid, bytes) in materializer.finish().unwrap().into_blocks() {
        store.put_keyed(&cid, &bytes).unwrap();
    }
    store.count.set(0);
    let cold = hydrate_context(&store, receipt.root()).unwrap();
    assert!(
        store.count.get() > 0,
        "fresh recovery authenticates referenced input blocks"
    );
    store.count.set(0);
    let mut materializer = Materializer::new(&store);
    assert_eq!(
        materializer
            .materialize_context(Some(&cold), cold.snapshot())
            .unwrap()
            .root(),
        cold.root()
    );
    assert_eq!(
        materializer.finish().unwrap().metrics(),
        crate::exact_context::MaterializationMetrics::default()
    );
    assert_eq!(
        store.count.get(),
        0,
        "held coupled receipt avoids repeat physical reads"
    );
    let mut changed = context.clone();
    {
        let updated = changed.owner.facts.lexical.replace(vec![]).unwrap();
        changed = changed.rebuild(|facts| facts.lexical = updated);
    }
    let mut materializer = Materializer::new(&store);
    let wrong = materializer
        .materialize_context(Some(&cold), &changed.snapshot())
        .unwrap();
    assert!(changed.bind_snapshot_baseline(&cold).is_err());
    assert_ne!(wrong.root(), cold.root());
}

#[test]
fn published_original_projection_keeps_checked_template_custody_in_its_parent() {
    let owners = [identity("fixture", "Alpha"), identity("fixture", "Beta")];
    for root_index in 0..owners.len() {
        for template_mask in 0u8..4 {
            let mut issued = published_original_fixture();
            edit_lexical(&mut issued, |nodes| {
                nodes
                    .iter_mut()
                    .find(|node| node.owner.module == "PrivateSourceTarget")
                    .unwrap()
                    .imports = owners.to_vec()
            });
            edit_lexical(&mut issued, |nodes| {
                nodes.push(ExactLexicalNode {
                    owner: owners[1].clone(),
                    imports: vec![],
                })
            });
            let template_owners = owners
                .iter()
                .enumerate()
                .filter(|(index, _)| template_mask & (1 << index) != 0)
                .map(|(_, owner)| owner.clone())
                .collect::<BTreeSet<_>>();
            let template = template_owners
                .iter()
                .map(|owner| format!("import {}\n", owner.module))
                .collect::<String>();
            {
                let updated =
                    RetainedTemplateImports::capture(&issued, &[template.clone()]).unwrap();
                issued = issued.rebuild(|facts| facts.template_imports = updated);
            }
            issued.normalize().unwrap();
            let before = issued.semantic_sha256();
            let custody = issued
                .owner
                .facts
                .compiler_support
                .artifacts()
                .descriptors();
            let retained = issued.owner.facts.template_imports.clone();
            let selection = issued
                .issue_published_source_original("revision", "input", &owners[root_index])
                .unwrap();
            let selected = selection.context();
            selected.normalize().unwrap();
            assert!(selected.owner.facts.template_imports.is_none());
            assert_eq!(
                selected.lexical_graph(),
                &[ExactLexicalNode {
                    owner: owners[root_index].clone(),
                    imports: vec![],
                }]
            );
            assert!(selected
                .owner
                .facts
                .compiler_support
                .artifacts()
                .descriptors()
                .iter()
                .all(|entry| entry.owner == owners[root_index]));
            assert_eq!(
                selected
                    .selected_template_imports(&[template.clone()])
                    .unwrap()
                    .roots,
                template_owners
                    .intersection(&BTreeSet::from([owners[root_index].clone()]))
                    .cloned()
                    .collect()
            );
            let mut parent = issued
                .clone()
                .with_published_source_originals(&selection)
                .unwrap();
            parent = parent.extend(&[], &[], vec![]).unwrap();
            assert_eq!(parent.owner.facts.template_imports, retained);
            assert_eq!(
                parent
                    .selected_template_imports(&[template.clone()])
                    .unwrap()
                    .roots,
                template_owners
            );
            let reopened = parent.published_source_original_selections().unwrap();
            assert_eq!(reopened.len(), 1);
            assert_eq!(
                reopened[0].context().semantic_sha256(),
                selected.semantic_sha256()
            );
            if let Some(retained) = &retained {
                let mut changed = issued.clone();
                let mut invalid = retained.as_ref().clone();
                invalid
                    .graph
                    .values_mut()
                    .next()
                    .unwrap()
                    .interface
                    .interface_sha256[0] ^= 1;
                {
                    let updated = Some(Arc::new(invalid));
                    changed = changed.rebuild(|facts| facts.template_imports = updated);
                }
                assert!(changed.normalize().is_err());
            }
            if template_mask & (1 << (1 - root_index)) != 0 {
                let mut missing = issued.clone();
                missing = missing.rebuild(|facts| {
                    facts.compiler_support = selected.compiler_source_support().clone()
                });
                {
                    let updated = selected.owner.facts.lexical.clone();
                    missing = missing.rebuild(|facts| facts.lexical = updated);
                }
                assert!(missing.normalize().is_err());
            }
            assert_eq!(issued.semantic_sha256(), before);
            assert_eq!(
                issued
                    .owner
                    .facts
                    .compiler_support
                    .artifacts()
                    .descriptors(),
                custody
            );
            assert_eq!(issued.owner.facts.template_imports, retained);
            issued.normalize().unwrap();
        }
    }
}

#[test]
fn published_original_selection_refuses_unissued_root_and_custody_loss() {
    let issued = published_original_fixture();
    let root = identity("fixture", "Alpha");
    assert!(
        issued
            .issue_published_source_original("revision", "input", &identity("fixture", "Beta"))
            .is_err(),
        "available but unimported native output is not public source authority"
    );
    let selection = issued
        .issue_published_source_original("revision", "input", &root)
        .unwrap();
    assert_eq!(
        selection.context().lexical_graph(),
        &[ExactLexicalNode {
            owner: root.clone(),
            imports: vec![]
        }]
    );
    assert!(
        selection
            .context()
            .artifact_view()
            .descriptors()
            .iter()
            .all(|row| row.owner == root),
        "private target and unrelated available originals are excluded"
    );
    let interface = selection
        .context()
        .compiler_input_roles()
        .iter()
        .find(|role| role.is_published_source_original())
        .unwrap()
        .interface();
    let narrowed = selection
        .context()
        .artifact_view()
        .select_roots(vec![interface])
        .unwrap();
    let projection = selection
        .context()
        .compiler_input_projection()
        .within_view(&narrowed);
    assert!(
        matches!(recover_fixture_compiler_support(projection, narrowed, []),
        Err(CompileError::ExtractFailed(detail)) if detail == "exact declaration context: bound recovery namespace differs from recorded compiler roles"),
        "missing native custody must refuse rather than downgrade policy"
    );
    assert!(
        selection.context().normalize().is_ok(),
        "a failed projection preserves its immutable owner"
    );
}

#[test]
fn published_original_manifest_binds_native_selection_and_restored_policy() {
    let issued = published_original_fixture();
    let root = identity("fixture", "Alpha");
    let selection = issued
        .issue_published_source_original("revision", "input", &root)
        .unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let request = selection
        .context()
        .prepare_compilation(scratch.path(), b"metadata producer")
        .unwrap();
    let bytes = std::fs::read(&request.manifest).unwrap();
    let manifest: Value = ciborium::de::from_reader(bytes.as_slice()).unwrap();
    let fields = current_scope_fields(&manifest);
    let published = fields[9].as_array().unwrap();
    assert_eq!(published.len(), 1);
    let row = published[0].as_array().unwrap();
    let originals = selection.context().recovery_products();
    assert_eq!(originals.len(), 1);
    assert_eq!(
        row[..4],
        [
            text(&root.unit),
            text(&root.module),
            text(hex(&originals[0].owner().skinny_iface_sha256)),
            text(hex(&originals[0].owner().product_sha256))
        ]
    );
    assert_eq!(row[4], text("revision"));
    assert_eq!(row[5], text("input"));
    let view = selection.context().artifact_view();
    let graph = view.capture_graph_selection();
    let inventory = RecoveredArtifactInventory {
        producer: issued.owner.facts.producer,
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
    assert!(
        inventory
            .context_with_roles(
                &view.artifact_ids(),
                &view
                    .selected_native_groups()
                    .into_iter()
                    .collect::<Vec<_>>(),
                &selection.context().compiler_input_roles(),
                selection.context().owner.facts.lexical.to_vec(),
            )
            .is_err(),
        "flat custody cannot restore exact publication policy"
    );
    let mut changed = selection.context().compiler_input_roles();
    if let CompilerInputRole::PublishedSourceOriginal {
        source_revision, ..
    } = &mut changed[0]
    {
        source_revision.push_str("-changed");
    } else {
        panic!("selected root must carry published policy");
    }
    assert!(
        inventory
            .context_with_graph_selection(
                &graph,
                &changed,
                selection.context().owner.facts.lexical.to_vec()
            )
            .is_err(),
        "a valid inventory cannot repair changed publication facts"
    );
    assert!(inventory
        .context_with_graph_selection(
            &graph,
            &selection.context().compiler_input_roles(),
            selection.context().owner.facts.lexical.to_vec()
        )
        .is_ok());
}

#[test]
fn publication_target_selection_distinguishes_authored_native_and_source_origins() {
    use crate::certified_products::PendingImportOwner;
    use tidepool_repr::execution_schema::SymbolIdentity;

    let issuer = published_original_history_fixture();
    let publication = issuer
        .issue_published_source_original("revision", "input", &identity("fixture", "Alpha"))
        .unwrap();
    let target = identity("main", "Tidepool.Session.Lib.G11");
    let mut histories = 0;
    for native_authored in [false, true] {
        let product = support_product_in_unit(&target.unit, &target.module);
        let product = if native_authored {
            crate::certified_products::fixture_native_declaration_product(
                product,
                issuer.owner.facts.producer,
                11,
            )
        } else {
            crate::certified_products::fixture_finalized_product(
                product,
                issuer.owner.facts.producer,
            )
        };
        let product = crate::certified_products::tests::recovered_witness_fixtures(&[product])
            .remove(0)
            .product;
        for historical in [false, true] {
            for unrelated_publication in [false, true] {
                for lexical_carrier in [false, true] {
                    histories += 1;
                    let mut context = ExactDeclarationContext::new(&[], &[], vec![]).unwrap();
                    if unrelated_publication {
                        context = context
                            .with_published_source_originals(&publication)
                            .unwrap();
                    }
                    context = context
                        .extend_checked_original_products(
                            issuer.owner.facts.producer,
                            std::slice::from_ref(&product),
                        )
                        .unwrap();
                    if lexical_carrier {
                        edit_lexical(&mut context, |nodes| {
                            nodes.push(ExactLexicalNode {
                                owner: target.clone(),
                                imports: vec![],
                            });
                        });
                    }
                    let mut owner = product.owner().clone();
                    if historical {
                        owner.module_version = ModuleVersion([99; 32]);
                    }
                    let import = PendingImportOwner::Source {
                        owner,
                        original_ordinal: 0,
                        binder: SymbolIdentity {
                            unit: target.unit.clone(),
                            module: target.module.clone(),
                            namespace: "value".into(),
                            occurrence: "severity".into(),
                            record_parent: None,
                        },
                    };
                    let selected = context.published_source_original_selections_for_target(
                        &BTreeSet::new(),
                        &[import],
                    );
                    let refuses = unrelated_publication
                        && !native_authored
                        && !historical
                        && !lexical_carrier;
                    assert_eq!(
                        selected.is_err(),
                        refuses,
                        "native_authored={native_authored}, historical={historical}, unrelated_publication={unrelated_publication}, lexical_carrier={lexical_carrier}"
                    );
                    if let Ok(selected) = selected {
                        assert!(
                            selected.is_empty(),
                            "unrelated publication stays unselected"
                        );
                    }
                }
            }
        }
    }
    assert_eq!(histories, 16);

    // A source import of the exact published original still selects its policy.
    let product = publication.context().recovery_products().remove(0);
    let owner = product.owner().clone();
    let import = PendingImportOwner::Source {
        binder: SymbolIdentity {
            unit: owner.unit.clone(),
            module: owner.module.clone(),
            namespace: "value".into(),
            occurrence: "published".into(),
            record_parent: None,
        },
        owner,
        original_ordinal: 0,
    };
    let selected = publication
        .context()
        .published_source_original_selections_for_target(&BTreeSet::new(), &[import])
        .unwrap();
    assert_eq!(selected.as_slice(), std::slice::from_ref(&publication));
}

#[test]
fn publication_target_retains_source_policy_without_published_native_group_demand() {
    use crate::certified_products::PendingImportOwner;

    let issuer = published_original_history_fixture();
    let root = identity("fixture", "Alpha");
    let publication = issuer
        .issue_published_source_original("revision", "input", &root)
        .unwrap();
    let consumer = crate::certified_products::tests::original_groups_fixture(
        "InlineConsumer",
        vec![(7, vec![])],
        1,
        &BTreeMap::new(),
    );
    let consumer = crate::certified_products::fixture_source_finalized_product(
        consumer,
        issuer.owner.facts.producer,
        vec![crate::certified_products::CanonicalSourceImport {
            qualifier: crate::cache::ImportQualifier::Unqualified,
            module: root.module.clone(),
            boot: false,
            home_unit: Some(root.unit.clone()),
        }],
    );
    let consumer = crate::certified_products::tests::recovered_witness_fixtures(&[consumer])
        .remove(0)
        .product;
    let owner = consumer.owner().clone();
    let binder = crate::certified_products::original_available_groups(&consumer)
        .unwrap()
        .find(|group| group.original_ordinal() == 7)
        .unwrap()
        .binders()[0]
        .clone();
    let mut context = publication
        .context()
        .as_ref()
        .clone()
        .extend_checked_original_products(issuer.owner.facts.producer, &[consumer])
        .unwrap();
    edit_lexical(&mut context, |nodes| {
        nodes.push(ExactLexicalNode {
            owner: identity(&owner.unit, &owner.module),
            imports: vec![root],
        });
    });
    context.normalize().unwrap();
    let imports = [PendingImportOwner::Source {
        binder,
        owner,
        original_ordinal: 7,
    }];
    let inputs = ProtectedScaffoldRequirements::default()
        .compiler_inputs(&Arc::new(context.clone()), None)
        .unwrap();
    let validation = PackageInterfaceValidation::default();
    let selected = crate::certified_products::CertifiedSourceSelection::from_compiler_inputs(
        &inputs,
        &inputs.metadata,
        &validation.inventory,
    )
    .unwrap();
    let mut fixed = inputs
        .artifacts()
        .inventory()
        .bind_segment(inputs.artifacts(), vec![], &selected)
        .unwrap();
    let target = inputs
        .artifacts()
        .inventory()
        .admit_bound_target(
            &mut fixed,
            inputs.artifacts(),
            crate::artifact_inventory::NativeArtifactDemand::CertifiedTargetImports(&imports),
            vec![],
            &selected,
        )
        .unwrap()
        .bind_target(None, imports.to_vec());
    let groups = target.resolve().unwrap().groups().clone();
    let published_original = publication
        .context()
        .compiler_input_roles()
        .iter()
        .find_map(CompilerInputRole::original)
        .unwrap();
    assert!(!groups.is_empty());
    assert!(groups
        .iter()
        .all(|group| group.binding.artifact() != published_original));
    assert_eq!(
        context
            .published_source_original_selections_for_target(&groups, &imports)
            .unwrap()
            .as_slice(),
        std::slice::from_ref(&publication),
        "source policy follows the selected consumer's lexical proof even when the published body has no remaining native call"
    );
}

#[test]
fn published_original_retains_lexical_only_interfaces_without_native_offer() {
    let mut issued = published_original_fixture();
    let root = identity("fixture", "Alpha");
    let lexical_only = identity("fixture", "Beta");
    edit_lexical(&mut issued, |nodes| {
        nodes
            .iter_mut()
            .find(|node| node.owner == root)
            .unwrap()
            .imports
            .push(lexical_only.clone())
    });
    edit_lexical(&mut issued, |nodes| {
        nodes.push(ExactLexicalNode {
            owner: lexical_only.clone(),
            imports: vec![],
        })
    });
    issued.normalize().unwrap();
    let selection = issued
        .issue_published_source_original("revision", "input", &root)
        .unwrap();
    let metadata = selection.context().compiler_metadata_snapshot().unwrap();
    assert!(matches!(
        metadata.entries[&root].payload,
        ArtifactPayload::Original(_)
    ));
    assert!(
        matches!(
            metadata.entries[&lexical_only].payload,
            ArtifactPayload::Canonical(_)
        ),
        "a lexical interface requirement does not select its available native implementation"
    );
    assert_eq!(selection.context().recovery_products().len(), 1);
    assert_eq!(selection.context().lexical_graph().len(), 2);
    assert_eq!(
        selection
            .context()
            .published_source_original_selections()
            .unwrap()[0]
            .context()
            .semantic_sha256(),
        selection.context().semantic_sha256()
    );
}

#[test]
fn published_original_refuses_same_interface_with_another_native_identity() {
    let mut issued = published_original_fixture();
    let root = identity("fixture", "Alpha");
    let selection = issued
        .issue_published_source_original("revision", "input", &root)
        .unwrap();
    let original = selection.context().recovery_products().remove(0);
    let interface = original.module_interface().unwrap().clone();
    let mut owner = original.owner().clone();
    owner.module_version = ModuleVersion([99; 32]);
    let certification = crate::certified_products::encode_home_certification_with_module(
        &owner,
        &[],
        &BTreeMap::new(),
        interface.requirements(),
        Sha256::digest(interface.certificate_bytes()).into(),
    )
    .unwrap();
    let mut variant = CertifiedRecoveryProduct::from_certification(
        owner,
        original.interface_bytes().to_vec(),
        original.product_bytes().to_vec(),
        original.package_imports_bytes().to_vec(),
        certification,
    )
    .with_module_interface(interface)
    .unwrap();
    if let Some(source) = original.source_sha256() {
        variant = variant.with_source_sha256(source);
    }
    let variant = crate::certified_products::tests::recovered_witness_fixtures(&[variant])
        .remove(0)
        .product;
    assert_eq!(variant.module_interface(), original.module_interface());
    assert_ne!(variant.owner(), original.owner());
    let mut entries = issued
        .owner
        .facts
        .compiler_support
        .artifacts()
        .entries()
        .into_iter()
        .filter(|entry| {
            entry.descriptor.owner != root || !matches!(entry.payload, ArtifactPayload::Original(_))
        })
        .collect::<Vec<_>>();
    entries.push(Arc::new(
        ArtifactEntry::original(issued.owner.facts.producer, variant).unwrap(),
    ));
    let inventory = ArtifactInventory::default();
    let view = inventory
        .admit_shared(&inventory.empty_view(), entries)
        .unwrap();
    let projection = CompilerInputProjection::from_issued_entries(&view.entries()).unwrap();
    let support = issue_fixture_compiler_support(projection, view).unwrap();
    issued = issued.rebuild(|facts| facts.compiler_support = support);
    let replacement = issued
        .issue_published_source_original("revision", "input", &root)
        .unwrap();
    let before = selection.context().semantic_sha256();
    assert!(selection
        .context()
        .as_ref()
        .clone()
        .with_published_source_originals(&replacement)
        .is_err());
    assert_eq!(selection.context().semantic_sha256(), before);
}

proptest::proptest! {
    #![proptest_config(policy_property_config())]
    #[test]
    fn published_original_composition_recovery_history(
        revision in "[a-z]{1,12}", history in proptest::collection::vec(0u8..4, 1..24),
    ) {
        let issued = published_original_history_fixture();
        let root = identity("fixture", "Alpha");
        let selection = issued.issue_published_source_original(&revision, "input", &root).unwrap();
        let mut context = ExactDeclarationContext::new(&[], &[], vec![]).unwrap().with_published_source_originals(&selection).unwrap();
        let expected = context.semantic_sha256();
        let original = context.compiler_input_roles().iter().find_map(CompilerInputRole::original).unwrap();
        for operation in history {
            match operation {
                0 => { context = context.with_published_source_originals(&selection).unwrap(); },
                1 => {
                    let view = context.artifact_view();
                    let graph = view.capture_graph_selection();
                    let inventory = RecoveredArtifactInventory {
                        producer: context.owner.facts.producer,
                        entries: view.entries().into_iter().map(|entry| (entry.descriptor.id, entry)).collect(),
                        recorded_inventory: true,
        bindings: graph.bindings.iter().copied().collect(), binding_edges: view.binding_dependencies(), interfaces: view.interface_dependencies(),
                    };
                    let ids = view.descriptors().into_iter().map(|row| row.id).collect::<Vec<_>>();
                    let groups = view.selected_native_groups().into_iter().collect::<Vec<_>>();
                    let roles = context.compiler_input_roles();
                    proptest::prop_assert!(inventory.context_with_roles(&ids, &groups, &roles, context.owner.facts.lexical.to_vec()).is_err());
                    context = inventory.context_with_snapshot(&context.snapshot()).unwrap();
                    let reopened = context.published_source_original_selections().unwrap();
                    proptest::prop_assert_eq!(reopened.len(), 1);
                    context = context.with_published_source_originals(&reopened[0]).unwrap();
                },
                2 => {
                    let conflicting = issued.issue_published_source_original(&(revision.clone() + "-other"), "input", &root).unwrap();
                    proptest::prop_assert!(context.clone().with_published_source_originals(&conflicting).is_err());
                },
                _ => {
                    let mut altered = context.clone();
                    { let updated = crate::exact_context::ExactLexicalSnapshot::default(); altered = altered.rebuild(|facts| facts.lexical = updated); }
                    proptest::prop_assert!(altered.normalize().is_err());
                    context = context.extend(&[], &[], vec![]).unwrap();
                },
            }
            proptest::prop_assert_eq!(context.semantic_sha256(), expected);
            proptest::prop_assert!(context.compiler_input_roles().iter().any(|role| role.original() == Some(original) && role.is_published_source_original()));
        }
    }
}

#[test]
fn recovery_capture_admits_embedded_canonical_and_refuses_corruption() {
    let producer = [2; 32];
    let root = tempfile::tempdir().unwrap();
    let package_path = root.path().join("External.hi");
    let package_bytes = [0x43];
    std::fs::write(&package_path, package_bytes).unwrap();
    let package_sha256: [u8; 32] = Sha256::digest(package_bytes).into();
    let mut binder = tidepool_repr::execution_schema::testing::identity("External", "entry");
    binder.unit = "external".into();
    let packages = BTreeMap::from([(
        ("external".into(), "External".into()),
        crate::certified_products::PackageInterfaceWitness {
            selected_path: package_path,
            sha256: package_sha256,
        },
    )]);
    let product = crate::certified_products::fixture_finalized_product(
        crate::certified_products::tests::original_witness_fixture(
            "Original",
            Some(crate::certified_products::PendingImportOwner::Package {
                unit: "external".into(),
                module: "External".into(),
                binder,
                interface_digest: package_sha256,
            }),
            7,
            &packages,
        ),
        producer,
    );
    let references = recovery_artifacts::materialize_certified_products(
        root.path(),
        producer,
        std::slice::from_ref(&product),
    )
    .unwrap();
    let embedded = references[0].module_interface.as_ref().unwrap();
    let captured =
        ExactDeclarationContext::capture_recovery(root.path(), &references, &[], &[], vec![])
            .unwrap();
    let recovered = captured.recovery_products();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].owner(), product.owner());
    assert_eq!(recovered[0].product_bytes(), product.product_bytes());
    let duplicate_explicit = ExactDeclarationContext::capture_recovery(
        root.path(),
        &references,
        &[embedded.clone(), embedded.clone()],
        &[],
        vec![],
    )
    .unwrap();
    assert_eq!(
        captured.semantic_sha256(),
        duplicate_explicit.semantic_sha256()
    );
    let groups = crate::certified_products::certify_owned_products_with_validation(
        &[&recovered[0]],
        &[],
        &mut PackageInterfaceValidation::default(),
    )
    .unwrap();
    assert_eq!(groups.len(), 1);
    assert!(matches!(
        groups[0].imports()[0],
        crate::certified_products::PendingImportOwner::Package { ref unit, ref module, .. }
            if unit == "external" && module == "External"
    ));

    let mut missing = references.clone();
    missing[0].module_interface = None;
    assert!(
        ExactDeclarationContext::capture_recovery(root.path(), &missing, &[], &[], vec![]).is_err()
    );
    let mut changed = references.clone();
    changed[0]
        .module_interface
        .as_mut()
        .unwrap()
        .certificate_sha256 = [99; 32];
    assert!(
        ExactDeclarationContext::capture_recovery(root.path(), &changed, &[], &[], vec![]).is_err()
    );
    let mut conflicting_explicit = embedded.clone();
    conflicting_explicit.certificate_path = PathBuf::from("artifacts/missing-canonical.cbor");
    assert!(
        ExactDeclarationContext::capture_recovery(
            root.path(),
            &references,
            &[conflicting_explicit],
            &[],
            vec![],
        )
        .is_err(),
        "an invalid explicit descriptor cannot hide behind a matching embedded owner"
    );
    let certificate = root.path().join(&embedded.certificate_path);
    let mut corrupted = std::fs::read(&certificate).unwrap();
    corrupted[0] ^= 1;
    std::fs::write(certificate, corrupted).unwrap();
    assert!(
        ExactDeclarationContext::capture_recovery(root.path(), &references, &[], &[], vec![])
            .is_err()
    );
}

#[test]
fn retained_projection_shared_lexical_rows_preserve_custody_and_reject_conflicts() {
    let (initial, _) = metadata_fixture();
    let shared = initial
        .lexical_graph()
        .iter()
        .find(|node| node.owner == identity("fixture", "Alpha"))
        .unwrap()
        .clone();
    let merged = initial
        .as_ref()
        .clone()
        .extend_lexical_joins(&[], &[shared.clone(), shared.clone()])
        .unwrap();
    assert_eq!(merged.semantic_sha256(), initial.semantic_sha256());
    assert_eq!(merged.artifact_view(), initial.artifact_view());
    assert_eq!(merged.recovery_products(), initial.recovery_products());
    assert_eq!(merged.lexical_graph().len(), initial.lexical_graph().len());
    assert!(!merged
        .lexical_graph()
        .iter()
        .any(|node| node.owner == identity("fixture", "Beta")));

    let mut conflicting = shared.clone();
    conflicting.imports.push(identity("fixture", "Joined"));
    let error = merged
        .extend_lexical_joins(&[], &[conflicting])
        .unwrap_err();
    assert!(matches!(error, CompileError::ExtractFailed(detail)
        if detail == "exact declaration context: conflicting selected lexical imports for fixture:Alpha"));

    // A constructor still requires one row per owner; only explicit
    // composition may combine equal selections from independent receipts.
    let malformed = initial.as_ref().clone();
    let mut duplicate = malformed.owner.facts.lexical.to_vec();
    duplicate.push(shared);
    let error = malformed
        .owner
        .facts
        .lexical
        .replace(duplicate)
        .unwrap_err();
    assert!(matches!(error, CompileError::ExtractFailed(detail)
        if detail == "duplicate selected lexical owner fixture:Alpha"));
}

#[test]
fn unselected_native_versions_preserve_issued_roles_and_refuse_conflicting_offers() {
    let (initial, producer) = metadata_fixture();
    let existing = initial
        .owner
        .facts
        .compiler_support
        .artifacts()
        .entries_for_owners(std::iter::once(identity("fixture", "Alpha")))
        .unwrap();
    let ArtifactPayload::Original(product) = &existing[&identity("fixture", "Alpha")].payload
    else {
        panic!("native fixture")
    };
    let interface = product.module_interface().unwrap().clone();
    let mut owner = product.owner().clone();
    owner.module_version = tidepool_repr::execution_schema::ModuleVersion([9; 32]);
    let certification = crate::certified_products::encode_home_certification_with_module(
        &owner,
        &[],
        &BTreeMap::new(),
        interface.requirements(),
        <sha2::Sha256 as sha2::Digest>::digest(interface.certificate_bytes()).into(),
    )
    .unwrap();
    let variant = CertifiedRecoveryProduct::from_certification(
        owner,
        product.interface_bytes().to_vec(),
        product.product_bytes().to_vec(),
        product.package_imports_bytes().to_vec(),
        certification,
    )
    .with_module_interface(interface)
    .unwrap();
    let variant = Arc::new(ArtifactEntry::original(initial.owner.facts.producer, variant).unwrap());
    assert!(matches!(
        CompilerInputProjection::from_issued_entries(&[
            Arc::clone(&existing[&identity("fixture", "Alpha")]), Arc::clone(&variant),
        ]),
        Err(CompileError::ArtifactInventory(error))
            if matches!(error.failure, ArtifactInventoryFailure::CompilerOriginalOfferConflict { .. })
    ));
    let mut context = initial.as_ref().clone();
    let incoming = context
        .artifact_view()
        .inventory()
        .admit_shared(context.artifact_view(), vec![variant])
        .unwrap();
    let support = context
        .compiler_source_support()
        .retain_custody(&incoming)
        .unwrap();
    context = context.rebuild(|facts| facts.compiler_support = support);
    let context = Arc::new(context);
    assert_ne!(initial.semantic_sha256(), context.semantic_sha256());
    assert_eq!(
        initial.compiler_input_roles(),
        context.compiler_input_roles()
    );
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("selected");
    std::fs::create_dir_all(&root).unwrap();
    let artifacts = context.materialize(&root).unwrap();
    let original_root = directory.path().join("original");
    std::fs::create_dir_all(&original_root).unwrap();
    let semantic_rows = |artifacts: Vec<DeclarationArtifact>| {
        artifacts
            .into_iter()
            .map(|artifact| {
                (
                    artifact.interface.unit,
                    artifact.interface.module,
                    artifact.interface.sha256,
                    artifact.interface.requirements,
                    artifact.product.map(|product| product.sha256),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        semantic_rows(artifacts.artifacts),
        semantic_rows(initial.materialize(&original_root).unwrap().artifacts)
    );
    let request = context
        .prepare_compilation_authorizing(
            &directory.path().join("authorized"),
            &producer,
            |identity| {
                assert_eq!(identity, context.semantic_sha256());
                Ok(Value::Null)
            },
        )
        .unwrap();
    context.validate_artifacts(&request.artifacts).unwrap();
}

#[test]
fn exact_scope_evidence_uses_admitted_artifact_roles() {
    let (context, _) = metadata_fixture();
    let root = tempfile::tempdir().unwrap();
    let metadata = context
        .owner
        .facts
        .compiler_support
        .artifacts()
        .metadata_snapshot();
    for entry in metadata.entries.values() {
        let value = scope_interface_evidence(
            entry,
            root.path(),
            &mut PackageInterfaceValidation::default(),
        )
        .unwrap();
        let row = value.as_array().unwrap();
        match entry.descriptor.kind {
            ArtifactKind::OriginalModule | ArtifactKind::CanonicalModuleInterface => {
                assert_eq!(row.len(), 5);
                assert_eq!(row[0], text("module"));
                assert!(row[3].as_text().is_some());
                assert!(row[4].as_text().is_some());
            }
            ArtifactKind::LexicalJoin => assert_eq!(row, &[text("join")]),
            ArtifactKind::ValueInterface => assert_eq!(row, &[text("value")]),
        }
    }
    let mut forged = metadata.entries[&identity("fixture", "Joined")]
        .as_ref()
        .clone();
    forged.descriptor.kind = ArtifactKind::CanonicalModuleInterface;
    assert!(scope_interface_evidence(
        &forged,
        root.path(),
        &mut PackageInterfaceValidation::default()
    )
    .is_err());
}

#[test]
fn source_original_scope_role_is_not_inferred_from_native_module_spelling() {
    let interface = crate::certified_products::fixture_module_interface(
        [7; 32],
        "main",
        "Tidepool.Session.Lib.G1",
        BTreeMap::new(),
    );
    let entry = ArtifactEntry::canonical(interface);
    let root = tempfile::tempdir().unwrap();
    let value = scope_interface_evidence(
        &entry,
        root.path(),
        &mut PackageInterfaceValidation::default(),
    )
    .unwrap();
    assert_eq!(value.as_array().unwrap()[0], text("module"));
    let mut conflicting = entry;
    conflicting.descriptor.owner.unit = "other".into();
    assert!(scope_interface_evidence(
        &conflicting,
        root.path(),
        &mut PackageInterfaceValidation::default()
    )
    .is_err());
}

#[test]
fn metadata_validation_indexes_owners_and_rechecks_late_corruption() {
    let (context, _) = metadata_fixture();
    let directory = tempfile::tempdir().unwrap();
    let artifacts = context.materialize_scratch(&directory).unwrap().artifacts;
    let before = context
        .owner
        .facts
        .compiler_support
        .artifacts()
        .inventory()
        .metrics();
    context.validate_artifacts(&artifacts).unwrap();
    let after = context
        .owner
        .facts
        .compiler_support
        .artifacts()
        .inventory()
        .metrics();
    // Materialization already warmed this immutable view projection.
    assert_eq!(after.graph_visits - before.graph_visits, 0);
    assert_eq!(after.view_queries - before.view_queries, 1);
    assert_eq!(after.entry_handle_copies - before.entry_handle_copies, 10);
    assert!(context.validate_artifacts(&artifacts[..3]).is_err());
    let mut duplicate = artifacts.clone();
    duplicate.push(artifacts[0].clone());
    assert!(context.validate_artifacts(&duplicate).is_err());
    let mut wrong_kind = artifacts.clone();
    wrong_kind[0].product = None;
    assert!(context.validate_artifacts(&wrong_kind).is_err());
    let mut wrong_kind = artifacts.clone();
    wrong_kind[2].product = artifacts[0].product.clone();
    assert!(context.validate_artifacts(&wrong_kind).is_err());
    let mut wrong_requirements = artifacts.clone();
    let required = wrong_requirements
        .iter_mut()
        .find(|artifact| artifact.interface.module == "Joined")
        .unwrap();
    assert!(!required.interface.requirements.is_empty());
    required.interface.requirements.clear();
    assert!(context.validate_artifacts(&wrong_requirements).is_err());
    let mut foreign = artifacts.clone();
    foreign[0].interface.module = "Foreign".into();
    assert!(context.validate_artifacts(&foreign).is_err());
    let recovery_root = directory.path().join("recovery");
    std::fs::create_dir_all(&recovery_root).unwrap();
    let (products, interfaces) = context
        .materialize_recovery_products_and_interfaces(&recovery_root)
        .unwrap();
    let (joins, values) = context
        .materialize_joined_and_value_interfaces_for_artifacts(
            &recovery_root,
            &context.artifact_view().artifact_ids().into_iter().collect(),
        )
        .unwrap();
    let mut descriptors = context.artifact_view().descriptors();
    descriptors
        .iter_mut()
        .find(|entry| entry.owner.module == "Joined")
        .unwrap()
        .kind = ArtifactKind::OriginalModule;
    let graph = context.artifact_view().capture_graph_selection();
    assert!(
        matches!(RecoveredArtifactInventory::capture_bound(
        &recovery_root, &products, &interfaces, &joins, &values, &descriptors,
        &graph.bindings, &context.artifact_view().binding_dependencies(),
    ), Err(RecoveryInventoryError::Certification(CompileError::ExtractFailed(detail)))
        if detail == "exact declaration context: bound recovery descriptors differ from certified bytes"),
        "corrupted descriptor metadata cannot turn a certified interface into native evidence"
    );
    let paths = [
        artifacts[0].interface.path.clone(),
        artifacts[0].product.as_ref().unwrap().path.clone(),
        artifacts[0].interface.path.with_extension("hi.packages"),
    ];
    for path in paths {
        let saved = std::fs::read(&path).unwrap();
        std::fs::write(&path, b"changed after preparation").unwrap();
        assert!(context.validate_artifacts(&artifacts).is_err());
        std::fs::write(&path, saved).unwrap();
        context.validate_artifacts(&artifacts).unwrap();
    }
}

#[test]
fn metadata_authorization_and_preparation_share_one_identity() {
    let (context, producer) = metadata_fixture();
    let directory = tempfile::tempdir().unwrap();
    let expected = context.semantic_sha256();
    let before = context
        .owner
        .facts
        .compiler_support
        .artifacts()
        .inventory()
        .metrics();
    let request = context
        .prepare_compilation_authorizing(directory.path(), &producer, |semantic_sha256| {
            assert_eq!(semantic_sha256, expected);
            Ok(Value::Array(vec![
                text("authorized"),
                text(hex(&semantic_sha256)),
            ]))
        })
        .unwrap();
    let after = context
        .owner
        .facts
        .compiler_support
        .artifacts()
        .inventory()
        .metrics();
    // The expected identity query already warmed this immutable view projection.
    assert_eq!(after.graph_visits - before.graph_visits, 0);
    assert_eq!(request.semantic_sha256, expected);
    let value: Value =
        ciborium::de::from_reader(std::fs::read(&request.manifest).unwrap().as_slice()).unwrap();
    let fields = row(&value, EXACT_SCOPE_SCHEMA_FIELDS).unwrap();
    assert_eq!(string(&fields[2]).unwrap(), hex(&expected));
    assert_eq!(row(&fields[8], 2).unwrap()[1], text(hex(&expected)));
    let mut foreign = request.artifacts.clone();
    foreign[0].interface.sha256 = sha256(b"different claimed owner");
    assert!(request.context().validate_artifacts(&foreign).is_err());
}

#[test]
fn fixture_delivery_transfers_materialization_cleanup_to_packet_owner() {
    let (context, producer) = metadata_fixture();
    let packet = tempfile::tempdir().unwrap();
    let request = context
        .prepare_fixture_compilation(packet.path(), &producer)
        .unwrap();
    let encoded = std::fs::read(&request.manifest).unwrap();
    let manifest: Value = ciborium::de::from_reader(encoded.as_slice()).unwrap();
    assert_eq!(
        manifest.as_array().unwrap()[10],
        Value::Array(vec![text("fresh-files")])
    );
    assert!(
        !encoded
            .windows(b"/proc/".len())
            .any(|bytes| bytes == b"/proc/"),
        "persistable fixture must never serialize a process-owned arena endpoint"
    );
    let paths = request
        .artifacts
        .iter()
        .map(|artifact| artifact.interface.path.clone())
        .collect::<Vec<_>>();
    let owned_root = request
        .materialization
        .as_ref()
        .unwrap()
        .directory()
        .path()
        .to_path_buf();
    assert!(owned_root.starts_with(packet.path()));
    assert!(!paths.is_empty());
    assert!(paths.iter().all(|path| path.starts_with(&owned_root)));
    assert!(context
        .prepare_fixture_compilation(&packet.path().join("second"), &producer)
        .is_err());
    assert!(!packet.path().join("second").exists());
    let weak = Arc::downgrade(request.materialization.as_ref().unwrap());
    drop(request);
    drop(context);
    assert!(weak.upgrade().is_none());
    assert!(paths.iter().all(|path| path.is_file()));
    drop(packet);
    assert!(!owned_root.exists());
    assert!(paths.iter().all(|path| !path.exists()));
}

#[test]
fn completed_materialization_is_retained_when_producer_stop_arrives_after_validation() {
    use tidepool_extract_cmd::{
        with_compiler_transaction_cancellable, CompilerTransactionCancellation,
        CompilerTransactionClose,
    };
    let (context, _) = metadata_fixture();
    let metadata = context.compiler_metadata_snapshot().unwrap();
    let cancellation = CompilerTransactionCancellation::new();
    let completed = with_compiler_transaction_cancellable(
        cancellation.clone(),
        |_| {},
        || {
            context
                .owner
                .facts
                .compiler_support
                .artifacts()
                .retain_materialization(&metadata, |parents| {
                    let directory = tempfile::tempdir().unwrap();
                    let result =
                        context.materialize_retained_artifacts(&metadata, parents, directory)?;
                    // The immutable private result has passed its final validation.
                    cancellation.cancel();
                    Ok(result)
                })
        },
    );
    let retained = completed.action.unwrap();
    assert_eq!(completed.close, CompilerTransactionClose::NotStarted);
    let weak = Arc::downgrade(&retained);
    let path = retained.directory().path().to_path_buf();
    assert!(Arc::ptr_eq(
        &retained,
        &context
            .owner
            .facts
            .compiler_support
            .artifacts()
            .retained_materialization(&metadata)
            .unwrap()
            .unwrap()
    ));
    drop(retained);
    assert!(weak.upgrade().is_some());
    assert!(path.exists());
    drop(context);
    assert!(weak.upgrade().is_none());
    assert!(!path.exists());
}

#[test]
fn interrupted_materialization_preserves_completed_owner_and_fresh_scope_recovers() {
    use tidepool_extract_cmd::{
        with_compiler_transaction_cancellable, CompilerTransactionCancellation,
        CompilerTransactionClose,
    };
    let (context, producer) = metadata_fixture();
    let scratch = tempfile::tempdir().unwrap();
    let cancelled = || {
        let cancellation = CompilerTransactionCancellation::new();
        cancellation.cancel();
        cancellation
    };
    let cold = with_compiler_transaction_cancellable(
        cancelled(),
        |_| {},
        || context.prepare_compilation(&scratch.path().join("stopped-cold"), &producer),
    );
    assert!(
        matches!(cold.action, Err(CompileError::Io(error)) if error.kind() == std::io::ErrorKind::Interrupted)
    );
    assert_eq!(cold.close, CompilerTransactionClose::NotStarted);
    let first = context
        .prepare_compilation(&scratch.path().join("first"), &producer)
        .unwrap();
    let retained = first.materialization.as_ref().unwrap();
    let weak = Arc::downgrade(retained);
    let owned = retained.directory().path().to_path_buf();
    let paths = first.artifacts.clone();
    let stopped = with_compiler_transaction_cancellable(
        cancelled(),
        |_| {},
        || context.prepare_compilation(&scratch.path().join("stopped-warm"), &producer),
    );
    assert!(
        matches!(stopped.action, Err(CompileError::Io(error)) if error.kind() == std::io::ErrorKind::Interrupted)
    );
    assert_eq!(stopped.close, CompilerTransactionClose::NotStarted);
    assert!(owned.is_dir());
    assert!(paths
        .iter()
        .all(|artifact| artifact.interface.path.is_file()));
    let fresh = with_compiler_transaction_cancellable(
        CompilerTransactionCancellation::new(),
        |_| {},
        || context.prepare_compilation(&scratch.path().join("fresh"), &producer),
    );
    let recovered = fresh.action.unwrap();
    assert_eq!(fresh.close, CompilerTransactionClose::NotStarted);
    assert!(Arc::ptr_eq(
        &weak.upgrade().unwrap(),
        recovered.materialization.as_ref().unwrap()
    ));
    assert_eq!(recovered.artifacts, paths);
    drop(context);
    drop(first);
    assert!(weak.upgrade().is_some());
    drop(recovered);
    assert!(weak.upgrade().is_none());
    assert!(!owned.exists());
}

#[test]
fn input_diagnostics_preserve_exact_payloads_after_transport_files_disappear() {
    let (context, producer) = metadata_fixture();
    let scratch = tempfile::tempdir().unwrap();
    let destination = tempfile::tempdir().unwrap();
    let request = context
        .prepare_compilation(&scratch.path().join("request"), &producer)
        .unwrap();
    let expected = request
        .compiler_inputs()
        .metadata
        .entries
        .values()
        .map(|entry| {
            let (interface, packages) = match &entry.payload {
                ArtifactPayload::Original(product) => {
                    (product.interface_bytes(), product.package_imports_bytes())
                }
                ArtifactPayload::Canonical(interface) => (
                    interface.interface_bytes(),
                    interface.package_imports_bytes(),
                ),
                ArtifactPayload::Interface(interface, _) => (
                    interface.interface_bytes(),
                    interface.package_imports_bytes(),
                ),
            };
            (
                entry.descriptor.clone(),
                interface.to_vec(),
                packages.to_vec(),
            )
        })
        .collect::<Vec<_>>();
    assert!(!expected.is_empty());
    for artifact in &request.artifacts {
        std::fs::remove_file(&artifact.interface.path).unwrap();
    }
    request
        .retain_input_diagnostics(destination.path())
        .unwrap();
    drop(request);
    drop(context);
    drop(scratch);
    let directory = destination.path().join("exact-request");
    let index: serde_json::Value =
        serde_json::from_slice(&std::fs::read(directory.join("input-artifacts.json")).unwrap())
            .unwrap();
    assert_eq!(index["authority"], false);
    let inputs = index["inputs"].as_array().unwrap();
    assert_eq!(inputs.len(), expected.len());
    for (descriptor, interface, packages) in expected {
        let record = inputs
            .iter()
            .find(|record| record["descriptor"] == serde_json::to_value(&descriptor).unwrap())
            .unwrap();
        assert!(record["observed"]["error"].is_null());
        for (kind, expected) in [("interface", interface), ("packages", packages)] {
            let file = &record["observed"]["files"][kind];
            let retained = std::fs::read(
                directory
                    .join("input-artifacts")
                    .join(file["file"].as_str().unwrap()),
            )
            .unwrap();
            assert_eq!(retained, expected);
            assert_eq!(file["sha256"], crate::checked_cell::hash(&retained));
            assert_eq!(file["bytes"], retained.len());
        }
    }
}

#[test]
fn retained_materialization_survives_request_scratch_and_releases_with_owner() {
    let (context, producer) = metadata_fixture();
    let scratch = tempfile::tempdir().unwrap();
    let first = context
        .prepare_compilation(&scratch.path().join("first"), &producer)
        .unwrap();
    let paths = first.artifacts.clone();
    let groups = Arc::clone(&first.groups);
    let retained = first.materialization.as_ref().unwrap();
    let owned_root = retained.directory().path().to_path_buf();
    let weak = Arc::downgrade(retained);
    let semantic = first.semantic_sha256;
    drop(first);
    std::fs::remove_dir_all(scratch.path().join("first")).unwrap();
    assert!(paths
        .iter()
        .all(|artifact| artifact.interface.path.is_file()));
    let second = context
        .prepare_compilation(&scratch.path().join("second"), &producer)
        .unwrap();
    assert_eq!(second.artifacts, paths);
    assert_eq!(second.semantic_sha256, semantic);
    assert_eq!(groups, second.groups);
    assert!(groups
        .iter()
        .zip(second.groups.iter())
        .all(|(left, right)| std::ptr::eq(left.group(), right.group())));
    assert!(Arc::ptr_eq(
        &weak.upgrade().unwrap(),
        second.materialization.as_ref().unwrap()
    ));
    let manifest: Value =
        ciborium::de::from_reader(std::fs::read(&second.manifest).unwrap().as_slice()).unwrap();
    for (artifact, row) in paths
        .iter()
        .zip(manifest.as_array().unwrap()[4].as_array().unwrap())
    {
        assert_eq!(
            row.as_array().unwrap()[2],
            path_value(&artifact.interface.path).unwrap()
        );
    }
    drop(context);
    assert!(
        owned_root.is_dir(),
        "the current request retains the private owner"
    );
    drop(second);
    assert!(weak.upgrade().is_none());
    assert!(
        !owned_root.exists(),
        "last owner release removes private materialization"
    );
}

#[test]
fn late_private_key_capture_keeps_selected_files_after_ancestor_retirement() {
    let (ancestor, producer) = metadata_fixture();
    let scratch = tempfile::tempdir().unwrap();
    let gamma = crate::certified_products::fixture_finalized_product(
        support_product("Gamma"),
        ancestor.owner.facts.producer,
    );
    let descendant = Arc::new(
        ancestor
            .as_ref()
            .clone()
            .extend_checked_original_products(ancestor.owner.facts.producer, &[gamma])
            .unwrap(),
    );
    let completed = descendant
        .prepare_compilation(&scratch.path().join("descendant"), &producer)
        .unwrap();
    let delta = scaffold_native_fixture(ancestor.owner.facts.producer, "fixture", "LatePrivate", 1);
    let producer_identity = CanonicalProducerIdentity::from_producer_bytes(&producer);
    let private = OriginalCompilerInputs::from_native_availability(
        &ancestor,
        producer_identity,
        std::slice::from_ref(&delta),
    )
    .unwrap();
    let late = ancestor
        .prepare_compilation_with_scaffold(
            &scratch.path().join("late-private"),
            &producer,
            None,
            &ProtectedScaffoldRequirements::default(),
            Some(private),
        )
        .unwrap();
    let late_file = late
        .artifacts
        .iter()
        .find(|artifact| artifact.interface.module == "LatePrivate")
        .unwrap()
        .interface
        .clone();
    let epsilon = crate::certified_products::fixture_finalized_product(
        support_product("Epsilon"),
        descendant.owner.facts.producer,
    );
    let next = Arc::new(
        descendant
            .as_ref()
            .clone()
            .extend_checked_original_products(descendant.owner.facts.producer, &[epsilon])
            .unwrap(),
    );
    let private =
        OriginalCompilerInputs::from_native_availability(&next, producer_identity, &[delta])
            .unwrap();
    let private_support =
        crate::artifact_inventory::CompilerSourceSupport::close_complete_output(&private).unwrap();
    let request = next
        .prepare_compilation_with_scaffold(
            &scratch.path().join("next-private"),
            &producer,
            None,
            &ProtectedScaffoldRequirements::default(),
            Some(private),
        )
        .unwrap();
    request.validate_artifacts().unwrap();
    assert_ne!(
        completed.compiler_inputs().metadata.materialization_key(),
        request.compiler_inputs().metadata.materialization_key()
    );
    let selected = &request.compiler_inputs().metadata.entries[&identity("fixture", "LatePrivate")];
    let id = selected.descriptor.id;
    let file = request
        .artifacts
        .iter()
        .find(|artifact| artifact.interface.module == "LatePrivate")
        .unwrap()
        .interface
        .clone();
    assert_eq!(file.sha256, late_file.sha256);
    assert_eq!(
        std::fs::read(&file.path).unwrap(),
        std::fs::read(&late_file.path).unwrap()
    );
    let mut captured = next.as_ref().clone();
    let support = private_support
        .retain_custody(request.compiler_inputs().artifacts())
        .unwrap()
        .select_support_roots(vec![id])
        .unwrap();
    captured = captured.rebuild(|facts| {
        facts.compiler_support = support;
        facts.lexical = crate::exact_context::ExactLexicalSnapshot::default();
    });
    drop(private_support);
    captured.normalize().unwrap();
    let captured = Arc::new(captured);
    let selected_owner = Arc::downgrade(
        &captured
            .compiler_metadata_snapshot()
            .unwrap()
            .retained
            .artifacts[&id],
    );
    let proof = Arc::clone(
        captured
            .owner
            .facts
            .compiler_support
            .artifacts()
            .selection_snapshot(),
    );
    drop(request);
    drop(late);
    drop(completed);
    drop(next);
    drop(descendant);
    drop(ancestor);
    assert!(
        file.path.exists(),
        "detached selection owns its selected file"
    );
    if file.path != late_file.path {
        assert!(
            !late_file.path.exists(),
            "unselected late source files retire"
        );
    }
    let restored = captured
        .prepare_compilation(&scratch.path().join("detached"), &producer)
        .unwrap();
    restored.validate_artifacts().unwrap();
    assert_eq!(restored.artifacts.len(), 1);
    assert_eq!(restored.artifacts[0].interface.path, file.path);
    drop(restored);
    drop(captured);
    assert!(!file.path.exists());
    assert!(selected_owner.upgrade().is_none());
    assert!(!proof.scopes.is_empty());
}

#[test]
fn completed_descendant_subsumes_late_ancestor_materialization() {
    let (ancestor, producer) = metadata_fixture();
    let scratch = tempfile::tempdir().unwrap();
    let gamma = crate::certified_products::fixture_finalized_product(
        support_product("Gamma"),
        ancestor.owner.facts.producer,
    );
    let recovered =
        crate::certified_products::tests::recovered_witness_fixtures(std::slice::from_ref(&gamma))
            .remove(0)
            .product;
    assert_eq!(recovered.owner(), gamma.owner());
    assert_eq!(recovered.interface_bytes(), gamma.interface_bytes());
    assert_eq!(recovered.product_bytes(), gamma.product_bytes());
    assert_eq!(recovered.certification_bytes(), gamma.certification_bytes());
    assert_eq!(
        crate::certified_products::original_available_groups(&recovered)
            .unwrap()
            .count(),
        0,
        "empty originals still need an authenticated availability census"
    );
    let gamma = recovered;
    let descendant = Arc::new(
        ancestor
            .as_ref()
            .clone()
            .extend_checked_original_products(ancestor.owner.facts.producer, &[gamma])
            .unwrap(),
    );
    let completed = descendant
        .prepare_compilation(&scratch.path().join("descendant"), &producer)
        .unwrap();
    let owner = completed.materialization.as_ref().unwrap();
    assert!(owner._parents.is_empty());
    let late = ancestor
        .prepare_compilation(&scratch.path().join("late-ancestor"), &producer)
        .unwrap();
    let delta = crate::certified_products::fixture_finalized_product(
        support_product("Delta"),
        descendant.owner.facts.producer,
    );
    let recovered =
        crate::certified_products::tests::recovered_witness_fixtures(std::slice::from_ref(&delta))
            .remove(0)
            .product;
    assert_eq!(recovered.owner(), delta.owner());
    assert_eq!(recovered.interface_bytes(), delta.interface_bytes());
    assert_eq!(recovered.product_bytes(), delta.product_bytes());
    assert_eq!(recovered.certification_bytes(), delta.certification_bytes());
    assert_eq!(
        crate::certified_products::original_available_groups(&recovered)
            .unwrap()
            .count(),
        0,
        "empty originals still need an authenticated availability census"
    );
    let delta = recovered;
    let next = Arc::new(
        descendant
            .as_ref()
            .clone()
            .extend_checked_original_products(descendant.owner.facts.producer, &[delta])
            .unwrap(),
    );
    let request = next
        .prepare_compilation(&scratch.path().join("next"), &producer)
        .unwrap();
    let next_owner = request.materialization.as_ref().unwrap();
    assert_eq!(next_owner.rows.len(), 1);
    assert_eq!(next_owner._parents.len(), 1);
    assert!(Arc::ptr_eq(&next_owner._parents[0], owner));
    assert!(completed
        .artifacts
        .iter()
        .all(|artifact| request.artifacts.contains(artifact)));
    assert!(late
        .artifacts
        .iter()
        .all(|artifact| !request.artifacts.contains(artifact)));
}

#[test]
fn retained_materialization_prefix_a_b_a_borrows_only_unchanged_entries() {
    let (a, producer) = metadata_fixture();
    let scratch = tempfile::tempdir().unwrap();
    let first = a
        .prepare_compilation(&scratch.path().join("a1"), &producer)
        .unwrap();
    let a_owner = first.materialization.as_ref().unwrap();
    let a_directory = a_owner._directory.as_ref().unwrap();
    let a_directory_weak = Arc::downgrade(a_directory);
    let a_files = private_directory_file_bytes(a_directory.path());
    let gamma = crate::certified_products::fixture_finalized_product(
        support_product("Gamma"),
        a.owner.facts.producer,
    );
    let recovered =
        crate::certified_products::tests::recovered_witness_fixtures(std::slice::from_ref(&gamma))
            .remove(0)
            .product;
    assert_eq!(recovered.owner(), gamma.owner());
    assert_eq!(recovered.interface_bytes(), gamma.interface_bytes());
    assert_eq!(recovered.product_bytes(), gamma.product_bytes());
    assert_eq!(recovered.certification_bytes(), gamma.certification_bytes());
    assert_eq!(
        crate::certified_products::original_available_groups(&recovered)
            .unwrap()
            .count(),
        0,
        "empty originals still need an authenticated availability census"
    );
    let gamma = recovered;
    let b = Arc::new(
        a.as_ref()
            .clone()
            .extend_checked_original_products(a.owner.facts.producer, &[gamma])
            .unwrap(),
    );
    let second = b
        .prepare_compilation(&scratch.path().join("b"), &producer)
        .unwrap();
    for artifact in &first.artifacts {
        assert!(
            second.artifacts.contains(artifact),
            "B references A's unchanged private paths"
        );
    }
    assert_eq!(second.artifacts.len(), first.artifacts.len() + 1);
    let b_owner = second.materialization.as_ref().unwrap();
    assert_eq!(b_owner.rows.len(), 1, "B owns only its new physical row");
    let b_metadata = b.compiler_metadata_snapshot().unwrap();
    assert!(b_owner.rows.contains_key(
        &b_metadata.entries[&identity("fixture", "Gamma")]
            .descriptor
            .id
    ));
    let b_rows = b_owner.selected_rows(&b_metadata);
    for artifact in &first.artifacts {
        let entry =
            &b_metadata.entries[&identity(&artifact.interface.unit, &artifact.interface.module)];
        let inherited = b_rows[&entry.descriptor.id];
        assert_eq!(inherited.artifact(), *artifact);
        assert!(Arc::ptr_eq(&inherited._directory, a_directory));
    }
    assert_eq!(private_directory_file_bytes(a_directory.path()), a_files);
    let new_file_bytes = private_directory_file_bytes(b_owner.directory().path())
        .values()
        .map(|bytes| bytes.len() as u64)
        .sum::<u64>();
    assert_eq!(
        b_owner.payload_work.written_bytes, new_file_bytes,
        "B writes its new files without rewriting inherited payloads"
    );
    assert!(new_file_bytes > 0);
    let gamma = second
        .artifacts
        .iter()
        .find(|artifact| artifact.interface.module == "Gamma")
        .unwrap();
    assert!(gamma.interface.path.starts_with(b_owner.directory().path()));
    let third = a
        .prepare_compilation(&scratch.path().join("a2"), &producer)
        .unwrap();
    assert_eq!(third.artifacts, first.artifacts);
    assert_eq!(third.semantic_sha256, first.semantic_sha256);
    assert_ne!(third.semantic_sha256, second.semantic_sha256);
    assert!(Arc::ptr_eq(
        third.materialization.as_ref().unwrap(),
        first.materialization.as_ref().unwrap()
    ));
    let root_a = first
        .materialization
        .as_ref()
        .unwrap()
        .directory()
        .path()
        .to_path_buf();
    drop(first);
    drop(third);
    drop(a);
    assert!(root_a.exists(), "B retains borrowed A paths");
    assert!(a_directory_weak.upgrade().is_some());
    assert_eq!(private_directory_file_bytes(&root_a), a_files);
    drop(second);
    drop(b);
    assert!(!root_a.exists());
    assert!(a_directory_weak.upgrade().is_none());
}

#[test]
fn retained_materialization_nonempty_groups_select_versions_and_shared_parents() {
    let (initial, producer) = metadata_fixture();
    let product = |module, version, generation| {
        let original = crate::certified_products::fixture_finalized_product(
            crate::certified_products::tests::original_witness_fixture(
                module,
                Some(crate::certified_products::PendingImportOwner::Retained {
                    identity: tidepool_repr::execution_schema::testing::identity("Value", "live"),
                    generation,
                }),
                version,
                &BTreeMap::new(),
            ),
            initial.owner.facts.producer,
        );
        crate::certified_products::tests::recovered_witness_fixtures(&[original])
            .remove(0)
            .product
    };
    let extend = |context: &Arc<ExactDeclarationContext>, product| {
        Arc::new(
            context
                .as_ref()
                .clone()
                .extend_checked_original_products(context.owner.facts.producer, &[product])
                .unwrap(),
        )
    };
    let scratch = tempfile::tempdir().unwrap();
    let native = product("Native", 7, 11);
    let base = extend(&initial, native.clone());
    let base_request = base
        .prepare_compilation(&scratch.path().join("base"), &producer)
        .unwrap();
    assert!(base_request.groups.is_empty());
    let demand = |request: &ExactCompilationRequest,
                  expected: &CertifiedRecoveryProduct|
     -> Result<CertifiedRecoveryProduct, CompileError> {
        let module = expected.owner().module.as_str();
        let inputs = request.compiler_inputs();
        assert!(inputs.artifacts().selected_native_groups().is_empty());
        let id = ArtifactEntry::original(request.producer_sha256, expected.clone())
            .unwrap()
            .descriptor
            .id;
        let product = expected;
        let definitions =
            crate::certified_products::original_native_group_definitions(&product, 7).unwrap();
        let binding = match &definitions.bindings()[0] {
            tidepool_repr::execution_schema::Group::NonRecursive(binding) => binding,
            tidepool_repr::execution_schema::Group::Recursive(bindings) => &bindings[0],
        };
        let imports = [crate::certified_products::PendingImportOwner::Source {
            owner: product.owner().clone(),
            original_ordinal: 7,
            binder: binding.identity.clone(),
        }];
        let validation = PackageInterfaceValidation::default();
        let selection = crate::certified_products::CertifiedSourceSelection::from_compiler_inputs(
            inputs,
            &inputs.metadata,
            &validation.inventory,
        )
        .unwrap();
        let mut fixed = inputs
            .artifacts()
            .inventory()
            .bind_segment(&inputs.artifacts(), vec![], &selection)
            .unwrap();
        let target = inputs.artifacts().inventory().admit_bound_target(
            &mut fixed,
            &inputs.artifacts(),
            crate::artifact_inventory::NativeArtifactDemand::CertifiedTargetImports(&imports),
            vec![],
            &selection,
        )?;
        let groups = target
            .clone()
            .bind_target(None, (&imports).to_vec())
            .resolve()
            .map(|resolved| resolved.groups().clone())
            .unwrap();
        assert_eq!(groups.len(), 1);
        let group = groups.iter().next().unwrap();
        assert_eq!(group.binding.artifact(), id);
        assert_eq!(group.original_ordinal, 7);
        assert_eq!(
            target.artifacts().selected_native_groups(),
            BTreeSet::from([crate::artifact_inventory::NativeGroupKey {
                artifact: id,
                original_ordinal: 7,
            }]),
            "target selection contains exactly its authenticated demand"
        );
        assert!(
            inputs.artifacts().selected_native_groups().is_empty(),
            "target demand does not promote compiler-input selection"
        );
        let physical = request
            .artifacts
            .iter()
            .find(|artifact| {
                artifact.interface.unit == product.owner().unit
                    && artifact.interface.module == module
            })
            .unwrap();
        assert_eq!(
            std::fs::read(&physical.product.as_ref().unwrap().path).unwrap(),
            product.product_bytes()
        );
        Ok(target
            .artifacts()
            .entries_for_artifacts(&BTreeSet::from([id]))
            .unwrap()
            .into_iter()
            .find_map(|entry| match &entry.payload {
                ArtifactPayload::Original(product) => Some(product.clone()),
                _ => None,
            })
            .expect("the resolved target retains its exact original payload"))
    };
    let base_native = demand(&base_request, &native).unwrap();
    assert_eq!(base_native.owner().module_version.0, [7; 32]);
    assert!(matches!(
        crate::certified_products::original_native_group_imports(&base_native, 7).unwrap()[0],
        crate::certified_products::PendingImportOwner::Retained { generation: 11, .. }
    ));
    let native_descriptor = |request: &ExactCompilationRequest| {
        let bytes = std::fs::read(&request.manifest).unwrap();
        let manifest: Value = ciborium::de::from_reader(bytes.as_slice()).unwrap();
        let row = current_scope_fields(&manifest)[6]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row.as_array().unwrap()[1] == text("Native"))
            .unwrap();
        let fields = row.as_array().unwrap();
        assert_eq!(fields.len(), 10);
        assert!(fields[6].as_array().unwrap().is_empty());
        assert_eq!(fields[8], Value::Array(vec![text("full-carrier")]));
        assert_eq!(fields[9], text("compiler-original"));
        let descriptor = fields[7].as_array().unwrap();
        let Value::Text(path) = &descriptor[0] else {
            unreachable!()
        };
        assert!(Path::new(path).is_absolute());
        let certificate = std::fs::read(path).unwrap();
        assert_eq!(descriptor[1], text(sha256(&certificate)));
        assert_eq!(
            certificate,
            request
                .context()
                .recovery_products()
                .iter()
                .find(|product| product.owner().module == "Native")
                .unwrap()
                .certification_bytes()
        );
        fields[7].clone()
    };
    let base_descriptor = native_descriptor(&base_request);
    let left_product = product("Left", 8, 12);
    let right_product = product("Right", 9, 13);
    let left = extend(&base, left_product.clone());
    let right = extend(&base, right_product.clone());
    let left_request = left
        .prepare_compilation(&scratch.path().join("left"), &producer)
        .unwrap();
    let right_request = right
        .prepare_compilation(&scratch.path().join("right"), &producer)
        .unwrap();
    let mut joined = left.as_ref().clone();
    let support = joined
        .compiler_source_support()
        .retain_custody(right.artifact_view())
        .unwrap();
    joined = joined.rebuild(|facts| facts.compiler_support = support);
    joined.normalize().unwrap();
    let joined = Arc::new(joined);
    let joined_request = joined
        .prepare_compilation(&scratch.path().join("joined"), &producer)
        .unwrap();
    assert_eq!(native_descriptor(&joined_request), base_descriptor);
    let joined_owner = joined_request.materialization.as_ref().unwrap();
    assert!(joined_request.groups.is_empty());
    assert!(
        joined_owner.groups.is_empty(),
        "availability selects no compiler group"
    );
    assert_eq!(joined_owner._parents.len(), 2);
    let base_owner = base_request.materialization.as_ref().unwrap();
    for branch in [&left_request, &right_request] {
        let owner = branch.materialization.as_ref().unwrap();
        assert!(branch.groups.is_empty());
        assert!(owner.groups.is_empty());
        assert_eq!(owner.rows.len(), 1);
        assert!(Arc::ptr_eq(&owner._parents[0], base_owner));
    }
    let mut visited = Vec::new();
    joined_owner.visit_owners(&mut BTreeSet::new(), &mut |owner| {
        visited.push(owner as *const RetainedArtifactMaterialization);
    });
    assert_eq!(visited.len(), 4, "the diamond visits its shared base once");
    assert_eq!(visited[0], Arc::as_ptr(base_owner));
    assert_eq!(visited[3], Arc::as_ptr(joined_owner));
    for (branch, expected, version, generation) in [
        (&base_request, &native, 7, 11),
        (&left_request, &left_product, 8, 12),
        (&right_request, &right_product, 9, 13),
    ] {
        let module = expected.owner().module.as_str();
        let original = demand(branch, expected).unwrap();
        let selected = if expected.owner() == right_product.owner() {
            let id = ArtifactEntry::original(joined_request.producer_sha256, expected.clone())
                .unwrap()
                .descriptor
                .id;
            let inputs = joined_request.compiler_inputs();
            assert!(inputs
                .projection()
                .snapshot_roles()
                .get(&identity(&expected.owner().unit, module))
                .is_none());
            assert!(inputs.native_availability(id).is_none());
            assert!(inputs
                .compiler_source_support()
                .native_offers()
                .iter()
                .all(|offer| offer.original() != id));
            assert!(matches!(
                demand(&joined_request, expected),
                Err(CompileError::ArtifactInventory(error)) if matches!(error.failure,
                    crate::artifact_inventory::ArtifactInventoryFailure::NativeGroupUnavailable {
                        artifact, original_ordinal: 7,
                    } if artifact == id)
            ));
            joined_request
                .context()
                .artifact_view()
                .entries_for_artifacts(&BTreeSet::from([id]))
                .unwrap()
                .into_iter()
                .find_map(|entry| match &entry.payload {
                    ArtifactPayload::Original(product) => Some(product.clone()),
                    _ => None,
                })
                .expect("custody-only join retains Right's actual original payload")
        } else {
            demand(&joined_request, expected).unwrap()
        };
        assert_eq!(selected.owner(), original.owner());
        assert_eq!(selected.owner().module_version.0, [version; 32]);
        assert_eq!(selected.product_bytes(), original.product_bytes());
        assert_eq!(
            selected.certification_bytes(),
            original.certification_bytes()
        );
        assert!(std::ptr::eq(
            crate::certified_products::original_native_group_definitions(&selected, 7)
                .unwrap()
                .bindings(),
            crate::certified_products::original_native_group_definitions(&original, 7)
                .unwrap()
                .bindings(),
        ));
        let imports =
            crate::certified_products::original_native_group_imports(&selected, 7).unwrap();
        assert!(std::ptr::eq(
            imports,
            crate::certified_products::original_native_group_imports(&original, 7).unwrap()
        ));
        assert!(
            matches!(imports[0], crate::certified_products::PendingImportOwner::Retained { generation: actual, .. } if actual == generation)
        );
        let physical = branch
            .artifacts
            .iter()
            .find(|artifact| artifact.interface.module == module)
            .unwrap();
        fn issued_row(
            owner: &RetainedArtifactMaterialization,
            id: ArtifactId,
        ) -> &Arc<RetainedArtifactRow> {
            let mut row = None;
            owner.visit_owners(&mut BTreeSet::new(), &mut |owner| {
                if let Some(issued) = owner.rows.get(&id) {
                    row = Some(issued);
                }
            });
            row.expect("physical custody retains the exact issued artifact row")
        }
        let id = ArtifactEntry::original(branch.producer_sha256, expected.clone())
            .unwrap()
            .descriptor
            .id;
        let joined_row = issued_row(joined_owner, id);
        let branch_row = issued_row(branch.materialization.as_ref().unwrap(), id);
        assert!(Arc::ptr_eq(joined_row, branch_row));
        assert_eq!(joined_row.artifact(), *physical);
        if expected.owner() == right_product.owner() {
            assert!(joined_request.artifacts.iter().all(|artifact| {
                artifact.interface.unit != expected.owner().unit
                    || artifact.interface.module != module
            }));
        } else {
            assert!(
                joined_request.artifacts.contains(physical),
                "selected compiler inputs share exact issued paths"
            );
        }
    }

    let replacement_product = product("Native", 10, 22);
    let replacement = extend(&initial, replacement_product.clone());
    let replacement_request = replacement
        .prepare_compilation(&scratch.path().join("replacement"), &producer)
        .unwrap();
    assert!(replacement_request.groups.is_empty());
    let selected = demand(&replacement_request, &replacement_product).unwrap();
    assert_eq!(selected.owner().module_version.0, [10; 32]);
    assert_ne!(selected.owner(), base_native.owner());
    assert!(matches!(
        crate::certified_products::original_native_group_imports(&selected, 7).unwrap()[0],
        crate::certified_products::PendingImportOwner::Retained { generation: 22, .. }
    ));
    {
        let metadata = &replacement_request
            .compiler_inputs()
            .native_transport
            .metadata;
        let mixed = RetainedArtifactMaterialization::select_custody_inputs(
            &[
                Arc::clone(joined_owner),
                Arc::clone(replacement_request.materialization.as_ref().unwrap()),
            ],
            &metadata.retained,
        );
        let expected = metadata
            .entries
            .values()
            .map(|entry| entry.descriptor.id)
            .collect::<BTreeSet<_>>();
        let mut actual = BTreeSet::new();
        let mut stored_rows = 0;
        let mut visited = BTreeSet::new();
        for owner in &mixed {
            owner.visit_owners(&mut visited, &mut |owner| {
                actual.extend(owner.rows.keys().copied());
                stored_rows += owner.rows.len();
                assert!(owner.groups.is_empty());
            });
        }
        assert_eq!(
            actual, expected,
            "mixed custody excludes old Native and unrelated Left/Right rows"
        );
        assert_eq!(
            stored_rows,
            expected.len(),
            "excluded or shadowed physical rows are not retained"
        );
        let selected_rows = mixed
            .iter()
            .flat_map(|owner| owner.selected_rows(metadata))
            .collect::<BTreeMap<_, _>>();
        let mut selected_files = selected_rows
            .values()
            .map(|row| row.artifact())
            .collect::<Vec<_>>();
        let mut expected_files = replacement_request.artifacts.clone();
        selected_files.sort_by(|left, right| left.interface.path.cmp(&right.interface.path));
        expected_files.sort_by(|left, right| left.interface.path.cmp(&right.interface.path));
        assert_eq!(
            selected_files, expected_files,
            "selected facts retain only the replacement's issuing files"
        );
    }
    let base_directory = Arc::downgrade(base_owner._directory.as_ref().unwrap());
    let base_file = base_request
        .artifacts
        .iter()
        .find(|artifact| artifact.interface.module == "Native")
        .unwrap()
        .product
        .as_ref()
        .unwrap()
        .path
        .clone();
    drop(base_request);
    drop(left_request);
    drop(right_request);
    drop(left);
    drop(right);
    drop(base);
    drop(joined);
    assert!(base_directory.upgrade().is_some());
    assert!(
        base_file.exists(),
        "joined request retains the issued physical file"
    );
    drop(joined_request);
    assert!(base_directory.upgrade().is_none());
    assert!(!base_file.exists());
}

fn check_retained_value_compiler_selection(old_epoch: u8, new_epoch: u8, demands_epoch: bool) {
    let (initial, _) = metadata_fixture();
    let epoch = |version: u8| {
        let finalized = crate::certified_products::fixture_finalized_product(
            support_product_with_interface("fixture", "Epoch", vec![version; 16]),
            initial.owner.facts.producer,
        );
        let recovered = crate::certified_products::tests::recovered_witness_fixtures(
            std::slice::from_ref(&finalized),
        )
        .remove(0)
        .product;
        assert_eq!(recovered.owner(), finalized.owner());
        assert_eq!(recovered.interface_bytes(), finalized.interface_bytes());
        assert_eq!(recovered.product_bytes(), finalized.product_bytes());
        assert_eq!(
            recovered.certification_bytes(),
            finalized.certification_bytes()
        );
        assert_eq!(
            crate::certified_products::original_available_groups(&recovered)
                .unwrap()
                .count(),
            0
        );
        recovered
    };
    let old = initial
        .as_ref()
        .clone()
        .extend_checked_original_products(initial.owner.facts.producer, &[epoch(old_epoch)])
        .unwrap();
    let child = initial
        .as_ref()
        .clone()
        .extend_checked_original_products(initial.owner.facts.producer, &[epoch(new_epoch)])
        .unwrap();
    let selected = old
        .artifact_view()
        .entries_for_owners(std::iter::once(identity(
            "fixture",
            if demands_epoch { "Epoch" } else { "Value" },
        )))
        .unwrap();
    let root = selected.values().next().unwrap().descriptor.id;
    let value = old.artifact_view().select_roots(vec![root]).unwrap();
    let surface = old.retain_value_source_surface(&value, &[]).unwrap();
    assert!(surface
        .artifacts
        .descriptors()
        .iter()
        .any(|row| row.owner == identity("fixture", "Epoch")
            && row.kind == ArtifactKind::OriginalModule));
    assert_eq!(
        surface
            .compiler_support
            .artifacts()
            .descriptors()
            .iter()
            .any(|row| row.owner == identity("fixture", "Epoch")),
        demands_epoch
    );
    let attached = child
        .artifact_view()
        .merge_fixture_selections(&surface.compiler_support.artifacts());
    assert_eq!(attached.is_ok(), !demands_epoch || old_epoch == new_epoch);
    if old_epoch != new_epoch {
        assert!(child
            .artifact_view()
            .merge_fixture_selections(&surface.artifacts)
            .is_err());
    }
    // Retiring the issuing source cannot revoke byte custody owned by the value.
    let old_ids = surface.artifacts.artifact_ids();
    drop(old);
    assert_eq!(surface.artifacts.artifact_ids(), old_ids);
}

#[test]
fn retained_value_compiler_selection_separates_unused_native_epochs() {
    check_retained_value_compiler_selection(1, 2, false);
    check_retained_value_compiler_selection(1, 2, true);
    check_retained_value_compiler_selection(1, 1, true);
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(24))]
    #[test]
    fn retained_value_compiler_selection_epoch_history(
        old in 0u8..8, new in 0u8..8, demands_epoch in proptest::bool::ANY
    ) {
        check_retained_value_compiler_selection(old, new, demands_epoch);
    }
}

#[test]
fn retained_value_keeps_nonlexical_originals_and_projected_file_custody() {
    let (initial, producer) = metadata_fixture();
    let native = crate::certified_products::fixture_finalized_product(
        crate::certified_products::tests::original_witness_fixture(
            "HiddenNative",
            Some(crate::certified_products::PendingImportOwner::Retained {
                identity: tidepool_repr::execution_schema::testing::identity("Value", "live"),
                generation: 11,
            }),
            7,
            &BTreeMap::new(),
        ),
        initial.owner.facts.producer,
    );
    let native = crate::certified_products::tests::recovered_witness_fixtures(&[native])
        .remove(0)
        .product;
    let context = Arc::new(
        initial
            .as_ref()
            .clone()
            .extend_checked_original_products(initial.owner.facts.producer, &[native])
            .unwrap(),
    );
    let scratch = tempfile::tempdir().unwrap();
    let original = context
        .prepare_compilation(&scratch.path().join("original"), &producer)
        .unwrap();
    assert!(
        original.groups.is_empty(),
        "Full availability alone selects no compiler group"
    );
    {
        let inputs = original.compiler_inputs();
        let (id, native) = inputs
            .metadata
            .entries
            .values()
            .find_map(|entry| match &entry.payload {
                ArtifactPayload::Original(native) if native.owner().module == "HiddenNative" => {
                    Some((entry.descriptor.id, native))
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(native.owner().module_version.0, [7; 32]);
        let definitions =
            crate::certified_products::original_native_group_definitions(native, 7).unwrap();
        let binding = match &definitions.bindings()[0] {
            tidepool_repr::execution_schema::Group::NonRecursive(binding) => binding,
            tidepool_repr::execution_schema::Group::Recursive(bindings) => &bindings[0],
        };
        let imports = [crate::certified_products::PendingImportOwner::Source {
            owner: native.owner().clone(),
            original_ordinal: 7,
            binder: binding.identity.clone(),
        }];
        assert!(matches!(
            crate::certified_products::original_native_group_imports(native, 7).unwrap()[0],
            crate::certified_products::PendingImportOwner::Retained { generation: 11, .. }
        ));
        let validation = PackageInterfaceValidation::default();
        let selection = crate::certified_products::CertifiedSourceSelection::from_compiler_inputs(
            inputs,
            &inputs.metadata,
            &validation.inventory,
        )
        .unwrap();
        let mut fixed = inputs
            .artifacts()
            .inventory()
            .bind_segment(&inputs.artifacts(), vec![], &selection)
            .unwrap();
        let target = inputs
            .artifacts()
            .inventory()
            .admit_bound_target(
                &mut fixed,
                &inputs.artifacts(),
                crate::artifact_inventory::NativeArtifactDemand::CertifiedTargetImports(&imports),
                vec![],
                &selection,
            )
            .unwrap();
        let groups = target
            .clone()
            .bind_target(None, (&imports).to_vec())
            .resolve()
            .map(|resolved| resolved.groups().clone())
            .unwrap();
        assert_eq!(groups.len(), 1);
        let group = groups.iter().next().unwrap();
        assert_eq!(group.binding.artifact(), id);
        assert_eq!(group.original_ordinal, 7);
        assert_eq!(
            target.artifacts().selected_native_groups(),
            BTreeSet::from([crate::artifact_inventory::NativeGroupKey {
                artifact: id,
                original_ordinal: 7,
            }]),
            "target selection contains exactly its authenticated demand"
        );
        assert!(
            inputs.artifacts().selected_native_groups().is_empty(),
            "target demand does not promote compiler-input selection"
        );
    }
    let private_owner = original.materialization.as_ref().unwrap();
    let private_root = private_owner.directory().path().to_path_buf();
    let weak = Arc::downgrade(private_owner);
    let value_entry = context
        .artifact_view()
        .entries_for_owners(std::iter::once(identity("fixture", "Value")))
        .unwrap();
    let value = context
        .artifact_view()
        .select_roots(vec![
            value_entry[&identity("fixture", "Value")].descriptor.id,
        ])
        .unwrap();
    let RetainedValueSourceSurface {
        artifacts: retained,
        lexical,
        ..
    } = context.retain_value_source_surface(&value, &[]).unwrap();
    assert!(lexical
        .iter()
        .all(|node| node.owner.module != "HiddenNative"));
    assert!(retained
        .descriptors()
        .iter()
        .any(|descriptor| descriptor.kind
            == crate::artifact_inventory::ArtifactKind::OriginalModule
            && descriptor.owner.module == "HiddenNative"));
    let interfaces = context
        .compiler_source_support()
        .selected_interfaces()
        .unwrap()
        .select_owners(
            context
                .compiler_input_projection()
                .snapshot_roles()
                .keys()
                .filter(|owner| owner.module != "Joined")
                .cloned()
                .collect(),
        )
        .unwrap();
    assert!(interfaces.descriptors().iter().all(
        |descriptor| descriptor.kind != crate::artifact_inventory::ArtifactKind::OriginalModule
    ));
    assert!(retained
        .descriptors()
        .iter()
        .all(|descriptor| descriptor.owner.module != "Joined"));
    assert!(
        matches!(recover_fixture_compiler_support(
        context.compiler_input_projection().clone(), retained.clone(), [],
    ), Err(CompileError::ExtractFailed(detail))
        if detail == "exact declaration context: bound recovery namespace differs from recorded compiler roles"),
        "the former Joined compiler role cannot outlive its retained interface"
    );
    let hidden_original = retained
        .descriptors()
        .into_iter()
        .find(|descriptor| {
            descriptor.kind == crate::artifact_inventory::ArtifactKind::OriginalModule
                && descriptor.owner.module == "HiddenNative"
        })
        .unwrap()
        .id;
    let hidden_role = context
        .compiler_input_roles()
        .into_iter()
        .find(|role| role.original() == Some(hidden_original))
        .unwrap();
    let projection = context.compiler_input_projection().within_view(&retained);
    let roles = projection.roles();
    let mut graph = retained.capture_graph_selection();
    graph.namespace = context
        .artifact_view()
        .capture_graph_selection()
        .namespace
        .into_iter()
        .filter(|binding| graph.bindings.contains(binding))
        .collect();
    let inventory = RecoveredArtifactInventory {
        producer: context.owner.facts.producer,
        entries: retained
            .entries()
            .into_iter()
            .map(|entry| (entry.descriptor.id, entry))
            .collect(),
        recorded_inventory: true,
        bindings: graph.bindings.iter().copied().collect(),
        binding_edges: retained.binding_dependencies(),
        interfaces: retained.interface_dependencies(),
    };
    let support = crate::artifact_inventory::CompilerSourceSupport::from_recovered_selection(
        inventory
            .authenticate_graph_selection(&graph, &roles)
            .unwrap(),
    )
    .unwrap()
    .retain_offers(
        context
            .compiler_source_support()
            .native_offers()
            .iter()
            .cloned(),
    )
    .unwrap()
    .retain_custody(&retained)
    .unwrap();
    let mut next = context.as_ref().clone();
    next = next.rebuild(|facts| {
        facts.compiler_support = support;
        facts.lexical = facts.lexical.replace(lexical).unwrap();
    });
    next.normalize().unwrap();
    assert!(next.compiler_input_roles().contains(&hidden_role));
    let next = Arc::new(next);
    let request = next
        .prepare_compilation(&scratch.path().join("projected"), &producer)
        .unwrap();
    let projected_owner = request.materialization.as_ref().unwrap();
    assert!(projected_owner.rows.is_empty());
    assert_eq!(projected_owner.payload_work.written_bytes, 0);
    assert!(
        request.groups.is_empty(),
        "retained private custody is not executable demand"
    );
    assert!(
        original.groups.is_empty(),
        "lexical visibility and Full availability do not select compiler execution"
    );
    assert!(request
        .context()
        .artifact_view()
        .descriptors()
        .iter()
        .any(|descriptor| descriptor.id == hidden_original));
    for artifact in &request.artifacts {
        assert!(original.artifacts.contains(artifact));
    }
    let blocked = program_request(scratch.path(), Arc::clone(&next));
    assert!(
        blocked
            .validate_receipt(
                &import_receipt(scratch.path(), &blocked, "HiddenNative"),
                None,
                &next,
            )
            .is_err(),
        "retained executable availability grants no authored lexical import"
    );
    drop(original);
    drop(context);
    drop(value);
    drop(value_entry);
    drop(initial);
    assert!(
        private_root.is_dir(),
        "projected custody retains original private files"
    );
    drop(interfaces);
    drop(blocked);
    drop(request);
    drop(next);
    assert!(weak.upgrade().is_none());
    assert!(!private_root.exists());
}

#[test]
fn retained_materialization_many_descendants_measure_metadata_and_payload_work() {
    for descendants in [24, 100] {
        let (mut context, producer) = metadata_fixture();
        let scratch = tempfile::tempdir().unwrap();
        let first = context
            .prepare_compilation(&scratch.path().join("base"), &producer)
            .unwrap();
        let mut previous = first.artifacts;
        let mut written_bytes = first
            .materialization
            .as_ref()
            .unwrap()
            .payload_work
            .written_bytes;
        let mut inherited_rows = 0;
        for index in 0..descendants {
            let product = crate::certified_products::fixture_finalized_product(
                support_product(&format!("Growing{index}")),
                context.owner.facts.producer,
            );
            let recovered = crate::certified_products::tests::recovered_witness_fixtures(
                std::slice::from_ref(&product),
            )
            .remove(0)
            .product;
            assert_eq!(recovered.owner(), product.owner());
            assert_eq!(recovered.interface_bytes(), product.interface_bytes());
            assert_eq!(recovered.product_bytes(), product.product_bytes());
            assert_eq!(
                recovered.certification_bytes(),
                product.certification_bytes()
            );
            assert_eq!(
                crate::certified_products::original_available_groups(&recovered)
                    .unwrap()
                    .count(),
                0,
                "empty originals still need an authenticated availability census"
            );
            let product = recovered;
            context = Arc::new(
                context
                    .as_ref()
                    .clone()
                    .extend_checked_original_products(context.owner.facts.producer, &[product])
                    .unwrap(),
            );
            let request = context
                .prepare_compilation(&scratch.path().join(format!("child{index}")), &producer)
                .unwrap();
            for artifact in &previous {
                assert!(request.artifacts.contains(artifact));
            }
            let retained = request.materialization.as_ref().unwrap();
            assert_eq!(
                retained.rows.len(),
                1,
                "a descendant retains only its new row"
            );
            let mut stored_rows = 0;
            retained.visit_owners(&mut BTreeSet::new(), &mut |owner| {
                stored_rows += owner.rows.len();
            });
            assert_eq!(
                stored_rows,
                index + 5,
                "ancestor row storage grows linearly"
            );
            let retained_files = std::fs::read_dir(retained.directory().path().join("artifacts"))
                .unwrap()
                .map(|entry| entry.unwrap().metadata().unwrap().len())
                .sum::<u64>();
            assert_eq!(
                retained.payload_work.written_bytes, retained_files,
                "only this descendant's new private payloads were written"
            );
            assert!(retained.payload_work.written_bytes > 0);
            inherited_rows += previous.len();
            written_bytes += retained.payload_work.written_bytes;
            previous = request.artifacts;
        }
        assert_eq!(
            inherited_rows,
            descendants * 4 + descendants * (descendants - 1) / 2
        );
        let retained = context
            .owner
            .facts
            .compiler_support
            .artifacts()
            .retained_materialization(&context.compiler_metadata_snapshot().unwrap())
            .unwrap()
            .unwrap();
        let mut stored_rows = 0;
        let mut stored_groups = 0;
        let mut stored_graph_paths = 0;
        retained.visit_owners(&mut BTreeSet::new(), &mut |owner| {
            stored_rows += owner.rows.len();
            stored_groups += owner.groups.len();
            stored_graph_paths += owner.graph_paths.len();
        });
        assert_eq!(stored_rows, descendants + 4);
        println!(
            "retained-descendants count={descendants} immutable_written_bytes={written_bytes} transient_inherited_rows={inherited_rows} retained_row_handles={stored_rows} retained_group_handles={stored_groups} retained_graph_handles={stored_graph_paths} final_rows={}",
            previous.len()
        );
    }
}

#[test]
fn metadata_identity_is_path_independent_and_tracks_graph_selection() {
    let (context, _) = metadata_fixture();
    let expected = context.semantic_sha256();
    let before = context
        .owner
        .facts
        .compiler_support
        .artifacts()
        .inventory()
        .metrics();
    assert_eq!(context.semantic_sha256(), expected);
    let after = context
        .owner
        .facts
        .compiler_support
        .artifacts()
        .inventory()
        .metrics();
    // The first identity query already warmed this immutable view projection.
    assert_eq!(after.graph_visits - before.graph_visits, 0);
    assert_eq!(after.view_queries - before.view_queries, 1);
    for _ in 0..2 {
        let directory = tempfile::tempdir().unwrap();
        context.materialize_scratch(&directory).unwrap();
        assert_eq!(context.semantic_sha256(), expected);
    }
    let mut changed = context.as_ref().clone();
    {
        let updated = [9; 32];
        changed = changed.rebuild(|facts| facts.producer = updated);
    }
    assert_ne!(changed.semantic_sha256(), expected);
    let mut changed = context.as_ref().clone();
    {
        let updated = crate::exact_context::ExactLexicalSnapshot::default();
        changed = changed.rebuild(|facts| facts.lexical = updated);
    }
    assert_ne!(changed.semantic_sha256(), expected);
    let mut reordered = context.as_ref().clone();
    edit_lexical(&mut reordered, |nodes| nodes.reverse());
    assert_eq!(reordered.semantic_sha256(), expected);
    let extended = context
        .as_ref()
        .clone()
        .extend_checked_original_products(
            context.owner.facts.producer,
            &[crate::certified_products::fixture_finalized_product(
                support_product("Later"),
                context.owner.facts.producer,
            )],
        )
        .unwrap();
    assert_ne!(extended.semantic_sha256(), expected);
    let owners = context
        .interface_owners()
        .into_iter()
        .map(|interface| interface.owner)
        .collect::<Vec<_>>();
    let interfaces = context
        .compiler_source_support()
        .selected_interfaces()
        .unwrap()
        .select_owners(owners)
        .unwrap();
    assert!(matches!(recover_fixture_compiler_support(
        context.compiler_input_projection().clone(), interfaces, [],
    ), Err(CompileError::ExtractFailed(detail))
        if detail == "exact declaration context: bound recovery namespace differs from recorded compiler roles"));
    let support = context
        .compiler_source_support()
        .selected_interfaces()
        .unwrap()
        .support_for_owners(
            context
                .compiler_input_projection()
                .snapshot_roles()
                .keys()
                .cloned()
                .collect(),
        )
        .unwrap();
    let mut type_only = context.as_ref().clone();
    type_only = type_only.rebuild(|facts| facts.compiler_support = support);
    type_only.normalize().unwrap();
    assert!(type_only
        .compiler_input_roles()
        .iter()
        .all(|role| role.original().is_none()));
    assert_eq!(type_only.interface_owners(), context.interface_owners());
    assert_eq!(type_only.owner.facts.lexical, context.owner.facts.lexical);
    assert!(type_only.recovery_products().is_empty());
    assert!(!context.recovery_products().is_empty());
    assert_ne!(type_only.semantic_sha256(), expected);
}

fn program_request(root: &Path, context: Arc<ExactDeclarationContext>) -> ExactCompilationRequest {
    let manifest = root.join("scope");
    std::fs::write(&manifest, b"scope").unwrap();
    ExactCompilationRequest {
        inputs: ExactRequestInputs::for_context(context),
        manifest,
        request_sha256: sha256(b"scope"),
        semantic_sha256: [1; 32],
        producer_sha256: [2; 32],
        artifacts: vec![],
        groups: Arc::from([]),
        materialization: None,
        program_support: None,
        program_source_lexical: Vec::new(),
        source_selected_support: BTreeSet::new(),
        source_search_include: None,
        checked_value_imports: Default::default(),
        generated_scaffold_imports: Vec::new(),
    }
}

pub(crate) fn generated_source_admission_fixture(
    root: &Path,
    request: &ExactCompilationRequest,
) -> (ExactSourceAdmission, String) {
    std::fs::create_dir_all(root).unwrap();
    let receipt = import_receipt(root, request, "Unadmitted");
    let mut value = read_receipt(&receipt);
    value.as_array_mut().unwrap()[8].as_array_mut().unwrap()[0]
        .as_array_mut()
        .unwrap()[3] = Value::Array(vec![]);
    write_receipt(&receipt, &value);
    let admission = request
        .validate_receipt(&receipt, None, request.context())
        .unwrap();
    let source = std::fs::read_to_string(root.join("Consumer.hs")).unwrap();
    (admission, source)
}

fn import_receipt(root: &Path, request: &ExactCompilationRequest, imported: &str) -> PathBuf {
    import_receipt_owner(root, request, "fixture", imported, "none", false)
}

fn import_receipt_owner(
    root: &Path,
    request: &ExactCompilationRequest,
    unit: &str,
    imported: &str,
    qualifier: &str,
    boot: bool,
) -> PathBuf {
    import_receipt_source_owner(
        root,
        request,
        unit,
        imported,
        qualifier,
        boot,
        "module Consumer where\n",
    )
}

fn import_receipt_source_owner(
    root: &Path,
    request: &ExactCompilationRequest,
    unit: &str,
    imported: &str,
    qualifier: &str,
    boot: bool,
    source: &str,
) -> PathBuf {
    let directory = root.join(format!("receipt-{unit}-{imported}-{qualifier}-{boot}"));
    std::fs::create_dir_all(&directory).unwrap();
    let path = root.join("Consumer.hs");
    std::fs::write(&path, source).unwrap();
    let snapshot = directory.join("source.hs");
    std::fs::write(&snapshot, source).unwrap();
    let evidence = crate::cache::DependencyEvidence {
        version: 4,
        cache_safe: true,
        selection_complete: true,
        sources: vec![crate::cache::SourceEvidence {
            path: path.clone(),
            sha256: sha256(source.as_bytes()),
        }],
        resolutions: vec![],
        packages: vec![],
        modules: vec![crate::cache::ModuleEvidence {
            unit: "fixture".into(),
            module: "Consumer".into(),
            boot: false,
            source: path.clone(),
            imports: vec![],
            product: crate::cache::ProductAvailability::Ready,
        }],
    };
    let value = Value::Array(vec![
        text("TPEXACTCOMPILE"),
        text("3"),
        text(&request.request_sha256),
        text(hex(&request.semantic_sha256)),
        path_value(&path).unwrap(),
        text(sha256(source.as_bytes())),
        path_value(&snapshot).unwrap(),
        text(serde_json::to_string(&evidence).unwrap()),
        Value::Array(vec![Value::Array(vec![
            text("fixture"),
            text("Consumer"),
            Value::Bool(false),
            Value::Array(vec![Value::Array(vec![
                text(qualifier),
                text(imported),
                Value::Bool(boot),
                text(unit),
            ])]),
        ])]),
        Value::Array(vec![Value::Array(vec![]), Value::Null]),
    ]);
    let receipt = directory.join("receipt.cbor");
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(&value, &mut bytes).unwrap();
    std::fs::write(&receipt, bytes).unwrap();
    receipt
}

fn write_receipt(path: &Path, value: &Value) {
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(value, &mut bytes).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn read_receipt(path: &Path) -> Value {
    ciborium::de::from_reader(std::fs::read(path).unwrap().as_slice()).unwrap()
}

#[test]
fn exact_receipt_admits_complete_inventories_above_scope_descriptor_bound() {
    use tidepool_repr::execution_schema::{InventoryDecodeLimits, InventoryOperation};

    let directory = tempfile::tempdir().unwrap();
    let (request, context, receipt) = source_selected_receipt(directory.path(), false, None);
    let mut include = (0..512)
        .map(|index| {
            let mut root = directory.path().join(format!("absent-{index}"));
            for _ in 0..8 {
                root.push("p".repeat(128));
            }
            root
        })
        .collect::<Vec<_>>();
    include.push(directory.path().to_path_buf());
    let request = request.with_source_search_context(&include);
    let candidates = |module: &str| {
        include
            .iter()
            .flat_map(|root| {
                ["hs", "lhs", "hsig", "lhsig"]
                    .map(|extension| root.join(format!("{module}.{extension}")))
            })
            .collect::<Vec<_>>()
    };
    let mut value = read_receipt(&receipt);
    let fields = value.as_array_mut().unwrap();
    let mut fresh: crate::cache::DependencyEvidence =
        serde_json::from_str(fields[7].as_text().unwrap()).unwrap();
    let source = "module Consumer where\nimport A\nimport Data.List\n";
    let source_path = PathBuf::from(fields[4].as_text().unwrap());
    std::fs::write(&source_path, source).unwrap();
    std::fs::write(receipt.parent().unwrap().join("source.hs"), source).unwrap();
    fields[5] = text(sha256(source.as_bytes()));
    fresh.sources[0].sha256 = sha256(source.as_bytes());
    fresh.modules[0]
        .imports
        .push(crate::cache::ModuleImportEvidence {
            qualifier: crate::cache::ImportQualifier::Unqualified,
            module: "Data.List".into(),
            boot: false,
            selected: None,
        });
    fresh.packages.push("Data.List".into());
    fresh.resolutions.push(crate::cache::ResolutionEvidence {
        qualifier: crate::cache::ImportQualifier::Unqualified,
        module: "Data.List".into(),
        boot: false,
        selected: None,
        candidates: candidates("Data/List"),
    });
    fields[7] = text(serde_json::to_string(&fresh).unwrap());
    let selection = fields[9].as_array_mut().unwrap();
    let mut selected: crate::cache::DependencyEvidence =
        serde_json::from_str(selection[1].as_text().unwrap()).unwrap();
    let mut selected_candidates = candidates("A");
    selected_candidates.truncate(selected_candidates.len() - 3);
    selected.resolutions[0].candidates = selected_candidates;
    selection[1] = text(serde_json::to_string(&selected).unwrap());
    write_receipt(&receipt, &value);
    let bytes = std::fs::read(&receipt).unwrap();
    assert!(bytes.len() > EXACT_SCOPE_BYTES_LIMIT);
    assert!(bytes.len() < crate::certified_products::COMPILER_RECEIPT_BYTES_LIMIT);
    let admission = request.validate_receipt(&receipt, None, &context).unwrap();
    assert_eq!(admission.selected_originals.len(), 1);
    assert_eq!(admission.evidence.resolutions[0].candidates.len(), 2052);

    let restricted = InventoryOperation::new(InventoryDecodeLimits {
        max_bytes: bytes.len() - 1,
        ..Default::default()
    });
    assert!(matches!(
        decode_exact_compilation_receipt_with_operation(&receipt, None, &restricted),
        Err(CompileError::CompilerEvidence(error))
            if matches!(error.as_ref(), crate::certified_products::CertificationError::EvidenceRead {
                path,
                failure: crate::certified_products::EvidenceReadFailure::SizeLimit { actual, limit },
            } if path == &receipt && *actual == bytes.len() as u64 && *limit == bytes.len() as u64 - 1)
    ));
    let exhausted = InventoryOperation::new(InventoryDecodeLimits {
        max_work: 128 << 20,
        ..Default::default()
    });
    assert!(matches!(
        decode_exact_compilation_receipt_with_operation(&receipt, None, &exhausted),
        Err(CompileError::CompilerEvidence(error))
            if matches!(error.as_ref(), crate::certified_products::CertificationError::Product(
                tidepool_repr::execution_schema::ParseError::LimitExceeded("work")
            ))
    ));
    let mut wrong_request = request.clone();
    wrong_request.request_sha256 = hex(&[99; 32]);
    assert!(wrong_request
        .validate_receipt(&receipt, None, &context)
        .is_err());
    let mut trailing = bytes.clone();
    trailing.push(0);
    std::fs::write(&receipt, trailing).unwrap();
    assert!(read_exact_compilation_receipt(&receipt).is_err());
    std::fs::write(&receipt, &bytes).unwrap();
    std::fs::write(receipt.parent().unwrap().join("source.hs"), b"changed").unwrap();
    assert!(read_exact_compilation_receipt(&receipt).is_err());
    std::fs::write(receipt.parent().unwrap().join("source.hs"), source).unwrap();
    std::fs::write(directory.path().join("A.hs"), b"changed original").unwrap();
    assert!(request.validate_receipt(&receipt, None, &context).is_err());
}

#[test]
fn exact_receipt_refuses_records_above_compiler_receipt_bound() {
    let directory = tempfile::tempdir().unwrap();
    let receipt = directory.path().join("receipt.cbor");
    let limit = crate::certified_products::COMPILER_RECEIPT_BYTES_LIMIT;
    std::fs::File::create(&receipt)
        .unwrap()
        .set_len(limit as u64 + 1)
        .unwrap();
    assert!(matches!(
        read_exact_compilation_receipt(&receipt),
        Err(CompileError::CompilerEvidence(error))
            if matches!(error.as_ref(), crate::certified_products::CertificationError::EvidenceRead {
                path,
                failure: crate::certified_products::EvidenceReadFailure::SizeLimit { actual, limit: bound },
            } if path == &receipt && *actual == limit as u64 + 1 && *bound == limit as u64)
    ));
}

#[test]
fn completed_receipt_preserves_replay_refusal_and_exact_import_authority() {
    let directory = tempfile::tempdir().unwrap();
    let context = Arc::new(ExactDeclarationContext::new(&[], &[], vec![]).unwrap());
    let request = program_request(directory.path(), context.clone());
    let receipt = import_receipt(directory.path(), &request, "Unadmitted");
    let mut value = read_receipt(&receipt);
    // Start with a complete control that claims no retained import.
    let modules = value.as_array_mut().unwrap()[8].as_array_mut().unwrap();
    modules[0].as_array_mut().unwrap()[3] = Value::Array(vec![]);
    write_receipt(&receipt, &value);
    request.validate_receipt(&receipt, None, &context).unwrap();

    let fields = value.as_array_mut().unwrap();
    let mut evidence: crate::cache::DependencyEvidence =
        serde_json::from_str(fields[7].as_text().unwrap()).unwrap();
    evidence.cache_safe = false;
    evidence.selection_complete = false;
    fields[7] = text(serde_json::to_string(&evidence).unwrap());
    write_receipt(&receipt, &value);
    let admitted = request.validate_receipt(&receipt, None, &context).unwrap();
    assert!(!admitted.evidence.cache_safe && !admitted.evidence.selection_complete);
    assert!(!admitted.evidence.valid("module Consumer where\n"));
    assert!(admitted
        .evidence
        .revalidate("module Consumer where\n")
        .is_ok());

    // Completed source observations do not authorize a retained import.
    let modules = value.as_array_mut().unwrap()[8].as_array_mut().unwrap();
    modules[0].as_array_mut().unwrap()[3] = Value::Array(vec![Value::Array(vec![
        text("none"),
        text("Unadmitted"),
        Value::Bool(false),
        text("fixture"),
    ])]);
    write_receipt(&receipt, &value);
    assert!(matches!(request.validate_receipt(&receipt, None, &context),
        Err(CompileError::ExtractFailed(detail)) if detail.contains("leaves selected lexical graph")));
}

#[test]
fn receipt_observations_reject_changed_snapshots_and_trailing_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let context = Arc::new(ExactDeclarationContext::new(&[], &[], vec![]).unwrap());
    let request = program_request(directory.path(), context);
    let receipt = import_receipt(directory.path(), &request, "Unadmitted");
    let expected = std::fs::read(&receipt).unwrap();
    assert!(read_exact_compilation_receipt(&receipt).is_ok());
    let snapshot = receipt.parent().unwrap().join("source.hs");
    let source = std::fs::read(&snapshot).unwrap();
    std::fs::write(&snapshot, b"module Changed where\n").unwrap();
    assert!(matches!(read_exact_compilation_receipt(&receipt),
        Err(CompileError::ExtractFailed(detail)) if detail.contains("snapshot changed")));
    std::fs::write(&snapshot, source).unwrap();
    let mut trailing = expected.clone();
    trailing.push(0);
    std::fs::write(&receipt, trailing).unwrap();
    assert!(matches!(read_exact_compilation_receipt(&receipt),
    Err(CompileError::CompilerEvidence(error))
        if matches!(error.as_ref(), crate::certified_products::CertificationError::Product(
            tidepool_repr::execution_schema::ParseError::TrailingBytes
        ))));
    std::fs::write(&receipt, expected).unwrap();
    assert!(read_exact_compilation_receipt(&receipt).is_ok());
}

fn source_selected_receipt(
    root: &Path,
    dependency: bool,
    shadow: Option<&Path>,
) -> (
    ExactCompilationRequest,
    Arc<ExactDeclarationContext>,
    PathBuf,
) {
    source_selected_receipt_with_sparse_history(root, dependency, shadow, false, None)
}

fn source_selected_receipt_with_sparse_history(
    root: &Path,
    dependency: bool,
    shadow: Option<&Path>,
    sparse: bool,
    historical_core: Option<&[u8]>,
) -> (
    ExactCompilationRequest,
    Arc<ExactDeclarationContext>,
    PathBuf,
) {
    let inventory = ArtifactInventory::default();
    let products = ["A", "B"].map(|module| {
        let source = format!("module {module} where\nvalue = 1\n");
        std::fs::write(root.join(format!("{module}.hs")), &source).unwrap();
        let imports = if dependency && module == "A" {
            vec![crate::certified_products::CanonicalSourceImport {
                qualifier: crate::cache::ImportQualifier::Unqualified,
                module: "B".into(),
                boot: false,
                home_unit: Some("main".into()),
            }]
        } else {
            vec![]
        };
        crate::certified_products::fixture_source_finalized_product(
            (if sparse {
                crate::certified_products::tests::original_groups_fixture_in_unit_with_sites(
                    "main",
                    module,
                    vec![(7, vec![]), (11, vec![])],
                    91,
                    &BTreeMap::new(),
                    format!("{module} sparse interface").into_bytes(),
                    &BTreeMap::new(),
                    &[],
                )
            } else {
                support_product_in_unit("main", module)
            })
            .with_source_sha256(Sha256::digest(source.as_bytes()).into()),
            [7; 32],
            imports,
        )
    });
    // Current-source replay and native availability are separate proofs.
    // These zero-group originals have genuine production recovery witnesses.
    let products = crate::certified_products::tests::recovered_witness_fixtures(&products)
        .into_iter()
        .map(|recovered| recovered.product)
        .collect::<Vec<_>>();
    let originals = products
        .iter()
        .map(|product| product.owner())
        .collect::<Vec<_>>();
    let entries = products
        .iter()
        .map(|product| Arc::new(ArtifactEntry::original([7; 32], product.clone()).unwrap()))
        .collect::<Vec<_>>();
    let projection = CompilerInputProjection::from_issued_entries(&entries).unwrap();
    let artifacts = if sparse {
        inventory
            .admit_recovery_selection(
                &inventory.empty_view(),
                entries.clone(),
                &entries
                    .iter()
                    .map(|entry| crate::artifact_inventory::NativeGroupKey {
                        artifact: entry.descriptor.id,
                        original_ordinal: 7,
                    })
                    .collect(),
            )
            .unwrap()
    } else {
        inventory
            .admit_shared(&inventory.empty_view(), entries)
            .unwrap()
    };
    let support = if sparse {
        issue_fixture_compiler_support(projection, artifacts).unwrap()
    } else {
        recover_fixture_compiler_support(projection, artifacts, []).unwrap()
    };
    let mut context = ExactDeclarationContext::from_facts(DeclarationContextFacts {
        compiler_support: support,
        producer: [7; 32],
        original_instance_environment: OriginalInstanceEnvironment::Unknown,
        template_imports: None,
        lexical: crate::exact_context::ExactLexicalSnapshot::default(),
    });
    if let Some(core) = historical_core {
        let current = products
            .iter()
            .find(|product| product.owner().module == "B")
            .unwrap();
        let historical = Arc::new(ArtifactEntry::canonical(
            crate::certified_products::fixture_module_core(
                current.module_interface().unwrap(),
                core.to_vec(),
            ),
        ));
        assert_ne!(
            historical.descriptor.id,
            ArtifactEntry::canonical(current.module_interface().unwrap().clone())
                .descriptor
                .id
        );
        let history_inventory = ArtifactInventory::default();
        let history = history_inventory
            .admit_shared(&history_inventory.empty_view(), vec![historical])
            .unwrap();
        let custody = context.artifact_view().retain_custody(&history).unwrap();
        let custody =
            recover_fixture_compiler_support(CompilerInputProjection::default(), custody, [])
                .unwrap();
        context = context.extend_issued_compiler_support(&custody).unwrap();
    }
    let context = Arc::new(context);
    let selected_interfaces = context.compiler_metadata_snapshot().unwrap().entries;
    let include = shadow
        .into_iter()
        .map(Path::to_path_buf)
        .chain(std::iter::once(root.to_path_buf()))
        .collect::<Vec<_>>();
    let mut request =
        program_request(root, Arc::clone(&context)).with_source_search_context(&include);
    request.producer_sha256 = [7; 32];
    let receipt = import_receipt_source_owner(
        root,
        &request,
        "main",
        "A",
        "none",
        false,
        "module Consumer where\nimport A\n",
    );
    let mut value = read_receipt(&receipt);
    let fields = value.as_array_mut().unwrap();
    let mut fresh: crate::cache::DependencyEvidence =
        serde_json::from_str(fields[7].as_text().unwrap()).unwrap();
    fresh.modules[0].unit = "main".into();
    fields[7] = text(serde_json::to_string(&fresh).unwrap());
    fields[8].as_array_mut().unwrap()[0].as_array_mut().unwrap()[0] = text("main");
    let count = if dependency { 2 } else { 1 };
    let mut sources = Vec::new();
    let mut modules = Vec::new();
    let mut claims = Vec::new();
    for owner in &originals[..count] {
        let source = root.join(format!("{}.hs", owner.module));
        sources.push(crate::cache::SourceEvidence {
            path: source.clone(),
            sha256: sha256(&std::fs::read(&source).unwrap()),
        });
        modules.push(crate::cache::ModuleEvidence {
            unit: owner.unit.clone(),
            module: owner.module.clone(),
            boot: false,
            source,
            product: crate::cache::ProductAvailability::InterfaceOnly,
            imports: if dependency && owner.module == "A" {
                vec![crate::cache::ModuleImportEvidence {
                    qualifier: crate::cache::ImportQualifier::Unqualified,
                    module: "B".into(),
                    boot: false,
                    selected: Some(root.join("B.hs")),
                }]
            } else {
                vec![]
            },
        });
        let interface =
            canonical_source_interface(&selected_interfaces[&identity(&owner.unit, &owner.module)])
                .unwrap();
        claims.push(Value::Array(vec![
            text(&owner.unit),
            text(&owner.module),
            text(sha256(interface.certificate_bytes())),
            text(hex(&interface.interface_sha256())),
            text(hex(&interface.source_sha256())),
        ]));
    }
    let mut a_candidates = if let Some(shadow) = shadow {
        vec![
            shadow.join("A.hs"),
            shadow.join("A.lhs"),
            shadow.join("A.hsig"),
            shadow.join("A.lhsig"),
        ]
    } else {
        vec![]
    };
    a_candidates.push(root.join("A.hs"));
    let mut resolutions = vec![crate::cache::ResolutionEvidence {
        qualifier: crate::cache::ImportQualifier::Unqualified,
        module: "A".into(),
        boot: false,
        selected: Some(root.join("A.hs")),
        candidates: a_candidates,
    }];
    if dependency {
        resolutions.push(crate::cache::ResolutionEvidence {
            qualifier: crate::cache::ImportQualifier::Unqualified,
            module: "B".into(),
            boot: false,
            selected: Some(root.join("B.hs")),
            candidates: vec![root.join("B.hs")],
        });
    }
    let evidence = crate::cache::DependencyEvidence {
        version: 4,
        cache_safe: true,
        selection_complete: true,
        sources,
        modules,
        resolutions,
        packages: vec![],
    };
    fields[9] = Value::Array(vec![
        Value::Array(claims),
        text(serde_json::to_string(&evidence).unwrap()),
    ]);
    write_receipt(&receipt, &value);
    (request, context, receipt)
}

#[test]
fn sparse_partial_program_source_proof_keeps_selected_maps_and_unissued_body_private() {
    for historical_core in [
        None,
        Some(b"old Core one".as_slice()),
        Some(b"old Core two".as_slice()),
        Some(b"old Core three".as_slice()),
        Some(b"old Core four".as_slice()),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let (request, context, receipt) = source_selected_receipt_with_sparse_history(
            directory.path(),
            true,
            None,
            true,
            historical_core,
        );
        let roles = context.compiler_input_projection().clone();
        let before_graph = context.artifact_view().capture_graph_selection();
        let selected = context.compiler_metadata_snapshot().unwrap().entries;
        let b = identity("main", "B");
        let expected = canonical_source_interface(&selected[&b]).unwrap().clone();
        assert_eq!(
            context
                .artifact_view()
                .descriptors()
                .iter()
                .filter(|entry| entry.owner == b
                    && entry.kind
                        == crate::artifact_inventory::ArtifactKind::CanonicalModuleInterface)
                .count(),
            if historical_core.is_some() { 2 } else { 1 }
        );
        for role in roles
            .roles()
            .iter()
            .filter(|role| role.original().is_some())
        {
            let original = role.original().unwrap();
            let offers = request
                .compiler_inputs()
                .native_transport
                .availability
                .get(&original)
                .unwrap();
            assert!(
                matches!(&offers.mode, NativeTransportMode::IssuedGroups)
                    && offers.issued_ordinals() == BTreeSet::from([7])
            );
            assert!(offers.permits(7));
            assert!(!offers.permits(11));
            assert!(before_graph
                .native_groups
                .iter()
                .filter(|group| group.binding.artifact() == original)
                .all(|group| group.original_ordinal == 7));
        }
        let admitted = request.validate_receipt(&receipt, None, &context).expect(
            "current source claims use current selected interfaces despite historical custody",
        );
        assert_eq!(admitted.selected_originals[&b].interface(), &expected);
        let mut local = request.clone();
        local.program_support = Some(
            ProgramSourceSupport::extend(
                None,
                context.compiler_source_support().clone(),
                [
                    (identity("main", "A"), vec![b.clone()]),
                    (b.clone(), vec![]),
                ],
            )
            .unwrap(),
        );
        let empty = ExactDeclarationContext::new(&[], &[], vec![]).unwrap();
        let program_admitted = local
            .validate_receipt(&receipt, None, &empty)
            .expect("same-offer program support carries the exact selected source proof");
        assert_eq!(
            program_admitted.selected_originals[&b].interface(),
            &expected
        );
        assert_eq!(
            program_admitted.home_imports().unwrap(),
            admitted.home_imports().unwrap()
        );
        assert_eq!(local.compiler_inputs().projection(), &roles);
        assert_eq!(
            local
                .compiler_inputs()
                .artifacts()
                .capture_graph_selection(),
            before_graph
        );
        for availability in local
            .compiler_inputs()
            .native_transport
            .availability
            .values()
        {
            assert!(
                matches!(&availability.mode, NativeTransportMode::IssuedGroups)
                    && availability.issued_ordinals() == BTreeSet::from([7])
            );
            assert!(!availability.permits(11));
        }
        assert!(empty.lexical_graph().is_empty());
        assert!(context.lexical_graph().is_empty());
        assert!(request.context().lexical_graph().is_empty());
        assert_eq!(
            context.artifact_view().capture_graph_selection(),
            before_graph
        );
        eprintln!(
            "sparse source-proof history: historical={} current+program consumers completed",
            historical_core.is_some()
        );
    }
}

#[test]
fn selected_original_instance_graph_keeps_mounted_value_without_public_lexical_edge() {
    use crate::cache::{
        DependencyEvidence, ImportQualifier, ModuleEvidence, ModuleImportEvidence,
        ProductAvailability, ResolutionEvidence, SourceEvidence,
    };
    let directory = tempfile::tempdir().unwrap();
    let (old_request, old_context, receipt) =
        source_selected_receipt(directory.path(), false, None);
    let admission = old_request
        .validate_receipt(&receipt, None, &old_context)
        .unwrap();
    let historical = admission.selected_originals.clone();
    let value_owner = tidepool_repr::SessionModule::val(tidepool_repr::Generation(1));
    let mounted = identity("main", &value_owner.module_name());
    let source_path = directory.path().join("A.hs");
    let source = std::fs::read(&source_path).unwrap();
    let product = crate::certified_products::fixture_source_finalized_product(
        support_product_in_unit("main", "A").with_source_sha256(Sha256::digest(&source).into()),
        [7; 32],
        vec![crate::certified_products::CanonicalSourceImport {
            qualifier: ImportQualifier::Unqualified,
            module: mounted.module.clone(),
            boot: false,
            home_unit: Some("main".into()),
        }],
    );
    let product = crate::certified_products::tests::recovered_witness_fixtures(&[product])
        .pop()
        .unwrap()
        .product;
    let canonical = product.module_interface().unwrap().clone();
    let value_evidence = support_product_in_unit("main", &mounted.module);
    let value = Arc::new(
        CertifiedValueInterface::from_checked_compilation(
            [7; 32],
            mounted.clone(),
            value_evidence.interface_bytes().to_vec(),
            value_evidence.package_imports_bytes().to_vec(),
            vec![],
        )
        .unwrap(),
    );
    let shared = vec![ExactLexicalNode {
        owner: identity("main", "A"),
        imports: vec![],
    }];
    let context = Arc::new(
        ExactDeclarationContext::new(&[], &[], vec![])
            .unwrap()
            .extend_checked_original_products([7; 32], &[product])
            .unwrap()
            .extend_with_value_interfaces(std::slice::from_ref(&value), shared.clone())
            .unwrap(),
    );
    let inputs = crate::checked_cell::CheckedValueInputs::capture_raw(vec![(
        value_owner,
        Arc::from(value.interface().interface_bytes()),
    )])
    .unwrap();
    let mut request = program_request(directory.path(), context.clone())
        .with_checked_value_imports(inputs.import_authority())
        .with_source_search_context(&[directory.path().to_path_buf()]);
    request.producer_sha256 = [7; 32];
    let evidence = DependencyEvidence {
        version: 4,
        cache_safe: true,
        selection_complete: true,
        sources: vec![SourceEvidence {
            path: source_path.clone(),
            sha256: sha256(&source),
        }],
        modules: vec![ModuleEvidence {
            unit: "main".into(),
            module: "A".into(),
            boot: false,
            source: source_path.clone(),
            product: ProductAvailability::InterfaceOnly,
            imports: vec![ModuleImportEvidence {
                qualifier: ImportQualifier::Unqualified,
                module: mounted.module.clone(),
                boot: false,
                selected: None,
            }],
        }],
        resolutions: vec![ResolutionEvidence {
            qualifier: ImportQualifier::Unqualified,
            module: "A".into(),
            boot: false,
            selected: Some(source_path.clone()),
            candidates: vec![source_path],
        }],
        packages: vec![],
    };
    // Change the raw producer receipt before admission. The current
    // request's real validator owns both the selected-source proof and
    // its exact import graph; no issued source proof is amended.
    let mut receipt_value = read_receipt(&receipt);
    let fields = receipt_value.as_array_mut().unwrap();
    fields[2] = text(&request.request_sha256);
    fields[3] = text(hex(&request.semantic_sha256));
    fields[8].as_array_mut().unwrap()[0].as_array_mut().unwrap()[3]
        .as_array_mut()
        .unwrap()
        .push(Value::Array(vec![
            text("none"),
            text(&mounted.module),
            Value::Bool(false),
            text(&mounted.unit),
        ]));
    fields[9] = Value::Array(vec![
        Value::Array(vec![Value::Array(vec![
            text("main"),
            text("A"),
            text(sha256(canonical.certificate_bytes())),
            text(hex(&canonical.interface_sha256())),
            text(hex(&canonical.source_sha256())),
        ])]),
        text(serde_json::to_string(&evidence).unwrap()),
    ]);
    write_receipt(&receipt, &receipt_value);
    let mut admission = request.validate_receipt(&receipt, None, &context).unwrap();
    let artifacts = context
        .artifact_view()
        .merge_fixture_selections(&support_view(&[scaffold_native_fixture(
            [7; 32], "main", "Consumer", 1,
        )]))
        .unwrap();
    let original = ExactProductAdmission {
        request: &request,
        source: &admission,
    }
    .original_execution_fixture(&artifacts)
    .unwrap();
    assert_eq!(context.lexical_graph(), shared);
    assert_eq!(
        original
            .lexical_graph()
            .iter()
            .find(|node| node.owner == identity("main", "A"))
            .unwrap()
            .imports,
        vec![mounted]
    );
    assert!(matches!(
        original.original_instance_environment(),
        OriginalInstanceEnvironment::Complete { .. }
    ));
    let captured_request = request
        .in_program_context(&directory.path().join("captured-inputs"), original.clone())
        .unwrap();
    assert!(
        ExactProductAdmission {
            request: &captured_request,
            source: &admission
        }
        .original_execution_fixture(&artifacts)
        .is_ok(),
        "complete inherited instance graphs must project shared rows before comparison"
    );
    // Owner-local negative corruption: a historical selection is not
    // an admission and must not satisfy the selected-owner consumer.
    admission.selected_originals = historical;
    assert!(
        ExactProductAdmission {
            request: &request,
            source: &admission
        }
        .original_execution_fixture(&artifacts)
        .is_err(),
        "historical certificate cannot replace the selected owner"
    );
    assert_eq!(context.lexical_graph(), shared);
}

#[test]
fn source_selected_original_receipt_preserves_native_custody_and_requires_current_reproof() {
    let directory = tempfile::tempdir().unwrap();
    let (mut request, context, receipt) = source_selected_receipt(directory.path(), true, None);
    let admission = request.validate_receipt(&receipt, None, &context).unwrap();
    assert_eq!(admission.selected_originals.len(), 2);
    assert_eq!(
        admission.home_imports().unwrap()[&identity("main", "Consumer")],
        vec![identity("main", "A")]
    );
    assert_eq!(
        admission.home_imports().unwrap()[&identity("main", "A")],
        vec![identity("main", "B")]
    );
    let originals = context
        .artifact_view()
        .entries_for_owners([identity("main", "A"), identity("main", "B")].into_iter())
        .unwrap();
    let products = ["A", "B"].map(
        |module| match &originals[&identity("main", module)].payload {
            ArtifactPayload::Original(product) => product.clone(),
            _ => panic!("original fixture"),
        },
    );
    let effective = request
        .admit_fixture_support(
            Arc::clone(&context),
            &support_offer(&products),
            &[admission],
            None,
        )
        .unwrap();
    let after = effective
        .artifact_view()
        .entries_for_owners([identity("main", "A"), identity("main", "B")].into_iter())
        .unwrap();
    for module in ["A", "B"] {
        assert!(Arc::ptr_eq(
            &originals[&identity("main", module)],
            &after[&identity("main", module)]
        ));
    }
    assert!(effective.lexical_graph().is_empty());
    assert!(context.lexical_graph().is_empty());
    assert_eq!(
        request.program_source_lexical(),
        &[
            ExactLexicalNode {
                owner: identity("main", "A"),
                imports: vec![identity("main", "B")],
            },
            ExactLexicalNode {
                owner: identity("main", "B"),
                imports: vec![],
            },
        ]
    );
    let RetainedValueSourceSurface { lexical, .. } = effective
        .retain_value_source_surface(
            &request
                .program_support
                .as_ref()
                .unwrap()
                .compiler_support
                .artifacts(),
            request.program_source_lexical(),
        )
        .unwrap();
    assert_eq!(lexical, request.program_source_lexical());
    assert_eq!(
        request
            .program_support
            .as_ref()
            .unwrap()
            .compiler_support
            .artifacts()
            .root_entries()
            .iter()
            .map(|entry| entry.descriptor.owner.clone())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([identity("main", "A"), identity("main", "B")])
    );
    request
        .validate_receipt(&receipt, None, &effective)
        .unwrap();
    let mut without_proof = read_receipt(&receipt);
    without_proof.as_array_mut().unwrap()[9] =
        Value::Array(vec![Value::Array(vec![]), Value::Null]);
    write_receipt(&receipt, &without_proof);
    assert!(
        request
            .validate_receipt(&receipt, None, &effective)
            .is_err(),
        "prior source-selected support must not become blanket import authority"
    );
    let (_, _, receipt) = source_selected_receipt(directory.path(), false, None);
    std::fs::write(directory.path().join("A.hs"), "module A where\nvalue = 2\n").unwrap();
    assert!(
        request
            .validate_receipt(&receipt, None, &effective)
            .is_err(),
        "a later stage must recheck current source bytes"
    );
}

#[test]
fn source_selected_original_receipt_refuses_canonical_identity_version_and_unreachable_claims() {
    let directory = tempfile::tempdir().unwrap();
    let (request, context, receipt) = source_selected_receipt(directory.path(), false, None);
    let valid = read_receipt(&receipt);
    for index in 2..5 {
        let mut forged = valid.clone();
        forged.as_array_mut().unwrap()[9].as_array_mut().unwrap()[0]
            .as_array_mut()
            .unwrap()[0]
            .as_array_mut()
            .unwrap()[index] = text(hex(&[99; 32]));
        write_receipt(&receipt, &forged);
        assert!(
            request.validate_receipt(&receipt, None, &context).is_err(),
            "forged original digest {index}"
        );
    }
    for version in ["1", "2"] {
        let mut forged = valid.clone();
        forged.as_array_mut().unwrap()[1] = text(version);
        write_receipt(&receipt, &forged);
        assert!(request.validate_receipt(&receipt, None, &context).is_err());
    }
    let mut duplicate = valid.clone();
    let claims = duplicate.as_array_mut().unwrap()[9].as_array_mut().unwrap()[0]
        .as_array_mut()
        .unwrap();
    claims.push(claims[0].clone());
    write_receipt(&receipt, &duplicate);
    assert!(request.validate_receipt(&receipt, None, &context).is_err());
    write_receipt(&receipt, &valid);
    let mut unbound = request.clone();
    unbound.source_search_include = None;
    assert!(unbound.validate_receipt(&receipt, None, &context).is_err());
    let mut wrong_producer = request.clone();
    wrong_producer.producer_sha256 = [99; 32];
    assert!(wrong_producer
        .validate_receipt(&receipt, None, &context)
        .is_err());
    let mut unused = valid.clone();
    unused.as_array_mut().unwrap()[8].as_array_mut().unwrap()[0]
        .as_array_mut()
        .unwrap()[3] = Value::Array(vec![]);
    write_receipt(&receipt, &unused);
    assert!(
        request.validate_receipt(&receipt, None, &context).is_err(),
        "retained custody alone cannot select an unused source"
    );
}

#[test]
fn source_selected_original_receipt_refuses_shadow_shortened_search_and_alternate_path() {
    let directory = tempfile::tempdir().unwrap();
    let shadow = directory.path().join("earlier");
    std::fs::create_dir(&shadow).unwrap();
    let (request, context, receipt) =
        source_selected_receipt(directory.path(), false, Some(&shadow));
    request.validate_receipt(&receipt, None, &context).unwrap();
    let valid = read_receipt(&receipt);
    for reordered in [false, true] {
        let mut forged = valid.clone();
        let fields = forged.as_array_mut().unwrap();
        let mut evidence: crate::cache::DependencyEvidence =
            serde_json::from_str(fields[9].as_array().unwrap()[1].as_text().unwrap()).unwrap();
        if reordered {
            evidence.resolutions[0].candidates.swap(0, 1);
        } else {
            evidence.resolutions[0].candidates.remove(0);
        }
        fields[9].as_array_mut().unwrap()[1] = text(serde_json::to_string(&evidence).unwrap());
        write_receipt(&receipt, &forged);
        assert!(request.validate_receipt(&receipt, None, &context).is_err());
    }
    write_receipt(&receipt, &valid);
    std::fs::write(shadow.join("A.hsig"), "signature A where\n").unwrap();
    assert!(
        request.validate_receipt(&receipt, None, &context).is_err(),
        "an earlier signature candidate also shadows the original source"
    );
    std::fs::remove_file(shadow.join("A.hsig")).unwrap();
    let mut alternate = valid;
    let fields = alternate.as_array_mut().unwrap();
    let mut evidence: crate::cache::DependencyEvidence =
        serde_json::from_str(fields[9].as_array().unwrap()[1].as_text().unwrap()).unwrap();
    let alias = directory.path().join("SameBytes.hs");
    std::fs::copy(directory.path().join("A.hs"), &alias).unwrap();
    evidence.sources[0].path = alias.clone();
    evidence.modules[0].source = alias;
    fields[9].as_array_mut().unwrap()[1] = text(serde_json::to_string(&evidence).unwrap());
    write_receipt(&receipt, &alternate);
    assert!(
        request.validate_receipt(&receipt, None, &context).is_err(),
        "equal bytes at another source path do not replace the original"
    );
}

#[test]
fn source_selected_original_receipt_requires_complete_current_source_import_closure() {
    let directory = tempfile::tempdir().unwrap();
    let (request, context, receipt) = source_selected_receipt(directory.path(), true, None);
    let admission = request.validate_receipt(&receipt, None, &context).unwrap();
    assert_eq!(
        admission.home_imports().unwrap()[&identity("main", "A")],
        vec![identity("main", "B")]
    );
    assert_eq!(admission.selected_originals.len(), 2);
    let valid = read_receipt(&receipt);
    let mut missing = valid.clone();
    let fields = missing.as_array_mut().unwrap();
    let selection = fields[9].as_array_mut().unwrap();
    selection[0].as_array_mut().unwrap().pop();
    let mut evidence: crate::cache::DependencyEvidence =
        serde_json::from_str(selection[1].as_text().unwrap()).unwrap();
    evidence.sources.pop();
    evidence.modules.pop();
    selection[1] = text(serde_json::to_string(&evidence).unwrap());
    write_receipt(&receipt, &missing);
    assert!(request.validate_receipt(&receipt, None, &context).is_err());
    let mut changed = valid;
    let selection = changed.as_array_mut().unwrap()[9].as_array_mut().unwrap();
    let mut evidence: crate::cache::DependencyEvidence =
        serde_json::from_str(selection[1].as_text().unwrap()).unwrap();
    evidence.modules[0].imports.clear();
    selection[1] = text(serde_json::to_string(&evidence).unwrap());
    write_receipt(&receipt, &changed);
    assert!(
        request.validate_receipt(&receipt, None, &context).is_err(),
        "a receipt cannot omit an original source import"
    );
}

#[test]
fn source_selected_canonical_receipt_admits_support_without_native_products() {
    let directory = tempfile::tempdir().unwrap();
    let (mut request, native, receipt) = source_selected_receipt(directory.path(), true, None);
    let originals = native
        .artifact_view()
        .entries_for_owners([identity("main", "A"), identity("main", "B")].into_iter())
        .unwrap();
    let inventory = ArtifactInventory::default();
    let entries = originals
        .values()
        .map(|entry| {
            Arc::new(ArtifactEntry::canonical(
                canonical_source_interface(entry).unwrap().clone(),
            ))
        })
        .collect::<Vec<_>>();
    let compiler_projection = CompilerInputProjection::from_issued_entries(&entries).unwrap();
    let context = Arc::new(ExactDeclarationContext::from_facts(
        DeclarationContextFacts {
            compiler_support: recover_fixture_compiler_support(
                compiler_projection,
                inventory
                    .admit_shared(&inventory.empty_view(), entries)
                    .unwrap(),
                [],
            )
            .unwrap(),

            producer: [7; 32],
            original_instance_environment: OriginalInstanceEnvironment::Unknown,
            template_imports: None,

            lexical: crate::exact_context::ExactLexicalSnapshot::default(),
        },
    ));
    request.inputs = ExactRequestInputs::for_context(Arc::clone(&context));
    let admission = request.validate_receipt(&receipt, None, &context).unwrap();
    let retained = request
        .admit_fixture_support(
            Arc::clone(&context),
            &support_offer(&[]),
            &[admission],
            None,
        )
        .unwrap();
    assert!(original_products(&retained.artifact_view().entries()).is_empty());
    assert_eq!(request.program_source_lexical().len(), 2);
    assert!(context.lexical_graph().is_empty());
    request.validate_receipt(&receipt, None, &retained).unwrap();
}

#[test]
fn authored_native_root_rejects_source_original_with_authored_module_spelling() {
    let context = ExactDeclarationContext::new(&[], &[], vec![])
        .unwrap()
        .extend_checked_original_products(
            [2; 32],
            &[support_product_in_unit("main", "Tidepool.Session.Lib.G1")],
        )
        .unwrap();
    for generation in [0, 1] {
        assert!(matches!(
            context.authored_native_root(generation),
            Err(CompileError::ArtifactInventory(error))
                if matches!(error.failure,
                    ArtifactInventoryFailure::AuthoredNativeRoot {
                        generation: actual, found: 0
                    } if actual == generation)
        ));
    }
}

#[test]
fn checked_value_import_authority_keeps_hidden_and_future_owners_unselected() {
    let root = tempfile::tempdir().unwrap();
    let context = Arc::new(
        ExactDeclarationContext::new(&[], &[], vec![])
            .unwrap()
            .extend_checked_original_products([2; 32], &[support_product("Hidden")])
            .unwrap(),
    );
    let owner = tidepool_repr::SessionModule::val(tidepool_repr::Generation(7));
    let inputs = crate::checked_cell::CheckedValueInputs::capture_raw(vec![(
        owner,
        Arc::from(b"original thin interface".as_slice()),
    )])
    .unwrap();
    let request = program_request(root.path(), context.clone())
        .with_checked_value_imports(inputs.import_authority());
    request.checked_value_imports.validate().unwrap();
    let receipt = import_receipt_owner(
        root.path(),
        &request,
        "main",
        &owner.module_name(),
        "none",
        false,
    );
    assert!(request.validate_receipt(&receipt, None, &context).is_ok());
    assert!(context.lexical_graph().is_empty());
    let mut value: Value =
        ciborium::de::from_reader(std::fs::read(&receipt).unwrap().as_slice()).unwrap();
    let fields = value.as_array_mut().unwrap();
    let snapshot = PathBuf::from(fields[6].as_text().unwrap());
    let replacement = "module Tidepool.Session.Val.G7 where\n";
    std::fs::write(&snapshot, replacement).unwrap();
    fields[5] = text(sha256(replacement.as_bytes()));
    let mut evidence: crate::cache::DependencyEvidence =
        serde_json::from_str(fields[7].as_text().unwrap()).unwrap();
    evidence.sources[0].sha256 = sha256(replacement.as_bytes());
    evidence.modules[0].unit = "main".into();
    evidence.modules[0].module = owner.module_name();
    fields[7] = text(serde_json::to_string(&evidence).unwrap());
    let module = fields[8].as_array_mut().unwrap()[0].as_array_mut().unwrap();
    module[0] = text("main");
    module[1] = text(owner.module_name());
    module[3] = Value::Array(vec![]);
    let mut collision = Vec::new();
    ciborium::ser::into_writer(&value, &mut collision).unwrap();
    std::fs::write(&receipt, collision).unwrap();
    assert!(request
        .validate_receipt(&receipt, None, &context)
        .err()
        .expect("source owner collision must fail")
        .to_string()
        .contains("fresh module replaced an admitted exact owner"));
    for (unit, module, qualifier, boot) in [
        ("fixture", "Hidden", "none", false),
        ("main", "Tidepool.Session.Val.G8", "none", false),
        ("foreign", "Tidepool.Session.Val.G7", "none", false),
        ("main", "Tidepool.Session.Val.G7", "other:main", false),
        ("main", "Tidepool.Session.Val.G7", "none", true),
    ] {
        let receipt = import_receipt_owner(root.path(), &request, unit, module, qualifier, boot);
        assert!(
            request.validate_receipt(&receipt, None, &context).is_err(),
            "{unit}:{module} {qualifier} boot={boot}"
        );
    }
    std::fs::write(
        inputs.root().join(owner.relative_hi_path()),
        b"changed thin interface",
    )
    .unwrap();
    assert!(request.checked_value_imports.validate().is_err());
}

const RECEIVER_INTERFACE_SOURCE: &str = "module Tidepool.Agent.Ref where\n";

fn interface_only_agent_ref_admission(root: &Path, unit: &str) -> ExactSourceAdmission {
    let source = RECEIVER_INTERFACE_SOURCE;
    let path = root.join("Ref.hs");
    std::fs::write(&path, source).unwrap();
    let context = Arc::new(ExactDeclarationContext::new(&[], &[], vec![]).unwrap());
    let request = program_request(root, context);
    let receipt = support_receipt(root, &request);
    let mut value = read_receipt(&receipt);
    let fields = value.as_array_mut().unwrap();
    let mut evidence: crate::cache::DependencyEvidence =
        serde_json::from_str(fields[7].as_text().unwrap()).unwrap();
    evidence
        .sources
        .retain(|source| source.path == root.join("Target.hs"));
    evidence.sources.push(crate::cache::SourceEvidence {
        path: path.clone(),
        sha256: sha256(source.as_bytes()),
    });
    evidence.resolutions.clear();
    evidence.modules = vec![crate::cache::ModuleEvidence {
        unit: unit.into(),
        module: "Tidepool.Agent.Ref".into(),
        boot: false,
        source: path,
        imports: vec![],
        product: crate::cache::ProductAvailability::InterfaceOnly,
    }];
    fields[7] = text(serde_json::to_string(&evidence).unwrap());
    fields[8] = Value::Array(vec![Value::Array(vec![
        text(unit),
        text("Tidepool.Agent.Ref"),
        Value::Bool(false),
        Value::Array(vec![]),
    ])]);
    write_receipt(&receipt, &value);
    request
        .validate_receipt(&receipt, None, request.context())
        .unwrap()
}

fn interface_only_agent_ref_offer(producer: [u8; 32], unit: &str) -> CompilerSupportFixture {
    let interface = crate::certified_products::fixture_source_module_interface(
        producer,
        unit,
        "Tidepool.Agent.Ref",
        Sha256::digest(RECEIVER_INTERFACE_SOURCE.as_bytes()).into(),
        BTreeMap::new(),
        None,
    );
    let projection = CompilerInputProjection::from_issued_entries(&[Arc::new(
        ArtifactEntry::canonical(interface.clone()),
    )])
    .unwrap();
    CompilerSupportFixture {
        artifacts: certified_product_artifact_view(producer, &[], &[interface], None).unwrap(),
        projection,
    }
}

fn receiver_value_interface() -> Arc<CertifiedValueInterface> {
    let evidence = support_product_in_unit("main", "Tidepool.Session.Val.G1");
    Arc::new(
        CertifiedValueInterface::from_checked_compilation(
            [2; 32],
            identity("main", "Tidepool.Session.Val.G1"),
            evidence.interface_bytes().to_vec(),
            evidence.package_imports_bytes().to_vec(),
            vec![identity("main", "Tidepool.Agent.Ref")],
        )
        .unwrap(),
    )
}

#[test]
fn program_support_preserves_interface_only_receiver_dependency_without_native_authority() {
    let directory = tempfile::tempdir().unwrap();
    let baseline = Arc::new(ExactDeclarationContext::new(&[], &[], vec![]).unwrap());
    let mut request = program_request(directory.path(), baseline.clone());
    let support = interface_only_agent_ref_offer([2; 32], "main");
    let context = request
        .admit_fixture_support(
            baseline,
            &support,
            &[interface_only_agent_ref_admission(directory.path(), "main")],
            None,
        )
        .unwrap();
    let owner = identity("main", "Tidepool.Agent.Ref");
    assert!(context.recovery_products().is_empty());
    assert!(context.lexical_graph().is_empty());
    assert!(!context
        .artifact_view()
        .source_implementation_roles()
        .contains_key(&owner));
    let entry = context
        .artifact_view()
        .entries_for_owners(std::iter::once(owner.clone()))
        .unwrap();
    assert_eq!(
        entry[&owner].descriptor.kind,
        crate::artifact_inventory::ArtifactKind::CanonicalModuleInterface
    );
    assert_eq!(
        request
            .program_support
            .as_ref()
            .unwrap()
            .compiler_support
            .artifacts()
            .artifact_ids(),
        support.artifacts.artifact_ids()
    );
    // These are the two value-interface consumers after checked/program support.
    for context in [
        (*context)
            .clone()
            .extend_with_value_interfaces(&[receiver_value_interface()], vec![])
            .unwrap(),
        (*context)
            .clone()
            .extend_program_value_interface(receiver_value_interface())
            .unwrap(),
    ] {
        assert!(context.recovery_products().is_empty());
        assert!(!context
            .lexical_graph()
            .iter()
            .any(|node| node.owner == owner));
        assert!(context.artifact_view().interface_dependencies().iter().any(
            |(_, required, dependency)| *required == entry[&owner].descriptor.id
                && matches!(
                    dependency,
                    crate::artifact_inventory::ArtifactDependency::Interface
                )
        ));
    }
}

#[test]
fn program_support_refuses_fresh_value_type_without_reserved_output_authority() {
    let directory = tempfile::tempdir().unwrap();
    let baseline = Arc::new(ExactDeclarationContext::new(&[], &[], vec![]).unwrap());
    let mut request = program_request(directory.path(), baseline.clone());
    let context = request
        .admit_fixture_support(
            baseline,
            &interface_only_agent_ref_offer([2; 32], "main"),
            &[interface_only_agent_ref_admission(directory.path(), "main")],
            None,
        )
        .unwrap();
    let value = receiver_value_interface();
    let candidate = (*context)
        .clone()
        .extend_program_value_interface(value.clone())
        .unwrap();
    let support = CompilerSupportFixture {
        projection: CompilerInputProjection::from_issued_entries(&[
            Arc::new(ArtifactEntry::interface(
                value.interface().clone(),
                JoinedInterfaceRole::ValueInterface,
                value.requirements().to_vec(),
            )),
            Arc::new(ArtifactEntry::canonical(
                crate::certified_products::fixture_source_module_interface(
                    [2; 32],
                    "main",
                    "Tidepool.Agent.Ref",
                    Sha256::digest(RECEIVER_INTERFACE_SOURCE.as_bytes()).into(),
                    BTreeMap::new(),
                    None,
                ),
            )),
        ])
        .unwrap(),
        artifacts: candidate
            .artifact_view()
            .select_roots(vec![value.artifact_id()])
            .unwrap(),
    };
    let previous = request
        .program_support
        .as_ref()
        .unwrap()
        .compiler_support
        .artifacts()
        .artifact_ids();
    let refusal = request
        .admit_fixture_support(context.clone(), &support, &[], None)
        .expect_err("a type certificate alone cannot supply fresh source or reserved outputs");
    assert!(matches!(refusal, CompileError::ExtractFailed(_)));
    assert_eq!(
        request
            .program_support
            .as_ref()
            .unwrap()
            .compiler_support
            .artifacts()
            .artifact_ids(),
        previous
    );
    assert!(!context
        .artifact_view()
        .artifact_ids()
        .contains(&value.artifact_id()));
    assert!(context.lexical_graph().is_empty());
    assert!(context.recovery_products().is_empty());
}

#[test]
fn interface_only_receiver_support_separates_type_custody_from_source_and_native_authority() {
    let directory = tempfile::tempdir().unwrap();
    let empty = Arc::new(ExactDeclarationContext::new(&[], &[], vec![]).unwrap());
    let assert_missing_ref = |error: CompileError| {
        assert!(matches!(error, CompileError::ArtifactInventory(error)
            if matches!(&error.failure, ArtifactInventoryFailure::MissingDependency {
                dependent, required, dependency: crate::artifact_inventory::ArtifactDependency::Interface, ..
            } if dependent == &identity("main", "Tidepool.Session.Val.G1")
                && required == &identity("main", "Tidepool.Agent.Ref"))));
    };
    assert_missing_ref(
        (*empty)
            .clone()
            .extend_program_value_interface(receiver_value_interface())
            .unwrap_err(),
    );
    let mut request = program_request(directory.path(), empty.clone());
    let wrong_owner = request
        .admit_fixture_support(
            empty.clone(),
            &interface_only_agent_ref_offer([2; 32], "foreign"),
            &[interface_only_agent_ref_admission(
                directory.path(),
                "foreign",
            )],
            None,
        )
        .unwrap();
    assert_missing_ref(
        (*wrong_owner)
            .clone()
            .extend_program_value_interface(receiver_value_interface())
            .unwrap_err(),
    );
    let support = interface_only_agent_ref_offer([2; 32], "main");
    let hidden = Arc::new(
        (*empty)
            .clone()
            .extend_issued_compiler_support(
                &recover_fixture_compiler_support(
                    support.projection.clone(),
                    support.artifacts.clone(),
                    [],
                )
                .unwrap(),
            )
            .unwrap(),
    );
    let mut request = program_request(directory.path(), hidden.clone());
    let retained = request
        .admit_fixture_support(
            hidden.clone(),
            &support,
            &[interface_only_agent_ref_admission(directory.path(), "main")],
            None,
        )
        .unwrap();
    assert!(retained.lexical_graph().is_empty());
    assert!(retained.recovery_products().is_empty());
    assert!(hidden.lexical_graph().is_empty());
    let uncaptured = interface_only_agent_ref_admission(directory.path(), "main");
    let mut uncaptured = Arc::unwrap_or_clone(uncaptured.evidence).into_evidence();
    uncaptured.modules[0].source = directory.path().join("uncaptured.hs");
    assert!(crate::cache::CompletedSourceEvidence::from_normalized(
        uncaptured,
        "module Target where\n",
    )
    .is_err());
    // A canonical certificate without consumed source grants type custody
    // while leaving authored names and source support unselected.
    let mut request = program_request(directory.path(), empty.clone());
    let retained = request
        .admit_fixture_support(
            empty.clone(),
            &interface_only_agent_ref_offer([2; 32], "main"),
            &[],
            None,
        )
        .unwrap();
    assert!(retained.recovery_products().is_empty());
    assert!(retained.lexical_graph().is_empty());
    assert!(request.program_support.is_none());
    assert!(request.program_source_lexical().is_empty());
    let mut foreign = program_request(directory.path(), empty.clone());
    assert!(foreign
        .admit_fixture_support(
            empty.clone(),
            &interface_only_agent_ref_offer([3; 32], "main"),
            &[interface_only_agent_ref_admission(directory.path(), "main")],
            None,
        )
        .is_err());
    assert!(foreign.program_support.is_none());
    assert!(empty.artifact_view().is_empty());
}

#[test]
fn program_support_scopes_published_roles_to_exact_carriers_without_retiring_parent() {
    let owners = [identity("fixture", "Alpha"), identity("fixture", "Beta")];
    let mut issuer = published_original_fixture();
    edit_lexical(&mut issuer, |nodes| {
        nodes
            .iter_mut()
            .find(|node| node.owner.module == "PrivateSourceTarget")
            .unwrap()
            .imports = owners.to_vec()
    });
    edit_lexical(&mut issuer, |nodes| {
        nodes.push(ExactLexicalNode {
            owner: owners[1].clone(),
            imports: vec![],
        })
    });
    issuer.normalize().unwrap();
    let mut parent = ExactDeclarationContext::new(&[], &[], vec![]).unwrap();
    for owner in &owners {
        parent = parent
            .with_published_source_originals(
                &issuer
                    .issue_published_source_original("revision", "input", owner)
                    .unwrap(),
            )
            .unwrap();
    }
    let roles = parent.compiler_input_roles();
    let before = parent.semantic_sha256();
    let custody = parent.artifact_view().artifact_ids();
    let mut histories = 0;
    for alpha in 0..3 {
        for beta in 0..3 {
            for unrelated in [false, true] {
                histories += 1;
                let states = [alpha, beta];
                let mut roots = Vec::new();
                for (index, role) in roles.iter().enumerate() {
                    if states[index] > 0 {
                        roots.push(role.interface());
                    }
                    if states[index] == 2 {
                        roots.push(role.original().unwrap());
                    }
                }
                let mut view = parent.artifact_view().select_roots(roots.clone()).unwrap();
                if unrelated {
                    view = view
                        .merge_fixture_selections(&support_view(&[support_product("Unrelated")]))
                        .unwrap();
                }
                let support = ProgramSourceSupport::extend(
                    None,
                    parent
                        .compiler_source_support()
                        .select_support_roots(roots.clone())
                        .unwrap(),
                    [],
                )
                .unwrap();
                let scoped = support.compiler_support.projection().roles();
                assert_eq!(
                    scoped.len(),
                    states.iter().filter(|state| **state > 0).count()
                );
                for (index, role) in roles.iter().enumerate() {
                    let observed = scoped
                        .iter()
                        .find(|candidate| candidate.interface() == role.interface());
                    match states[index] {
                        0 => assert!(observed.is_none()),
                        1 => {
                            assert_eq!(observed.unwrap().original(), None);
                            assert!(!observed.unwrap().is_published_source_original());
                        }
                        2 => assert_eq!(observed.unwrap(), role),
                        _ => unreachable!(),
                    }
                }
                assert_eq!(
                    support.compiler_support.artifacts().artifact_ids(),
                    view.artifact_ids()
                );
                assert_eq!(
                    support
                        .compiler_support
                        .artifacts()
                        .selected_native_groups(),
                    view.selected_native_groups()
                );
                // Repeated extension/reopening cannot convert a type carrier to native authority.
                let reopened = ProgramSourceSupport::extend(
                    Some(&support),
                    parent
                        .compiler_source_support()
                        .select_support_roots(roots.clone())
                        .unwrap(),
                    [],
                )
                .unwrap();
                assert_eq!(
                    *reopened.compiler_support.projection(),
                    support.compiler_support.projection().clone()
                );
                assert_eq!(reopened.imports, support.imports);
                if states != [2, 2] {
                    assert!(
                        parent
                            .owner
                            .facts
                            .compiler_support
                            .projection()
                            .within_view(&view)
                            .validate(&view)
                            .is_err(),
                        "persistent publication still refuses missing required custody"
                    );
                }
                assert_eq!(parent.semantic_sha256(), before);
                assert_eq!(parent.artifact_view().artifact_ids(), custody);
                assert_eq!(parent.compiler_input_roles(), roles);
                assert_eq!(
                    parent.published_source_original_selections().unwrap().len(),
                    2
                );
            }
        }
    }
    assert_eq!(histories, 18);
}

#[test]
fn program_support_completion_keeps_unrelated_publication_out_of_transient_offer() {
    let directory = tempfile::tempdir().unwrap();
    let mut issued = published_original_fixture();
    // The decoder/capture consumer also authenticates native witnesses.
    // Recover the fixture's existing products without changing any issued identity.
    let inventory = ArtifactInventory::default();
    let entries = issued
        .artifact_view()
        .entries()
        .into_iter()
        .map(|entry| {
            let ArtifactPayload::Original(product) = &entry.payload else {
                return entry;
            };
            let recovered = crate::certified_products::tests::recovered_witness_fixtures(
                std::slice::from_ref(product),
            )
            .remove(0)
            .product;
            let recovered =
                ArtifactEntry::original(issued.owner.facts.producer, recovered).unwrap();
            assert_eq!(recovered.descriptor, entry.descriptor);
            Arc::new(recovered)
        })
        .collect();
    {
        let updated = inventory
            .admit_shared(&inventory.empty_view(), entries)
            .unwrap();
        let support = recover_fixture_compiler_support(
            issued.compiler_input_projection().clone(),
            updated,
            issued
                .compiler_source_support()
                .native_offers()
                .iter()
                .cloned(),
        )
        .unwrap();
        issued = issued.rebuild(|facts| facts.compiler_support = support);
    }
    issued.normalize().unwrap();
    let publication = issued
        .issue_published_source_original("revision", "input", &identity("fixture", "Alpha"))
        .unwrap();
    let baseline = publication.context().clone();
    let before = baseline.semantic_sha256();
    let roles = baseline.compiler_input_roles();
    let custody = baseline.artifact_view().artifact_ids();
    let products = ["InstanceOwner", "InstanceRelay"]
        .map(|module| scaffold_native_fixture(baseline.owner.facts.producer, "fixture", module, 1));
    let offer = support_offer(&products);
    let mut request = program_request(directory.path(), baseline.clone());
    request.producer_sha256 = baseline.owner.facts.producer;
    let context = request
        .admit_fixture_support(
            baseline.clone(),
            &offer,
            &[support_admission(directory.path())],
            None,
        )
        .unwrap();
    let transient = request.program_support.as_ref().unwrap();
    assert_eq!(
        transient.compiler_support.projection().roles(),
        offer.projection.roles()
    );
    assert_eq!(
        transient.compiler_support.artifacts().artifact_ids(),
        offer.artifacts.artifact_ids()
    );
    assert!(!transient
        .compiler_support
        .projection()
        .roles()
        .iter()
        .any(|role| role.is_published_source_original()));
    assert!(context
        .compiler_input_roles()
        .iter()
        .any(|role| role == &roles[0]));
    assert_eq!(baseline.semantic_sha256(), before);
    assert_eq!(baseline.artifact_view().artifact_ids(), custody);
    assert_eq!(baseline.compiler_input_roles(), roles);
    let effective = request
        .in_program_context(&directory.path().join("program-inputs"), context)
        .unwrap();
    let receipt = import_receipt(directory.path(), &effective, "InstanceRelay");
    let admission = effective
        .validate_receipt(&receipt, None, effective.context())
        .unwrap();
    let consumer = scaffold_native_fixture(baseline.owner.facts.producer, "fixture", "Consumer", 1);
    let artifacts = effective
        .context()
        .artifact_view()
        .merge_fixture_selections(&support_view(&[consumer]))
        .unwrap();
    let original = ExactProductAdmission {
        request: &effective,
        source: &admission,
    }
    .original_execution_fixture(&artifacts)
    .unwrap();
    assert_eq!(
        original
            .lexical_graph()
            .iter()
            .find(|node| node.owner == identity("fixture", "InstanceRelay"))
            .unwrap()
            .imports,
        vec![identity("fixture", "InstanceOwner")]
    );
    assert_eq!(baseline.semantic_sha256(), before);
    // The issued completion owns captured generated bytes, independently
    // of later changes to the compiler's primary scratch source.
    let decoded = read_exact_compilation_receipt(&receipt).unwrap();
    assert_eq!(decoded.source_path, admission.witness.source_path);
    assert_eq!(decoded.source_sha256, admission.witness.source_sha256);
    let encoded = read_receipt(&receipt);
    let snapshot = PathBuf::from(encoded.as_array().unwrap()[6].as_text().unwrap());
    let captured = std::fs::read(&snapshot).unwrap();
    assert_eq!(sha256(&captured), hex(&decoded.source_sha256));
    assert_ne!(snapshot, decoded.source_path);
    std::fs::write(&decoded.source_path, b"changed primary scratch source").unwrap();
    let reopened = effective
        .validate_receipt(&receipt, None, effective.context())
        .unwrap();
    assert_eq!(
        reopened.witness.source_sha256,
        admission.witness.source_sha256
    );
    assert_eq!(std::fs::read(&snapshot).unwrap(), captured);
    std::fs::write(&snapshot, b"changed retained source snapshot").unwrap();
    assert!(matches!(
        effective.validate_receipt(&receipt, None, effective.context()),
        Err(CompileError::ExtractFailed(detail))
            if detail == "exact declaration context: compile source snapshot changed"
    ));
    std::fs::write(&snapshot, captured).unwrap();
    assert!(effective
        .validate_receipt(&receipt, None, effective.context())
        .is_ok());
    assert_eq!(baseline.semantic_sha256(), before);
}

#[test]
fn program_support_retains_selected_home_edges_without_public_exposure() {
    let directory = tempfile::tempdir().unwrap();
    let hidden = support_product("Hidden");
    let baseline = Arc::new(
        ExactDeclarationContext::new(&[], &[], vec![])
            .unwrap()
            .extend_checked_original_products([2; 32], &[hidden])
            .unwrap(),
    );
    let mut request = program_request(directory.path(), baseline.clone());
    let admission = support_admission(directory.path());
    let context = request
        .admit_fixture_support(
            baseline.clone(),
            &support_offer(&[
                support_product("InstanceOwner"),
                support_product("InstanceRelay"),
            ]),
            &[admission],
            None,
        )
        .unwrap();
    assert!(context.lexical_graph().is_empty());
    assert!(baseline.lexical_graph().is_empty());
    let relay = context
        .artifact_view()
        .entries_for_owners(std::iter::once(identity("fixture", "InstanceRelay")))
        .unwrap();
    // The source import selects lexical support; this fixture's canonical
    // interface deliberately has no type dependency on InstanceOwner.
    assert!(relay[&identity("fixture", "InstanceRelay")]
        .requirements
        .is_empty());
    assert_eq!(
        request.program_source_lexical(),
        &[
            ExactLexicalNode {
                owner: identity("fixture", "InstanceOwner"),
                imports: vec![]
            },
            ExactLexicalNode {
                owner: identity("fixture", "InstanceRelay"),
                imports: vec![identity("fixture", "InstanceOwner")]
            },
        ]
    );
    let effective = request
        .in_program_context(&directory.path().join("program-inputs"), context)
        .unwrap();
    let receipt = import_receipt(directory.path(), &effective, "InstanceRelay");
    let admission = effective
        .validate_receipt(&receipt, None, effective.context())
        .unwrap();
    let artifacts = effective
        .context()
        .artifact_view()
        .merge_fixture_selections(&support_view(&[support_product("Consumer")]))
        .unwrap();
    let original = ExactProductAdmission {
        request: &effective,
        source: &admission,
    }
    .original_execution_fixture(&artifacts)
    .unwrap();
    assert!(matches!(original.original_instance_environment(),
        OriginalInstanceEnvironment::Complete { target }
            if target == &identity("fixture", "Consumer")));
    assert_eq!(
        original
            .lexical_graph()
            .iter()
            .find(|node| node.owner == identity("fixture", "InstanceRelay"))
            .unwrap()
            .imports,
        vec![identity("fixture", "InstanceOwner")]
    );
    assert!(original
        .lexical_graph()
        .iter()
        .any(|node| node.owner == identity("fixture", "InstanceOwner")));
    assert!(!original
        .lexical_graph()
        .iter()
        .any(|node| node.owner == identity("fixture", "Hidden")));
    assert!(effective.context().lexical_graph().is_empty());
    let mut incomplete = effective.clone();
    Arc::make_mut(&mut incomplete.program_support.as_mut().unwrap().imports)
        .remove(&identity("fixture", "InstanceOwner"));
    let incomplete = ExactProductAdmission {
        request: &incomplete,
        source: &admission,
    }
    .original_execution_fixture(&artifacts)
    .unwrap();
    assert!(
        matches!(incomplete.original_instance_environment(),
        OriginalInstanceEnvironment::MissingOriginalOwners(owners)
            if owners.contains(&identity("fixture", "InstanceOwner"))),
        "retained interface custody cannot invent an absent original import row"
    );
    let receipt = import_receipt(directory.path(), &effective, "Hidden");
    assert!(effective
        .validate_receipt(&receipt, None, effective.context())
        .is_err());
}

#[test]
fn program_support_refuses_retained_hidden_owner_and_forged_source() {
    let directory = tempfile::tempdir().unwrap();
    let products = [
        support_product("InstanceOwner"),
        support_product("InstanceRelay"),
    ];
    let baseline = Arc::new(
        ExactDeclarationContext::new(&[], &[], vec![])
            .unwrap()
            .extend_checked_original_products([2; 32], &products)
            .unwrap(),
    );
    let mut request = program_request(directory.path(), baseline.clone());
    assert!(request
        .admit_fixture_support(
            baseline,
            &support_offer(&products),
            &[support_admission(directory.path())],
            None
        )
        .is_err());
    let original = support_admission(directory.path());
    let mut forged = Arc::unwrap_or_clone(original.evidence).into_evidence();
    forged.modules[0].source = directory.path().join("AnotherOwner.hs");
    assert!(crate::cache::CompletedSourceEvidence::from_normalized(
        forged,
        "module Target where\n",
    )
    .is_err());
}

#[test]
fn program_support_refuses_conflicting_selected_edges() {
    let directory = tempfile::tempdir().unwrap();
    let empty = Arc::new(ExactDeclarationContext::new(&[], &[], vec![]).unwrap());
    let mut request = program_request(directory.path(), empty.clone());
    let original = support_admission(directory.path());
    let mut changed = support_admission(directory.path());
    let mut evidence = Arc::unwrap_or_clone(changed.evidence).into_evidence();
    evidence.modules[1].imports.clear();
    changed.evidence =
        crate::cache::CompletedSourceEvidence::from_normalized(evidence, "module Target where\n")
            .unwrap()
            .into();
    assert!(request
        .admit_fixture_support(
            empty,
            &support_offer(&[
                support_product("InstanceOwner"),
                support_product("InstanceRelay")
            ]),
            &[original, changed],
            None
        )
        .is_err());
}

#[test]
fn program_support_dependency_closure_does_not_select_hidden_names() {
    let directory = tempfile::tempdir().unwrap();
    let hidden = support_product("Hidden");
    let relay = crate::certified_products::fixture_finalized_product_with_requirements(
        support_product("InstanceRelay"),
        [2; 32],
        Some(BTreeMap::from([(
            ("fixture".into(), "Hidden".into()),
            hidden.module_interface().unwrap().interface_sha256(),
        )])),
    );
    let context = Arc::new(
        ExactDeclarationContext::new(&[], &[], vec![])
            .unwrap()
            .extend_checked_original_products([2; 32], &[hidden, relay])
            .unwrap(),
    );
    let entries = context
        .artifact_view()
        .entries_for_owners(std::iter::once(identity("fixture", "InstanceRelay")))
        .unwrap();
    let mut request = program_request(directory.path(), context.clone());
    request.program_support = Some(ProgramSourceSupport {
        compiler_support: context
            .compiler_source_support()
            .select_support_roots(vec![
                entries[&identity("fixture", "InstanceRelay")].descriptor.id,
            ])
            .unwrap(),
        imports: Arc::new(BTreeMap::new()),
    });
    let support = &request
        .program_support
        .as_ref()
        .unwrap()
        .compiler_support
        .artifacts();
    let mut actual_owners = support
        .descriptors()
        .into_iter()
        .map(|descriptor| (descriptor.owner, descriptor.kind))
        .collect::<Vec<_>>();
    actual_owners.sort();
    assert_eq!(
        actual_owners,
        vec![
            (
                identity("fixture", "Hidden"),
                ArtifactKind::CanonicalModuleInterface
            ),
            (
                identity("fixture", "InstanceRelay"),
                ArtifactKind::OriginalModule
            ),
            (
                identity("fixture", "InstanceRelay"),
                ArtifactKind::CanonicalModuleInterface
            ),
        ]
    );
    assert_eq!(
        support
            .root_entries()
            .iter()
            .map(|entry| entry.descriptor.owner.clone())
            .collect::<Vec<_>>(),
        vec![identity("fixture", "InstanceRelay")]
    );
    assert!(!support
        .source_implementation_roles()
        .contains_key(&identity("fixture", "Hidden")));
    assert_eq!(
        entries[&identity("fixture", "InstanceRelay")].requirements,
        vec![identity("fixture", "Hidden")]
    );
    let receipt = import_receipt(directory.path(), &request, "InstanceRelay");
    assert!(request.validate_receipt(&receipt, None, &context).is_ok());
    let receipt = import_receipt(directory.path(), &request, "Hidden");
    assert!(request.validate_receipt(&receipt, None, &context).is_err());
    assert!(context.lexical_graph().is_empty());
    let RetainedValueSourceSurface {
        artifacts: retained,
        lexical,
        ..
    } = context
        .retain_value_source_surface(
            &request
                .program_support
                .as_ref()
                .unwrap()
                .compiler_support
                .artifacts(),
            &[],
        )
        .unwrap();
    assert!(support
        .artifact_ids()
        .iter()
        .all(|id| retained.artifact_ids().contains(id)));
    assert!(retained
        .descriptors()
        .iter()
        .any(|entry| entry.owner == identity("fixture", "Hidden")
            && entry.kind == ArtifactKind::OriginalModule));
    assert!(lexical.is_empty());
}

#[test]
fn retained_value_source_surface_preserves_selected_support_and_refuses_hidden_owners() {
    let directory = tempfile::tempdir().unwrap();
    let baseline = Arc::new(
        ExactDeclarationContext::new(&[], &[], vec![])
            .unwrap()
            .extend_checked_original_products([2; 32], &[support_product("Hidden")])
            .unwrap(),
    );
    let mut request = program_request(directory.path(), baseline.clone());
    let context = request
        .admit_fixture_support(
            baseline,
            &support_offer(&[
                support_product("InstanceOwner"),
                support_product("InstanceRelay"),
            ]),
            &[support_admission(directory.path())],
            None,
        )
        .unwrap();
    assert!(context.lexical_graph().is_empty());
    let owner = identity("fixture", "InstanceRelay");
    let entries = context
        .artifact_view()
        .entries_for_owners(std::iter::once(owner.clone()))
        .unwrap();
    let value = context
        .artifact_view()
        .select_roots(vec![entries[&owner].descriptor.id])
        .unwrap();
    let shared =
        PreparedValueSourceSupport::new(context.clone(), request.program_source_lexical()).unwrap();
    for roots in [
        vec![identity("fixture", "InstanceOwner")],
        vec![owner.clone()],
    ] {
        let selected = context
            .artifact_view()
            .entries_for_owners(roots.clone().into_iter())
            .unwrap();
        let selected = context
            .artifact_view()
            .select_roots(
                roots
                    .iter()
                    .map(|root| selected[root].descriptor.id)
                    .collect(),
            )
            .unwrap();
        let surface = shared.retain_surface(&selected, &BTreeSet::new()).unwrap();
        let expected = if roots == [owner.clone()] {
            vec![identity("fixture", "InstanceOwner"), owner.clone()]
        } else {
            roots
        };
        assert_eq!(
            surface
                .lexical
                .iter()
                .map(|node| node.owner.clone())
                .collect::<Vec<_>>(),
            expected
        );
        assert!(surface
            .lexical
            .iter()
            .all(|node| node.owner != identity("fixture", "Hidden")));
    }
    let RetainedValueSourceSurface {
        artifacts: view,
        lexical,
        ..
    } = context
        .retain_value_source_surface(&value, request.program_source_lexical())
        .unwrap();
    assert!(lexical
        .iter()
        .all(|node| node.owner != identity("fixture", "Hidden")));
    let typed_owners = |view: &ArtifactView| {
        let mut owners = view
            .descriptors()
            .into_iter()
            .map(|entry| (entry.owner, entry.kind))
            .collect::<Vec<_>>();
        owners.sort();
        owners
    };
    let expected_owners = vec![
        (identity("fixture", "Hidden"), ArtifactKind::OriginalModule),
        (
            identity("fixture", "Hidden"),
            ArtifactKind::CanonicalModuleInterface,
        ),
        (
            identity("fixture", "InstanceOwner"),
            ArtifactKind::OriginalModule,
        ),
        (
            identity("fixture", "InstanceOwner"),
            ArtifactKind::CanonicalModuleInterface,
        ),
        (
            identity("fixture", "InstanceRelay"),
            ArtifactKind::OriginalModule,
        ),
        (
            identity("fixture", "InstanceRelay"),
            ArtifactKind::CanonicalModuleInterface,
        ),
    ];
    assert_eq!(typed_owners(&view), expected_owners);

    let later = (*context)
        .clone()
        .extend_checked_original_products([2; 32], &[support_product("LaterSupport")])
        .unwrap();
    let mut later_support = request.program_source_lexical().to_vec();
    later_support.push(ExactLexicalNode {
        owner: identity("fixture", "LaterSupport"),
        imports: vec![],
    });
    let RetainedValueSourceSurface {
        artifacts: earlier_view,
        lexical: earlier_lexical,
        ..
    } = later
        .retain_value_source_surface(&value, &later_support)
        .unwrap();
    assert!(earlier_view
        .descriptors()
        .iter()
        .any(|entry| entry.owner == identity("fixture", "LaterSupport")
            && entry.kind == ArtifactKind::OriginalModule));
    assert_eq!(
        typed_owners(&view),
        expected_owners,
        "the already captured view remains immutable"
    );
    assert_eq!(earlier_lexical, lexical);
    assert_eq!(
        lexical,
        vec![
            ExactLexicalNode {
                owner: identity("fixture", "InstanceOwner"),
                imports: vec![]
            },
            ExactLexicalNode {
                owner,
                imports: vec![identity("fixture", "InstanceOwner")]
            },
        ]
    );
    let mut following = (*context).clone();
    {
        let updated = following
            .owner
            .facts
            .lexical
            .replace(lexical.clone())
            .unwrap();
        following = following.rebuild(|facts| facts.lexical = updated);
    }
    following.normalize().unwrap();
    let following = Arc::new(following);
    let next = program_request(directory.path(), following.clone());
    assert!(next
        .validate_receipt(
            &import_receipt(directory.path(), &next, "InstanceRelay"),
            None,
            &following
        )
        .is_ok());
    assert!(next
        .validate_receipt(
            &import_receipt(directory.path(), &next, "Hidden"),
            None,
            &following
        )
        .is_err());
    // A subsequent output keeps the already selected source surface even
    // when that request has no new source-selected supporting products.
    assert_eq!(
        following
            .retain_value_source_surface(&value, &[])
            .unwrap()
            .lexical,
        lexical
    );
    let conflict = [ExactLexicalNode {
        owner: identity("fixture", "InstanceRelay"),
        imports: vec![],
    }];
    assert!(following
        .retain_value_source_surface(&value, &conflict)
        .is_err());
    let incomplete = [ExactLexicalNode {
        owner: identity("fixture", "InstanceRelay"),
        imports: vec![identity("fixture", "Hidden")],
    }];
    assert!(context
        .retain_value_source_surface(&value, &incomplete)
        .is_err());
    let receipt = import_receipt(directory.path(), &next, "InstanceRelay");
    let mut replacement: Value =
        ciborium::de::from_reader(std::fs::read(&receipt).unwrap().as_slice()).unwrap();
    let fields = replacement.as_array_mut().unwrap();
    let replacement_source = "module InstanceRelay where\nreplaced = 99 :: Int\n";
    std::fs::write(fields[6].as_text().unwrap(), replacement_source).unwrap();
    fields[5] = text(sha256(replacement_source.as_bytes()));
    let mut evidence: crate::cache::DependencyEvidence =
        serde_json::from_str(fields[7].as_text().unwrap()).unwrap();
    evidence.sources[0].sha256 = sha256(replacement_source.as_bytes());
    evidence.modules[0].module = "InstanceRelay".into();
    fields[7] = text(serde_json::to_string(&evidence).unwrap());
    let module = fields[8].as_array_mut().unwrap()[0].as_array_mut().unwrap();
    module[1] = text("InstanceRelay");
    module[3] = Value::Array(vec![]);
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(&replacement, &mut bytes).unwrap();
    std::fs::write(&receipt, bytes).unwrap();
    let refused = next
        .validate_receipt(&receipt, None, &following)
        .err()
        .unwrap();
    assert!(refused
        .to_string()
        .contains("fresh module replaced an admitted exact owner"));
}

#[test]
fn program_support_partitions_inherited_products_from_new_selected_roots() {
    let directory = tempfile::tempdir().unwrap();
    let hidden = support_product("Hidden");
    let owner = support_product("InstanceOwner");
    let relay = support_product("InstanceRelay");
    let baseline = Arc::new(
        ExactDeclarationContext::new(&[], &[], vec![])
            .unwrap()
            .extend_checked_original_products([2; 32], std::slice::from_ref(&hidden))
            .unwrap(),
    );
    let mut request = program_request(directory.path(), baseline.clone());
    let context = request
        .admit_fixture_support(
            baseline,
            &support_offer(&[hidden.clone(), owner.clone(), relay.clone()]),
            &[support_admission(directory.path())],
            None,
        )
        .unwrap();
    assert_eq!(
        request
            .program_support
            .as_ref()
            .unwrap()
            .compiler_support
            .artifacts()
            .root_entries()
            .len(),
        2
    );
    let mut request = request
        .in_program_context(&directory.path().join("program-inputs"), context.clone())
        .unwrap();
    let path = directory.path().join("Additional.hs");
    std::fs::write(&path, b"module Additional where\n").unwrap();
    let receipt = support_receipt(directory.path(), &request);
    let mut value = read_receipt(&receipt);
    let fields = value.as_array_mut().unwrap();
    let snapshot = PathBuf::from(fields[6].as_text().unwrap());
    let source = "module Additional where\n";
    std::fs::write(&snapshot, source).unwrap();
    let evidence = crate::cache::DependencyEvidence {
        version: 4,
        cache_safe: true,
        selection_complete: true,
        sources: vec![crate::cache::SourceEvidence {
            path: path.clone(),
            sha256: sha256(source.as_bytes()),
        }],
        resolutions: vec![],
        packages: vec![],
        modules: vec![crate::cache::ModuleEvidence {
            unit: "fixture".into(),
            module: "Additional".into(),
            boot: false,
            source: path.clone(),
            imports: vec![],
            product: crate::cache::ProductAvailability::Ready,
        }],
    };
    fields[4] = path_value(&path).unwrap();
    fields[5] = text(sha256(source.as_bytes()));
    fields[7] = text(serde_json::to_string(&evidence).unwrap());
    fields[8] = Value::Array(vec![Value::Array(vec![
        text("fixture"),
        text("Additional"),
        Value::Bool(false),
        Value::Array(vec![Value::Array(vec![
            text("none"),
            text("InstanceRelay"),
            Value::Bool(false),
            text("fixture"),
        ])]),
    ])]);
    write_receipt(&receipt, &value);
    let later = request
        .validate_receipt(&receipt, None, request.context())
        .unwrap();
    let additional = support_product("Additional");
    let mut expected_roots = BTreeMap::new();
    let mut expected_roles = BTreeMap::new();
    for product in [&owner, &relay, &additional] {
        let native = ArtifactEntry::original([2; 32], product.clone()).unwrap();
        let interface = ArtifactEntry::canonical(product.module_interface().unwrap().clone());
        expected_roles.insert(
            native.descriptor.owner.clone(),
            CompilerInputRole::ReusableOriginal {
                interface: interface.descriptor.id,
                original: native.descriptor.id,
            },
        );
        expected_roots.insert(interface.descriptor.id, interface.descriptor);
        expected_roots.insert(native.descriptor.id, native.descriptor);
    }
    let context = request
        .admit_fixture_support(
            context,
            &support_offer(&[hidden.clone(), owner, relay, additional]),
            &[later],
            None,
        )
        .unwrap();
    let support = &request.program_support.as_ref().unwrap().compiler_support;
    // Merged support roots retain each source role's canonical interface
    // and native original as distinct issued payloads.
    assert_eq!(
        support
            .artifacts()
            .root_entries()
            .into_iter()
            .map(|entry| (entry.descriptor.id, entry.descriptor.clone()))
            .collect::<BTreeMap<_, _>>(),
        expected_roots
    );
    let actual_roles = support
        .projection()
        .snapshot_roles()
        .iter()
        .map(|(owner, role)| (owner.clone(), role.clone()))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(actual_roles, expected_roles);
    assert!(!actual_roles.contains_key(&identity("fixture", "Hidden")));
    assert!(context.lexical_graph().is_empty());
    let effective = request
        .in_program_context(&directory.path().join("program-inputs"), context.clone())
        .unwrap();
    let receipt = import_receipt(directory.path(), &effective, "Additional");
    assert!(effective.validate_receipt(&receipt, None, &context).is_ok());
    let receipt = import_receipt(directory.path(), &effective, "Hidden");
    assert!(effective
        .validate_receipt(&receipt, None, &context)
        .is_err());
    let altered = support_product_with_interface(
        "fixture",
        "Hidden",
        b"different inherited interface".to_vec(),
    );
    assert!(request
        .admit_fixture_support(context, &support_offer(&[altered]), &[], None)
        .is_err());
}

#[test]
fn program_support_refuses_same_canonical_interface_with_another_native_owner() {
    let native_variant = |product: &CertifiedRecoveryProduct| {
        let interface = product.module_interface().unwrap().clone();
        let mut owner = product.owner().clone();
        owner.module_version = ModuleVersion([99; 32]);
        let certification = crate::certified_products::encode_home_certification_with_module(
            &owner,
            &[],
            &BTreeMap::new(),
            interface.requirements(),
            Sha256::digest(interface.certificate_bytes()).into(),
        )
        .unwrap();
        let mut variant = CertifiedRecoveryProduct::from_certification(
            owner,
            product.interface_bytes().to_vec(),
            product.product_bytes().to_vec(),
            product.package_imports_bytes().to_vec(),
            certification,
        )
        .with_module_interface(interface)
        .unwrap();
        if let Some(source_sha256) = product.source_sha256() {
            variant = variant.with_source_sha256(source_sha256);
        }
        crate::certified_products::tests::recovered_witness_fixtures(&[variant])
            .remove(0)
            .product
    };
    let directory = tempfile::tempdir().unwrap();
    let original = support_product("Hidden");
    let variant = native_variant(&original);
    assert_eq!(variant.module_interface(), original.module_interface());
    assert_ne!(variant.owner(), original.owner());
    let context = Arc::new(
        ExactDeclarationContext::new(&[], &[], vec![])
            .unwrap()
            .extend_checked_original_products([2; 32], &[original])
            .unwrap(),
    );
    let mut request = program_request(directory.path(), context.clone());
    let support = support_offer(&[variant]);
    let assert_native_refusal = |error| {
        assert!(matches!(error, CompileError::ArtifactInventory(error)
            if matches!(error.failure, ArtifactInventoryFailure::CompilerOriginalOfferConflict { .. })));
    };
    let inherited_roles = context.compiler_input_roles();
    let inherited_originals = context.compiler_original_products().unwrap();
    let retained = request
        .admit_fixture_support(context.clone(), &support, &[], None)
        .unwrap();
    assert_eq!(retained.recovery_products().len(), 2);
    assert_eq!(retained.compiler_input_roles(), inherited_roles);
    assert_eq!(
        retained.compiler_original_products().unwrap(),
        inherited_originals
    );
    assert!(retained.lexical_graph().is_empty());
    assert!(request.program_support.is_none());
    assert!(request.program_source_lexical().is_empty());
    assert_eq!(context.recovery_products().len(), 1);
    let unchanged = context.recovery_products();
    let unchanged_support = support_offer(&unchanged);
    let unchanged_context = request
        .admit_fixture_support(context.clone(), &unchanged_support, &[], None)
        .unwrap();
    assert_eq!(
        unchanged_context.recovery_products()[0].owner(),
        unchanged[0].owner()
    );
    let canonical = certified_product_artifact_view(
        [2; 32],
        &[],
        &[unchanged[0].module_interface().unwrap().clone()],
        None,
    )
    .unwrap();
    let canonical_context = Arc::new(
        ExactDeclarationContext::new(&[], &[], vec![])
            .unwrap()
            .extend_issued_compiler_support(
                &recover_fixture_compiler_support(
                    CompilerInputProjection::from_issued_entries(&[Arc::new(
                        ArtifactEntry::canonical(unchanged[0].module_interface().unwrap().clone()),
                    )])
                    .unwrap(),
                    canonical,
                    [],
                )
                .unwrap(),
            )
            .unwrap(),
    );
    let mut canonical_request = program_request(directory.path(), canonical_context.clone());
    let promoted = canonical_request
        .admit_fixture_support(canonical_context, &unchanged_support, &[], None)
        .unwrap();
    assert_eq!(
        promoted.recovery_products()[0].owner(),
        unchanged[0].owner()
    );
    assert!(promoted.compiler_original_products().unwrap().is_empty());
    assert!(promoted
        .compiler_input_roles()
        .iter()
        .all(|role| matches!(role, CompilerInputRole::InterfaceOnly { .. })));
    assert!(canonical_request.program_support.is_none());
    assert!(promoted.lexical_graph().is_empty());

    // A real validated current-source receipt cannot replace native identity
    // merely because the canonical interface agrees.
    let (mut request, context, receipt) = source_selected_receipt(directory.path(), true, None);
    let admission = request.validate_receipt(&receipt, None, &context).unwrap();
    let products = context.recovery_products();
    let support = support_offer(
        &products
            .iter()
            .map(|product| {
                if product.owner().module == "A" {
                    native_variant(product)
                } else {
                    product.clone()
                }
            })
            .collect::<Vec<_>>(),
    );
    assert_native_refusal(
        request
            .admit_fixture_support(context.clone(), &support, &[admission], None)
            .unwrap_err(),
    );
    assert!(request.program_support.is_none());
    assert!(request.program_source_lexical().is_empty());
    assert_eq!(context.recovery_products().len(), 2);
}

#[test]
fn authenticated_home_resume_source_surface_follows_ordinary_source_policy() {
    let directory = tempfile::tempdir().unwrap();
    let baseline = Arc::new(ExactDeclarationContext::new(&[], &[], vec![]).unwrap());
    let mut request = program_request(directory.path(), baseline.clone());
    let receipt = support_receipt(directory.path(), &request);
    let mut value = read_receipt(&receipt);
    let fields = value.as_array_mut().unwrap();
    let mut evidence: crate::cache::DependencyEvidence =
        serde_json::from_str(fields[7].as_text().unwrap()).unwrap();
    let path = directory.path().join("Resume.hs");
    let source = "module Tidepool.Internal.Resume where\n";
    std::fs::write(&path, source).unwrap();
    evidence
        .sources
        .retain(|source| source.path == directory.path().join("Target.hs"));
    evidence.sources.push(crate::cache::SourceEvidence {
        path: path.clone(),
        sha256: sha256(source.as_bytes()),
    });
    evidence.modules = vec![crate::cache::ModuleEvidence {
        unit: "main".into(),
        module: "Tidepool.Internal.Resume".into(),
        boot: false,
        source: path,
        imports: vec![],
        product: crate::cache::ProductAvailability::Ready,
    }];
    evidence.resolutions.clear();
    fields[7] = text(serde_json::to_string(&evidence).unwrap());
    fields[8] = Value::Array(vec![Value::Array(vec![
        text("main"),
        text("Tidepool.Internal.Resume"),
        Value::Bool(false),
        Value::Array(vec![]),
    ])]);
    write_receipt(&receipt, &value);
    let admission = request
        .validate_receipt(&receipt, None, request.context())
        .unwrap();
    let context = request
        .admit_fixture_support(
            baseline,
            &support_offer(&[support_product_in_unit("main", "Tidepool.Internal.Resume")]),
            &[admission],
            None,
        )
        .unwrap();
    assert_eq!(request.program_source_lexical().len(), 1);
    assert_eq!(
        request.program_source_lexical()[0].owner,
        identity("main", "Tidepool.Internal.Resume")
    );
    let native = &request
        .program_support
        .as_ref()
        .unwrap()
        .compiler_support
        .artifacts();
    assert_eq!(native.descriptors().len(), 2);
    assert_eq!(
        context
            .retain_value_source_surface(native, request.program_source_lexical())
            .unwrap()
            .lexical,
        request.program_source_lexical()
    );
    let current = request
        .in_program_context(&directory.path().join("program-inputs"), context.clone())
        .unwrap();
    let receipt = import_receipt_owner(
        directory.path(),
        &current,
        "main",
        "Tidepool.Internal.Resume",
        "none",
        false,
    );
    let admission = current.validate_receipt(&receipt, None, &context).unwrap();
    let artifacts = context
        .artifact_view()
        .merge_fixture_selections(&support_view(&[support_product("Consumer")]))
        .unwrap();
    let original = ExactProductAdmission {
        request: &current,
        source: &admission,
    }
    .original_execution_fixture(&artifacts)
    .unwrap();
    assert!(matches!(
        original.original_instance_environment(),
        OriginalInstanceEnvironment::Complete { .. }
    ));
    assert!(original
        .lexical_graph()
        .iter()
        .any(|node| node.owner == identity("main", "Tidepool.Internal.Resume")));
    assert!(context.lexical_graph().is_empty());
    let next = program_request(directory.path(), context.clone());
    let receipt = import_receipt_owner(
        directory.path(),
        &next,
        "main",
        "Tidepool.Internal.Resume",
        "none",
        false,
    );
    assert!(next.validate_receipt(&receipt, None, &context).is_err());
}

#[test]
fn program_support_refuses_ambiguous_selected_source_owner() {
    let directory = tempfile::tempdir().unwrap();
    let admission = support_admission(directory.path());
    let mut evidence = Arc::unwrap_or_clone(admission.evidence).into_evidence();
    evidence.modules.push(evidence.modules[0].clone());
    assert!(consumed_source_home_imports(&evidence, &admission.exact_imports).is_err());
    assert!(crate::cache::CompletedSourceEvidence::from_normalized(
        evidence,
        "module Target where\n",
    )
    .is_err());
}

#[test]
fn program_values_retain_private_type_owners_without_selecting_their_names() {
    let directory = tempfile::tempdir().unwrap();
    let baseline = Arc::new(
        ExactDeclarationContext::new(&[], &[], vec![])
            .unwrap()
            .extend_checked_original_products([2; 32], &[support_product("Public")])
            .unwrap(),
    );
    let mut baseline = (*baseline).clone();
    edit_lexical(&mut baseline, |nodes| {
        nodes.push(ExactLexicalNode {
            owner: identity("fixture", "Public"),
            imports: vec![],
        })
    });
    let baseline = Arc::new(baseline);
    let mut request = program_request(directory.path(), baseline.clone());
    let context = request
        .admit_fixture_support(
            baseline,
            &support_offer(&[
                support_product("InstanceOwner"),
                support_product("InstanceRelay"),
            ]),
            &[support_admission(directory.path())],
            None,
        )
        .unwrap();
    let value = |module: &str, requirements| {
        let evidence = support_product(module);
        Arc::new(
            CertifiedValueInterface::from_checked_compilation(
                [2; 32],
                identity("fixture", module),
                evidence.interface_bytes().to_vec(),
                evidence.package_imports_bytes().to_vec(),
                requirements,
            )
            .unwrap(),
        )
    };
    let first = value(
        "ValFirst",
        vec![
            identity("fixture", "Public"),
            identity("fixture", "InstanceRelay"),
        ],
    );
    let context = (*context)
        .clone()
        .extend_program_value_interface(first)
        .unwrap();
    assert_eq!(
        context.lexical_graph()[1].imports,
        vec![identity("fixture", "Public")]
    );
    let entry = context
        .artifact_view()
        .entries_for_owners(std::iter::once(identity("fixture", "ValFirst")))
        .unwrap();
    assert_eq!(
        entry[&identity("fixture", "ValFirst")].requirements,
        vec![
            identity("fixture", "InstanceRelay"),
            identity("fixture", "Public")
        ]
    );
    let second = value(
        "ValSecond",
        vec![
            identity("fixture", "ValFirst"),
            identity("fixture", "InstanceOwner"),
        ],
    );
    let context = context.extend_program_value_interface(second).unwrap();
    assert_eq!(
        context.lexical_graph()[2].imports,
        vec![identity("fixture", "ValFirst")]
    );
    assert!(!context
        .lexical_graph()
        .iter()
        .any(|node| node.owner.module.starts_with("Instance")));
    let forged = value("ValForged", vec![identity("fixture", "Absent")]);
    let error = context.extend_program_value_interface(forged).unwrap_err();
    assert!(matches!(error, CompileError::ArtifactInventory(error)
        if matches!(&error.failure, crate::artifact_inventory::ArtifactInventoryFailure::MissingDependency {
            required, dependency: crate::artifact_inventory::ArtifactDependency::Interface, ..
        } if required == &identity("fixture", "Absent"))));
}

#[test]
fn first_program_context_growth_materializes_and_checks_new_original() {
    let interface = b"interface".to_vec();
    let value = Value::Array(vec![
        text("TPMOD"),
        Value::Integer(1.into()),
        Value::Array(vec![Value::Array(vec![
            text("fixture"),
            text("Support"),
            Value::Bytes(interface.clone()),
            Value::Array(vec![]),
        ])]),
    ]);
    let mut product_bytes = Vec::new();
    ciborium::ser::into_writer(&value, &mut product_bytes).unwrap();
    let owner = CachedHomeOwner {
        unit: "fixture".into(),
        module: "Support".into(),
        module_version: ModuleVersion([1; 32]),
        skinny_iface_sha256: Sha256::digest(&interface).into(),
        product_sha256: Sha256::digest(&product_bytes).into(),
    };
    let certification =
        crate::certified_products::encode_home_certification(&owner, &[], &BTreeMap::new())
            .unwrap();
    let package_value = Value::Array(vec![
        text("TPPKGROOTS"),
        text("2"),
        Value::Array(vec![
            text(&owner.unit),
            text(&owner.module),
            text(sha256(&interface)),
        ]),
        Value::Array(vec![]),
        Value::Array(vec![]),
    ]);
    let mut package_bytes = Vec::new();
    ciborium::ser::into_writer(&package_value, &mut package_bytes).unwrap();
    let product = crate::certified_products::fixture_finalized_product(
        CertifiedRecoveryProduct::from_certification(
            owner,
            interface,
            product_bytes,
            package_bytes,
            certification,
        ),
        [2; 32],
    );
    let baseline = Arc::new(ExactDeclarationContext::new(&[], &[], vec![]).unwrap());
    let context = Arc::new(
        baseline
            .as_ref()
            .clone()
            .extend_checked_original_products([2; 32], &[product])
            .unwrap(),
    );
    let directory = tempfile::tempdir().unwrap();
    let request = ExactCompilationRequest {
        inputs: ExactRequestInputs::for_context(baseline),
        manifest: directory.path().join("scope"),
        request_sha256: String::new(),
        semantic_sha256: [1; 32],
        producer_sha256: [2; 32],
        artifacts: vec![],
        groups: Arc::from([]),
        materialization: None,
        program_support: None,
        program_source_lexical: Vec::new(),
        source_selected_support: BTreeSet::new(),
        source_search_include: None,
        checked_value_imports: Default::default(),
        generated_scaffold_imports: Vec::new(),
    };
    let root = directory.path().join("program-inputs");
    assert!(!root.exists());
    let effective = request.in_program_context(&root, context).unwrap();
    assert_eq!(effective.artifacts.len(), 1);
    assert!(effective.artifacts[0].interface.path.is_file());
    effective
        .context()
        .validate_artifacts(&effective.artifacts)
        .unwrap();
    std::fs::write(&effective.artifacts[0].interface.path, b"tampered").unwrap();
    assert!(effective
        .context()
        .validate_artifacts(&effective.artifacts)
        .is_err());
}
#[test]
fn program_context_promotes_canonical_owner_without_duplicate_materialization() {
    let product = support_product("Support");
    let unchanged = support_product("Unchanged");
    let inventory = ArtifactInventory::default();
    let interfaces = inventory
        .admit(
            &inventory.empty_view(),
            vec![
                ArtifactEntry::canonical(product.module_interface().unwrap().clone()),
                ArtifactEntry::canonical(unchanged.module_interface().unwrap().clone()),
            ],
        )
        .unwrap();
    let baseline = Arc::new(
        ExactDeclarationContext::from_authenticated_interfaces(
            [2; 32],
            &interfaces,
            CompilerInputProjection::from_issued_entries(&[
                Arc::new(ArtifactEntry::canonical(
                    product.module_interface().unwrap().clone(),
                )),
                Arc::new(ArtifactEntry::canonical(
                    unchanged.module_interface().unwrap().clone(),
                )),
            ])
            .unwrap(),
        )
        .unwrap(),
    );
    let context = Arc::new(
        baseline
            .as_ref()
            .clone()
            .extend_checked_original_products([2; 32], &[product])
            .unwrap(),
    );
    assert_eq!(
        context.artifact_view().artifact_ids().len(),
        baseline.artifact_view().artifact_ids().len() + 1,
        "promotion retains the canonical carrier and adds its original product"
    );
    let directory = tempfile::tempdir().unwrap();
    let request = ExactCompilationRequest {
        artifacts: baseline.materialize_scratch(&directory).unwrap().artifacts,
        inputs: ExactRequestInputs::for_context(baseline),
        manifest: directory.path().join("scope"),
        request_sha256: String::new(),
        semantic_sha256: [1; 32],
        producer_sha256: [2; 32],
        groups: Arc::from([]),
        materialization: None,
        program_support: None,
        program_source_lexical: Vec::new(),
        source_selected_support: BTreeSet::new(),
        source_search_include: None,
        checked_value_imports: Default::default(),
        generated_scaffold_imports: Vec::new(),
    };
    let support = request
        .artifacts
        .iter()
        .position(|artifact| artifact.interface.module == "Support")
        .unwrap();
    let old_path = &request.artifacts[support].interface.path;
    let old_bytes = std::fs::read(old_path).unwrap();
    let root = directory.path().join("program-inputs");
    std::fs::write(old_path, b"tampered").unwrap();
    assert!(request.in_program_context(&root, context.clone()).is_err());
    assert!(
        !root.exists(),
        "displaced consumed bytes are checked before writing"
    );
    std::fs::write(old_path, old_bytes).unwrap();
    let mut duplicate = request.clone();
    duplicate.artifacts.push(request.artifacts[support].clone());
    assert!(duplicate
        .in_program_context(&root, context.clone())
        .is_err());
    let mut relative = request.clone();
    relative.artifacts[support].interface.path = PathBuf::from("relative.hi");
    assert!(relative.in_program_context(&root, context.clone()).is_err());

    let effective = request.in_program_context(&root, context.clone()).unwrap();
    assert_eq!(effective.artifacts.len(), 2);
    let promoted = effective
        .artifacts
        .iter()
        .find(|artifact| artifact.interface.module == "Support")
        .unwrap();
    assert!(promoted.product.is_some());
    assert_ne!(&promoted.interface.path, old_path);
    let unchanged_path = |artifacts: &[DeclarationArtifact]| {
        artifacts
            .iter()
            .find(|artifact| artifact.interface.module == "Unchanged")
            .unwrap()
            .interface
            .path
            .clone()
    };
    assert_eq!(
        unchanged_path(&effective.artifacts),
        unchanged_path(&request.artifacts)
    );
    context.validate_artifacts(&effective.artifacts).unwrap();
    let recomputed_directory = tempfile::tempdir().unwrap();
    let recomputed = context.materialize_scratch(&recomputed_directory).unwrap();
    let selections = |artifacts: &[DeclarationArtifact]| {
        artifacts
            .iter()
            .map(|artifact| {
                (
                    identity(&artifact.interface.unit, &artifact.interface.module),
                    (
                        artifact.interface.sha256.clone(),
                        artifact.interface.requirements.clone(),
                        artifact
                            .product
                            .as_ref()
                            .map(|product| product.sha256.clone()),
                    ),
                )
            })
            .collect::<BTreeMap<_, _>>()
    };
    assert_eq!(
        selections(&effective.artifacts),
        selections(&recomputed.artifacts)
    );
    let repeated_root = directory.path().join("unchanged-inputs");
    let repeated = effective
        .in_program_context(&repeated_root, context.clone())
        .unwrap();
    assert!(!repeated_root.exists());
    assert_eq!(repeated.artifacts.len(), 2);
    context.validate_artifacts(&repeated.artifacts).unwrap();
    std::fs::write(&promoted.product.as_ref().unwrap().path, b"tampered").unwrap();
    assert!(context.validate_artifacts(&repeated.artifacts).is_err());
}

#[test]
fn unchanged_program_context_reuses_materialization_but_admission_checks_tampering() {
    let product = support_product_with_interface("fixture", "Support", b"interface".to_vec());
    let product_sha256 = hex(&product.owner().product_sha256);
    let context = Arc::new(
        ExactDeclarationContext::new(&[], &[], Vec::new())
            .unwrap()
            .extend_checked_original_products([2; 32], &[product])
            .unwrap(),
    );
    let directory = tempfile::tempdir().unwrap();
    let artifacts = vec![DeclarationArtifact {
        interface: ExactIfaceArtifact {
            unit: "fixture".into(),
            module: "Support".into(),
            path: directory.path().join("missing.hi"),
            sha256: sha256(b"interface"),
            requirements: Vec::new(),
        },
        product: Some(ModuleSnapshot {
            module: "Support".into(),
            path: directory.path().join("missing.bin"),
            sha256: product_sha256,
        }),
    }];
    let request = ExactCompilationRequest {
        inputs: ExactRequestInputs::for_context(context.clone()),
        manifest: directory.path().join("scope"),
        request_sha256: String::new(),
        semantic_sha256: [1; 32],
        producer_sha256: [2; 32],
        artifacts,
        groups: Arc::from([]),
        materialization: None,
        program_support: None,
        program_source_lexical: Vec::new(),
        source_selected_support: BTreeSet::new(),
        source_search_include: None,
        checked_value_imports: Default::default(),
        generated_scaffold_imports: Vec::new(),
    };
    let materialization_root = directory.path().join("program-inputs");
    let effective = request
        .in_program_context(&materialization_root, context)
        .unwrap();
    assert!(!materialization_root.exists());
    assert_eq!(
        effective.artifacts[0].interface.path,
        request.artifacts[0].interface.path
    );
    assert!(Arc::ptr_eq(&effective.groups, &request.groups));
    assert!(effective
        .context()
        .validate_artifacts(&effective.artifacts)
        .is_err());
}

#[test]
fn supporting_originals_preserve_owned_products_without_lexical_exposure() {
    let product = support_product_with_interface("fixture", "Support", b"interface".to_vec());
    let owner = product.owner().clone();
    let certification = product.certification_bytes().to_vec();
    let context = ExactDeclarationContext::new(&[], &[], Vec::new())
        .unwrap()
        .extend_checked_original_products([2; 32], std::slice::from_ref(&product))
        .unwrap();
    assert!(context.lexical_graph().is_empty());
    assert_eq!(context.artifact_view().descriptors().len(), 2);
    let repeated = context
        .clone()
        .extend_checked_original_products([2; 32], std::slice::from_ref(&product))
        .unwrap();
    assert_eq!(context.semantic_sha256(), repeated.semantic_sha256());
    assert_eq!(repeated.artifact_view().inventory().node_count(), 2);
    let changed = CertifiedRecoveryProduct::from_certification(
        owner,
        b"interface".to_vec(),
        b"changed".to_vec(),
        Vec::new(),
        certification,
    );
    assert!(context
        .clone()
        .extend_checked_original_products([2; 32], &[changed])
        .is_err());
    assert!(context
        .extend_checked_original_products([99; 32], &[product])
        .is_err());
}

fn execution_entry(
    original: crate::execution_source::PreparedExecutionSourceOriginal,
    graph: Arc<crate::execution_source::CertifiedExecutionSourceGraph>,
) -> Arc<ArtifactEntry> {
    let owner = original.owner;
    let mut validation = PackageInterfaceValidation::default();
    let seal = crate::certified_products::encode_home_certification(&owner, &[], &BTreeMap::new())
        .unwrap();
    let seal = crate::certified_products::bind_home_execution_source(
        &seal,
        &owner,
        graph.digest(),
        &mut validation,
    )
    .unwrap();
    let mut packages = Vec::new();
    ciborium::ser::into_writer(
        &Value::Array(vec![
            text("TPPKGROOTS"),
            text("2"),
            Value::Array(vec![
                text(&owner.unit),
                text(&owner.module),
                text(hex(&owner.skinny_iface_sha256)),
            ]),
            Value::Array(vec![]),
            Value::Array(vec![]),
        ]),
        &mut packages,
    )
    .unwrap();
    let product = crate::certified_products::fixture_finalized_product(
        CertifiedRecoveryProduct::from_certification(
            owner,
            original.interface,
            original.product,
            packages,
            seal,
        ),
        [7; 32],
    )
    .with_execution_source_with_validation(graph, &mut validation)
    .unwrap();
    Arc::new(ArtifactEntry::original([7; 32], product).unwrap())
}

#[test]
fn execution_support_keeps_carried_partial_availability_and_instance_origin() {
    use crate::artifact_inventory::{IssuedNativeOffer, NativeOriginalRequirement};
    let producer = [2; 32];
    let product = crate::certified_products::fixture_finalized_product(
        crate::certified_products::tests::original_groups_fixture_with_interface(
            "ExecutionSupport",
            vec![(7, vec![]), (9001, vec![])],
            122,
            &BTreeMap::new(),
            b"execution support".to_vec(),
        ),
        producer,
    );
    let product = crate::certified_products::tests::recovered_witness_fixtures(&[product])
        .remove(0)
        .product;
    let entry = Arc::new(ArtifactEntry::original(producer, product).unwrap());
    let inventory = ArtifactInventory::default();
    let artifacts = inventory
        .admit_shared(&inventory.empty_view(), vec![entry.clone()])
        .unwrap();
    let projection = CompilerInputProjection::from_issued_entries(&[entry.clone()]).unwrap();
    let proof = NativeOriginalRequirement::from_selected_view(entry.descriptor.id, &artifacts)
        .unwrap()
        .unwrap();
    let full =
        IssuedNativeOffer::issue(entry.descriptor.id, &artifacts, Some(proof.clone())).unwrap();
    let partial = full
        .within_projection(&projection, &inventory.empty_view())
        .unwrap()
        .unwrap();
    assert!(!partial.is_full());
    assert!(proof.qualify_availability(&artifacts).unwrap().is_some());
    let target = entry.descriptor.owner.clone();
    let support =
        recover_fixture_compiler_support(projection, artifacts, [partial.clone()]).unwrap();
    let context = ExactDeclarationContext::from_authenticated_execution_support(
        producer,
        support.clone(),
        vec![ExactLexicalNode {
            owner: target.clone(),
            imports: vec![],
        }],
        target.clone(),
        &[],
    )
    .unwrap();
    assert_eq!(
        context.compiler_source_support().native_offers(),
        &[partial]
    );
    assert!(!context.compiler_source_support().native_offers()[0].is_full());
    assert_eq!(context.original_instance_target().unwrap(), &target);
    assert_eq!(
        context.artifact_view().capture_graph_selection(),
        support.artifacts().capture_graph_selection()
    );
    assert!(
        ExactDeclarationContext::from_authenticated_execution_support(
            [99; 32],
            support,
            vec![],
            target,
            &[],
        )
        .is_err(),
        "carried support cannot change its producer"
    );
}

#[test]
fn fresh_source_owner_distinguishes_compiler_roles_from_retained_custody() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = support_offer(&[support_product("Consumer")]);
    let hidden_support = recover_fixture_compiler_support(
        CompilerInputProjection::default(),
        fixture.artifacts.clone(),
        [],
    )
    .unwrap();
    let hidden = Arc::new(
        ExactDeclarationContext::new(&[], &[], vec![])
            .unwrap()
            .extend_issued_compiler_support(&hidden_support)
            .unwrap(),
    );
    assert!(hidden
        .interface_owners()
        .iter()
        .any(|interface| { interface.owner == identity("fixture", "Consumer") }));
    assert!(hidden
        .compiler_metadata_snapshot()
        .unwrap()
        .entries
        .is_empty());
    let request = program_request(directory.path(), hidden.clone());
    let receipt = import_receipt(directory.path(), &request, "Unadmitted");
    let mut value = read_receipt(&receipt);
    value.as_array_mut().unwrap()[8].as_array_mut().unwrap()[0]
        .as_array_mut()
        .unwrap()[3] = Value::Array(vec![]);
    write_receipt(&receipt, &value);
    request.validate_receipt(&receipt, None, &hidden).unwrap();

    let selected_support =
        recover_fixture_compiler_support(fixture.projection, fixture.artifacts, []).unwrap();
    let selected = hidden
        .as_ref()
        .clone()
        .extend_issued_compiler_support(&selected_support)
        .unwrap();
    for (context, planned) in [
        (&selected, None),
        (hidden.as_ref(), Some(identity("fixture", "Consumer"))),
    ] {
        assert!(request
            .validate_receipt(&receipt, planned.as_ref(), context)
            .err()
            .expect("selected or planned source owner must refuse replacement")
            .to_string()
            .contains("fresh module replaced an admitted exact owner"));
    }
}

#[test]
fn exact_product_binding_preserves_lexical_evidence_and_request_identity() {
    use crate::artifact_inventory::CanonicalProducerIdentity;
    use tidepool_repr::execution_schema::{InventoryDecodeLimits, InventoryOperation};

    let directory = tempfile::tempdir().unwrap();
    let first_context = ExactDeclarationContext::new(&[], &[], vec![])
        .unwrap()
        .admit_producer(CanonicalProducerIdentity::from_producer_bytes(b"first producer").sha256())
        .unwrap();
    let second_context = ExactDeclarationContext::new(&[], &[], vec![])
        .unwrap()
        .admit_producer(CanonicalProducerIdentity::from_producer_bytes(b"second producer").sha256())
        .unwrap();
    let roots = [
        directory.path().join("first"),
        directory.path().join("second"),
    ];
    let first_request = ExactCompileContext::new(Arc::new(first_context))
        .prepare_compilation(&directory.path().join("first-request"), b"first producer")
        .unwrap();
    let second_request = ExactCompileContext::new(Arc::new(second_context))
        .prepare_compilation(&directory.path().join("second-request"), b"second producer")
        .unwrap();
    let (first, source) = generated_source_admission_fixture(&roots[0], &first_request);
    let (second, second_source) = generated_source_admission_fixture(&roots[1], &second_request);
    assert_eq!(source, second_source);
    let input = roots[0].join("Consumer.hs");
    let second_input = roots[1].join("Consumer.hs");
    let mut evidence: crate::cache::DependencyEvidence =
        serde_json::from_slice(first.evidence_bytes()).unwrap();
    evidence.cache_safe = false;
    evidence.selection_complete = false;
    let bytes = serde_json::to_vec(&evidence).unwrap();
    let operation = InventoryOperation::new(Default::default());
    first
        .match_product(&first_request, &input, &source, &bytes, &operation)
        .unwrap();
    assert_eq!(operation.work_usage().unwrap().0, bytes.len() * 64);
    let mut second_evidence: crate::cache::DependencyEvidence =
        serde_json::from_slice(second.evidence_bytes()).unwrap();
    second_evidence.cache_safe = false;
    second_evidence.selection_complete = false;
    let second_bytes = serde_json::to_vec(&second_evidence).unwrap();
    second
        .match_product(
            &second_request,
            &second_input,
            &second_source,
            &second_bytes,
            &operation,
        )
        .unwrap();
    assert!(first
        .match_product(&second_request, &input, &source, &bytes, &operation)
        .is_err());
    assert!(first
        .match_product(&first_request, &second_input, &source, &bytes, &operation)
        .is_err());
    assert!(first
        .match_product(
            &first_request,
            &second_input,
            &source,
            &second_bytes,
            &operation,
        )
        .is_err());
    assert!(first
        .match_product(&first_request, &input, &source, &second_bytes, &operation)
        .is_err());

    // JSON formatting is not source identity; lexical path spelling is.
    let reordered: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let pretty = serde_json::to_vec_pretty(&reordered).unwrap();
    assert_ne!(pretty, bytes);
    first
        .match_product(&first_request, &input, &source, &pretty, &operation)
        .unwrap();
    for (cache_safe, selection_complete) in [(true, false), (false, true), (true, true)] {
        let mut changed = evidence.clone();
        changed.cache_safe = cache_safe;
        changed.selection_complete = selection_complete;
        let changed = serde_json::to_vec(&changed).unwrap();
        assert!(first
            .match_product(&first_request, &input, &source, &changed, &operation)
            .is_err());
    }
    for alias in [
        roots[0].join(".").join("Consumer.hs"),
        PathBuf::from(format!("{}//Consumer.hs", roots[0].display())),
    ] {
        assert_eq!(alias, input);
        assert_eq!(
            std::fs::canonicalize(&alias).unwrap(),
            std::fs::canonicalize(&input).unwrap()
        );
        let mut changed = evidence.clone();
        changed.sources[0].path = alias.clone();
        changed.modules[0].source = alias;
        let changed = serde_json::to_vec(&changed).unwrap();
        assert_ne!(changed, bytes);
        let refusal = first
            .match_product(&first_request, &input, &source, &changed, &operation)
            .err()
            .expect("lexically different evidence must not match the issued receipt");
        assert!(
            matches!(&refusal, CompileError::ExtractFailed(message)
                if message == "exact declaration context: source-cache-ineligible evidence differs from exact fresh proof"),
            "{refusal:?}"
        );
    }
    let restricted = InventoryOperation::new(InventoryDecodeLimits {
        max_work: bytes.len() * 64 - 1,
        ..Default::default()
    });
    assert!(matches!(
        first.match_product(&first_request, &input, &source, &bytes, &restricted),
        Err(CompileError::CompilerEvidence(_))
    ));
    assert_eq!(restricted.work_usage().unwrap().0, 0);
}

#[test]
fn program_segment_preserves_physical_source_authority_across_item_outputs() {
    let directory = tempfile::tempdir().unwrap();
    let inventory = tidepool_repr::execution_schema::InventoryOperation::new(Default::default());
    let baseline = Arc::new(ExactDeclarationContext::new(&[], &[], vec![]).unwrap());
    let mut request = program_request(directory.path(), baseline.clone());
    let source = "module Consumer where\n";
    let source_path = directory.path().join("Consumer.hs");
    let fixture = import_receipt(directory.path(), &request, "Unadmitted");
    let mut receipt = read_receipt(&fixture);
    receipt.as_array_mut().unwrap()[8].as_array_mut().unwrap()[0]
        .as_array_mut()
        .unwrap()[3] = Value::Array(vec![]);
    let output = directory.path().join("outputs");
    for name in ["first", "second"] {
        let root = output.join(".exact-compilations").join(name);
        std::fs::create_dir_all(&root).unwrap();
        let snapshot = root.join("source.hs");
        std::fs::write(&snapshot, source).unwrap();
        let mut projected = receipt.clone();
        projected.as_array_mut().unwrap()[6] = path_value(&snapshot).unwrap();
        write_receipt(&root.join("receipt.cbor"), &projected);
    }
    let segment = request.admit_program_segment(&output).unwrap();
    assert_eq!(segment.admissions().len(), 2);
    let mut evidence: crate::cache::DependencyEvidence =
        serde_json::from_str(receipt.as_array().unwrap()[7].as_text().unwrap()).unwrap();
    evidence.cache_safe = false;
    evidence.selection_complete = false;
    let evidence = serde_json::to_vec(&evidence).unwrap();
    let support = support_offer(&[support_product("Consumer")]);
    let context = request
        .admit_fixture_segment_support(baseline, &support, &segment, None)
        .unwrap();
    assert!(context
        .interface_owners()
        .iter()
        .any(|interface| interface.owner == identity("fixture", "Consumer")));

    // The subsequent item uses the evolved output view, while its source
    // authority still comes from the same successful physical request.
    let mut projected = request
        .in_program_context(&directory.path().join("program-inputs"), context.clone())
        .unwrap();
    projected
        .context()
        .validate_artifacts(&projected.artifacts)
        .unwrap();
    let admission = segment
        .product_admission(&projected, &source_path, source, &evidence, &inventory)
        .unwrap();
    assert!(admission
        .exact
        .request
        .context()
        .interface_owners()
        .is_empty());
    let repeated = projected
        .admit_fixture_segment_support(context.clone(), &support, &segment, None)
        .unwrap();
    assert_eq!(repeated.semantic_sha256(), context.semantic_sha256());
    let ordinary_refusal = projected
        .admit_program_segment(&output)
        .err()
        .expect("a new source receipt cannot replace an admitted owner");
    let assert_context_refusal = |actual: &CompileError, reason: &str| {
        let expected = failure(reason);
        assert!(
            matches!((actual, &expected),
                (CompileError::ExtractFailed(actual), CompileError::ExtractFailed(expected))
                    if actual == expected),
            "{actual:?}"
        );
    };
    assert_context_refusal(
        &ordinary_refusal,
        "fresh module replaced an admitted exact owner",
    );
    let hidden_refusal = projected
        .admit_fixture_support(context.clone(), &support, segment.admissions(), None)
        .unwrap_err();
    assert_context_refusal(
        &hidden_refusal,
        "program support cannot select a retained hidden owner",
    );

    let before = projected.context().semantic_sha256();
    let changed = support_offer(&[support_product_with_interface(
        "fixture",
        "Consumer",
        b"changed canonical interface".to_vec(),
    )]);
    let changed_refusal = projected
        .admit_fixture_segment_support(context, &changed, &segment, None)
        .unwrap_err();
    assert_context_refusal(
        &changed_refusal,
        "projected segment source changed its admitted canonical owner",
    );
    assert_eq!(projected.context().semantic_sha256(), before);
    assert!(segment
        .product_admission(
            &projected,
            &source_path,
            "module Changed where\n",
            &evidence,
            &inventory
        )
        .is_err());
    assert!(segment
        .product_admission(&projected, &source_path, source, b"{}", &inventory)
        .is_err());
    for changed in 0..4 {
        let mut altered: crate::cache::DependencyEvidence =
            serde_json::from_slice(&evidence).unwrap();
        match changed {
            0 => altered.modules[0].unit = "foreign".into(),
            1 => altered.modules[0].module = "Different".into(),
            2 => altered.modules[0].product = crate::cache::ProductAvailability::InterfaceOnly,
            _ => altered.sources[0].sha256 = sha256(b"different source"),
        }
        let refusal = segment
            .product_admission(
                &projected,
                &source_path,
                source,
                &serde_json::to_vec(&altered).unwrap(),
                &inventory,
            )
            .err()
            .expect("matching source must retain its exact final evidence");
        assert_context_refusal(
            &refusal,
            "source-cache-ineligible evidence differs from exact fresh proof",
        );
    }
    for changed in 0..3 {
        let mut foreign = projected.clone();
        match changed {
            0 => foreign.request_sha256 = sha256(b"another request"),
            1 => foreign.semantic_sha256 = [9; 32],
            _ => foreign.producer_sha256 = [9; 32],
        }
        assert!(segment
            .product_admission(&foreign, &source_path, source, &evidence, &inventory)
            .is_err());
    }
    assert_eq!(segment.admissions().len(), 2);
}

#[test]
fn generated_source_owner_accepts_separate_projection_receipts_and_checks_every_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let context = Arc::new(ExactDeclarationContext::new(&[], &[], vec![]).unwrap());
    let request = program_request(directory.path(), context);
    let source = "module Consumer where\n";
    let fixture = import_receipt(directory.path(), &request, "Unadmitted");
    let mut receipt = read_receipt(&fixture);
    receipt.as_array_mut().unwrap()[8].as_array_mut().unwrap()[0]
        .as_array_mut()
        .unwrap()[3] = Value::Array(vec![]);
    let output = directory.path().join("outputs");
    let receipts = ["first", "second"].map(|name| {
        let root = output.join(".exact-compilations").join(name);
        std::fs::create_dir_all(&root).unwrap();
        let snapshot = root.join("source.hs");
        std::fs::write(&snapshot, source).unwrap();
        let mut projected = receipt.clone();
        projected.as_array_mut().unwrap()[6] = path_value(&snapshot).unwrap();
        let path = root.join("receipt.cbor");
        write_receipt(&path, &projected);
        path
    });
    let expected = identity("fixture", "Consumer");
    let admissions = request.validate_outputs(&output).unwrap();
    assert_eq!(admissions.len(), 2);
    assert_eq!(
        ExactSourceAdmission::matching_generated_source_owner(&admissions, source).unwrap(),
        expected
    );
    assert!(ExactSourceAdmission::matching_generated_source_owner(&[], source).is_err());
    assert!(ExactSourceAdmission::matching_generated_source_owner(
        &admissions,
        "module Changed where\n"
    )
    .is_err());

    let snapshot = receipts[1].parent().unwrap().join("source.hs");
    std::fs::write(&snapshot, b"changed after projection").unwrap();
    assert!(request.validate_outputs(&output).is_err());
    std::fs::write(&snapshot, source).unwrap();
    let valid = read_receipt(&receipts[1]);
    let mut changed_hash = valid.clone();
    changed_hash.as_array_mut().unwrap()[5] = text(sha256(b"different source"));
    write_receipt(&receipts[1], &changed_hash);
    assert!(request.validate_outputs(&output).is_err());

    let mut malformed = valid.clone();
    let fields = malformed.as_array_mut().unwrap();
    let mut evidence: crate::cache::DependencyEvidence =
        serde_json::from_str(fields[7].as_text().unwrap()).unwrap();
    evidence.modules[0].unit.clear();
    fields[7] = text(serde_json::to_string(&evidence).unwrap());
    write_receipt(&receipts[1], &malformed);
    assert!(request.validate_outputs(&output).is_err());

    let mut distinct = valid.clone();
    let fields = distinct.as_array_mut().unwrap();
    let mut evidence: crate::cache::DependencyEvidence =
        serde_json::from_str(fields[7].as_text().unwrap()).unwrap();
    evidence.modules[0].unit = "other".into();
    fields[7] = text(serde_json::to_string(&evidence).unwrap());
    fields[8].as_array_mut().unwrap()[0].as_array_mut().unwrap()[0] = text("other");
    write_receipt(&receipts[1], &distinct);
    let distinct_admissions = request.validate_outputs(&output).unwrap();
    assert_eq!(distinct_admissions.len(), 2);
    assert!(
        ExactSourceAdmission::matching_generated_source_owner(&distinct_admissions, source)
            .is_err()
    );

    write_receipt(&receipts[1], &valid);
    let restored = request.validate_outputs(&output).unwrap();
    assert_eq!(restored.len(), 2);
    assert_eq!(
        ExactSourceAdmission::matching_generated_source_owner(&restored, source).unwrap(),
        expected
    );
}

#[test]
fn package_resume_does_not_select_home_native_variants_or_grant_home_imports() {
    let directory = tempfile::tempdir().unwrap();
    let producer = b"package Resume input";
    let producer_sha256 = Sha256::digest(producer).into();
    let first = scaffold_native_fixture(producer_sha256, "main", "Tidepool.Internal.Resume", 1);
    let second = scaffold_native_fixture(producer_sha256, "main", "Tidepool.Internal.Resume", 2);
    assert_eq!(first.module_interface(), second.module_interface());
    let mut public = ExactDeclarationContext::new(&[], &[], vec![])
        .unwrap()
        .extend_checked_original_products(producer_sha256, &[first])
        .unwrap();
    {
        let updated = public
            .owner
            .facts
            .compiler_support
            .artifacts()
            .retain_custody(&support_view(&[second]))
            .unwrap();
        let support = public
            .compiler_source_support()
            .retain_custody(&updated)
            .unwrap();
        public = public.rebuild(|facts| facts.compiler_support = support);
    }
    let projection = public.compiler_input_projection().interface_only();
    let public = Arc::new(public.recover_fixture_projection(projection).unwrap());
    let source = "{-# LANGUAGE PackageImports #-}\nmodule Consumer where\nimport qualified \"tidepool-resume\" Tidepool.Internal.Resume as TidepoolResume\n";
    assert!(!template_selects_owner(
        &[source.to_owned()],
        &identity("main", "Tidepool.Internal.Resume")
    ));
    let request = ExactCompileContext::new(public.clone())
        .prepare_compilation(&directory.path().join("scope"), producer)
        .unwrap();
    assert_eq!(request.semantic_sha256, public.semantic_sha256());
    assert!(request.compiler_original_products().unwrap().is_empty());
    assert!(request.generated_scaffold_imports.is_empty());
    assert!(public.lexical_graph().is_empty());
    let manifest: Value =
        ciborium::de::from_reader(std::fs::read(&request.manifest).unwrap().as_slice()).unwrap();
    assert!(manifest.as_array().unwrap()[6]
        .as_array()
        .unwrap()
        .is_empty());
    // A forged home resolution of the package import has no request-local
    // permission, even when matching home products remain in custody.
    let receipt = import_receipt_source_owner(
        directory.path(),
        &request,
        "main",
        "Tidepool.Internal.Resume",
        "none",
        false,
        source,
    );
    assert!(request.validate_receipt(&receipt, None, &public).is_err());
}

pub(crate) fn assert_source_selected_receipt_pairing(
    root: &Path,
    module: tidepool_repr::SessionModule,
    original: &str,
    support: &str,
    request: ExactCompilationRequest,
    endpoint: tidepool_extract_cmd::CompilerEndpoint,
    offer: crate::artifacts::ModuleCandidateOffer,
) {
    let original_path = root.join(module.relative_hs_path());
    let support_path = root.join("PrefixSelectedSupport.hs");
    let includes = vec![root.to_path_buf()];
    let owner = identity("main", "PrefixSelectedSupport");
    let persisted = Arc::clone(request.context());
    let empty = ExactDeclarationContext::new(&[], &[], vec![]).unwrap();
    let output = root.join("consumer-output");
    std::fs::create_dir(&output).unwrap();
    let inspection_module = |imports: bool| {
        let target = "result";
        let import = if imports {
            "import PrefixSelectedSupport\n"
        } else {
            ""
        };
        let expression = if imports { "selected" } else { "(0 :: Int)" };
        (format!("module Consumer where\n{import}{target} :: Int\n{target} = {expression}\n__tidepool_inspect_0 = {target}\n"), target)
    };
    let source_path = output.join("Consumer.hs");
    let (inspection_source, inspection_target) = inspection_module(true);
    std::fs::write(&source_path, inspection_source).unwrap();
    let inspection_output = output.join("inspection.cbor");
    let mut command = tidepool_extract_cmd::ExtractCmd::new().unwrap();
    command
        .input(&source_path)
        .includes(&includes)
        .inspect_type(inspection_target)
        .inspect_out(&inspection_output)
        .inspection_strict()
        .output_dir(&output);
    let offer = offer.apply_to(command, &output).unwrap();
    let command = offer.command();
    let request_bytes = command.request_bytes();
    let decoded = tidepool_extract_cmd::ExtractRequest::decode(&request_bytes).unwrap();
    assert!(decoded.target_names().is_empty());
    assert!(decoded.retained_generations().is_empty());
    std::fs::write(root.join("worker-request.cbor"), request_bytes).unwrap();
    let compiler_identity = endpoint.identity().clone();
    // Inspect the actual issuing owner, not a second native-row decoder.
    let materialization = request
        .materialization
        .as_ref()
        .expect("live input materialization");
    let metadata = &request.inputs.compiler.native_transport.metadata;
    let rows = materialization.selected_rows(metadata);
    let mut supplied_kinds = BTreeSet::new();
    let mut aliases = BTreeSet::new();
    for entry in metadata.entries.values() {
        for part in rows[&entry.descriptor.id]
            .original_input_parts(entry)
            .unwrap()
        {
            supplied_kinds.insert(part.kind);
            aliases.insert(part.path);
        }
    }
    for graph in materialization.graph_path_refs().values() {
        supplied_kinds.insert(original_inputs::OriginalInputKind::Graph);
        aliases.insert(graph.path.clone());
    }
    assert_eq!(
        supplied_kinds,
        BTreeSet::from([
            original_inputs::OriginalInputKind::Interface,
            original_inputs::OriginalInputKind::Packages,
            original_inputs::OriginalInputKind::Certificate,
            original_inputs::OriginalInputKind::Core,
            original_inputs::OriginalInputKind::Native,
            original_inputs::OriginalInputKind::Census,
            original_inputs::OriginalInputKind::Graph,
        ]),
        "genuine executable input history must supply every part kind"
    );
    for (index, path) in aliases.iter().enumerate() {
        if index % 2 == 0 {
            std::fs::remove_file(path).unwrap();
        } else {
            std::fs::write(path, b"changed after owned capture").unwrap();
        }
    }
    let trace = PathBuf::from(
        std::env::var_os("TIDEPOOL_PERFORMANCE_COMPILER_TRACE")
            .expect("owned resident runner must retain its compiler trace"),
    );
    let trace_rows = || {
        let bytes = std::fs::read(&trace).expect("read owned compiler trace");
        // The append writer may have only published part of its last row.
        bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map(|end| {
                std::str::from_utf8(&bytes[..end])
                    .expect("invalid complete compiler trace text")
                    .lines()
                    .map(|line| {
                        serde_json::from_str::<serde_json::Value>(line)
                            .expect("malformed complete compiler trace row")
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    let observations = |cursor: usize, ordinal: u64| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let events = trace_rows();
            assert!(events.len() >= cursor, "owned compiler trace was truncated");
            let completed = events[cursor..]
                .iter()
                .filter(|event| {
                    event["fields"]["phase"] == "compiler_service"
                        && event["span"]["request_mode"] == "inspection"
                        && event["span"]["transaction"] == true
                })
                .collect::<Vec<_>>();
            assert!(
                completed.len() <= 1,
                "ambiguous completed physical inspection request"
            );
            if let Some(completed) = completed.first() {
                assert_eq!(completed["fields"]["exit_code"].as_i64(), Some(0));
                let span = &completed["span"];
                let epoch = span["daemon_epoch"]
                    .as_str()
                    .expect("physical daemon epoch");
                let admission = span["admission_id"].as_u64().expect("physical admission");
                let worker = span["worker_pid"].as_u64().expect("physical worker");
                let physical = span["physical_execution"]
                    .as_str()
                    .expect("physical request");
                assert!(!epoch.is_empty() && admission > 0 && worker > 0);
                assert_eq!(span["request_ordinal"].as_u64(), Some(ordinal));
                let rows = events[cursor..]
                    .iter()
                    .filter_map(|event| {
                        if event["span"]["physical_execution"].as_str() != Some(physical) {
                            return None;
                        }
                        let raw = event["fields"]["line"]
                            .as_str()?
                            .strip_prefix("tidepool-validation ")?;
                        let row: serde_json::Value =
                            serde_json::from_str(raw).expect("malformed validation summary");
                        if row["owner"] != "original_inputs" || row["reason"] != "scope_acquisition"
                        {
                            return None;
                        }
                        assert_eq!(event["span"]["daemon_epoch"].as_str(), Some(epoch));
                        assert_eq!(event["span"]["admission_id"].as_u64(), Some(admission));
                        assert_eq!(event["span"]["request_ordinal"].as_u64(), Some(ordinal));
                        assert_eq!(event["span"]["worker_pid"].as_u64(), Some(worker));
                        assert_eq!(row["worker_pid"].as_u64(), Some(worker));
                        Some(row)
                    })
                    .collect::<Vec<_>>();
                assert!(rows.len() <= 1, "ambiguous scope acquisition summary");
                if !rows.is_empty() {
                    for row in &rows {
                        assert_eq!(
                            row["outcome"], "success",
                            "scope acquisition did not complete"
                        );
                        for kind in ["certificate", "packages", "census", "graph"] {
                            for suffix in ["hits", "misses"] {
                                row["counts"][format!("original_inputs.{kind}.{suffix}")]
                                    .as_u64()
                                    .expect("required measured counter is absent");
                            }
                        }
                    }
                    return (rows, (epoch.to_owned(), admission, worker));
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "completed physical request or its scope acquisition summary was not observed"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    };
    let first_cursor = trace_rows().len();
    let history = tidepool_extract_cmd::with_compiler_transaction(
        |_| {},
        || {
            let run = crate::CompileError::compiler_invocation_result(
                endpoint.execute_with_input_files(&command, offer.input_transport_files(), |_| {}),
            )
            .unwrap();
            std::fs::write(root.join("consumer.stdout"), &run.output.stdout).unwrap();
            std::fs::write(root.join("consumer.stderr"), &run.output.stderr).unwrap();
            crate::diag::decode_extract_result(
                run.success(),
                &run.output.stdout,
                &run.output.stderr,
            )
            .expect("actual current-source inspection compiles");
            assert!(std::fs::metadata(&inspection_output).unwrap().len() > 0);
            let persisted_admissions = request.validate_outputs(&output).unwrap();
            assert_eq!(persisted_admissions.len(), 1);
            assert!(persisted_admissions[0]
                .selected_originals
                .contains_key(&owner));
            assert_ne!(
                compiler_identity.as_bytes(),
                compiler_identity.producer_bytes(),
                "owned input reuse requires the declared resident compiler"
            );
            let (first_observations, physical_owner) = observations(first_cursor, 1);
            let worker_pid = physical_owner.2;
            let count = |rows: &[serde_json::Value], wanted: &str| -> u64 {
                rows.iter()
                    .map(|row| {
                        assert_eq!(
                            row["worker_pid"].as_u64(),
                            Some(worker_pid),
                            "history rotated physical worker"
                        );
                        row["counts"][wanted]
                            .as_u64()
                            .expect("required measured counter is absent")
                    })
                    .sum()
            };
            assert!(
                count(&first_observations, "original_inputs.certificate.misses") > 0,
                "real history needs an observed cold admitted certificate read"
            );
            assert!(count(&first_observations, "original_inputs.census.misses") > 0);
            assert!(count(&first_observations, "original_inputs.graph.misses") > 0);
            // The compiler universe owns its retained input reader across A/B/A.
            // Each request and its backing files remain live until actual close.
            for (label, context, imports, ordinal) in [
                ("empty", Arc::new(empty.clone()), false, 2),
                ("again", persisted.clone(), true, 3),
            ] {
                let next_output = root.join(format!("consumer-{label}"));
                std::fs::create_dir(&next_output).unwrap();
                let next_source = next_output.join("Consumer.hs");
                let (inspection_source, inspection_target) = inspection_module(imports);
                std::fs::write(&next_source, inspection_source).unwrap();
                let (next_endpoint, _) = crate::toolchain::bind_extract_endpoint().unwrap();
                let next_offer = crate::artifacts::ModuleCandidateOffer::select_inspection(
                    compiler_identity.producer_bytes(),
                    &includes,
                    &root.join(format!("scope-{label}")),
                    Some(Arc::new(ExactCompileContext::new(context.clone()))),
                    Vec::new(),
                    &[],
                )
                .unwrap();
                let mut command = tidepool_extract_cmd::ExtractCmd::new().unwrap();
                command
                    .input(&next_source)
                    .includes(&includes)
                    .inspect_type(inspection_target)
                    .inspect_out(next_output.join("inspection.cbor"))
                    .inspection_strict()
                    .output_dir(&next_output);
                let applied = next_offer.apply_to(command, &next_output).unwrap();
                let next_cursor = trace_rows().len();
                let next_run = crate::CompileError::compiler_invocation_result(
                    next_endpoint.execute_with_input_files(
                        applied.command(),
                        applied.input_transport_files(),
                        |_| {},
                    ),
                )
                .unwrap();
                std::fs::write(
                    root.join(format!("consumer-{label}.stdout")),
                    &next_run.output.stdout,
                )
                .unwrap();
                std::fs::write(
                    root.join(format!("consumer-{label}.stderr")),
                    &next_run.output.stderr,
                )
                .unwrap();
                let (next_observations, next_physical_owner) = observations(next_cursor, ordinal);
                assert_eq!(
                    next_physical_owner, physical_owner,
                    "history changed physical transaction or worker"
                );
                for row in &next_observations {
                    assert_eq!(row["worker_pid"].as_u64(), Some(worker_pid));
                }
                crate::diag::decode_extract_result(
                    next_run.success(),
                    &next_run.output.stdout,
                    &next_run.output.stderr,
                )
                .expect("actual resident input history compiles");
                let admissions = applied.admit_exact_outputs(&next_output).unwrap();
                assert_eq!(admissions.len(), 1);
                if imports {
                    assert!(admissions[0].selected_originals.contains_key(&owner));
                    assert_eq!(context.semantic_sha256(), persisted.semantic_sha256());
                    assert_eq!(
                        count(&next_observations, "original_inputs.certificate.misses"),
                        0
                    );
                    for metric in [
                        "original_inputs.certificate.hits",
                        "original_inputs.packages.hits",
                        "original_inputs.census.hits",
                        "original_inputs.graph.hits",
                    ] {
                        assert!(
                            count(&next_observations, metric) > 0,
                            "real resident A/B/A did not reuse {metric}"
                        );
                    }
                }
            }
            persisted_admissions
        },
    );
    assert_eq!(
        history.close,
        tidepool_extract_cmd::CompilerTransactionClose::Clean
    );
    let persisted_admissions = history.action;
    let receipts = std::fs::read_dir(output.join(".exact-compilations"))
        .unwrap()
        .map(|entry| entry.unwrap().path().join("receipt.cbor"))
        .collect::<Vec<_>>();
    assert_eq!(receipts.len(), 1);
    let receipt = &receipts[0];
    assert!(
        request.validate_receipt(receipt, None, &empty).is_err(),
        "current worker claims cannot supply absent prior custody"
    );
    let mut local = request.clone();
    local.program_support = Some(ProgramSourceSupport {
        compiler_support: persisted.compiler_source_support().clone(),
        imports: Arc::new(BTreeMap::new()),
    });
    let local_admission = local.validate_receipt(receipt, None, &empty).unwrap();
    assert_eq!(
        local_admission.home_imports().unwrap(),
        persisted_admissions[0].home_imports().unwrap()
    );
    assert_eq!(
        local_admission.selected_originals.len(),
        persisted_admissions[0].selected_originals.len()
    );
    assert!(empty.artifact_view().is_empty());
    assert!(empty.lexical_graph().is_empty());
    let actual = read_receipt(receipt);
    let mut changed = actual.clone();
    changed.as_array_mut().unwrap()[9].as_array_mut().unwrap()[0]
        .as_array_mut()
        .unwrap()[0]
        .as_array_mut()
        .unwrap()[1] = text("UnadmittedOwner");
    write_receipt(receipt, &changed);
    assert!(local.validate_receipt(receipt, None, &empty).is_err());
    let mut changed = actual.clone();
    let selection = changed.as_array_mut().unwrap()[9].as_array_mut().unwrap();
    let mut evidence: crate::cache::DependencyEvidence =
        serde_json::from_str(selection[1].as_text().unwrap()).unwrap();
    evidence.modules[0]
        .imports
        .push(crate::cache::ModuleImportEvidence {
            qualifier: crate::cache::ImportQualifier::Unqualified,
            module: "UnadmittedOwner".into(),
            boot: false,
            selected: None,
        });
    selection[1] = text(serde_json::to_string(&evidence).unwrap());
    write_receipt(receipt, &changed);
    assert!(local.validate_receipt(receipt, None, &empty).is_err());
    write_receipt(receipt, &actual);
    std::fs::write(
        &support_path,
        support.replace("selected = 42", "selected = 43"),
    )
    .unwrap();
    assert!(local.validate_receipt(receipt, None, &empty).is_err());
    let conflicting_root = root.join("conflicting-issuer");
    std::fs::create_dir(&conflicting_root).unwrap();
    let changed = crate::artifacts::test_support::certify_authored_declaration(
        module,
        &original_path,
        original,
        &includes,
        &conflicting_root,
    )
    .expect("actual compiler issues the changed original proof");
    std::fs::write(&support_path, support).unwrap();
    let dependent_context =
        ExactDeclarationContext::new(&[Arc::new(changed.clone())], &[], vec![]).unwrap();
    let producer = CanonicalProducerIdentity::from_compiler(&compiler_identity);
    let configured_support = changed
        .recovery_products()
        .into_iter()
        .filter(|product| {
            product.owner().unit == owner.unit && product.owner().module == owner.module
        })
        .collect::<Vec<_>>();
    assert_eq!(configured_support.len(), 1);
    let wrong_support = persisted
        .recovery_products()
        .into_iter()
        .filter(|product| {
            product.owner().unit == owner.unit && product.owner().module == owner.module
        })
        .collect::<Vec<_>>();
    assert_eq!(wrong_support.len(), 1);
    assert_ne!(configured_support[0].owner(), wrong_support[0].owner());
    assert!(dependent_context
        .compiler_input_roles()
        .iter()
        .all(|role| role.original().is_none()));
    let support_entry = ArtifactEntry::original_with_validation(
        producer.sha256(),
        configured_support[0].clone(),
        &mut PackageInterfaceValidation::default(),
    )
    .unwrap();
    let issued_support = changed
        .compiler_source_support()
        .native_offers()
        .iter()
        .filter(|offer| offer.original() == support_entry.descriptor.id)
        .collect::<Vec<_>>();
    assert!(
        !issued_support.is_empty(),
        "changed compiler proof must issue its exact support availability"
    );
    let propagated_support = dependent_context
        .compiler_source_support()
        .native_offers()
        .iter()
        .filter(|offer| offer.original() == support_entry.descriptor.id)
        .collect::<Vec<_>>();
    assert_eq!(
        propagated_support, issued_support,
        "type-only support must retain its issuer's exact carrier, interface, availability class and selection"
    );
    // Opaque availability survives type-only roles independently of
    // executable group demand. A redundant private support offer is optional.
    OriginalCompilerInputs::from_native_availability(
        &dependent_context,
        producer,
        &configured_support,
    )
    .unwrap();
    let selected_private = OriginalCompilerInputs::from_selected_authored_declarations(
        &dependent_context,
        producer,
        &[],
    )
    .unwrap()
    .expect("complete selected authored original can reuse exact selected support");
    let selected_request = ExactCompileContext::new(Arc::new(dependent_context.clone()))
        .prepare_compilation_with_private_input(
            &root.join("selected-authored-scope"),
            compiler_identity.producer_bytes(),
            None,
            Some(selected_private),
        )
        .unwrap();
    assert_eq!(selected_request.context().as_ref(), &dependent_context);
    assert_eq!(
        selected_request
            .compiler_original_products()
            .unwrap()
            .iter()
            .map(|product| product.owner().clone())
            .collect::<Vec<_>>(),
        vec![changed.product().owner().clone()]
    );
    let inventory = dependent_context
        .owner
        .facts
        .compiler_support
        .artifacts()
        .inventory();
    let type_only = ExactDeclarationContext::from_facts(DeclarationContextFacts {
        compiler_support: recover_fixture_compiler_support(
            dependent_context
                .owner
                .facts
                .compiler_support
                .projection()
                .clone(),
            inventory
                .admit_recovery_selection(
                    &inventory.empty_view(),
                    dependent_context
                        .owner
                        .facts
                        .compiler_support
                        .artifacts()
                        .entries(),
                    &BTreeSet::new(),
                )
                .unwrap(),
            [],
        )
        .unwrap(),

        producer: dependent_context.owner.facts.producer,

        lexical: dependent_context.owner.facts.lexical.clone(),
        template_imports: dependent_context.owner.facts.template_imports.clone(),
        original_instance_environment: dependent_context
            .owner
            .facts
            .original_instance_environment
            .clone(),
    });
    assert!(type_only
        .owner
        .facts
        .compiler_support
        .artifacts()
        .metadata_snapshot()
        .selected_native_groups
        .is_empty());
    assert_eq!(
        type_only.compiler_input_roles(),
        dependent_context.compiler_input_roles()
    );
    let required_support_ordinals =
        crate::certified_products::original_available_groups(changed.product())
            .unwrap()
            .flat_map(|group| {
                crate::certified_products::original_native_group_imports(
                    changed.product(),
                    group.original_ordinal(),
                )
                .unwrap()
            })
            .filter_map(|import| match import {
                crate::certified_products::PendingImportOwner::Source {
                    owner: required_owner,
                    original_ordinal,
                    binder,
                } if required_owner == configured_support[0].owner()
                    && binder.occurrence == "selected" =>
                {
                    assert!(
                        crate::certified_products::authenticates_original_native_entry(
                            &configured_support[0],
                            *original_ordinal,
                            binder
                        )
                    );
                    Some(*original_ordinal)
                }
                _ => None,
            })
            .collect::<BTreeSet<_>>();
    assert_eq!(
        required_support_ordinals.len(),
        1,
        "changed fixture must require one exact selected support group"
    );
    let required_support_ordinal = *required_support_ordinals.first().unwrap();
    let missing_support = OriginalCompilerInputs::from_native_availability(
        &type_only,
        producer,
        std::slice::from_ref(changed.product()),
    )
    .unwrap_err();
    assert!(
        matches!(&missing_support, CompileError::ArtifactInventory(error)
        if matches!(&error.failure, crate::artifact_inventory::ArtifactInventoryFailure::NativeGroupUnavailable {
            artifact, original_ordinal,
        } if *artifact == support_entry.descriptor.id && *original_ordinal == required_support_ordinal)),
        "unexpected absent-offer refusal: {missing_support:#?}"
    );
    let conflicting_support = OriginalCompilerInputs::from_selected_authored_declarations(
        &dependent_context,
        producer,
        &wrong_support,
    )
    .unwrap_err();
    assert!(
        matches!(conflicting_support, CompileError::ExtractFailed(detail)
        if detail.contains("private native availability differs from selected canonical interface"))
    );
    let private = OriginalCompilerInputs::from_selected_authored_declarations(
        &dependent_context,
        producer,
        &configured_support,
    )
    .unwrap()
    .unwrap();
    let private_request = ExactCompileContext::new(Arc::new(dependent_context.clone()))
        .prepare_compilation_with_private_input(
            &root.join("configured-authored-scope"),
            compiler_identity.producer_bytes(),
            None,
            Some(private),
        )
        .unwrap();
    assert_eq!(private_request.context().as_ref(), &dependent_context);
    assert_eq!(
        private_request
            .compiler_original_products()
            .unwrap()
            .iter()
            .map(|product| identity(&product.owner().unit, &product.owner().module))
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            identity(
                &changed.product().owner().unit,
                &changed.product().owner().module
            ),
            owner.clone(),
        ])
    );
    let changed_interfaces = changed
        .compiler_source_support()
        .selected_interfaces()
        .unwrap()
        .select_owners(vec![owner])
        .unwrap();
    local.program_support = Some(ProgramSourceSupport {
        compiler_support: changed
            .compiler_source_support()
            .select_support_roots(
                changed
                    .compiler_input_projection()
                    .within_view(&changed_interfaces)
                    .roles()
                    .iter()
                    .map(CompilerInputRole::interface)
                    .collect(),
            )
            .unwrap(),
        imports: Arc::new(BTreeMap::new()),
    });
    assert!(
        local.validate_receipt(receipt, None, &persisted).is_err(),
        "local custody cannot override a conflicting persisted canonical owner"
    );
}

#[test]
#[ignore = "requires the matched Haskell worker and frontend"]
fn generated_planned_import_is_bound_to_original_certificate_and_target() {
    let directory = tempfile::tempdir().unwrap();
    let owner = tidepool_repr::SessionModule::lib(tidepool_repr::Generation(1));
    let original_source = include_str!("../../tests/fixtures/checked-prefix-publication/G1.hs");
    let original_path = directory.path().join(owner.relative_hs_path());
    std::fs::create_dir_all(original_path.parent().unwrap()).unwrap();
    std::fs::write(&original_path, original_source).unwrap();
    std::fs::write(
        directory.path().join("PrefixSelectedSupport.hs"),
        include_str!("../../tests/fixtures/checked-prefix-publication/PrefixSelectedSupport.hs"),
    )
    .unwrap();
    let certificate = Arc::new(
        crate::artifacts::test_support::certify_authored_declaration(
            owner,
            &original_path,
            original_source,
            &[directory.path().to_path_buf()],
            directory.path(),
        )
        .expect("matched issuer supplies the original native and canonical carriers"),
    );
    assert!(
        crate::certified_products::original_interface_requirements(certificate.product())
            .unwrap()
            .contains_key(&("main".into(), "PrefixSelectedSupport".into()))
    );
    let issued =
        ExactDeclarationContext::new(std::slice::from_ref(&certificate), &[], Vec::new()).unwrap();
    let original_id =
        ArtifactEntry::original_artifact_id(issued.owner.facts.producer, certificate.product());
    let projection = issued.compiler_input_projection().interface_only();
    let context = Arc::new(issued.recover_fixture_projection(projection).unwrap());
    assert_ne!(
        context.compiler_metadata_snapshot().unwrap().entries
            [&identity("main", &owner.module_name())]
            .descriptor
            .id,
        original_id,
        "the baseline winner is an interface; the protected recipe offers its original"
    );
    let persistent_semantic = context.semantic_sha256();
    let endpoint = tidepool_extract_cmd::ExtractCmd::new()
        .unwrap()
        .bind()
        .unwrap();
    let producer = endpoint.identity().producer_bytes();
    assert!(context.lexical_graph().is_empty());
    let source = format!(
        "module Consumer where\n-- tidepool-preamble-imports-v1\nimport {}\n",
        owner.module_name()
    );
    let ordinary = ExactCompileContext::new(context.clone())
        .prepare_compilation(&directory.path().join("ordinary"), producer)
        .unwrap();
    let mut request = ExactCompileContext::new(context.clone())
        .with_generated_planned_imports(Some(&certificate), [source.as_str()])
        .unwrap()
        .prepare_compilation(&directory.path().join("planned"), producer)
        .unwrap();
    assert_eq!(ordinary.semantic_sha256, request.semantic_sha256);
    assert_eq!(
        request.compiler_inputs().metadata.entries[&identity("main", &owner.module_name())]
            .descriptor
            .id,
        original_id
    );
    let manifest: Value =
        ciborium::de::from_reader(std::fs::read(&request.manifest).unwrap().as_slice()).unwrap();
    let rows = manifest.as_array().unwrap()[6].as_array().unwrap();
    let selected = rows
        .iter()
        .filter(|row| row.as_array().unwrap()[1] == text(owner.module_name()))
        .collect::<Vec<_>>();
    assert_eq!(selected.len(), 1);
    assert_eq!(
        selected[0].as_array().unwrap()[2],
        text(hex(&certificate.product().owner().module_version.0))
    );
    let receipt = import_receipt_source_owner(
        directory.path(),
        &request,
        "main",
        &owner.module_name(),
        "none",
        false,
        &source,
    );
    let admitted = request.validate_receipt(&receipt, None, &context).unwrap();
    assert_eq!(
        admitted.scaffold_roots.get(&0),
        Some(&BTreeSet::from([identity("main", &owner.module_name()),])),
        "original scope records only the actual protected recipe root"
    );
    let ordinary_receipt = import_receipt_source_owner(
        &directory.path().join("ordinary-source"),
        &ordinary,
        "main",
        &owner.module_name(),
        "none",
        false,
        &source,
    );
    let later_error = ordinary
        .validate_receipt(&ordinary_receipt, None, &context)
        .err()
        .expect("ordinary input lacks protected original permission");
    assert!(matches!(later_error, CompileError::ExtractFailed(detail)
        if detail.contains("exact import witness leaves selected lexical graph: source fixture:Consumer")));
    let mut wrong_context = (*context).clone();
    {
        let updated = [0; 32];
        wrong_context = wrong_context.rebuild(|facts| facts.producer = updated);
    }
    assert!(ExactCompileContext::new(Arc::new(wrong_context))
        .with_generated_planned_imports(Some(&certificate), [source.as_str()])
        .is_err());
    let absent = Arc::new(ExactDeclarationContext::new(&[], &[], Vec::new()).unwrap());
    assert!(ExactCompileContext::new(absent)
        .with_generated_planned_imports(Some(&certificate), [source.as_str()])
        .is_err());
    let without_certificate = ExactCompileContext::new(context.clone())
        .with_generated_planned_imports(None, [source.as_str()])
        .unwrap();
    assert!(without_certificate.protected_scaffold.native.is_empty());
    let inputs = request.compiler_inputs();
    let selection = crate::certified_products::CertifiedSourceSelection::from_compiler_inputs(
        inputs,
        &inputs.metadata,
        &tidepool_repr::execution_schema::InventoryOperation::new(Default::default()),
    )
    .unwrap();
    let private =
        OriginalCompilerInputs::from_selection(&selection, context.artifact_view()).unwrap();
    assert!(!private.native_original_requirements().is_empty());
    assert_eq!(
        private.native_original_requirements(),
        inputs.native_original_requirements()
    );
    let available_private = OriginalCompilerInputs::from_native_availability(
        &context,
        CanonicalProducerIdentity::from_compiler(&endpoint.identity()),
        std::slice::from_ref(certificate.product()),
    )
    .unwrap();
    assert_eq!(
        available_private.native_original_requirements(),
        private.native_original_requirements(),
        "a partial private offer preserves the selected original before admission"
    );
    // The recipe keeps its certificate while the declaration baseline is
    // narrowed to types. Its original arrives through the private issuer,
    // after baseline native choices have been captured independently.
    let type_baseline = context.original_interface_context().unwrap();
    type_baseline.normalize().unwrap();
    assert!(!type_baseline
        .artifact_view()
        .artifact_ids()
        .contains(&original_id));
    let private_request = ExactCompileContext::new(context.clone())
        .with_generated_planned_imports(Some(&certificate), [source.as_str()])
        .unwrap()
        .with_declarations(Arc::new(type_baseline))
        .prepare_compilation_with_private_input(
            &directory.path().join("private-planned"),
            producer,
            None,
            Some(available_private),
        )
        .unwrap();
    assert_eq!(
        private_request
            .compiler_inputs()
            .native_original_requirements(),
        private.native_original_requirements()
    );
    let private_receipt =
        crate::certified_products::CertifiedSourceSelection::from_compiler_inputs(
            private_request.compiler_inputs(),
            &private_request.compiler_inputs().metadata,
            &tidepool_repr::execution_schema::InventoryOperation::new(Default::default()),
        )
        .unwrap();
    assert_eq!(
        private_receipt
            .native_original_requirements()
            .cloned()
            .collect::<Vec<_>>(),
        private.native_original_requirements()
    );
    let published = request
        .admit_program_support_with_selection(
            context.clone(),
            context.artifact_view(),
            &[],
            None,
            &selection,
        )
        .unwrap();
    assert_eq!(published.semantic_sha256(), persistent_semantic);
    assert!(published.lexical_graph().is_empty());
    assert_eq!(
        published.compiler_input_projection(),
        context.compiler_input_projection()
    );
    assert!(published.compiler_original_products().unwrap().is_empty());

    // The protected target remains valid; a second source copying both
    // its marker and original import cannot borrow its edge permission.
    let mut value: Value =
        ciborium::de::from_reader(std::fs::read(&receipt).unwrap().as_slice()).unwrap();
    let fields = value.as_array_mut().unwrap();
    let mut evidence: crate::cache::DependencyEvidence =
        serde_json::from_str(fields[7].as_text().unwrap()).unwrap();
    let helper = directory.path().join("Helper.hs");
    let helper_source = source.replace("module Consumer where", "module Helper where");
    std::fs::write(&helper, &helper_source).unwrap();
    evidence.sources.push(crate::cache::SourceEvidence {
        path: helper.clone(),
        sha256: sha256(helper_source.as_bytes()),
    });
    evidence.modules.push(crate::cache::ModuleEvidence {
        unit: "fixture".into(),
        module: "Helper".into(),
        boot: false,
        source: helper,
        imports: vec![],
        product: crate::cache::ProductAvailability::Ready,
    });
    fields[7] = text(serde_json::to_string(&evidence).unwrap());
    fields[8].as_array_mut().unwrap().push(Value::Array(vec![
        text("fixture"),
        text("Helper"),
        Value::Bool(false),
        Value::Array(vec![Value::Array(vec![
            text("none"),
            text(owner.module_name()),
            Value::Bool(false),
            text("main"),
        ])]),
    ]));
    write_receipt(&receipt, &value);
    let helper_error = request
        .validate_receipt(&receipt, None, &context)
        .err()
        .expect("helper cannot borrow the protected target edge");
    assert!(matches!(helper_error, CompileError::ExtractFailed(detail)
        if detail.contains("exact import witness leaves selected lexical graph: source fixture:Helper")));
}

#[test]
fn checked_template_graph_retains_lexical_dependencies_without_artifact_promotion() {
    let (initial, _) = metadata_fixture();
    let template = "module Expr where\nimport Joined hiding (joinedValue)\n";
    let graph = initial
        .template_interface_graph(&[template.to_owned()])
        .unwrap();
    assert_eq!(graph.len(), 2);
    assert_eq!(
        graph[&identity("fixture", "Joined")].imports,
        vec![identity("fixture", "Alpha")]
    );
    assert!(graph.contains_key(&identity("fixture", "Alpha")));
    assert!(
        !graph.contains_key(&identity("fixture", "Beta")),
        "artifact requirements cannot grant lexical instances"
    );
    assert!(
        !template_selects_owner(&[template.to_owned()], &identity("fixture", "JoinedExtra")),
        "an import does not select a longer module name"
    );
    assert!(
        !template_selects_owner(
            &["import JoinedExtra hiding (x)".to_owned()],
            &identity("fixture", "Joined")
        ),
        "a longer imported module name does not select a prefix owner"
    );
    assert!(
        template_selects_owner(
            &["import qualified Joined as J hiding (x)".to_owned()],
            &identity("fixture", "Joined")
        ),
        "qualified imports with aliases and modifiers retain their exact owner"
    );
    let mut current = initial.as_ref().clone();
    {
        let updated = crate::exact_context::ExactLexicalSnapshot::default();
        current = current.rebuild(|facts| facts.lexical = updated);
    }
    let authority = GeneratedScaffoldImportAuthority {
        role: GeneratedScaffoldRole::InitialTemplateInterfaces {
            producer: initial.owner.facts.producer,
            roots: BTreeSet::from([identity("fixture", "Joined")]),
            graph: Arc::new(graph.clone()),
        },
        protected_templates: Arc::from([template.to_owned()]),
    };
    assert_eq!(
        authority
            .original_instance_graph()
            .iter()
            .map(|node| node.owner.clone())
            .collect::<BTreeSet<_>>(),
        graph.keys().cloned().collect(),
        "protected instance scope retains the graph issued at template admission"
    );
    let target = Path::new("/owned/Expr.hs");
    assert!(
        authority.permits(&current, template, target, target, "fixture", "Joined", "none", false)
    );
    assert!(
        !authority.permits(
            &current,
            &format!("{template}import Alpha\n"),
            target,
            target,
            "fixture",
            "Alpha",
            "none",
            false
        ),
        "retained lexical dependencies are not independent target imports"
    );
}

#[test]
fn checked_template_interfaces_preserve_seals_without_native_or_lexical_authority() {
    let make_context = |bytes: &[u8], selected: bool| {
        let owner = identity("main", "CapturedInterface");
        let mut packages = Vec::new();
        ciborium::ser::into_writer(
            &Value::Array(vec![
                text("TPPKGROOTS"),
                text("2"),
                Value::Array(vec![
                    text(&owner.unit),
                    text(&owner.module),
                    text(sha256(bytes)),
                ]),
                Value::Array(vec![]),
                Value::Array(vec![]),
            ]),
            &mut packages,
        )
        .unwrap();
        let interface = CertifiedJoinedInterface::from_certification(
            [7; 32],
            owner.unit.clone(),
            owner.module.clone(),
            bytes.to_vec(),
            packages,
        )
        .unwrap();
        let inventory = ArtifactInventory::default();
        let entries = vec![Arc::new(ArtifactEntry::interface(
            interface,
            JoinedInterfaceRole::LexicalJoin,
            vec![],
        ))];
        let compiler_projection = CompilerInputProjection::from_issued_entries(&entries).unwrap();
        Arc::new(ExactDeclarationContext::from_facts(
            DeclarationContextFacts {
                compiler_support: recover_fixture_compiler_support(
                    compiler_projection,
                    inventory
                        .admit_shared(&inventory.empty_view(), entries)
                        .unwrap(),
                    [],
                )
                .unwrap(),

                producer: [7; 32],
                original_instance_environment: OriginalInstanceEnvironment::Unknown,
                template_imports: None,

                lexical: crate::exact_context::ExactLexicalSnapshot::default()
                    .replace(if selected {
                        vec![ExactLexicalNode {
                            owner,
                            imports: vec![],
                        }]
                    } else {
                        vec![]
                    })
                    .unwrap(),
            },
        ))
    };
    let initial = make_context(b"first interface", true);
    let current = make_context(b"first interface", false);
    let changed = make_context(b"changed interface", false);
    assert!(initial.original_preview_interface_graph().is_err());
    let joined_target = ExactDeclarationContext::from_authenticated_execution(
        initial.owner.facts.producer,
        initial.artifact_view(),
        initial.owner.facts.compiler_support.projection().clone(),
        initial.lexical_graph().to_vec(),
        identity("main", "CapturedInterface"),
        &[identity("main", "CapturedInterface")],
    )
    .unwrap();
    assert!(
        joined_target.original_instance_target().is_err(),
        "a lexical join cannot replace the original canonical target's import census"
    );
    assert!(joined_target.original_preview_interface_graph().is_err());
    let canonical_target =
        ArtifactEntry::canonical(crate::certified_products::fixture_module_interface(
            initial.owner.facts.producer,
            "main",
            "OriginalTarget",
            BTreeMap::new(),
        ));
    let inventory = initial.owner.facts.compiler_support.artifacts().inventory();
    let retained = inventory
        .admit(initial.artifact_view(), vec![canonical_target.clone()])
        .unwrap();
    let mut lexical = initial.lexical_graph().to_vec();
    lexical.push(ExactLexicalNode {
        owner: identity("main", "OriginalTarget"),
        imports: vec![],
    });
    let original = ExactDeclarationContext::from_authenticated_execution(
        initial.owner.facts.producer,
        &retained,
        initial
            .compiler_input_projection()
            .interface_only()
            .merge(
                &CompilerInputProjection::from_issued_entries(&[Arc::new(canonical_target)])
                    .unwrap(),
            )
            .unwrap(),
        lexical,
        identity("main", "OriginalTarget"),
        &[identity("main", "CapturedInterface")],
    )
    .unwrap();
    assert_eq!(
        original.original_instance_target().unwrap(),
        &identity("main", "OriginalTarget")
    );
    let mut exchanged = original.clone();
    {
        let updated = OriginalInstanceEnvironment::Complete {
            target: identity("main", "OtherTarget"),
        };
        exchanged = exchanged.rebuild(|facts| facts.original_instance_environment = updated);
    }
    assert_ne!(
        original.semantic_sha256(),
        exchanged.semantic_sha256(),
        "the context commitment binds the original target identity"
    );
    assert!(
        exchanged.original_preview_interface_graph().is_err(),
        "a retained graph cannot replace its missing original target"
    );
    let original_graph = original.original_preview_interface_graph().unwrap();
    assert_eq!(original_graph.len(), 2);
    assert!(original
        .template_interface_graph(&["module Input where\n".to_owned()])
        .unwrap()
        .is_empty());
    let mut malformed = original.clone();
    edit_lexical(&mut malformed, |nodes| {
        nodes[0].imports.push(identity("main", "MissingOriginal"))
    });
    assert!(malformed.original_preview_interface_graph().is_err());
    let mut foreign = original;
    edit_lexical(&mut foreign, |nodes| nodes[0].owner.unit = "foreign".into());
    assert!(foreign.original_preview_interface_graph().is_err());
    let source = "module Expr where\nimport CapturedInterface\n";
    let authority = GeneratedScaffoldImportAuthority {
        role: GeneratedScaffoldRole::InitialTemplateInterfaces {
            producer: initial.owner.facts.producer,
            roots: BTreeSet::from([identity("main", "CapturedInterface")]),
            graph: Arc::new(
                initial
                    .template_interface_graph(&[source.to_owned()])
                    .unwrap(),
            ),
        },
        protected_templates: Arc::from([source.to_owned()]),
    };
    let target = Path::new("/owned/Expr.hs");
    let permits = |context: &ExactDeclarationContext, source: &str, module_source: &Path| {
        authority.permits(
            context,
            source,
            target,
            module_source,
            "main",
            "CapturedInterface",
            "none",
            false,
        )
    };
    assert!(permits(&current, source, target));
    assert!(current.recovery_products().is_empty());
    assert!(current.lexical_graph().is_empty());
    assert!(!permits(&changed, source, target));
    assert!(!permits(&current, source, Path::new("/owned/Authored.hs")));
    assert!(!permits(
        &current,
        "module Expr where\nimport qualified CapturedInterface as Hidden\n",
        target
    ));
    assert!(!permits(
        &current,
        &format!("{source}import CapturedInterface\n"),
        target
    ));
    let repeated_source = "module Expr where\n\
        import CapturedInterface hiding (hidden)\n\
        import qualified CapturedInterface as Captured\n\
        import qualified CapturedInterface as Captured\n";
    let repeated = GeneratedScaffoldImportAuthority {
        protected_templates: Arc::from([repeated_source.to_owned()]),
        ..authority.clone()
    };
    let repeated_permits = |source: &str| {
        repeated.permits(
            &current,
            source,
            target,
            target,
            "main",
            "CapturedInterface",
            "none",
            false,
        )
    };
    assert!(
        repeated_permits(repeated_source),
        "each protected alias and repeated occurrence retains its authority"
    );
    assert!(
        !repeated_permits(&repeated_source.replacen(
            "import qualified CapturedInterface as Captured\n",
            "",
            1,
        )),
        "a missing protected occurrence cannot be hidden by owner equality"
    );
    assert!(
        !repeated_permits(&repeated_source.replacen("as Captured", "as Changed", 1,)),
        "a changed protected alias needs new source authority"
    );
    assert!(
        !repeated_permits(&format!(
            "{repeated_source}import qualified CapturedInterface as Captured\n"
        )),
        "an extra authored occurrence needs ordinary source proof"
    );
    assert!(
        !repeated_permits(&repeated_source.replace("hiding (hidden)", "(hidden)",)),
        "changing hiding to an explicit import cannot preserve the protected grant"
    );
    let no_initial_selection = GeneratedScaffoldImportAuthority {
        role: GeneratedScaffoldRole::InitialTemplateInterfaces {
            producer: current.owner.facts.producer,
            roots: BTreeSet::new(),
            graph: Arc::new(
                current
                    .template_interface_graph(&[source.to_owned()])
                    .unwrap(),
            ),
        },
        protected_templates: Arc::from([source.to_owned()]),
    };
    assert!(!no_initial_selection.permits(
        &current,
        source,
        target,
        target,
        "main",
        "CapturedInterface",
        "none",
        false
    ));
    let no_protected_import = GeneratedScaffoldImportAuthority {
        role: GeneratedScaffoldRole::InitialTemplateInterfaces {
            producer: initial.owner.facts.producer,
            roots: BTreeSet::new(),
            graph: Arc::new(
                initial
                    .template_interface_graph(&["module Expr where\n".to_owned()])
                    .unwrap(),
            ),
        },
        protected_templates: Arc::from(["module Expr where\n".to_owned()]),
    };
    assert!(!no_protected_import.permits(
        &current,
        source,
        target,
        target,
        "main",
        "CapturedInterface",
        "none",
        false
    ));
    assert_eq!(initial.lexical_graph().len(), 1);
    let directory = tempfile::tempdir().unwrap();
    let empty = Arc::new(ExactDeclarationContext::new(&[], &[], vec![]).unwrap());
    let request = empty
        .prepare_compilation_authorizing(directory.path(), b"new compiler producer", |_| {
            Ok(Value::Null)
        })
        .unwrap();
    let unchanged = request
        .clone()
        .with_initial_template_interfaces(empty, &[source.to_owned()])
        .unwrap();
    assert!(unchanged.generated_scaffold_imports.is_empty());
    let wrong_producer = request.with_initial_template_interfaces(initial, &[source.to_owned()]);
    assert!(
        matches!(wrong_producer, Err(CompileError::ExtractFailed(reason))
        if reason == "exact declaration context: checked template interface producer differs")
    );
}

fn execution_scope_fixture(
    entries: &[Arc<ArtifactEntry>],
    parent: &Path,
) -> Result<Option<Value>, CompileError> {
    let root = tempfile::tempdir_in(parent)?.keep();
    execution_scope_value(entries, &root)
}

#[test]
fn execution_scope_borrows_parent_owned_graph_files_without_payload_writes() {
    let source = tempfile::tempdir().unwrap();
    let (graph, originals) = crate::execution_source::test_prepared_graph(source.path());
    let entries = [
        execution_entry(originals[0].clone(), Arc::clone(&graph)),
        execution_entry(originals[1].clone(), Arc::clone(&graph)),
    ];
    let parent = tempfile::tempdir().unwrap();
    let child = tempfile::tempdir().unwrap();
    let mut files = BTreeMap::new();
    let mut parent_written_bytes = 0;
    let first = execution_scope_value_with_graph_paths(
        &entries,
        parent.path(),
        &mut files,
        &mut parent_written_bytes,
    )
    .unwrap();
    assert_eq!(parent_written_bytes, graph.bytes().len() as u64);
    assert_eq!(files.len(), 1);
    let mut child_written_bytes = 0;
    let second = execution_scope_value_with_graph_paths(
        &entries,
        child.path(),
        &mut files,
        &mut child_written_bytes,
    )
    .unwrap();
    assert_eq!(
        first, second,
        "the child selects its parent's exact descriptor"
    );
    assert_eq!(child_written_bytes, 0);
    assert_eq!(std::fs::read_dir(child.path()).unwrap().count(), 0);
    assert!(files[&graph.digest()].starts_with(parent.path()));
    assert_eq!(
        std::fs::read(&files[&graph.digest()]).unwrap(),
        graph.bytes()
    );
    println!(
        "retained-graph parent_written_bytes={parent_written_bytes} descendant_written_bytes={child_written_bytes} graph_files=1"
    );
}

#[test]
fn execution_scope_shares_graph_and_matches_selected_original_closure() {
    let source = tempfile::tempdir().unwrap();
    let (graph, originals) = crate::execution_source::test_prepared_graph(source.path());
    let owners = originals
        .iter()
        .map(|original| original.owner.clone())
        .collect::<Vec<_>>();
    let a = execution_entry(originals[0].clone(), graph.clone());
    let b = execution_entry(originals[1].clone(), graph.clone());
    let scope = execution_scope_fixture(&[a, b.clone()], source.path())
        .unwrap()
        .unwrap();
    let rows = scope.as_array().unwrap();
    assert_eq!(rows[0].as_array().unwrap().len(), 1);
    assert_eq!(rows[1].as_array().unwrap().len(), 2);
    let graph_a =
        crate::execution_source::test_graph_requiring_original(&graph, &owners[1], graph.digest());
    let a = execution_entry(originals[0].clone(), graph_a);
    let scope = execution_scope_fixture(&[a.clone(), b.clone()], source.path())
        .unwrap()
        .unwrap();
    assert_eq!(scope.as_array().unwrap()[0].as_array().unwrap().len(), 2);
    assert_eq!(scope.as_array().unwrap()[1].as_array().unwrap().len(), 2);
    let scope = execution_scope_fixture(&[a], source.path())
        .unwrap()
        .unwrap();
    assert!(
        scope.as_array().unwrap()[1].as_array().unwrap().is_empty(),
        "a graph digest cannot discover a missing original owner"
    );
    let mut wrong = owners[1].clone();
    wrong.module_version = tidepool_repr::execution_schema::ModuleVersion([99; 32]);
    let graph_a =
        crate::execution_source::test_graph_requiring_original(&graph, &wrong, graph.digest());
    let scope = execution_scope_fixture(
        &[execution_entry(originals[0].clone(), graph_a), b],
        source.path(),
    )
    .unwrap()
    .unwrap();
    let roots = scope.as_array().unwrap()[1].as_array().unwrap();
    assert_eq!(
        roots.len(),
        1,
        "wrong original version refuses dependent execution only"
    );
    assert_eq!(roots[0].as_array().unwrap()[1], text("B"));
    let large =
        crate::execution_source::test_graph_with_large_origin(&graph, EXACT_SCOPE_BYTES_LIMIT);
    assert!(execution_scope_fixture(
        &[execution_entry(originals[0].clone(), large)],
        source.path()
    )
    .unwrap()
    .is_some());
    let megabyte = crate::execution_source::test_graph_with_large_origin(&graph, 1 << 20);
    assert!(
        validate_execution_graph_budget(std::iter::repeat_n(megabyte.as_ref(), 64)).is_err(),
        "graph budget overflow explicitly refuses preparation before transport copies"
    );
    assert!(
        validate_execution_graph_budget(std::iter::repeat_n(
            graph.as_ref(),
            EXACT_SCOPE_GRAPHS_LIMIT + 1
        ))
        .is_err(),
        "graph count is bounded before transport allocation"
    );
}

#[test]
fn execution_scope_preserves_complete_retained_closure_above_four_mib() {
    let temporary = tempfile::tempdir().unwrap();
    let root = std::env::var_os("TIDEPOOL_EXECUTION_SOURCE_FIXTURE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| temporary.path().to_path_buf())
        .join("large-closure");
    std::fs::create_dir_all(&root).unwrap();
    let (graph, originals) = crate::execution_source::test_prepared_graph(&root);
    let owners = originals
        .iter()
        .map(|original| original.owner.clone())
        .collect::<Vec<_>>();
    let retained = crate::execution_source::test_graph_with_large_origin(&graph, 2_300_000);
    let graph = crate::execution_source::test_graph_requiring_original(
        &retained,
        &owners[1],
        retained.digest(),
    );
    assert!(graph.bytes().len() + retained.bytes().len() > EXACT_SCOPE_BYTES_LIMIT);
    let entries = [
        execution_entry(originals[0].clone(), graph),
        execution_entry(originals[1].clone(), retained),
    ];
    let execution = execution_scope_value(&entries, &root).unwrap().unwrap();
    assert_eq!(
        execution.as_array().unwrap()[0].as_array().unwrap().len(),
        2
    );
    assert_eq!(
        execution.as_array().unwrap()[1].as_array().unwrap().len(),
        2
    );
    let interfaces = owners[..2]
        .iter()
        .map(|owner| {
            Value::Array(vec![
                text(&owner.unit),
                text(&owner.module),
                path_value(&root.join(format!("{}.hi", owner.module))).unwrap(),
                text(hex(&owner.skinny_iface_sha256)),
                Value::Array(vec![]),
                path_value(&root.join(format!("{}.packages", owner.module))).unwrap(),
                text(hex(&[0; 32])),
            ])
        })
        .collect();
    let products = owners[..2]
        .iter()
        .map(|owner| {
            Value::Array(vec![
                text(&owner.unit),
                text(&owner.module),
                text(hex(&owner.module_version.0)),
                text(hex(&owner.skinny_iface_sha256)),
                text(hex(&owner.product_sha256)),
                path_value(&root.join(format!("{}.tpmod", owner.module))).unwrap(),
                Value::Array(vec![]),
                Value::Array(vec![
                    path_value(&root.join(format!("{}.owners", owner.module))).unwrap(),
                    text(hex(&[1; 32])),
                ]),
                Value::Array(vec![text("full-carrier")]),
            ])
        })
        .collect();
    let fields = vec![
        text("TPEXACTSCOPE"),
        text(EXACT_SCOPE_SCHEMA_VERSION),
        text(hex(&[8; 32])),
        text(hex(&[7; 32])),
        Value::Array(interfaces),
        Value::Array(vec![]),
        Value::Array(products),
    ];
    let manifest = encode_scope_manifest(fields, Some(execution), None).unwrap();
    let decoded: Value = ciborium::de::from_reader(manifest.as_slice()).unwrap();
    current_scope_fields(&decoded);
    std::fs::write(root.join("exact-declaration-scope.cbor"), manifest).unwrap();
    let missing = execution_scope_fixture(&entries[..1], &root)
        .unwrap()
        .unwrap();
    assert!(missing.as_array().unwrap()[1]
        .as_array()
        .unwrap()
        .is_empty());
    println!(
        "complete retained closure: two graphs above four MiB retained; missing original refused"
    );
}

#[test]
fn execution_scope_retains_recipe_custody_across_local_owner_drift() {
    let source = tempfile::tempdir().unwrap();
    let (graph, originals) =
        crate::execution_source::test_prepared_graph_with_local_source_dependency(source.path());
    let owners = originals
        .iter()
        .map(|original| original.owner.clone())
        .collect::<Vec<_>>();
    let a = execution_entry(originals[0].clone(), Arc::clone(&graph));
    let native_entry = |owner: CachedHomeOwner| {
        let seal =
            crate::certified_products::encode_home_certification(&owner, &[], &BTreeMap::new())
                .unwrap();
        let mut packages = Vec::new();
        ciborium::ser::into_writer(
            &(
                "TPPKGROOTS",
                "2",
                (&owner.unit, &owner.module, hex(&owner.skinny_iface_sha256)),
                Vec::<Value>::new(),
                Vec::<Value>::new(),
            ),
            &mut packages,
        )
        .unwrap();
        let product = crate::certified_products::fixture_finalized_product(
            CertifiedRecoveryProduct::from_certification(
                owner,
                originals[1].interface.clone(),
                originals[1].product.clone(),
                packages,
                seal,
            ),
            graph.producer_sha256(),
        );
        Arc::new(ArtifactEntry::original(graph.producer_sha256(), product).unwrap())
    };
    let b1 = native_entry(owners[1].clone());
    let scope = execution_scope_fixture(&[Arc::clone(&a), Arc::clone(&b1)], source.path())
        .unwrap()
        .unwrap();
    assert_eq!(scope.as_array().unwrap()[1].as_array().unwrap().len(), 1);
    let mut changed = owners[1].clone();
    changed.module_version = ModuleVersion([99; 32]);
    let scope = execution_scope_fixture(&[Arc::clone(&a), native_entry(changed)], source.path())
        .unwrap()
        .unwrap();
    assert!(scope.as_array().unwrap()[1].as_array().unwrap().is_empty());
    assert!(
        scope.as_array().unwrap()[0].as_array().unwrap().is_empty(),
        "unadmitted graphs stay in custody, outside the execution projection"
    );
    let ArtifactPayload::Original(original) = &a.payload else {
        unreachable!()
    };
    assert!(Arc::ptr_eq(original.execution_source().unwrap(), &graph));
    let scope = execution_scope_fixture(&[a, b1], source.path())
        .unwrap()
        .unwrap();
    assert_eq!(scope.as_array().unwrap()[1].as_array().unwrap().len(), 1);
}

#[test]
fn execution_manifest_separates_graph_budget_and_never_erases_authority() {
    use crate::cache::{DependencyEvidence, ModuleEvidence, ProductAvailability, SourceEvidence};
    use crate::execution_source::{
        CertifiedExecutionSourceGraph, ExecutionSourceAdmission, ExecutionSourceGraphInput,
    };

    let temporary = tempfile::tempdir().unwrap();
    let root = std::env::var_os("TIDEPOOL_EXECUTION_SOURCE_FIXTURE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| temporary.path().to_path_buf());
    std::fs::create_dir_all(&root).unwrap();
    let source_root = root.join("source");
    std::fs::create_dir_all(&source_root).unwrap();
    let generated = format!("module Target where\n--{}\n", "x".repeat(2300000));
    let authored = "module A where\n";
    let target_path = source_root.join("Target.hs");
    let source_path = source_root.join("A.hs");
    std::fs::write(&target_path, &generated).unwrap();
    std::fs::write(&source_path, authored).unwrap();
    let product = support_product("A");
    let owner = product.owner().clone();
    let producer = b"execution manifest producer";
    let producer_sha256 = Sha256::digest(producer).into();
    let product = crate::certified_products::fixture_finalized_product(product, producer_sha256);
    let evidence = DependencyEvidence {
        version: 4,
        cache_safe: true,
        selection_complete: true,
        sources: vec![
            SourceEvidence {
                path: "@generated-source".into(),
                sha256: sha256(generated.as_bytes()),
            },
            SourceEvidence {
                path: source_path.clone(),
                sha256: sha256(authored.as_bytes()),
            },
        ],
        resolutions: vec![],
        packages: vec![],
        modules: vec![ModuleEvidence {
            unit: owner.unit.clone(),
            module: owner.module.clone(),
            boot: false,
            source: source_path,
            imports: vec![],
            product: ProductAvailability::Ready,
        }],
    };
    let graph = CertifiedExecutionSourceGraph::admit(ExecutionSourceGraphInput {
        producer: crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(
            producer,
        ),
        semantic_sha256: None,
        include: &[source_root],
        source_path: &target_path,
        source: &generated,
        evidence: &evidence,
        owners: std::slice::from_ref(&owner),
        fresh_owners: &BTreeSet::from([identity(&owner.unit, &owner.module)]),
        retained_sources: &BTreeMap::new(),
        exact_imports: &BTreeMap::new(),
        packages: &BTreeMap::new(),
    })
    .unwrap();
    let ExecutionSourceAdmission::Available(graph) = graph else {
        panic!("authored original recipe unavailable")
    };
    let mut validation = PackageInterfaceValidation::default();
    let seal = crate::certified_products::bind_home_execution_source(
        product.certification_bytes(),
        &owner,
        graph.digest(),
        &mut validation,
    )
    .unwrap();
    let product = CertifiedRecoveryProduct::from_certification(
        owner,
        product.interface_bytes().to_vec(),
        product.product_bytes().to_vec(),
        product.package_imports_bytes().to_vec(),
        seal,
    )
    .with_module_interface(product.module_interface().unwrap().clone())
    .unwrap()
    .with_execution_source_with_validation(graph.clone(), &mut validation)
    .unwrap();
    let context = Arc::new(
        ExactDeclarationContext::new(&[], &[], vec![])
            .unwrap()
            .extend_checked_original_products(producer_sha256, &[product])
            .unwrap(),
    );
    let collision = root.join("collision");
    std::fs::create_dir_all(&collision).unwrap();
    let collision_graph = collision.join(format!("execution-{}.cbor", hex(&graph.digest())));
    std::fs::write(&collision_graph, b"prior immutable capture").unwrap();
    let collision_request = context.prepare_compilation(&collision, producer).unwrap();
    assert!(collision_request.materialization.is_some());
    assert_eq!(
        std::fs::read(&collision_graph).unwrap(),
        b"prior immutable capture",
        "graph capture cannot replace or truncate an existing request artifact"
    );
    let request = context
        .prepare_compilation(&root.join("ordinary"), producer)
        .unwrap();
    let bytes = std::fs::read(&request.manifest).unwrap();
    let value: Value = ciborium::de::from_reader(bytes.as_slice()).unwrap();
    let fields = current_scope_fields(&value);
    assert_eq!(fields[8], Value::Null);
    assert_eq!(
        fields[7].as_array().unwrap()[1].as_array().unwrap().len(),
        1
    );
    std::fs::write(root.join("execution-source.cbor"), graph.bytes()).unwrap();
    let graph_row = &fields[7].as_array().unwrap()[0].as_array().unwrap()[0];
    let graph_path = graph_row.as_array().unwrap()[1].as_text().unwrap();
    assert_eq!(std::fs::read(graph_path).unwrap(), graph.bytes());
    assert!(context
        .prepare_compilation(&root.join("ordinary"), producer)
        .is_err());
    assert_eq!(
        std::fs::read(graph_path).unwrap(),
        graph.bytes(),
        "a reused request directory cannot truncate an already captured graph"
    );
    let mut inflated = fields[..7].to_vec();
    let interfaces = inflated[4].as_array_mut().unwrap();
    for index in 0..4000 {
        interfaces.push(Value::Array(vec![
            text("fixture"),
            text(format!("Padding{index}{}", "X".repeat(550))),
            text(root.join(format!("padding-{index}.hi")).to_str().unwrap()),
            text(hex(&[0; 32])),
            Value::Array(vec![]),
            text(
                root.join(format!("padding-{index}.packages"))
                    .to_str()
                    .unwrap(),
            ),
            text(hex(&[0; 32])),
        ]));
    }
    let inflated_bytes = encode_scope_manifest(inflated, Some(fields[7].clone()), None).unwrap();
    assert!(graph.bytes().len() < EXACT_SCOPE_BYTES_LIMIT);
    assert!(inflated_bytes.len() < EXACT_SCOPE_BYTES_LIMIT);
    assert!(graph.bytes().len() + inflated_bytes.len() > EXACT_SCOPE_BYTES_LIMIT);
    let inflated_value: Value = ciborium::de::from_reader(inflated_bytes.as_slice()).unwrap();
    assert_eq!(inflated_value.as_array().unwrap()[7], fields[7]);
    std::fs::write(root.join("ordinary/budget-scope.cbor"), &inflated_bytes).unwrap();
    println!(
        "independent budgets: metadata={} graph={} combined={}",
        inflated_bytes.len(),
        graph.bytes().len(),
        inflated_bytes.len() + graph.bytes().len()
    );

    let base = fields[..7].to_vec();
    let authorization = Value::Array(vec![
        text("authorized"),
        text(hex(&request.semantic_sha256)),
    ]);
    for authorization in [None, Some(authorization)] {
        let encoded = encode_scope_manifest(base.clone(), None, authorization.clone()).unwrap();
        let decoded: Value = ciborium::de::from_reader(encoded.as_slice()).unwrap();
        let without_recipe = current_scope_fields(&decoded);
        assert_eq!(without_recipe[7], Value::Null);
        assert_eq!(
            without_recipe[8],
            authorization.clone().unwrap_or(Value::Null),
            "omitting execution graphs must preserve explicit purpose authority"
        );
        assert_eq!(without_recipe[9], Value::Array(vec![]));
        let overflow = Value::Array(vec![
            Value::Bytes(vec![0; EXACT_SCOPE_BYTES_LIMIT - 32]),
            Value::Array(vec![]),
        ]);
        assert!(
            encode_scope_manifest(base.clone(), Some(overflow), authorization.clone()).is_err(),
            "metadata overflow must never erase admitted execution authority"
        );
        let with_recipe =
            encode_scope_manifest(base.clone(), Some(fields[7].clone()), authorization.clone())
                .unwrap();
        let decoded: Value = ciborium::de::from_reader(with_recipe.as_slice()).unwrap();
        let decoded = current_scope_fields(&decoded);
        assert_eq!(decoded[7], fields[7]);
        assert_eq!(decoded[8], authorization.unwrap_or(Value::Null));
    }
}
