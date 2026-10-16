//! Complete-original admission keeps explicit roots distinct from reachable
//! dependencies. Expected demand comes from the authored fixture facts.
use super::*;
use crate::artifact_inventory::{
    ArtifactBindingNode, ArtifactEntry, ArtifactInventory, ArtifactView, NativeArtifactDemand,
    NativeRequirementRoot,
};
use crate::recovery_artifacts::CertifiedRecoveryProduct;
use proptest::prelude::*;
use proptest::test_runner::{contextualize_config, Config, FileFailurePersistence, TestRunner};
use std::cell::Cell;

const ORDINALS: [u32; 3] = [3, 11, 29];

fn source(product: &CertifiedRecoveryProduct, ordinal: u32) -> PendingImportOwner {
    PendingImportOwner::Source {
        owner: product.owner().clone(),
        original_ordinal: ordinal,
        binder: testing::identity(&product.owner().module, &format!("entry_{ordinal}")),
    }
}

fn groups(view: &ArtifactView) -> BTreeSet<(String, u32)> {
    let entries = view.entries();
    view.selected_native_groups()
        .iter()
        .map(|key| {
            let entry = entries
                .iter()
                .find(|entry| entry.descriptor.id == key.artifact)
                .unwrap();
            (entry.descriptor.owner.module.clone(), key.original_ordinal)
        })
        .collect()
}

fn explicit_roots(view: &ArtifactView) -> BTreeSet<(String, Option<u32>)> {
    let entries = view.entries();
    view.capture_graph_selection()
        .roots
        .iter()
        .map(|node| {
            let (id, ordinal) = match node {
                ArtifactBindingNode::Artifact(binding) => (binding.artifact, None),
                ArtifactBindingNode::Group(group) => {
                    (group.binding.artifact(), Some(group.original_ordinal))
                }
            };
            let entry = entries
                .iter()
                .find(|entry| entry.descriptor.id == id)
                .unwrap();
            (entry.descriptor.owner.module.clone(), ordinal)
        })
        .collect()
}

fn recover(view: &ArtifactView) -> (ArtifactInventory, ArtifactView) {
    let inventory = ArtifactInventory::default();
    let restored = inventory
        .recover_selection(
            &inventory.empty_view(),
            view.entries(),
            &view.capture_graph_selection(),
            &view.binding_dependencies(),
        )
        .unwrap();
    assert_eq!(
        restored.capture_graph_selection(),
        view.capture_graph_selection()
    );
    assert_eq!(restored.binding_dependencies(), view.binding_dependencies());
    (inventory, restored)
}

#[derive(Clone, Debug)]
enum Operation {
    Complete,
    Recover,
    Read,
}

