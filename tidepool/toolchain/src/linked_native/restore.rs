//! Restore exact selected grants and the same immutable component owners.
//! Stored references choose products; no module, symbol or inventory search links code.

use super::*;
use tidepool_repr::native_code::{
    NativeCodeComponent, NativeCodeComponentFacts, NativeCodeEdgeFacts,
};

pub(crate) fn restore_acquired_native_closures(
    components: &BTreeMap<[u8; 32], Arc<NativeCodeComponentFacts>>,
    closures: &BTreeMap<[u8; 32], Arc<NativeAcquiredClosureFacts>>,
    artifacts: &ArtifactView,
    acquired_packages: &BTreeMap<ArtifactId, Arc<CertifiedPackageCodeProduct>>,
    validation: &mut crate::recovery_artifacts::PackageInterfaceValidation,
) -> Result<Vec<Arc<AcquiredNativeClosure>>, CompileError> {
    let operation = Arc::clone(&validation.inventory);
    operation
        .reserve::<([u8; 32], Arc<NativeAcquiredClosureFacts>)>(closures.len())
        .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
    let mut prepared = Vec::new();
    let mut bodies = BTreeMap::<NativeCodeLocation, NativeCodeBody>::new();
    for (identity, facts) in closures {
        crate::host_work::checkpoint()?;
        if facts.version != 1
            || facts
                .occurrences
                .windows(2)
                .any(|pair| pair[0].occurrence >= pair[1].occurrence)
            || facts
                .package_receipts
                .windows(2)
                .any(|pair| pair[0].stage_id >= pair[1].stage_id)
        {
            return Err(refusal("noncanonical acquired closure record"));
        }
        let bindings = facts
            .occurrences
            .iter()
            .filter_map(|row| match row.occurrence {
                NativeOccurrenceId::Home(binding) => Some(binding),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        let home = crate::certified_products::acquire_linked_home_grants(
            artifacts, &bindings, validation,
        )?;
        let selected_occurrences = facts
            .occurrences
            .iter()
            .map(|row| row.occurrence)
            .collect::<BTreeSet<_>>();
        let mut receipts = BTreeMap::new();
        for receipt in &facts.package_receipts {
            if receipt.occurrences.is_empty()
                || receipt
                    .occurrences
                    .windows(2)
                    .any(|pair| pair[0].occurrence >= pair[1].occurrence)
            {
                return Err(refusal("noncanonical selected package receipt"));
            }
            let rows = receipt
                .occurrences
                .iter()
                .map(|row| (row.occurrence, row))
                .collect::<BTreeMap<_, _>>();
            for row in &receipt.occurrences {
                if row.children.iter().any(|child| !rows.contains_key(child)) {
                    return Err(refusal("selected package receipt omits its exact child"));
                }
                if !selected_occurrences.contains(&NativeOccurrenceId::Package {
                    stage_id: receipt.stage_id,
                    occurrence: row.occurrence,
                }) {
                    return Err(refusal(
                        "selected package proof has an ungranted occurrence",
                    ));
                }
                if !acquired_packages.contains_key(&row.product) {
                    return Err(refusal(
                        "selected package product has no exact acquired reference",
                    ));
                }
            }
            receipts.insert(
                receipt.stage_id,
                NativePackageReceipt::from_facts(Arc::clone(receipt), None, &operation)?,
            );
        }
        for row in &facts.occurrences {
            operation
                .reserve::<(NativeCodeLocation, NativeCodeBody)>(1)
                .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
            let body = match row.occurrence {
                NativeOccurrenceId::Home(binding) => {
                    let grant = home
                        .get(&binding)
                        .ok_or_else(|| refusal("recorded HOME grant is absent"))?;
                    NativeCodeBody::Home(CertifiedGroupCode::admit(
                        grant.product().owner().clone(),
                        grant.product().group().clone(),
                    )?)
                }
                NativeOccurrenceId::Package {
                    stage_id,
                    occurrence,
                } => {
                    let receipt = receipts
                        .get(&stage_id)
                        .ok_or_else(|| refusal("recorded package receipt is absent"))?;
                    let selected = receipt
                        .row(occurrence)
                        .ok_or_else(|| refusal("recorded package occurrence is absent"))?;
                    let product = acquired_packages
                        .get(&selected.product)
                        .ok_or_else(|| refusal("recorded package acquisition is absent"))?;
                    if product.artifact_id() != selected.product {
                        return Err(refusal(
                            "acquired package descriptor changed its exact identity",
                        ));
                    }
                    NativeCodeBody::Package(product.code().clone())
                }
            };
            let component = components
                .get(&row.code.component)
                .ok_or_else(|| refusal("recorded code component is absent"))?;
            if row.code.member as usize >= component.nodes.len() {
                return Err(refusal("recorded code member is absent"));
            }
            if let Some(previous) = bodies.insert(row.code, body.clone()) {
                if previous != body {
                    return Err(refusal(
                        "one code location has incompatible acquired definitions",
                    ));
                }
            }
        }
        prepared.push((*identity, Arc::clone(facts), home, receipts));
    }
    let mut dependencies = BTreeMap::<[u8; 32], BTreeSet<[u8; 32]>>::new();
    let mut dependents = BTreeMap::<[u8; 32], BTreeSet<[u8; 32]>>::new();
    for (identity, facts) in components {
        if facts.nodes.is_empty() {
            return Err(refusal("empty recorded code component"));
        }
        let mut children = BTreeSet::new();
        for (member, node) in facts.nodes.iter().enumerate() {
            let member = u32::try_from(member)
                .map_err(|_| refusal("recorded code member index exceeds u32"))?;
            if !bodies.contains_key(&NativeCodeLocation {
                component: *identity,
                member,
            }) {
                return Err(refusal(
                    "recorded component member has no actual acquired body grant",
                ));
            }
            for edge in &node.imports {
                operation
                    .charge(1)
                    .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
                if let NativeCodeEdgeFacts::External { code, .. } = edge {
                    if !components.contains_key(&code.component) {
                        return Err(refusal("external code component is missing"));
                    }
                    children.insert(code.component);
                    dependents
                        .entry(code.component)
                        .or_default()
                        .insert(*identity);
                }
            }
        }
        dependencies.insert(*identity, children);
    }
    let mut ready = dependencies
        .iter()
        .filter_map(|(id, dependencies)| dependencies.is_empty().then_some(*id))
        .collect::<BTreeSet<_>>();
    let mut restored = BTreeMap::new();
    while let Some(identity) = ready.pop_first() {
        crate::host_work::checkpoint()?;
        let facts = &components[&identity];
        let node_bodies = (0..facts.nodes.len())
            .map(|member| {
                bodies
                    .get(&NativeCodeLocation {
                        component: identity,
                        member: member as u32,
                    })
                    .cloned()
                    .ok_or_else(|| refusal("component body is absent"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut external = BTreeMap::new();
        for node in &facts.nodes {
            for edge in &node.imports {
                if let NativeCodeEdgeFacts::External { code, .. } = edge {
                    let component: &Arc<NativeCodeComponent> = restored
                        .get(&code.component)
                        .ok_or_else(|| refusal("dependency component was not restored"))?;
                    let reference = component
                        .member(code.member)
                        .ok_or_else(|| refusal("external code member is missing"))?;
                    external.insert(*code, reference);
                }
            }
        }
        // All current records, exact acquired bodies and external references have
        // been validated above. This cache shares immutable code only; it cannot
        // supply missing grants, products or component facts to this context.
        let component = if let Some(cached) = validation.native_code_components.get(&identity) {
            if cached.facts() != facts.as_ref()
                || node_bodies.iter().enumerate().any(|(member, body)| {
                    cached
                        .member(member as u32)
                        .is_none_or(|code| code.body() != body)
                })
            {
                return Err(refusal(
                    "cached native component differs from current acquired facts",
                ));
            }
            Arc::clone(cached)
        } else {
            let component = NativeCodeComponent::restore(facts, node_bodies, &external, &operation)
                .map_err(|error| {
                    refusal(&format!("recorded native component rejected: {error}"))
                })?;
            if component.identity() != &identity {
                return Err(refusal("recorded native component commitment differs"));
            }
            validation
                .native_code_components
                .insert(identity, Arc::clone(&component));
            component
        };
        restored.insert(identity, component);
        for parent in dependents.get(&identity).into_iter().flatten() {
            let children = dependencies
                .get_mut(parent)
                .ok_or_else(|| refusal("dependency parent is missing"))?;
            children.remove(&identity);
            if children.is_empty() {
                ready.insert(*parent);
            }
        }
    }
    if restored.len() != components.len() {
        return Err(refusal("cycles between recorded native components"));
    }
    let mut result = Vec::new();
    for (identity, facts, home, receipts) in prepared {
        let mut nodes = Vec::new();
        for row in &facts.occurrences {
            let component = restored
                .get(&row.code.component)
                .ok_or_else(|| refusal("acquired component was not restored"))?;
            let code = component
                .member(row.code.member)
                .ok_or_else(|| refusal("acquired code member was not restored"))?;
            let acquisition = match row.occurrence {
                NativeOccurrenceId::Home(binding) => NativeAcquisition::Home {
                    grant: home
                        .get(&binding)
                        .cloned()
                        .ok_or_else(|| refusal("actual HOME acquisition is absent"))?,
                },
                NativeOccurrenceId::Package {
                    stage_id,
                    occurrence,
                } => {
                    let receipt = receipts
                        .get(&stage_id)
                        .ok_or_else(|| refusal("actual package receipt is absent"))?;
                    let selected = receipt
                        .row(occurrence)
                        .ok_or_else(|| refusal("actual package occurrence is absent"))?;
                    NativeAcquisition::Package {
                        product: Arc::clone(
                            acquired_packages
                                .get(&selected.product)
                                .ok_or_else(|| refusal("actual package product is absent"))?,
                        ),
                        receipt: Arc::clone(receipt),
                        occurrence,
                    }
                }
            };
            nodes.push(NativeAcquiredOccurrence {
                occurrence: row.occurrence,
                code,
                acquisition,
                imports: Arc::clone(&row.imports),
            });
        }
        let closure =
            AcquiredNativeClosure::issue(nodes, artifacts.inventory().empty_view(), &operation)?;
        if closure.identity() != &identity || closure.recorded_facts().as_ref() != facts.as_ref() {
            return Err(refusal(
                "acquired closure facts changed during authenticated restoration",
            ));
        }
        result.push(closure);
    }
    Ok(result)
}
