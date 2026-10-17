//! A canonical/Core-only consumer gains native groups after original source removal.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tidepool_repr::execution_schema::Group;
use tidepool_toolchain::artifact_inventory::ArtifactKind;
use tidepool_toolchain::artifacts::{
    compile_invocation_in_context, prepare_frozen_production_entry, CompileInvocation,
    FrozenEntrySources,
};
use tidepool_toolchain::certified_products::ProductOrigin;
use tidepool_toolchain::declaration_join::{ExactDeclarationContext, ExactModuleIdentity};
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
    let (interfaces, execution) = products
        .original_compile_input
        .as_ref()
        .expect("original entry retains its consumed input proof")
        .original_contexts(
            &original.target_owned(),
            &products.certified_groups,
            &products.pending_imports,
            &products.package_interfaces,
            original.table(),
            original.yield_sites(),
        )
        .unwrap();
    let original_producer = interfaces.toolchain_identity_sha256();
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
    // The complete entry may retain native originals. Recover the consumer
    // independently from only its sealed canonical interfaces and Core.
    let durable = tempfile::tempdir().unwrap();
    let canonical_interfaces = interfaces
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
    let context = ExactDeclarationContext::capture_recovery(
        durable.path(),
        &[],
        &canonical_interfaces,
        &[],
        lexical.values().cloned().collect(),
    )
    .unwrap();
    assert!(context.recovery_products().is_empty());
    assert!(context
        .compiler_input_roles()
        .iter()
        .all(|role| role.original().is_none()));
    assert!(context.artifact_view().selected_native_groups().is_empty());
    assert!(context
        .artifact_view()
        .capture_graph_selection()
        .native_groups
        .is_empty());
    let consumer_seals = context.artifact_view().descriptors();
    assert_eq!(consumer_seals.len(), lexical.len());
    assert_eq!(
        context
            .compiler_input_roles()
            .iter()
            .map(|role| role.interface())
            .collect::<BTreeSet<_>>(),
        consumer_seals
            .iter()
            .map(|descriptor| descriptor.id)
            .collect::<BTreeSet<_>>()
    );
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

    // Recover the real support interfaces while omitting the requested owner.
    // Its lexical graph still requires that owner's own canonical proof.
    let support_interfaces = canonical_interfaces
        .iter()
        .filter(|reference| {
            reference.interface.unit != owner.unit || reference.interface.module != owner.module
        })
        .cloned()
        .collect::<Vec<_>>();
    let missing = ExactDeclarationContext::capture_recovery(
        durable.path(),
        &[],
        &support_interfaces,
        &[],
        vec![],
    )
    .unwrap();
    assert!(missing
        .artifact_view()
        .descriptors()
        .iter()
        .all(|descriptor| descriptor.owner != owner));
    assert!(missing
        .artifact_view()
        .descriptors()
        .iter()
        .any(|descriptor| descriptor.owner == support));
    let refusal = missing
        .extend(&[], &[], lexical.values().cloned().collect())
        .unwrap_err();
    assert!(matches!(&refusal, CompileError::ExtractFailed(_)));
    assert!(refusal
        .to_string()
        .contains("selected lexical owner lacks its interface main:RetainedReceiveOwner"));

    drop(original);
    drop(execution);
    drop(interfaces);
    drop(frozen);
    retained.close().unwrap();
    std::fs::remove_file(&owner_source).unwrap();
    std::fs::remove_file(&support_source).unwrap();
    assert!(!owner_source.exists() && !support_source.exists());
    let compiled = tidepool_testing::with_settlement(|settlement| {
        compile_invocation_in_context(
            &CompileInvocation {
                source: include_str!("../tests/fixtures/RetainedReceiveConsumer.hs"),
                targets: &["__prepared"],
                include: &include,
                fallback_module_name: "RetainedReceiveConsumer",
            },
            Arc::new(context),
            |_, _, _| {},
            settlement,
        )
    })
    .expect("exact consumer prepares original Core without authored source");
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
            group.owner().module == support.module
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
    let receive_definitions = receive.group().definitions();
    let mut state = crate::session::PersistentSession::new(None, 4096);
    let program = crate::install_compiled_target(&mut state, &compiled, "__prepared")
        .expect("install the target and its complete certified native closure");
    let table = state.session_table().clone();
    crate::typed_receive_test_support::assert_typed_receive_json(
        state.prepared_mut().unwrap(),
        program,
        table,
        receive_definitions,
    );
    assert!(state.prepared_mut().unwrap().unpin(program));
}