fn replay(count: usize, explicit_dependency: bool, tail: &[Operation]) {
    let dependency = full_native_fixture(
        "AdmissionDependency",
        ORDINALS.iter().map(|ordinal| (*ordinal, vec![])).collect(),
        7,
    );
    let authored = recovered_witness_fixtures(&[fixture_finalized_product_with_requirements(
        original_groups_fixture(
            "AdmissionAuthored",
            ORDINALS[..count]
                .iter()
                .map(|ordinal| (*ordinal, vec![source(&dependency, *ordinal)]))
                .collect(),
            7,
            &BTreeMap::new(),
        ),
        [1; 32],
        Some(BTreeMap::from([(
            ("fixture".into(), "AdmissionDependency".into()),
            dependency.module_interface().unwrap().interface_sha256(),
        )])),
    )])
    .remove(0)
    .product;
    let products = [authored.clone(), dependency.clone()];
    let entries = products
        .iter()
        .map(|product| Arc::new(ArtifactEntry::original([1; 32], product.clone()).unwrap()))
        .collect::<Vec<_>>();
    let authored_id = entries[0].descriptor.id;
    let dependency_id = entries[1].descriptor.id;
    let inventory = ArtifactInventory::default();
    let available = inventory
        .admit_shared_with_demand(
            &inventory.empty_view(),
            entries.clone(),
            NativeArtifactDemand::ScopeInterfaces,
        )
        .unwrap();
    let mut imports = vec![source(&authored, 3)];
    if explicit_dependency {
        imports.push(source(&dependency, 11));
    }
    let staged = inventory
        .admit_shared_with_demand(
            &available,
            entries.clone(),
            NativeArtifactDemand::CertifiedTargetImports(&imports),
        )
        .unwrap();
    // Remove availability roots before the history. Dependency group 3 remains
    // executable only because authored group 3 reaches it.
    let mut partial_roots = vec![authored_id];
    if explicit_dependency {
        partial_roots.push(dependency_id);
    }
    let partial = staged.select_roots(partial_roots).unwrap();
    drop(staged);
    let mut expected_partial = BTreeSet::from([
        ("AdmissionAuthored".into(), 3),
        ("AdmissionDependency".into(), 3),
    ]);
    if explicit_dependency {
        expected_partial.insert(("AdmissionDependency".into(), 11));
    }
    assert_eq!(groups(&partial), expected_partial);
    assert!(partial.select_admission_roots(vec![authored_id]).is_err());
    assert!(partial.select_admission_roots(vec![dependency_id]).is_err());
    let selection = CertifiedSourceSelection::from_projected_originals(
        &products,
        &InventoryOperation::new(Default::default()),
    )
    .unwrap();
    let partial_graph = partial.capture_graph_selection();
    let expected_groups = ORDINALS[..count]
        .iter()
        .flat_map(|ordinal| {
            [
                ("AdmissionAuthored".into(), *ordinal),
                ("AdmissionDependency".into(), *ordinal),
            ]
        })
        .collect::<BTreeSet<_>>();
    let expected_roots = std::iter::once(("AdmissionAuthored".into(), None))
        .chain(
            ORDINALS[..count]
                .iter()
                .map(|ordinal| ("AdmissionAuthored".into(), Some(*ordinal))),
        )
        .collect::<BTreeSet<_>>();
    let closure = selection
        .selected_authored_original_closure(&partial, "fixture", "AdmissionAuthored")
        .unwrap();
    assert_eq!(closure.products(), &[authored.clone()]);
    let mut current = closure.native_closure.clone();
    assert_eq!(groups(&current), expected_groups);
    assert_eq!(explicit_roots(&current), expected_roots);
    assert_eq!(partial.capture_graph_selection(), partial_graph);
    assert!(!explicit_roots(&current).contains(&("AdmissionDependency".into(), None)));
    // The dependency's existing artifact binding must become an explicit root,
    // and its previously hidden group must become an explicit group root.
    let promotion = inventory
        .admit_shared_with_demand(
            &current,
            vec![entries[1].clone()],
            NativeArtifactDemand::AllGroups,
        )
        .unwrap();
    assert!(
        explicit_roots(&promotion).contains(&("AdmissionDependency".into(), None)),
        "a group carrier in root_entries is not an explicit artifact root"
    );
    let promoted = promotion
        .select_admission_roots(vec![dependency_id])
        .unwrap();
    drop(promotion);
    assert_eq!(
        groups(&promoted),
        ORDINALS
            .iter()
            .map(|ordinal| ("AdmissionDependency".into(), *ordinal))
            .collect()
    );
    assert_eq!(
        explicit_roots(&promoted),
        std::iter::once(("AdmissionDependency".into(), None))
            .chain(
                ORDINALS
                    .iter()
                    .map(|ordinal| ("AdmissionDependency".into(), Some(*ordinal)))
            )
            .collect()
    );
    let (restored_inventory, restored) = recover(&promoted);
    drop(restored);
    assert_eq!(restored_inventory.node_count(), 0);
    drop(promoted);
    drop(closure);
    drop(partial);
    drop(available);
    let mut recovery_inventories = Vec::new();
    for operation in tail {
        match operation {
            Operation::Complete => {
                current = selection
                    .selected_authored_original_closure(&current, "fixture", "AdmissionAuthored")
                    .unwrap()
                    .native_closure;
            }
            Operation::Recover => {
                let (owner, restored) = recover(&current);
                current = restored;
                recovery_inventories.push(owner);
            }
            Operation::Read => {
                current
                    .native_requirements_from_roots(&[NativeRequirementRoot::AllGroups(
                        authored_id,
                    )])
                    .unwrap();
            }
        }
        assert_eq!(groups(&current), expected_groups);
        assert_eq!(explicit_roots(&current), expected_roots);
    }
    let (last_inventory, last) = recover(&current);
    drop(current);
    assert_eq!(groups(&last), expected_groups);
    assert_eq!(explicit_roots(&last), expected_roots);
    assert_eq!(inventory.node_count(), 0);
    assert!(recovery_inventories
        .iter()
        .all(|owner| owner.node_count() == 0));
    drop(last);
    assert_eq!(last_inventory.node_count(), 0);
}

#[test]
fn complete_original_readmission_promotes_hidden_and_inherited_roots() {
    for count in [2, 3] {
        for explicit_dependency in [false, true] {
            replay(
                count,
                explicit_dependency,
                &[
                    Operation::Complete,
                    Operation::Read,
                    Operation::Recover,
                    Operation::Complete,
                    Operation::Recover,
                ],
            );
        }
    }
}

#[test]
fn complete_original_admission_matches_explicit_root_histories() {
    let mut config = Config::default();
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = 64;
    }
    if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
        config.failure_persistence = Some(Box::new(FileFailurePersistence::Direct(path)));
    }
    let mut config = contextualize_config(config);
    config.source_file = Some(file!());
    config.test_name = Some(concat!(
        module_path!(),
        "::complete_original_admission_matches_explicit_root_histories"
    ));
    let configured = config.cases;
    let callbacks = Cell::new(0usize);
    let completed = Cell::new(0usize);
    let strategy = (
        2usize..=3,
        any::<bool>(),
        prop::collection::vec(
            prop_oneof![
                Just(Operation::Complete),
                Just(Operation::Recover),
                Just(Operation::Read)
            ],
            0..12,
        ),
    );
    let result = TestRunner::new(config).run(&strategy, |(count, explicit_dependency, tail)| {
        callbacks.set(callbacks.get() + 1);
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            replay(count, explicit_dependency, &tail)
        })) {
            Ok(()) => {
                completed.set(completed.get() + 1);
                Ok(())
            }
            Err(_) => Err(proptest::test_runner::TestCaseError::fail(format!(
                "count={count}, explicit_dependency={explicit_dependency}, tail={tail:?}"
            ))),
        }
    });
    eprintln!("complete original admission: configured={configured}, callbacks={}, completed={}; each case exercises partial demand, complete readmission, exact selection, dependency promotion, recovery and parent release", callbacks.get(), completed.get());
    result.unwrap();
}
