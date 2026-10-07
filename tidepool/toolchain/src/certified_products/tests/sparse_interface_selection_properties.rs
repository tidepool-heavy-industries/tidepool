use super::*;
use crate::artifact_inventory::{
    ArtifactEntry, ArtifactId, ArtifactInventory, ArtifactPayload, ArtifactView,
    CompilerInputProjection,
};
use proptest::prelude::*;
use proptest::test_runner::{Config, FileFailurePersistence, TestCaseError, TestRunner};
use std::collections::BTreeMap;
use std::sync::Arc;
use tidepool_repr::execution_schema::{CachedHomeOwner, InventoryOperation, NativeGroupKey};

type SelectedRoles = BTreeMap<crate::declaration_join::ExactModuleIdentity, RoleFacts>;

#[derive(Clone, Debug, Eq, PartialEq)]
struct RoleFacts {
    interface: ArtifactId,
    original: Option<ArtifactId>,
    native_owner: Option<CachedHomeOwner>,
}

#[derive(Debug, Default)]
struct Coverage {
    histories: usize,
    reconstructions: usize,
    extra_additions: usize,
    extra_removals: usize,
}

fn property_config() -> Config {
    let mut config = Config::default();
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = 128;
    }
    if std::env::var_os("PROPTEST_MAX_SHRINK_ITERS").is_none() {
        config.max_shrink_iters = 4096;
    }
    if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
        config.failure_persistence = Some(Box::new(FileFailurePersistence::Direct(path)));
    }
    config
}

fn original_entry(
    product: crate::recovery_artifacts::CertifiedRecoveryProduct,
) -> Arc<ArtifactEntry> {
    let producer =
        crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(&[3; 32])
            .sha256();
    Arc::new(
        ArtifactEntry::original_with_validation(
            producer,
            product,
            &mut PackageInterfaceValidation::default(),
        )
        .unwrap(),
    )
}

fn canonical_entry(
    product: &crate::recovery_artifacts::CertifiedRecoveryProduct,
) -> Arc<ArtifactEntry> {
    Arc::new(ArtifactEntry::canonical(
        product.module_interface().unwrap().clone(),
    ))
}

fn issued_archive(
    root: &Path,
) -> (
    ArtifactView,
    Vec<Arc<ArtifactEntry>>,
    Vec<Arc<ArtifactEntry>>,
) {
    let fresh = super::issued_interface_selection_history::issued_original(
        &root.join("fresh"),
        "Fresh",
        0x42,
    );
    let side = super::issued_interface_selection_history::issued_original(
        &root.join("side"),
        "Side",
        0x44,
    );
    let other = super::issued_interface_selection_history::issued_original(
        &root.join("other"),
        "Other",
        0x45,
    );
    let fresh_original = original_entry(fresh);
    let side_original = original_entry(side.clone());
    let other_original = original_entry(other.clone());
    let selected_entries = vec![
        Arc::clone(&fresh_original),
        canonical_entry(&side),
        canonical_entry(&other),
    ];
    let native_extras = vec![side_original, other_original];
    let inventory = ArtifactInventory::default();
    let available = inventory
        .admit_recovery_selection(
            &inventory.empty_view(),
            selected_entries
                .iter()
                .cloned()
                .chain(native_extras.iter().cloned())
                .collect(),
            &BTreeSet::<NativeGroupKey>::new(),
        )
        .unwrap();
    (available, selected_entries, native_extras)
}

fn issued_projection(
    entries: &[Arc<ArtifactEntry>],
    selected_interfaces: u8,
) -> CompilerInputProjection {
    let mut selected = vec![Arc::clone(&entries[0])];
    for bit in 0..2 {
        if selected_interfaces & (1 << bit) != 0 {
            selected.push(Arc::clone(&entries[bit + 1]));
        }
    }
    CompilerInputProjection::from_issued_entries(&selected).unwrap()
}

