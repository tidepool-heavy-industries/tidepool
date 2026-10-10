//! Publication facts remain fixed while the same original gains native demand.
//! The reference model uses authored ordinals, independently of graph queries.
use super::*;
use crate::artifact_inventory::{NativeArtifactDemand, NativeGroupKey};
use crate::certified_products::PendingImportOwner;
use proptest::prelude::*;
use tidepool_repr::execution_schema::SymbolIdentity;

const ORDINALS: [u32; 3] = [3, 11, 29];

#[derive(Clone, Debug)]
enum Operation {
    Demand(usize),
    Compose,
    Recover,
    Reopen,
    AlterSelection(u8),
}

fn operations() -> impl Strategy<Value = Vec<Operation>> {
    proptest::collection::vec(
        prop_oneof![
            4 => (0..ORDINALS.len()).prop_map(Operation::Demand),
            2 => Just(Operation::Compose),
            2 => Just(Operation::Recover),
            1 => Just(Operation::Reopen),
            1 => (0u8..4).prop_map(Operation::AlterSelection),
        ],
        0..24,
    )
}

fn fixture(initial: usize) -> (Arc<ArtifactEntry>, Arc<PublishedSourceOriginalSelection>) {
    let producer = [2; 32];
    let product = crate::certified_products::tests::recovered_witness_fixtures(&[
        crate::certified_products::fixture_finalized_product(
            crate::certified_products::tests::original_groups_fixture(
                "HistoryRoot",
                ORDINALS.iter().map(|ordinal| (*ordinal, vec![])).collect(),
                1,
                &BTreeMap::new(),
            ),
            producer,
        ),
    ])
    .remove(0)
    .product;
    let root = Arc::new(ArtifactEntry::original(producer, product).unwrap());
    let target = Arc::new(ArtifactEntry::canonical(
        crate::certified_products::fixture_module_interface(
            producer,
            "fixture",
            "HistoryTarget",
            BTreeMap::new(),
        ),
    ));
    let inventory = ArtifactInventory::default();
    let view = inventory
        .admit_recovery_selection(
            &inventory.empty_view(),
            vec![root.clone(), target.clone()],
            &BTreeSet::from([NativeGroupKey {
                artifact: root.descriptor.id,
                original_ordinal: ORDINALS[initial],
            }]),
        )
        .unwrap();
    let issued = ExactDeclarationContext::from_authenticated_execution(
        producer,
        &view,
        CompilerInputProjection::from_issued_entries(&[root.clone(), target.clone()]).unwrap(),
        vec![
            ExactLexicalNode {
                owner: target.descriptor.owner.clone(),
                imports: vec![root.descriptor.owner.clone()],
            },
            ExactLexicalNode {
                owner: root.descriptor.owner.clone(),
                imports: vec![],
            },
        ],
        target.descriptor.owner.clone(),
        &[
            target.descriptor.owner.clone(),
            root.descriptor.owner.clone(),
        ],
    )
    .unwrap();
    let publication = issued
        .issue_published_source_original(
            "history-revision",
            "history-input",
            &root.descriptor.owner,
        )
        .unwrap();
    (root, publication)
}

fn continued_context(
    context: &ExactDeclarationContext,
    view: &ArtifactView,
    root: &ArtifactEntry,
) -> Result<ExactDeclarationContext, CompileError> {
    // This is the original_execution_context production construction boundary:
    // original instance evidence and its issued compiler projection travel together.
    ExactDeclarationContext::from_authenticated_execution(
        context.producer,
        view,
        context.compiler_projection.clone(),
        context.lexical.clone(),
        root.descriptor.owner.clone(),
        &[root.descriptor.owner.clone()],
    )
}

fn restored(
    context: &ExactDeclarationContext,
    roles: &[CompilerInputRole],
) -> Result<ExactDeclarationContext, CompileError> {
    let view = context.artifact_view();
    RecoveredArtifactInventory {
        producer: context.producer,
        entries: view
            .entries()
            .into_iter()
            .map(|entry| (entry.descriptor.id, entry))
            .collect(),
        recorded_inventory: true,
        interfaces: view.interface_dependencies(),
    }
    .context_with_published_roles(
        &view.artifact_ids(),
        &view
            .selected_native_groups()
            .into_iter()
            .collect::<Vec<_>>(),
        roles,
        context.lexical.clone(),
    )
}

fn property_config() -> proptest::test_runner::Config {
    let mut config = proptest::test_runner::Config::default();
    if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
        config.failure_persistence = Some(Box::new(
            proptest::test_runner::FileFailurePersistence::Direct(path),
        ));
    }
    config
}

