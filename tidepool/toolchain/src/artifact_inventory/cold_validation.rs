//! Authenticate selected outgoing rows without choosing owners from ambient custody.
use super::*;

#[cfg(test)]
mod tests;

type BindingRows =
    BTreeMap<ArtifactBindingNode, BTreeSet<(ArtifactBindingNode, ArtifactDependency)>>;

#[derive(Clone, Copy, Debug, Default)]
struct ValidationWork {
    rows: u64,
    edges: u64,
    competing_keys: u64,
    reverse_searches: u64,
    reverse_nodes: u64,
    reverse_edges: u64,
}

#[cfg(test)]
thread_local! {
    static LAST_WORK: std::cell::Cell<ValidationWork> = const {
        std::cell::Cell::new(ValidationWork {
            rows: 0, edges: 0, competing_keys: 0, reverse_searches: 0,
            reverse_nodes: 0, reverse_edges: 0,
        })
    };
}

pub(super) fn validate_selected_rows(
    state: &InventoryState,
    entries: &BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
    plan: &BindingRows,
) -> Result<(), CompileError> {
    let mut work = ValidationWork::default();
    let result = (|| {
        for (source, row) in plan {
            work.rows += 1;
            work.edges += row.len() as u64;
            let entry = entries
                .get(&source.artifact())
                .ok_or_else(|| failure("selected row payload missing"))?;
            if row.iter().any(|(target, _)| !plan.contains_key(target)) {
                return Err(failure("selected row target missing"));
            }
            // Artifact rows have no nominal exact-binding obligations that can
            // justify two scopes of one payload. Group rows check those below.
            if matches!(source, ArtifactBindingNode::Artifact(_))
                && row
                    .iter()
                    .map(|(target, label)| (target.logical(), label))
                    .collect::<BTreeSet<_>>()
                    .len()
                    != row.len()
            {
                return Err(row_failure());
            }
            match source {
                ArtifactBindingNode::Artifact(_) => validate_artifact_row(entry, entries, row)?,
                ArtifactBindingNode::Group(group) => {
                    validate_group_row(entry, group.original_ordinal, entries, plan, row)?
                }
            }
        }
        validate_transitive_conflicts(entries, plan, &mut work)
    })();
    state
        .admission_owner_lookups
        .fetch_add(work.rows + work.edges, Ordering::Relaxed);
    #[cfg(test)]
    {
        state
            .dependency_graph_visits
            .fetch_add(work.reverse_nodes, Ordering::Relaxed);
        LAST_WORK.with(|last| last.set(work));
    }
    result
}

fn row_failure() -> CompileError {
    failure("bound recovery exact edges differ from certified requirements")
}

fn validate_artifact_row(
    entry: &ArtifactEntry,
    entries: &BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
    row: &BTreeSet<(ArtifactBindingNode, ArtifactDependency)>,
) -> Result<(), CompileError> {
    if let ArtifactPayload::PackageCode(product) = &entry.payload {
        return validate_package_row(entry.descriptor.id, product, entries, row);
    }
    let canonical = match &entry.payload {
        ArtifactPayload::Original(product) => Some(product.module_interface()),
        ArtifactPayload::Canonical(interface) => Some(interface),
        ArtifactPayload::Interface(_, _) | ArtifactPayload::PackageCode(_) => None,
    };
    if let Some(canonical) = canonical {
        let sealed = canonical
            .requirements()
            .iter()
            .map(|((unit, module), seal)| {
                (
                    ExactModuleIdentity {
                        unit: unit.clone(),
                        module: module.clone(),
                    },
                    *seal,
                )
            })
            .collect::<BTreeMap<_, _>>();
        if entry.interface_seals != sealed
            || entry.requirements.iter().collect::<BTreeSet<_>>() != sealed.keys().collect()
        {
            return Err(admission_failure(
                ArtifactInventoryFailure::MetadataConflict {
                    artifact: entry.descriptor.id,
                },
            ));
        }
    }
    if let ArtifactPayload::Original(_) = &entry.payload {
        let carrier = ArtifactEntry::canonical(canonical.expect("original canonical").clone());
        if row.len() != 1
            || !row.iter().all(|(target, label)| {
                matches!(target, ArtifactBindingNode::Artifact(binding)
                    if binding.artifact == carrier.descriptor.id)
                    && *label == ArtifactDependency::Interface
            })
        {
            return Err(row_failure());
        }
        return Ok(());
    }
    let mut requirements = entry.requirements.iter().collect::<BTreeSet<_>>();
    for (target, label) in row {
        let ArtifactBindingNode::Artifact(binding) = target else {
            return Err(row_failure());
        };
        let required = entries
            .get(&binding.artifact)
            .ok_or_else(|| failure("selected dependency payload missing"))?;
        if *label != ArtifactDependency::Interface
            || !required.is_compiler_interface()
            || !requirements.remove(&required.descriptor.owner)
        {
            return Err(row_failure());
        }
        if entry
            .interface_seals
            .get(&required.descriptor.owner)
            .is_some_and(|seal| {
                required.descriptor.interface_sha256 != *seal
                    || required.descriptor.producer_sha256 != entry.descriptor.producer_sha256
            })
        {
            return Err(admission_failure(
                ArtifactInventoryFailure::InterfaceSealMismatch {
                    dependent: entry.descriptor.owner.clone(),
                    required: required.descriptor.owner.clone(),
                },
            ));
        }
    }
    if !requirements.is_empty() {
        return Err(row_failure());
    }
    Ok(())
}