fn expected_roles(entries: &[Arc<ArtifactEntry>], selected_interfaces: u8) -> SelectedRoles {
    let mut expected = BTreeMap::new();
    for (index, entry) in entries.iter().enumerate() {
        if index > 0 && selected_interfaces & (1 << (index - 1)) == 0 {
            continue;
        }
        let facts = match &entry.payload {
            ArtifactPayload::Original(product) => RoleFacts {
                interface: ArtifactEntry::canonical(
                    product
                        .module_interface()
                        .expect("issued original has canonical interface")
                        .clone(),
                )
                .descriptor
                .id,
                original: Some(entry.descriptor.id),
                native_owner: Some(product.owner().clone()),
            },
            ArtifactPayload::Canonical(_) => RoleFacts {
                interface: entry.descriptor.id,
                original: None,
                native_owner: None,
            },
            _ => unreachable!("fixture contains only original and canonical entries"),
        };
        expected.insert(entry.descriptor.owner.clone(), facts);
    }
    expected
}

fn observed_roles(
    projection: &CompilerInputProjection,
    view: &ArtifactView,
) -> Result<SelectedRoles, String> {
    let metadata = view.metadata_snapshot();
    let entries = projection
        .entries_from_metadata(&metadata)
        .map_err(|error| format!("projection does not resolve: {error:?}"))?;
    let mut observed = BTreeMap::new();
    for (owner, entry) in entries {
        let facts = match &entry.payload {
            ArtifactPayload::Original(product) => RoleFacts {
                interface: ArtifactEntry::canonical(
                    product
                        .module_interface()
                        .ok_or_else(|| "selected original has no canonical interface".to_owned())?
                        .clone(),
                )
                .descriptor
                .id,
                original: Some(entry.descriptor.id),
                native_owner: Some(product.owner().clone()),
            },
            ArtifactPayload::Canonical(_) => RoleFacts {
                interface: entry.descriptor.id,
                original: None,
                native_owner: None,
            },
            _ => return Err("selection contains a non-module artifact".into()),
        };
        observed.insert(owner, facts);
    }
    Ok(observed)
}

fn archive_view(
    available: &ArtifactView,
    entries: &[Arc<ArtifactEntry>],
    native_extras: &[Arc<ArtifactEntry>],
    fresh_present: bool,
    interface_mask: u8,
    native_mask: u8,
) -> ArtifactView {
    let mut roots = Vec::new();
    if fresh_present {
        roots.push(entries[0].descriptor.id);
    }
    for bit in 0..2 {
        if interface_mask & (1 << bit) != 0 {
            roots.push(entries[bit + 1].descriptor.id);
        }
        if native_mask & (1 << bit) != 0 {
            roots.push(native_extras[bit].descriptor.id);
        }
    }
    available.select_roots(roots).unwrap()
}

fn source_selection(
    projection: &CompilerInputProjection,
    archive: &ArtifactView,
) -> CertifiedSourceSelection {
    CertifiedSourceSelection::from_compiler_projection(
        projection,
        &archive.metadata_snapshot(),
        &InventoryOperation::new(Default::default()),
    )
    .unwrap()
}

fn assert_roles(
    projection: &CompilerInputProjection,
    view: &ArtifactView,
    expected: &SelectedRoles,
) {
    assert_eq!(observed_roles(projection, view).unwrap(), *expected);
}

