//! A canonical/Core-only consumer gains native groups after original source removal.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use fvm_ipld_blockstore::{Blockstore, MemoryBlockstore};
use tidepool_repr::execution_schema::Group;
use tidepool_toolchain::artifact_inventory::{ArtifactDescriptor, ArtifactKind};
use tidepool_toolchain::artifacts::{
    prepare_frozen_production_entry, CompileInvocation, FrozenEntrySources,
};
use tidepool_toolchain::certified_products::ProductOrigin;
use tidepool_toolchain::declaration_join::{
    ExactModuleIdentity, RecoveredArtifactAcquisition, RecoveryInventoryError,
    RecoveryPayloadReference,
};
use tidepool_toolchain::exact_context::{hydrate_context, Materializer};
use tidepool_toolchain::CompileError;

#[test]
fn retained_core_typed_receive_executes_after_original_sources_are_removed() {
    tidepool_testing::eval_harness::require_extract();
    let surface = tidepool_testing::effect_surface::TestEffectSurface::minimal(&[
        tidepool_mcp::actor_local_decl(),
    ])
    .expect("typed receive effect surface");
    let work = tempfile::tempdir().unwrap();
    let owner_source = work.path().join("RetainedReceiveOwner.hs");
    let support_source = work.path().join("RetainedReceiveSupport.hs");
    std::fs::write(
        &owner_source,
        include_str!("../tests/fixtures/RetainedReceiveOwner.hs"),
    )
    .unwrap();
    std::fs::write(
        &support_source,
        include_str!("../tests/fixtures/RetainedReceiveSupport.hs"),
    )
    .unwrap();
    let mut include = vec![work.path().to_path_buf()];
    include.extend(
        surface
            .include_path_refs()
            .into_iter()
            .map(|path| path.to_path_buf()),
    );
    let owner = ExactModuleIdentity {
        unit: "main".into(),
        module: "RetainedReceiveOwner".into(),
    };
    let support = ExactModuleIdentity {
        unit: "main".into(),
        module: "RetainedReceiveSupport".into(),
    };
    let probe_source = work.path().join("RetainedReceiveProbe.hs");
    std::fs::write(
        &probe_source,
        format!(
            "{}\n__prepared :: Int\n__prepared = result\n",
            include_str!("../tests/fixtures/RetainedReceiveProbe.hs")
        ),
    )
    .unwrap();
    let frozen = FrozenEntrySources::capture(&include, &probe_source).unwrap();
    let retained = tempfile::tempdir().unwrap();
    let original = tidepool_testing::with_settlement(|settlement| {
        prepare_frozen_production_entry(
            &frozen,
            retained.path(),
            &retained.path().join("entry"),
            settlement,
        )
    })
    .expect("production entry issuer seals the original consumed homes");
    let products = original.products();
    let execution = products
        .original_compile_input
        .as_ref()
        .expect("original entry retains its consumed input proof")
        .original_execution_context(
            &original.target_owned(),
            &products.certified_groups,
            products.pending_imports(),
            &products.package_interfaces,
            original.table(),
            original.yield_sites(),
        )
        .unwrap();
    let original_producer = execution.toolchain_identity_sha256();
    assert_ne!(original_producer, [0; 32]);

    // Retain the original consumed home-import graph. A lexical selection is
    // separate from native demand, and its complete interface closure is
    // admitted by the production inventory owner.
    let mut pending = vec![owner.clone()];
    let mut lexical = BTreeMap::new();
    while let Some(selected) = pending.pop() {
        if lexical.contains_key(&selected) {
            continue;
        }
        let node = execution
            .lexical_graph()
            .iter()
            .find(|node| node.owner == selected)
            .expect("selected source owner has its original compiler import graph");
        pending.extend(node.imports.iter().cloned());
        lexical.insert(selected, node.clone());
    }
    assert!(lexical[&owner].imports.contains(&support));
    // Restore the exact output selection. Its unrelated entry target may retain
    // native custody; the requested homes were only imported for their types.
    let durable = tempfile::tempdir().unwrap();
    let canonical_interfaces = execution
        .materialize_module_interfaces(durable.path())
        .unwrap()
        .into_iter()
        .filter(|reference| {
            lexical.contains_key(&ExactModuleIdentity {
                unit: reference.interface.unit.clone(),
                module: reference.interface.module.clone(),
            })
        })
        .collect::<Vec<_>>();
    let snapshot = execution.snapshot();
    let blocks = MemoryBlockstore::new();
    let mut writer = Materializer::new(&blocks);
    let persisted = writer.materialize_context(None, &snapshot).unwrap();
    for (cid, bytes) in writer.finish().unwrap().into_blocks() {
        blocks.put_keyed(&cid, &bytes).unwrap();
    }
    let acquired = hydrate_context(&blocks, persisted.root()).unwrap();
    let selection = acquired.snapshot().graph_selection().unwrap();
    let edges = acquired.snapshot().binding_dependencies().unwrap();
    let mut ids = selection
        .bindings
        .iter()
        .map(|binding| binding.artifact)
        .collect::<BTreeSet<_>>();
    ids.extend(snapshot.native_package_artifacts_introduced(None));
    let payloads = execution
        .materialize_recovery_artifacts(durable.path(), &ids)
        .unwrap();
    let descriptors = payloads
        .iter()
        .map(|reference| match reference {
            RecoveryPayloadReference::Original(reference) => {
                ArtifactDescriptor::from_recovery_product(reference)
            }
            RecoveryPayloadReference::Canonical(reference) => {
                ArtifactDescriptor::from_recovery_module_interface(reference)
            }
            RecoveryPayloadReference::Join(reference) => {
                ArtifactDescriptor::from_recovery_join(reference)
            }
            RecoveryPayloadReference::Value(reference) => {
                ArtifactDescriptor::from_recovery_value_interface(reference)
            }
            RecoveryPayloadReference::PackageCode(reference) => {
                ArtifactDescriptor::from_recovery_package_code(reference)
            }
        })
        .collect::<Vec<_>>();
    let context = RecoveredArtifactAcquisition::new(durable.path())
        .capture_payloads_bound(&payloads, &descriptors, &selection.bindings, &edges)
        .unwrap()
        .context_for_expected_snapshot(persisted.root(), &acquired)
        .unwrap();
    assert_eq!(
        context.artifact_view().capture_graph_selection(),
        execution.artifact_view().capture_graph_selection()
    );
    assert_eq!(
        context.compiler_input_roles(),
        execution.compiler_input_roles()
    );
    assert_eq!(context.lexical_graph(), execution.lexical_graph());
    for expected in [&owner, &support] {
        assert!(context.recovery_products().iter().all(|product| {
            product.owner().unit != expected.unit || product.owner().module != expected.module
        }));
    }
    let consumer_seals = context
        .artifact_view()
        .descriptors()
        .into_iter()
        .filter(|descriptor| descriptor.owner == owner || descriptor.owner == support)
        .collect::<Vec<_>>();
    assert_eq!(consumer_seals.len(), 2);
    assert!(consumer_seals.iter().all(|descriptor| {
        context
            .compiler_input_roles()
            .iter()
            .any(|role| role.interface() == descriptor.id && role.original().is_none())
    }));
    assert!(consumer_seals.iter().all(|descriptor| {
        descriptor.kind == ArtifactKind::CanonicalModuleInterface
            && descriptor.product_sha256.is_none()
    }));
    for expected in [&owner, &support] {
        let refs = canonical_interfaces
            .iter()
            .filter(|reference| {
                reference.interface.unit == expected.unit
                    && reference.interface.module == expected.module
            })
            .collect::<Vec<_>>();
        assert_eq!(
            refs.len(),
            1,
            "the canonical materializer issues one exact owner"
        );
        let reference = refs[0];
        assert_eq!(
            reference.interface.toolchain_identity_sha256,
            original_producer
        );
        assert!(
            reference.core.is_some(),
            "retained Core must come from the canonical issuer"
        );
        let recovered = consumer_seals
            .iter()
            .filter(|descriptor| descriptor.owner == *expected)
            .collect::<Vec<_>>();
        assert_eq!(recovered.len(), 1);
        assert_eq!(
            recovered[0].producer_sha256,
            reference.interface.toolchain_identity_sha256
        );
        assert_eq!(
            recovered[0].interface_sha256,
            reference.interface.skinny_iface_sha256
        );
    }

    // Keep the persisted exact selection unchanged while omitting a required
    // owner. Acquisition must refuse before a compiler context can be restored.
    let incomplete = payloads
        .iter()
        .zip(&descriptors)
        .filter(|(_, descriptor)| descriptor.owner != owner)
        .map(|(reference, descriptor)| (reference.clone(), descriptor.clone()))
        .collect::<Vec<_>>();
    let (incomplete_payloads, incomplete_descriptors): (Vec<_>, Vec<_>) =
        incomplete.into_iter().unzip();
    let Err(refusal) = RecoveredArtifactAcquisition::new(durable.path()).capture_payloads_bound(
        &incomplete_payloads,
        &incomplete_descriptors,
        &selection.bindings,
        &edges,
    ) else {
        panic!("missing canonical owner must refuse exact acquisition");
    };
    assert!(matches!(
        refusal,
        RecoveryInventoryError::Certification(CompileError::ExtractFailed(_))
            | RecoveryInventoryError::Certification(CompileError::ArtifactInventory(_))
    ));

    drop(original);
    drop(execution);
    drop(frozen);
    retained.close().unwrap();
    std::fs::remove_file(&owner_source).unwrap();
    std::fs::remove_file(&support_source).unwrap();
    assert!(!owner_source.exists() && !support_source.exists());
    let mut source_session = crate::session::PersistentSession::new(None, 4096);
    let source_lease = source_session.mint_isolated_scope_with_lease().unwrap();
    let prepared = tidepool_testing::with_settlement(|settlement| {
        crate::finish_runtime_targets(
            source_lease
                .with_source_compiler_transaction(
                    tidepool_toolchain::artifacts::CompileWorkload::Foreground,
                    crate::CompilerTransactionCancellation::new(),
                    None,
                    settlement,
                    |owner, report| {
                        crate::prepare_runtime_targets_in_context(
                            owner,
                            &CompileInvocation {
                                source: include_str!(
                                    "../tests/fixtures/RetainedReceiveConsumer.hs"
                                ),
                                targets: &["__prepared", "__payload"],
                                include: &include,
                                fallback_module_name: "RetainedReceiveConsumer",
                            },
                            Arc::new(context),
                            |_, _, _| {},
                            report,
                        )
                    },
                )
                .unwrap(),
        )
    })
    .expect("exact consumer prepares original Core without authored source");
    let compiled = prepared.artifacts();
    for expected in [&owner, &support] {
        let groups = compiled
            .certified_groups
            .iter()
            .filter(|group| {
                group.owner().unit == expected.unit && group.owner().module == expected.module
            })
            .collect::<Vec<_>>();
        assert!(
            !groups.is_empty(),
            "original native demand must emit groups"
        );
        assert!(groups
            .iter()
            .all(|group| group.origin() == ProductOrigin::RetainedCore));
        assert!(groups
            .iter()
            .flat_map(|group| group.group().binders())
            .all(|binder| { binder.unit == expected.unit && binder.module == expected.module }));
        let canonical_seal = consumer_seals
            .iter()
            .find(|descriptor| descriptor.owner == *expected)
            .unwrap();
        assert!(compiled
            .artifact_view
            .descriptors()
            .contains(canonical_seal));
    }
    let recursive = compiled
        .certified_groups
        .iter()
        .find(|group| {
            group.owner().unit == support.unit
                && group.owner().module == support.module
                && group
                    .group()
                    .binders()
                    .iter()
                    .any(|binder| binder.occurrence == "routeEven")
        })
        .expect("transitive original retains its recursive routeEven group");
    assert!(recursive
        .group()
        .definitions()
        .bindings()
        .iter()
        .any(|group| {
            matches!(group, Group::Recursive(bindings) if bindings.iter().any(|binding| {
                binding.identity.occurrence == "routeEven"
            }))
        }));
    let declared_site = compiled.targets["__prepared"]
        .asks
        .sites()
        .into_iter()
        .find(|site| site.origin == "RetainedReceiveOwner.result")
        .expect("the complete compiler output retains the original receive site");
    let receive = compiled
        .certified_groups
        .iter()
        .filter(|group| group.owner().unit == owner.unit && group.owner().module == owner.module)
        .filter(|group| {
            group.group().binders().iter().any(|binder| {
                binder.unit == owner.unit
                    && binder.module == owner.module
                    && binder.occurrence == "result"
            })
        })
        .find(|group| {
            group
                .group()
                .definitions()
                .sites()
                .iter()
                .any(|site| site.site == declared_site.site)
        })
        .expect("the certified retained owner carries its original receive site");
    assert_eq!(receive.origin(), ProductOrigin::RetainedCore);
    let mut state = crate::session::PersistentSession::new(None, 4096);
    let (program, _scope) = prepared
        .install_in(&mut state, "__prepared")
        .expect("install the target and its complete certified native closure");
    let table = state.session_table().clone();
    let payload_group = reached_payload_construction(&compiled);
    let payload_layout = crate::typed_receive_test_support::json_layout(
        payload_group.group().definitions(),
        &format!(
            "{:?} original group {} authored payload",
            payload_group.owner(),
            payload_group.group().original_ordinal()
        ),
    );
    let mut producer = crate::session::PersistentSession::new(None, 4096);
    let (payload_program, _payload_scope) = prepared
        .install_in(&mut producer, "__payload")
        .expect("install the payload target and its complete certified native closure");
    crate::typed_receive_test_support::assert_typed_receive_json(
        state.prepared_mut().unwrap(),
        program,
        table,
        declared_site.site,
        (
            producer.prepared_mut().unwrap(),
            payload_program,
            payload_layout,
        ),
        crate::session::prepared::assert_parked_receive_schema,
    );
    assert!(state.prepared_mut().unwrap().unpin(program));
    assert!(producer.prepared_mut().unwrap().unpin(payload_program));
}