fn validate_package_row(
    artifact: ArtifactId,
    product: &crate::certified_products::CertifiedPackageCodeProduct,
    entries: &BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
    row: &BTreeSet<(ArtifactBindingNode, ArtifactDependency)>,
) -> Result<(), CompileError> {
    let invalid =
        |reason| admission_failure(ArtifactInventoryFailure::PackageCode { artifact, reason });
    let expected = product.code().globals().len();
    if row.len() != expected {
        return Err(invalid(PackageCodeRefusal::ImportCount {
            expected,
            actual: row.len(),
        }));
    }
    let mut seen = BTreeSet::new();
    for (target, label) in row {
        let (
            ArtifactBindingNode::Artifact(binding),
            ArtifactDependency::PackageCodeImport { global_index },
        ) = (target, label)
        else {
            return Err(invalid(PackageCodeRefusal::WrongKind));
        };
        if !seen.insert(*global_index) || *global_index as usize >= expected {
            return Err(invalid(PackageCodeRefusal::ImportContract));
        }
        let target = entries
            .get(&binding.artifact)
            .ok_or_else(|| invalid(PackageCodeRefusal::MissingBinding { binding: *binding }))?;
        let ArtifactPayload::PackageCode(target) = &target.payload else {
            return Err(invalid(PackageCodeRefusal::WrongKind));
        };
        crate::linked_native::resolve_package_import(product, *global_index as usize, target)?;
    }
    Ok(())
}

/// Authenticate the exact pair and full symbol without issuing group permission.
fn original_requirement(
    reference: &crate::certified_products::ReceiptImportOwner,
    entries: &BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
    plan: &BindingRows,
) -> Result<(ArtifactBinding, u32), CompileError> {
    let crate::certified_products::ReceiptImportOwner::OriginalBinding {
        interface,
        original,
        ..
    } = reference
    else {
        return Err(row_failure());
    };
    let canonical = entries
        .get(&interface.artifact)
        .ok_or_else(|| failure("nominal Original canonical payload missing"))?;
    let code = entries
        .get(&original.artifact)
        .ok_or_else(|| failure("nominal Original code payload missing"))?;
    if !plan.contains_key(&ArtifactBindingNode::Artifact(*interface))
        || !plan
            .get(&ArtifactBindingNode::Artifact(*original))
            .is_some_and(|row| {
                row.len() == 1
                    && row.contains(&(
                        ArtifactBindingNode::Artifact(*interface),
                        ArtifactDependency::Interface,
                    ))
            })
    {
        return Err(failure("nominal Original exact canonical pair changed"));
    }
    let (_, ordinal) = crate::certified_products::original_reference_payload_member(
        reference, canonical, code, None,
    )
    .map_err(|error| CompileError::CompilerEvidence(Box::new(error)))?;
    Ok((*original, ordinal))
}

/// Cold custody includes nominal bodies even when no group was executable.
/// Live selected-closure checks deliberately do not require unrelated metadata.
pub(super) fn validate_nominal_custody(
    entries: &BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
    plan: &BindingRows,
) -> Result<(), CompileError> {
    let mut seen = BTreeSet::new();
    for node in plan.keys() {
        if !matches!(node, ArtifactBindingNode::Artifact(_)) || !seen.insert(node.artifact()) {
            continue;
        }
        let entry = &entries[&node.artifact()];
        for ordinal in &entry.native_group_ordinals {
            for requirement in entry.native_requirement_refs_for(*ordinal)? {
                if let NativeRequirementRef::Original { reference } = requirement {
                    original_requirement(reference, entries, plan)?;
                }
            }
        }
    }
    Ok(())
}

