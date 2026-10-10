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
            // Distinct scopes cannot duplicate one logical dependency in a row.
            if row
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
                    validate_group_row(entry, group.original_ordinal, entries, row)?
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
    let canonical = match &entry.payload {
        ArtifactPayload::Original(product) => Some(
            product
                .module_interface()
                .ok_or_else(|| failure("native canonical carrier missing"))?,
        ),
        ArtifactPayload::Canonical(interface) => Some(interface),
        ArtifactPayload::Interface(_, _) => None,
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
            || required.is_native()
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

fn validate_group_row(
    entry: &ArtifactEntry,
    ordinal: u32,
    entries: &BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
    row: &BTreeSet<(ArtifactBindingNode, ArtifactDependency)>,
) -> Result<(), CompileError> {
    if !entry.is_native() || !entry.native_group_ordinals.contains(&ordinal) {
        return Err(row_failure());
    }
    let mut requirements = entry
        .native_requirements_for(ordinal)
        .iter()
        .collect::<BTreeSet<_>>();
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
        if !requirements.remove(&(required.descriptor.owner.clone(), label.clone())) {
            return Err(row_failure());
        }
        match (target, label, &required.payload) {
            (
                ArtifactBindingNode::Group(group),
                ArtifactDependency::NativeGroup {
                    dependent_ordinal,
                    required_ordinal,
                },
                ArtifactPayload::Original(product),
            ) if *dependent_ordinal == ordinal
                && group.original_ordinal == *required_ordinal
                && required.native_group_ordinals.contains(required_ordinal)
                && entry.native_owners.get(&required.descriptor.owner)
                    == Some(&NativeOwnerKey::from_owner(product.owner())) => {}
            (
                ArtifactBindingNode::Artifact(_),
                ArtifactDependency::NativeBinding {
                    dependent_ordinal, ..
                },
                _,
            ) if *dependent_ordinal == ordinal && !required.is_native() => {}
            _ => return Err(row_failure()),
        }
    }
    if !carrier_seen || !requirements.is_empty() {
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
            _ => ConflictOwner::Interface(entry.descriptor.owner.clone()),
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