fn reached_payload_construction(
    compiled: &crate::CompiledArtifacts,
) -> &tidepool_toolchain::certified_products::PendingCertifiedGroup {
    use tidepool_toolchain::certified_products::PendingImportOwner;
    let mut pending = compiled.targets["__payload"]
        .pending_imports
        .iter()
        .collect::<Vec<_>>();
    let mut visited = BTreeSet::new();
    let mut payload: Option<&tidepool_toolchain::certified_products::PendingCertifiedGroup> = None;
    while let Some(import) = pending.pop() {
        let PendingImportOwner::Source {
            owner,
            original_ordinal,
            binder,
        } = import
        else {
            continue;
        };
        let mut matches = compiled.certified_groups.iter().filter(|group| {
            group.owner() == owner && group.group().original_ordinal() == *original_ordinal
        });
        let group = matches
            .next()
            .unwrap_or_else(|| panic!("missing certified source group for {import:?}"));
        assert!(
            matches.next().is_none(),
            "duplicate certified source group for {import:?}"
        );
        assert!(
            group.group().binders().contains(binder),
            "certified group does not export exact dependency {import:?}"
        );
        if binder.unit == "main"
            && binder.module == "RetainedReceiveConsumer"
            && binder.namespace == "value"
            && binder.occurrence == "payload"
            && binder.record_parent.is_none()
        {
            if let Some(previous) = payload {
                assert!(
                    std::ptr::eq(previous, group),
                    "multiple reached authored payload construction owners"
                );
            }
            payload = Some(group);
        }
        if visited.insert((owner.clone(), *original_ordinal)) {
            pending.extend(group.imports());
        }
    }
    payload.expect("__payload must reach the exact certified authored payload construction")
}