fn validate_group_row(
    entry: &ArtifactEntry,
    ordinal: u32,
    entries: &BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
    plan: &BindingRows,
    row: &BTreeSet<(ArtifactBindingNode, ArtifactDependency)>,
) -> Result<(), CompileError> {
    if !entry.is_native() || !entry.native_group_ordinals.contains(&ordinal) {
        return Err(row_failure());
    }
    let all_requirements = entry
        .native_requirements_for(ordinal)
        .iter()
        .collect::<BTreeSet<_>>();
    let mut requirements = all_requirements.clone();
    let mut originals = BTreeMap::new();
    for requirement in entry.native_requirement_refs_for(ordinal)? {
        if let NativeRequirementRef::Original { reference } = requirement {
            originals.insert(reference, original_requirement(reference, entries, plan)?);
        }
    }
    let mut matched_originals = BTreeMap::new();
    let mut carrier_seen = false;
    for (target, label) in row {
        let required = entries
            .get(&target.artifact())
            .ok_or_else(|| failure("selected dependency payload missing"))?;
        if *label == ArtifactDependency::Interface {
            if carrier_seen
                || !matches!(target, ArtifactBindingNode::Artifact(binding)
                    if binding.artifact == entry.descriptor.id)
            {
                return Err(row_failure());
            }
            carrier_seen = true;
            continue;
        }
        let source_kind_matches = match (target, label, &required.payload) {
            (
                ArtifactBindingNode::Group(group),
                ArtifactDependency::NativeGroup {
                    dependent_ordinal,
                    required_ordinal,
                },
                ArtifactPayload::Original(product),
            ) => {
                *dependent_ordinal == ordinal
                    && group.original_ordinal == *required_ordinal
                    && required.native_group_ordinals.contains(required_ordinal)
                    && entry.native_owners.get(&required.descriptor.owner)
                        == Some(&NativeOwnerKey::from_owner(product.owner()))
            }
            (
                ArtifactBindingNode::Artifact(_),
                ArtifactDependency::NativeBinding {
                    dependent_ordinal, ..
                },
                _,
            ) => *dependent_ordinal == ordinal && required.is_compiler_interface(),
            _ => false,
        };
        let source_key = (required.descriptor.owner.clone(), label.clone());
        let source_matches = source_kind_matches && all_requirements.contains(&source_key);
        if source_matches && !requirements.remove(&source_key) {
            return Err(row_failure());
        }
        let mut original_matches = false;
        if let (
            ArtifactBindingNode::Group(group),
            ArtifactDependency::NativeGroup {
                dependent_ordinal,
                required_ordinal,
            },
        ) = (target, label)
        {
            if *dependent_ordinal != ordinal || group.original_ordinal != *required_ordinal {
                return Err(row_failure());
            }
            let carrier = plan.get(target).and_then(|child| {
                child.iter().find_map(|(node, edge)| {
                    (*edge == ArtifactDependency::Interface).then_some(*node)
                })
            });
            for (reference, (original, expected_ordinal)) in &originals {
                if *expected_ordinal == *required_ordinal
                    && carrier == Some(ArtifactBindingNode::Artifact(*original))
                {
                    if matched_originals.insert(*reference, *group).is_some() {
                        return Err(row_failure());
                    }
                    original_matches = true;
                }
            }
        }
        if !source_matches && !original_matches {
            return Err(row_failure());
        }
    }
    if !carrier_seen || !requirements.is_empty() || matched_originals.len() != originals.len() {
        return Err(row_failure());
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum ConflictOwner {
    Interface(ExactModuleIdentity),
    Native(NativeOwnerKey),
}

fn validate_transitive_conflicts(
    entries: &BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
    plan: &BindingRows,
    work: &mut ValidationWork,
) -> Result<(), CompileError> {
    let mut owners =
        BTreeMap::<ConflictOwner, BTreeMap<ArtifactId, Vec<ArtifactBindingNode>>>::new();
    for node in plan.keys() {
        let entry = &entries[&node.artifact()];
        let owner = match &entry.payload {
            ArtifactPayload::Original(product) => {
                ConflictOwner::Native(NativeOwnerKey::from_owner(product.owner()))
            }
            ArtifactPayload::PackageCode(_) => continue,
            ArtifactPayload::Canonical(_) | ArtifactPayload::Interface(_, _) => {
                ConflictOwner::Interface(entry.descriptor.owner.clone())
            }
        };
        owners
            .entry(owner)
            .or_default()
            .entry(node.artifact())
            .or_default()
            .push(*node);
    }
    owners.retain(|_, payloads| payloads.len() > 1);
    work.competing_keys = owners.len() as u64;
    if owners.is_empty() {
        return Ok(());
    }
    let mut incoming = BTreeMap::<ArtifactBindingNode, Vec<ArtifactBindingNode>>::new();
    for (source, row) in plan {
        for (target, _) in row {
            incoming.entry(*target).or_default().push(*source);
        }
    }
    // Work scales with the competing payloads' reverse closures. Most exact
    // contexts have no competing owner keys and perform no reachability search.
    for (owner, payloads) in owners {
        let mut reached_by = BTreeMap::new();
        for (payload, occurrences) in payloads {
            work.reverse_searches += 1;
            let mut pending = occurrences;
            let mut seen = BTreeSet::new();
            while let Some(node) = pending.pop() {
                if !seen.insert(node) {
                    continue;
                }
                work.reverse_nodes += 1;
                if let Some(previous) = reached_by.insert(node, payload) {
                    if previous != payload {
                        let owner = match owner {
                            ConflictOwner::Interface(owner) => owner,
                            ConflictOwner::Native(owner) => owner.owner,
                        };
                        return Err(admission_failure(ArtifactInventoryFailure::OwnerConflict {
                            owner,
                        }));
                    }
                }
                if let Some(parents) = incoming.get(&node) {
                    work.reverse_edges += parents.len() as u64;
                    pending.extend(parents);
                }
            }
        }
    }
    Ok(())
}