#[test]
fn generated_publication_support_and_recovery_preserve_issuer_selection() {
    let mut config = proptest::test_runner::contextualize_config(property_config());
    config.source_file =
        Some("tidepool/toolchain/src/declaration_context/published_source/history_properties.rs");
    config.test_name = Some(concat!(
        module_path!(),
        "::generated_publication_support_and_recovery_preserve_issuer_selection"
    ));
    let configured_cases = config.cases;
    let mut runner = proptest::test_runner::TestRunner::new(config);
    let callbacks = std::cell::Cell::new(0usize);
    let observed: [std::cell::Cell<usize>; 5] = std::array::from_fn(|_| std::cell::Cell::new(0));
    let result = runner.run(&(0..ORDINALS.len(), operations()), |(initial, history)| {
        callbacks.set(callbacks.get() + 1);
        let (root, publication) = fixture(initial);
        let original = publication.context();
        let expected_publication = original.semantic_sha256();
        let expected_roles = original.compiler_input_roles();
        let expected_artifacts = original.artifact_view().artifact_ids();
        let initial_groups = BTreeSet::from([NativeGroupKey {
            artifact: root.descriptor.id,
            original_ordinal: ORDINALS[initial],
        }]);
        let mut demanded = BTreeSet::from([ORDINALS[initial]]);
        let mut context = original.as_ref().clone();
        // Every history reaches the causal interaction, even after shrinking:
        // later same-owner body -> compose publication -> durable reconstruction.
        let history = history.into_iter().chain([
            Operation::Demand((initial + 1) % ORDINALS.len()),
            Operation::Compose,
            Operation::Recover,
            Operation::Reopen,
            Operation::AlterSelection(1),
        ]);
        for operation in history {
            match operation {
                Operation::Demand(index) => {
                    observed[0].set(observed[0].get() + 1);
                    let ordinal = ORDINALS[index];
                    let ArtifactPayload::Original(product) = &root.payload else {
                        unreachable!()
                    };
                    let import = PendingImportOwner::Source {
                        owner: product.owner().clone(),
                        original_ordinal: ordinal,
                        binder: SymbolIdentity {
                            unit: "fixture".into(),
                            module: "HistoryRoot".into(),
                            namespace: "value".into(),
                            occurrence: format!("entry_{ordinal}"),
                            record_parent: None,
                        },
                    };
                    let view = context
                        .inventory
                        .inventory()
                        .admit_shared_with_demand(
                            context.artifact_view(),
                            vec![root.clone()],
                            NativeArtifactDemand::CertifiedTargetImports(&[import]),
                        )
                        .unwrap();
                    context = continued_context(&context, &view, &root).unwrap();
                    demanded.insert(ordinal);
                }
                Operation::Compose => {
                    observed[1].set(observed[1].get() + 1);
                    context = context
                        .with_published_source_originals(&publication)
                        .unwrap();
                }
                Operation::Recover => {
                    observed[2].set(observed[2].get() + 1);
                    let before = context.artifact_view().clone();
                    let roles: Vec<CompilerInputRole> = serde_json::from_slice(
                        &serde_json::to_vec(&context.compiler_input_roles()).unwrap(),
                    )
                    .unwrap();
                    context = restored(&context, &roles).unwrap();
                    prop_assert_eq!(context.artifact_view(), &before);
                    prop_assert_eq!(
                        context.original_instance_environment(),
                        &OriginalInstanceEnvironment::Unknown
                    );
                }
                Operation::Reopen => {
                    observed[3].set(observed[3].get() + 1);
                    let reopened = context.published_source_original_selections().unwrap();
                    prop_assert_eq!(reopened.len(), 1);
                    prop_assert_eq!(
                        reopened[0].context().semantic_sha256(),
                        expected_publication
                    );
                    prop_assert_eq!(
                        reopened[0]
                            .context()
                            .artifact_view()
                            .selected_native_groups(),
                        initial_groups.clone()
                    );
                }
                Operation::AlterSelection(partition) => {
                    observed[4].set(observed[4].get() + 1);
                    let mut roles = serde_json::to_value(context.compiler_input_roles()).unwrap();
                    let field = if partition < 2 {
                        "native_groups"
                    } else {
                        "artifacts"
                    };
                    let selection = roles[0]["selection"][field].as_array_mut().unwrap();
                    match partition {
                        0 | 2 => selection.clear(),
                        1 => selection.push(
                            serde_json::to_value(NativeGroupKey {
                                artifact: root.descriptor.id,
                                original_ordinal: ORDINALS[(initial + 1) % ORDINALS.len()],
                            })
                            .unwrap(),
                        ),
                        _ => selection.push(selection[0].clone()),
                    }
                    let altered: Vec<CompilerInputRole> = serde_json::from_value(roles).unwrap();
                    prop_assert!(restored(&context, &altered).is_err());
                }
            }
            prop_assert_eq!(context.compiler_input_roles(), expected_roles.clone());
            prop_assert_eq!(context.lexical_graph(), original.lexical_graph());
            prop_assert_eq!(
                context.artifact_view().artifact_ids(),
                expected_artifacts.clone()
            );
            prop_assert_eq!(
                context.artifact_view().selected_native_groups(),
                demanded
                    .iter()
                    .map(|ordinal| NativeGroupKey {
                        artifact: root.descriptor.id,
                        original_ordinal: *ordinal,
                    })
                    .collect::<BTreeSet<_>>()
            );
        }
        prop_assert!(demanded.len() >= 2);
        Ok(())
    });
    eprintln!("publication history configured_cases={configured_cases} callbacks={} demand={} compose={} recovery={} reopen={} refusal={}", callbacks.get(), observed[0].get(), observed[1].get(), observed[2].get(), observed[3].get(), observed[4].get());
    result.unwrap();
}

#[test]
fn published_role_refuses_missing_issuer_selection() {
    let (_, publication) = fixture(0);
    let mut roles = serde_json::to_value(publication.context().compiler_input_roles()).unwrap();
    roles[0].as_object_mut().unwrap().remove("selection");
    assert!(
        serde_json::from_value::<Vec<CompilerInputRole>>(roles).is_err(),
        "legacy publication facts cannot reconstruct the issuer's selection from broader custody"
    );
}