#[test]
fn exhaustive_sparse_interface_archive_and_selection_masks_preserve_roles() {
    let root = tempfile::tempdir().unwrap();
    let (available, entries, native_extras) = issued_archive(root.path());

    for selected in 0..4 {
        let issued = issued_projection(&entries, selected);
        let expected = expected_roles(&entries, selected);
        let selection = source_selection(&issued, &available);

        for interface_extras in 0..4 {
            for native_extras_mask in 0..4 {
                let archived = selected | interface_extras;
                let view = archive_view(
                    &available,
                    &entries,
                    &native_extras,
                    true,
                    archived,
                    native_extras_mask,
                );
                let restored = CompilerInputProjection::restore(&view, &issued.roles()).unwrap();
                assert_roles(&restored, &view, &expected);

                let reconstructed = selection.compiler_projection(&view).unwrap();
                assert_roles(&reconstructed, &view, &expected);
                let reconstructed_again = selection.compiler_projection(&view).unwrap();
                assert_eq!(reconstructed_again, reconstructed);

                for role in 0..3 {
                    if role > 0 && selected & (1 << (role - 1)) == 0 {
                        continue;
                    }
                    let missing_fresh = role == 0;
                    let missing_bit = if role == 0 { 0 } else { 1 << (role - 1) };
                    let missing = archive_view(
                        &view,
                        &entries,
                        &native_extras,
                        !missing_fresh,
                        archived & !missing_bit,
                        native_extras_mask & !missing_bit,
                    );
                    assert!(CompilerInputProjection::restore(&missing, &issued.roles()).is_err());
                    assert!(selection.compiler_projection(&missing).is_err());
                }
            }
        }
    }
}

#[test]
fn generated_archive_histories_keep_sparse_selection_exact() {
    let root = tempfile::tempdir().unwrap();
    let (available, entries, native_extras) = issued_archive(root.path());
    let history = (
        0u8..4,
        prop::collection::vec(0u8..16, 0..8),
        prop::collection::vec(0u8..16, 0..8),
    );
    let mut runner = TestRunner::new(property_config());
    let mut coverage = Coverage::default();

    runner
        .run(&history, |(selected, prefix, suffix)| {
            let issued = issued_projection(&entries, selected);
            let expected = expected_roles(&entries, selected);
            let selection = source_selection(&issued, &available);

            // Keep the sequence valid by always retaining every selected role.
            // The fixed 0 -> all -> 0 segment embeds unrelated-archive add and
            // remove transitions in arbitrary prefixes and suffixes.
            let mut archive_states = prefix;
            archive_states.extend([0, 15, 0]);
            archive_states.extend(suffix);
            let mut previous_extras = None;
            for raw_archive in archive_states {
                let archived_interfaces = selected | (raw_archive & 3);
                let archived_native = raw_archive >> 2;
                let extras = (archived_interfaces & !selected) | (archived_native << 2);
                if let Some(previous) = previous_extras {
                    coverage.extra_additions += (extras & !previous).count_ones() as usize;
                    coverage.extra_removals += (previous & !extras).count_ones() as usize;
                }
                previous_extras = Some(extras);
                let view = archive_view(
                    &available,
                    &entries,
                    &native_extras,
                    true,
                    archived_interfaces,
                    archived_native,
                );

                let restored = CompilerInputProjection::restore(&view, &issued.roles())
                    .map_err(|error| TestCaseError::fail(format!("restore failed: {error:?}")))?;
                let restored_roles =
                    observed_roles(&restored, &view).map_err(TestCaseError::fail)?;
                prop_assert_eq!(restored_roles, expected.clone());

                let reconstructed = selection.compiler_projection(&view).map_err(|error| {
                    TestCaseError::fail(format!("reconstruction failed: {error:?}"))
                })?;
                let actual = observed_roles(&reconstructed, &view).map_err(TestCaseError::fail)?;
                prop_assert_eq!(actual, expected.clone());
                let repeated = selection.compiler_projection(&view).map_err(|error| {
                    TestCaseError::fail(format!("repeat reconstruction failed: {error:?}"))
                })?;
                prop_assert_eq!(repeated, reconstructed);
                coverage.reconstructions += 2;
            }
            coverage.histories += 1;
            Ok(())
        })
        .unwrap();

    eprintln!("sparse interface archive history coverage: {coverage:?}");
    assert!(coverage.histories > 0, "no generated histories executed");
    assert!(
        coverage.reconstructions >= coverage.histories * 6,
        "{coverage:?}"
    );
    assert!(coverage.extra_additions > 0, "{coverage:?}");
    assert!(coverage.extra_removals > 0, "{coverage:?}");
}
