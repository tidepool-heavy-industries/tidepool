//! Genuine source-only originals gain native groups through exact consumption.
use std::sync::Arc;

use super::compiler_test_support::selected_lexical_closure;

use tidepool_repr::execution_schema::Group;
use tidepool_toolchain::artifact_inventory::ArtifactKind;
use tidepool_toolchain::artifacts::{
    compile_invocation, compile_invocation_in_context, CompileInvocation,
};
use tidepool_toolchain::cache::ProductAvailability;
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
        include_str!("fixtures/RetainedReceiveOwner.hs"),
    )
    .unwrap();
    std::fs::write(
        &support_source,
        include_str!("fixtures/RetainedReceiveSupport.hs"),
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
    let original = tidepool_testing::with_settlement(|settlement| {
        compile_invocation(
            &CompileInvocation {
                source: include_str!("fixtures/RetainedReceiveProbe.hs"),
                targets: &["result"],
                include: &include,
                fallback_module_name: "RetainedReceiveProbe",
            },
            |_, _, _| {},
            settlement,
        )
    })
    .expect("production issuer finalizes import-only originals");
    let inventory = original
        .module_inventory
        .as_ref()
        .expect("original compiler graph");
    for expected in [&owner, &support] {
        let module = inventory
            .iter()
            .find(|module| {
                !module.boot && module.unit == expected.unit && module.module == expected.module
            })
            .expect("source-only original in the consumed graph");
        assert_eq!(module.product, ProductAvailability::InterfaceOnly);
        assert!(!original.recovery_products.iter().any(|product| {
            product.owner().unit == expected.unit && product.owner().module == expected.module
        }));
        assert!(!original.certified_groups.iter().any(|group| {
            group.owner().unit == expected.unit && group.owner().module == expected.module
        }));
    }

    // Retain the original consumed home-import graph. A lexical selection is
    // separate from native demand, and its complete interface closure is
    // admitted by the production inventory owner.
    let lexical = selected_lexical_closure(&original, vec![owner.clone()]);
    assert!(lexical[&owner].imports.contains(&support));
    let roots = lexical.keys().cloned().collect::<Vec<_>>();
    let interfaces = original
        .artifact_view
        .interface_projection(&roots)
        .expect("genuine canonical original interface closure");
    assert!(interfaces.descriptors().iter().all(|descriptor| {
        descriptor.kind == ArtifactKind::CanonicalModuleInterface
            && descriptor.product_sha256.is_none()
    }));
    let original_seals = interfaces.descriptors();
    let context = ExactDeclarationContext::new(&[], &[], vec![])
        .unwrap()
        .extend_interface_artifacts(&interfaces)
        .unwrap()
        .extend(&[], &[], lexical.values().cloned().collect())
        .unwrap();
    assert!(context.recovery_products().is_empty());

    // Omit the owner through a real interface projection. The production
    // context cannot admit its lexical graph without its canonical proof.
    let missing = original
        .artifact_view
        .interface_projection(
            &roots
                .iter()
                .filter(|selected| **selected != owner)
                .cloned()
                .collect::<Vec<_>>(),
        )
        .unwrap();
    assert!(missing
        .descriptors()
        .iter()
        .all(|descriptor| descriptor.owner != owner));
    let refusal = ExactDeclarationContext::new(&[], &[], vec![])
        .unwrap()
        .extend_interface_artifacts(&missing)
        .unwrap()
        .extend(&[], &[], lexical.values().cloned().collect())
        .unwrap_err();
    assert!(matches!(&refusal, CompileError::ExtractFailed(_)));
    assert!(refusal
        .to_string()
        .contains("selected lexical owner lacks its interface main:RetainedReceiveOwner"));

    std::fs::remove_file(&owner_source).unwrap();
    std::fs::remove_file(&support_source).unwrap();
    assert!(!owner_source.exists() && !support_source.exists());
    drop(original);
    let mut compiled = tidepool_testing::with_settlement(|settlement| {
        compile_invocation_in_context(
            &CompileInvocation {
                source: include_str!("fixtures/RetainedReceiveConsumer.hs"),
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
        let original_seal = original_seals
            .iter()
            .find(|descriptor| descriptor.owner == *expected)
            .unwrap();
        assert!(compiled.artifact_view.descriptors().contains(original_seal));
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
    let prepared = compiled
        .targets
        .remove("__prepared")
        .unwrap()
        .prepared
        .into_prepared();
    assert!(prepared
        .sites()
        .iter()
        .any(|site| site.origin == "RetainedReceiveOwner.result"));
    super::prepared_execution::assert_typed_receive_json(prepared, compiled.table);
}
