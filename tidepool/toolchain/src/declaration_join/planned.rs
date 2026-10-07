//! Same-offer original declaration certification. The owning offer binds the
//! worker inventory to its request, final source, and sealed interface bytes.

use super::*;
use crate::artifacts::SealedTurnProducts;
use crate::declaration_context::{ExactProductAdmission, ExactSourceAdmission};
use std::collections::{BTreeMap, BTreeSet};

/// Retain interfaces reachable from selected originals, including source
/// imports with no native product. Other whole-check consumers stay transient.
pub(super) fn authored_interface_context(
    baseline: Option<&Arc<ExactDeclarationContext>>,
    compiled: &crate::artifact_inventory::ArtifactView,
    original_owners: impl IntoIterator<Item = ExactModuleIdentity>,
    source_imports: &BTreeMap<ExactModuleIdentity, Vec<ExactModuleIdentity>>,
) -> Result<Arc<ExactDeclarationContext>, CompileError> {
    let mut owners = BTreeSet::new();
    let mut pending = original_owners.into_iter().collect::<Vec<_>>();
    while let Some(owner) = pending.pop() {
        if owners.insert(owner.clone()) {
            pending.extend(source_imports.get(&owner).into_iter().flatten().cloned());
        }
    }
    let retained = compiled
        .interface_owners()
        .into_iter()
        .map(|interface| interface.owner)
        .filter(|owner| owners.contains(owner))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let interfaces = compiled.interface_projection(&retained)?;
    let context = match baseline {
        Some(context) => context.as_ref().clone(),
        None => ExactDeclarationContext::new(&[], &[], vec![])?,
    };
    Ok(Arc::new(context.extend_interface_artifacts(&interfaces)?))
}

// Includes materialization, closure certification and descriptor assembly.
// Caller inventory and final context admission remain outside this stage.
pub(super) fn admit_authored_artifact_closure(
    products: &[CertifiedRecoveryProduct],
    selected_owner: &ExactModuleIdentity,
    toolchain_identity_sha256: [u8; 32],
    evidence: &[crate::cache::ModuleEvidence],
    source_admission: Option<&ExactSourceAdmission>,
    context: Option<&Arc<ExactDeclarationContext>>,
    program_source_lexical: &[ExactLexicalNode],
    includes: &[PathBuf],
) -> Result<
    (
        tempfile::TempDir,
        Vec<DeclarationArtifact>,
        Vec<ExactInterfaceOwner>,
        Vec<ExactLexicalNode>,
        Vec<crate::recovery_artifacts::CertifiedJoinedInterface>,
    ),
    CompileError,
> {
    let started = std::time::Instant::now();
    let result = admit_authored_artifact_closure_inner(
        products,
        selected_owner,
        toolchain_identity_sha256,
        evidence,
        source_admission,
        context,
        program_source_lexical,
        includes,
    )?;
    let elapsed = started.elapsed();
    let bytes = products
        .iter()
        .flat_map(|product| product.original_byte_anchors())
        .map(|bytes| bytes.len() as u64)
        .sum();
    crate::timing::record_stage_with_owners(
        crate::timing::NO_NODE,
        crate::timing::NO_ROUND,
        "products.authored_closure_admission",
        elapsed,
        bytes,
        products.len(),
    );
    Ok(result)
}

fn admit_authored_artifact_closure_inner(
    products: &[CertifiedRecoveryProduct],
    selected_owner: &ExactModuleIdentity,
    toolchain_identity_sha256: [u8; 32],
    evidence: &[crate::cache::ModuleEvidence],
    source_admission: Option<&ExactSourceAdmission>,
    context: Option<&Arc<ExactDeclarationContext>>,
    program_source_lexical: &[ExactLexicalNode],
    includes: &[PathBuf],
) -> Result<
    (
        tempfile::TempDir,
        Vec<DeclarationArtifact>,
        Vec<ExactInterfaceOwner>,
        Vec<ExactLexicalNode>,
        Vec<crate::recovery_artifacts::CertifiedJoinedInterface>,
    ),
    CompileError,
> {
    let scratch = tempfile::tempdir()?;
    let mut validation = crate::recovery_artifacts::PackageInterfaceValidation::default();
    let references = crate::recovery_artifacts::materialize_certified_products_with_validation(
        scratch.path(),
        toolchain_identity_sha256,
        products,
        &mut validation,
        crate::recovery_artifacts::MaterializationMode::Scratch,
    )
    .map_err(|error| contract(format!("authored artifact closure rejected: {error}")))?;
    let owned = products.iter().collect::<Vec<_>>();
    crate::certified_products::certify_owned_products_in_context_with_validation(
        &owned,
        &[],
        &owned,
        &mut validation,
    )
    .map_err(|error| contract(format!("authored artifact closure rejected: {error}")))?;
    let mut artifacts = Vec::with_capacity(products.len());
    let mut original_imports = Vec::new();
    let mut source_lexical_imports = Vec::new();
    for (product, reference) in products.iter().zip(&references) {
        let owner = product.owner();
        let identity = ExactModuleIdentity {
            unit: owner.unit.clone(),
            module: owner.module.clone(),
        };
        let row = evidence
            .iter()
            .find(|row| !row.boot && row.unit == owner.unit && row.module == owner.module);
        let mut requirements = if let Some(row) = row {
            let mut imports = Vec::new();
            for imported in &row.imports {
                let Some(path) = &imported.selected else {
                    continue;
                };
                let import_owner = evidence.iter().find(|candidate| {
                    !candidate.boot
                        && candidate.module == imported.module
                        && candidate.source == *path
                });
                if let Some(import_owner) = import_owner {
                    let key = ExactModuleIdentity {
                        unit: import_owner.unit.clone(),
                        module: import_owner.module.clone(),
                    };
                    if !imports.contains(&key) {
                        imports.push(key);
                    }
                } else if includes.iter().any(|root| path.starts_with(root)) {
                    return Err(contract(
                        "authored home import has no exact dependency owner",
                    ));
                }
            }
            if let Some(admitted) = source_admission {
                let exact = admitted
                    .exact_imports
                    .get(&identity)
                    .ok_or_else(|| contract("fresh authored module lacks exact import witness"))?;
                imports.extend_from_slice(exact);
            }
            imports.sort();
            imports.dedup();
            original_imports.push(ExactInterfaceOwner {
                owner: identity.clone(),
                requirements: imports.clone(),
            });
            imports
        } else {
            context
                .and_then(|context| {
                    context
                        .interface_owners()
                        .into_iter()
                        .find(|entry| entry.owner == identity)
                })
                .ok_or_else(|| {
                    contract("certified product absent from source graph and exact context")
                })?
                .requirements
                .clone()
        };
        requirements.extend(
            crate::certified_products::original_interface_requirements(product)
                .map_err(|error| {
                    contract(format!("original interface requirements rejected: {error}"))
                })?
                .into_keys()
                .map(|(unit, module)| ExactModuleIdentity { unit, module }),
        );
        requirements.sort();
        requirements.dedup();
        let requirements = requirements
            .into_iter()
            .map(|owner| (owner.unit, owner.module))
            .collect();
        let interface_path = scratch.path().join(&reference.interface_path);
        let product_path = scratch.path().join(&reference.product_path);
        artifacts.push(DeclarationArtifact {
            interface: ExactIfaceArtifact {
                unit: owner.unit.clone(),
                module: owner.module.clone(),
                path: interface_path,
                sha256: sha256(product.interface_bytes()),
                requirements,
            },
            product: Some(ModuleSnapshot {
                module: owner.module.clone(),
                path: product_path,
                sha256: sha256(product.product_bytes()),
            }),
        });
    }
    if let Some(admitted) = source_admission {
        let mut available_owners = products
            .iter()
            .map(|product| ExactModuleIdentity {
                unit: product.owner().unit.clone(),
                module: product.owner().module.clone(),
            })
            .collect::<std::collections::BTreeSet<_>>();
        if let Some(context) = context {
            available_owners.extend(
                context
                    .artifact_view()
                    .descriptors()
                    .into_iter()
                    .map(|descriptor| descriptor.owner),
            );
        }
        let source_imports = admitted.home_imports()?;
        let inherited_source = merge_program_source_lexical(
            context.map_or(&[], |context| context.lexical_graph()),
            program_source_lexical,
        )?;
        if tracing::enabled!(
            target: "tidepool_toolchain::planned_source_admission",
            tracing::Level::DEBUG
        ) {
            let witness_source = bounded_source_path(admitted.witness.source_path());
            let source_owner = admitted
                .evidence
                .modules
                .iter()
                .find(|module| {
                    !module.boot && module.source.as_path() == admitted.witness.source_path()
                })
                .map(|module| ExactModuleIdentity {
                    unit: module.unit.clone(),
                    module: module.module.clone(),
                });
            let witness_source_owner_count = admitted
                .evidence
                .modules
                .iter()
                .filter(|module| {
                    !module.boot && module.source.as_path() == admitted.witness.source_path()
                })
                .count();
            let evidence_source_owners = admitted
                .evidence
                .modules
                .iter()
                .take(64)
                .map(|module| {
                    (
                        module.unit.as_str(),
                        module.module.as_str(),
                        module.boot,
                        bounded_source_path(&module.source),
                    )
                })
                .collect::<Vec<_>>();
            let exact_import_owners = admitted.exact_imports.keys().take(64).collect::<Vec<_>>();
            let selected_original_owners = admitted
                .selected_originals
                .keys()
                .take(64)
                .collect::<Vec<_>>();
            let direct_imports = source_owner
                .as_ref()
                .and_then(|owner| source_imports.get(owner))
                .cloned()
                .unwrap_or_default();
            let inherited = inherited_source
                .iter()
                .map(|node| node.owner.clone())
                .collect::<std::collections::BTreeSet<_>>();
            let available_owner_keys = available_owners.iter().take(64).collect::<Vec<_>>();
            let inherited_owner_keys = inherited.iter().take(64).collect::<Vec<_>>();
            let home_import_rows = source_imports
                .iter()
                .take(64)
                .map(|(owner, requirements)| {
                    (
                        owner,
                        requirements.iter().take(64).collect::<Vec<_>>(),
                        requirements.len().saturating_sub(64),
                    )
                })
                .collect::<Vec<_>>();
            let inherited_lexical_rows = inherited_source
                .iter()
                .take(64)
                .map(|node| {
                    (
                        &node.owner,
                        node.imports.iter().take(64).collect::<Vec<_>>(),
                        node.imports.len().saturating_sub(64),
                    )
                })
                .collect::<Vec<_>>();
            let missing_adjacency = direct_imports
                .iter()
                .filter(|owner| {
                    available_owners.contains(*owner)
                        && !source_imports.contains_key(*owner)
                        && !inherited.contains(*owner)
                })
                .cloned()
                .collect::<Vec<_>>();
            let adjacency_status = direct_imports
                .iter()
                .map(|owner| {
                    (
                        owner,
                        source_imports.contains_key(owner),
                        available_owners.contains(owner),
                        inherited.contains(owner),
                    )
                })
                .take(16)
                .collect::<Vec<_>>();
            tracing::debug!(
                target: "tidepool_toolchain::planned_source_admission",
                witness_source = %witness_source,
                source_owner = ?source_owner,
                witness_source_owner_count,
                evidence_source_owner_count = admitted.evidence.modules.len(),
                evidence_source_owners = ?evidence_source_owners,
                evidence_source_owners_omitted = admitted.evidence.modules.len().saturating_sub(64),
                exact_import_owners = ?exact_import_owners,
                exact_import_owners_omitted = admitted.exact_imports.len().saturating_sub(64),
                selected_original_owners = ?selected_original_owners,
                selected_original_owners_omitted = admitted.selected_originals.len().saturating_sub(64),
                available_owner_count = available_owners.len(),
                available_owner_keys = ?available_owner_keys,
                available_owner_keys_omitted = available_owners.len().saturating_sub(64),
                inherited_owner_count = inherited.len(),
                inherited_owner_keys = ?inherited_owner_keys,
                inherited_owner_keys_omitted = inherited.len().saturating_sub(64),
                inherited_lexical_rows = ?inherited_lexical_rows,
                inherited_lexical_rows_omitted = inherited_source.len().saturating_sub(64),
                home_import_owner_count = source_imports.len(),
                home_import_owners = ?source_imports.keys().take(64).collect::<Vec<_>>(),
                home_import_owners_omitted = source_imports.len().saturating_sub(64),
                home_import_rows = ?home_import_rows,
                home_import_rows_omitted = source_imports.len().saturating_sub(64),
                source_owner_direct_import_count = direct_imports.len(),
                source_owner_direct_imports = ?direct_imports.iter().take(16).collect::<Vec<_>>(),
                source_owner_direct_imports_omitted = direct_imports.len().saturating_sub(16),
                source_owner_adjacency_status = ?adjacency_status,
                missing_adjacency_count = missing_adjacency.len(),
                missing_adjacency = ?missing_adjacency.iter().take(16).collect::<Vec<_>>(),
                missing_adjacency_omitted = missing_adjacency.len().saturating_sub(16),
                "validated planned declaration source imports"
            );
        }
        merge_admitted_source_imports(&mut original_imports, &source_imports, &available_owners)?;
        let mut implementations = context
            .into_iter()
            .flat_map(|context| context.artifact_view().source_implementation_roles())
            .collect::<BTreeMap<_, _>>();
        for product in products {
            let owner = ExactModuleIdentity {
                unit: product.owner().unit.clone(),
                module: product.owner().module.clone(),
            };
            if implementations
                .insert(
                    owner,
                    crate::artifact_inventory::ArtifactKind::OriginalModule,
                )
                .is_some_and(|prior| {
                    prior != crate::artifact_inventory::ArtifactKind::OriginalModule
                })
            {
                return Err(contract(
                    "planned source owner has a conflicting session artifact kind",
                ));
            }
        }
        source_lexical_imports = inherited_source_lexical_imports(
            selected_owner,
            &source_imports,
            &inherited_source,
            &implementations,
            &available_owners,
        )?;
    }
    let joined_interfaces =
        context.map_or_else(Vec::new, |context| context.joined_interfaces().to_vec());
    if let Some(context) = context {
        let inherited = context.materialize_scratch(&scratch)?;
        artifacts.extend(inherited.artifacts.into_iter().filter(|artifact| {
            artifact.product.is_none()
                && !products.iter().any(|product| {
                    product.owner().unit == artifact.interface.unit
                        && product.owner().module == artifact.interface.module
                })
        }));
    }
    Ok((
        scratch,
        artifacts,
        original_imports,
        source_lexical_imports,
        joined_interfaces,
    ))
}

/// Add only source adjacency authenticated by the current exact admission.
/// Interface requirements establish artifact dependencies, not lexical edges.
fn merge_admitted_source_imports(
    original_imports: &mut Vec<ExactInterfaceOwner>,
    admitted: &BTreeMap<ExactModuleIdentity, Vec<ExactModuleIdentity>>,
    available_owners: &std::collections::BTreeSet<ExactModuleIdentity>,
) -> Result<(), CompileError> {
    let mut merged = BTreeMap::new();
    for row in original_imports.drain(..) {
        let mut requirements = row.requirements;
        requirements.sort();
        requirements.dedup();
        if merged.insert(row.owner, requirements).is_some() {
            return Err(contract(
                "planned source closure has duplicate import owners",
            ));
        }
    }
    for (owner, imports) in admitted {
        if !available_owners.contains(owner) {
            continue;
        }
        let mut imports = imports.clone();
        imports.sort();
        imports.dedup();
        if merged
            .get(owner)
            .is_some_and(|existing| existing != &imports)
        {
            return Err(contract(
                "planned source imports differ from exact admission",
            ));
        }
        merged.entry(owner.clone()).or_insert(imports);
    }
    *original_imports = merged
        .into_iter()
        .map(|(owner, requirements)| ExactInterfaceOwner {
            owner,
            requirements,
        })
        .collect();
    Ok(())
}

// Both inputs retain admitted lexical facts; artifact availability alone is
// insufficient. Same-program source selections remain private to the request.
fn merge_program_source_lexical(
    baseline: &[ExactLexicalNode],
    program: &[ExactLexicalNode],
) -> Result<Vec<ExactLexicalNode>, CompileError> {
    let mut inherited = BTreeMap::new();
    for node in baseline.iter().chain(program) {
        if node.owner.module.starts_with("Tidepool.Session.") {
            continue;
        }
        if inherited
            .insert(node.owner.clone(), node.imports.clone())
            .is_some_and(|previous| previous != node.imports)
        {
            return Err(contract(
                "program source imports differ from retained context",
            ));
        }
    }
    Ok(inherited
        .into_iter()
        .map(|(owner, imports)| ExactLexicalNode { owner, imports })
        .collect())
}

fn bounded_source_path(path: &Path) -> String {
    let rendered = path.to_string_lossy();
    if rendered.chars().count() <= 256 {
        rendered.into_owned()
    } else {
        format!("{}…", rendered.chars().take(256).collect::<String>())
    }
}

fn inherited_source_lexical_imports(
    selected_owner: &ExactModuleIdentity,
    admitted: &BTreeMap<ExactModuleIdentity, Vec<ExactModuleIdentity>>,
    inherited: &[ExactLexicalNode],
    implementations: &BTreeMap<ExactModuleIdentity, crate::artifact_inventory::ArtifactKind>,
    available_owners: &std::collections::BTreeSet<ExactModuleIdentity>,
) -> Result<Vec<ExactLexicalNode>, CompileError> {
    let roots = admitted
        .get(selected_owner)
        .ok_or_else(|| contract("planned original lacks its admitted source import row"))?;
    let closure = source_lexical_closure(roots, admitted, inherited, implementations)?;
    if closure
        .lexical
        .iter()
        .any(|node| !available_owners.contains(&node.owner))
    {
        return Err(contract(
            "reachable original source import lacks its exact artifact owner",
        ));
    }
    Ok(closure
        .lexical
        .into_iter()
        .filter(|node| !admitted.contains_key(&node.owner))
        .collect())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlannedInventory {
    original_unit: String,
    original_module: String,
    interface_fingerprint: String,
    selection: PlannedSelection,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlannedSelection {
    exports: Vec<DeclarationExport>,
    instances: InstanceInventory,
    family_closure: Vec<ExportIdentity>,
}

impl PlannedInventory {
    fn validate(
        &self,
        owner: &ExactModuleIdentity,
        interfaces: &[ExactInterfaceOwner],
    ) -> Result<(), CompileError> {
        if self.original_unit != owner.unit
            || self.original_module != owner.module
            || self.interface_fingerprint.len() != 32
            || !self
                .interface_fingerprint
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(contract(
                "planned inventory has a different original interface owner",
            ));
        }
        let owned = |identity: &ExportIdentity| {
            identity.unit == owner.unit && identity.module == owner.module
        };
        let valid_identity = |identity: &ExportIdentity| {
            !identity.unit.is_empty()
                && !identity.module.is_empty()
                && !identity.occurrence.is_empty()
                && (identity.record_parent.is_none()
                    || identity.namespace == ExportNamespace::Field)
                && identity
                    .record_parent
                    .as_ref()
                    .is_none_or(|parent| !parent.is_empty())
        };
        let mut exported = BTreeSet::new();
        for export in &self.selection.exports {
            let head_namespace = match export.kind {
                DeclarationKind::Value => ExportNamespace::Value,
                DeclarationKind::Type | DeclarationKind::Class => ExportNamespace::Type,
            };
            if !owned(&export.head)
                || !valid_identity(&export.head)
                || export.head.namespace != head_namespace
                || !exported.insert(&export.head)
                || export.children.iter().collect::<BTreeSet<_>>().len() != export.children.len()
                || export
                    .children
                    .iter()
                    .any(|child| !owned(child) || !valid_identity(child))
            {
                return Err(contract(
                    "planned authored exports have invalid exact identities",
                ));
            }
        }
        validate_instance_inventory(&self.selection.instances)?;
        if self.selection.instances.classes.iter().any(|entry| {
            !owned(&entry.dfun)
                || !valid_identity(&entry.dfun)
                || !valid_identity(&entry.class)
                || entry
                    .selected_axioms
                    .iter()
                    .any(|axiom| !owned(axiom) || !valid_identity(axiom))
        }) || self
            .selection
            .instances
            .families
            .iter()
            .any(|axiom| !owned(axiom) || !valid_identity(axiom))
        {
            return Err(contract(
                "planned instances are not owned by the original declaration",
            ));
        }
        let closure = self
            .selection
            .family_closure
            .iter()
            .collect::<BTreeSet<_>>();
        if closure.len() != self.selection.family_closure.len()
            || self
                .selection
                .instances
                .families
                .iter()
                .any(|axiom| !closure.contains(axiom))
            || closure.iter().any(|axiom| {
                !valid_identity(axiom)
                    || !interfaces.iter().any(|entry| {
                        entry.owner.unit == axiom.unit && entry.owner.module == axiom.module
                    })
            })
        {
            return Err(contract(
                "planned family closure differs from retained home interfaces",
            ));
        }
        Ok(())
    }
}

/// Called only after the owning offer has bound TPEXACTDECL8 to the original
/// request, final source, reserved module, and sealed original interface SHA.
/// The JSON carries compiler facts; it does not independently authorize them.
#[allow(clippy::too_many_arguments)]
pub(crate) fn certify_same_offer_planned_declaration(
    module: SessionModule,
    normalized_source: &str,
    producer_identity: &[u8],
    sealed: &SealedTurnProducts,
    admission: &ExactProductAdmission<'_>,
    includes: &[PathBuf],
    baseline: Option<&Arc<ExactDeclarationContext>>,
    inventory_json: &[u8],
) -> Result<CertifiedAuthoredDeclaration, CompileError> {
    let source_admission = admission.source;
    let module_name = module.module_name();
    let source_path = source_admission.witness.source_path();
    let source_sha256: [u8; 32] = Sha256::digest(normalized_source.as_bytes()).into();
    if module.kind != SessionModuleKind::Lib
        || module.gen.0 == 0
        || producer_identity.is_empty()
        || includes.is_empty()
        || !source_path.is_absolute()
        || !source_admission
            .witness
            .matches_source(source_path, normalized_source)
        || std::fs::read(source_path)? != normalized_source.as_bytes()
    {
        return Err(contract(
            "planned original source differs from its exact admission",
        ));
    }
    let toolchain_identity_sha256 =
        crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(
            producer_identity,
        )
        .sha256();
    if baseline
        .is_some_and(|context| context.toolchain_identity_sha256() != toolchain_identity_sha256)
    {
        return Err(contract(
            "planned original producer differs from retained context",
        ));
    }
    if inventory_json.len() > 4 * 1024 * 1024 {
        return Err(contract("planned inventory receipt exceeds its bound"));
    }
    let inventory: PlannedInventory = serde_json::from_slice(inventory_json)
        .map_err(|error| contract(format!("invalid planned inventory receipt: {error}")))?;
    let evidence = &source_admission.evidence.modules;
    let candidates = evidence
        .iter()
        .filter(|row| row.module == module_name)
        .collect::<Vec<_>>();
    if candidates.len() != 1
        || candidates[0].boot
        || candidates[0].product != ProductAvailability::Ready
        || !candidates[0].is_generated_source()
    {
        return Err(contract(
            "planned original is absent or ambiguous in its final source graph",
        ));
    }
    let original_owner = ExactModuleIdentity {
        unit: candidates[0].unit.clone(),
        module: module_name,
    };
    // Keep the original graph, its admitted baseline, and native originals
    // promoted from that request's retained canonical Core. Promotions have no
    // fresh source row or prior native owner; their issuer's packet retains
    // the exact newly certified product.
    let products = sealed
        .recovery_products
        .iter()
        .filter(|product| {
            evidence.iter().any(|row| {
                !row.boot
                    && row.unit == product.owner().unit
                    && row.module == product.owner().module
            }) || baseline.is_some_and(|context| {
                context
                    .recovery_products()
                    .iter()
                    .any(|retained| retained.owner() == product.owner())
            }) || sealed.retained_core_products.contains_original(product)
        })
        .cloned()
        .collect::<Vec<_>>();
    let mut owners = BTreeSet::new();
    if products
        .iter()
        .any(|product| !owners.insert((&product.owner().unit, &product.owner().module)))
    {
        return Err(contract(
            "planned original closure has duplicate product owners",
        ));
    }
    let selected = products
        .iter()
        .find(|product| {
            product.owner().unit == original_owner.unit
                && product.owner().module == original_owner.module
        })
        .ok_or_else(|| contract("planned original has no sealed recovery product"))?;
    if selected.source_sha256() != Some(source_sha256) {
        return Err(contract(
            "planned original sealed product has a different source digest",
        ));
    }
    if selected
        .module_interface()
        .map(|interface| interface.origin())
        != Some(
            crate::certified_products::CanonicalOrigin::NativeAuthoredDeclaration {
                generation: module.gen.0,
            },
        )
    {
        return Err(contract(
            "planned original sealed product has a different declaration origin",
        ));
    }
    let artifact_context = authored_interface_context(
        baseline,
        &sealed.artifact_view,
        products.iter().map(|product| ExactModuleIdentity {
            unit: product.owner().unit.clone(),
            module: product.owner().module.clone(),
        }),
        &source_admission.home_imports()?,
    )?;
    let (_scratch, artifacts, original_imports, source_lexical_imports, joined_interfaces) =
        admit_authored_artifact_closure(
            &products,
            &original_owner,
            toolchain_identity_sha256,
            evidence,
            Some(source_admission),
            Some(&artifact_context),
            admission.request.program_source_lexical(),
            includes,
        )?;
    let interfaces = artifacts
        .iter()
        .map(|artifact| ExactInterfaceOwner {
            owner: ExactModuleIdentity {
                unit: artifact.interface.unit.clone(),
                module: artifact.interface.module.clone(),
            },
            requirements: artifact
                .interface
                .requirements
                .iter()
                .map(|(unit, module)| ExactModuleIdentity {
                    unit: unit.clone(),
                    module: module.clone(),
                })
                .collect(),
        })
        .collect::<Vec<_>>();
    inventory.validate(&original_owner, &interfaces)?;
    let artifacts = crate::declaration_context::certified_artifact_view(
        toolchain_identity_sha256,
        &products,
        &interfaces,
        &joined_interfaces,
        &sealed.artifact_view,
        Some(artifact_context.as_ref()),
    )?;
    let compiler_projection =
        authored_compiler_projection(&artifact_context, &artifacts, selected)?;
    Ok(CertifiedAuthoredDeclaration {
        product: selected.clone(),
        artifacts,
        compiler_projection,
        lexical_exports: inventory.selection.exports.clone(),
        introduced_exports: inventory.selection.exports,
        instances: inventory.selection.instances,
        family_closure: inventory.selection.family_closure,
        source_sha256,
        toolchain_identity_sha256,
        original_imports,
        source_lexical_imports,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn module(module: &str) -> ExactModuleIdentity {
        ExactModuleIdentity {
            unit: "main".into(),
            module: module.into(),
        }
    }

    #[test]
    fn authored_interface_context_retains_type_only_source_closure() {
        use crate::artifact_inventory::{ArtifactInventory, ArtifactKind};
        use crate::certified_products::fixture_module_interface;

        let root = module("Authored");
        let source_owner = module("TypeOnlySource");
        let dependency_owner = module("TypeDependency");
        let admitted = BTreeMap::from([
            (root.clone(), vec![source_owner.clone()]),
            (source_owner, vec![dependency_owner.clone()]),
            (dependency_owner, vec![]),
        ]);
        let dependency =
            fixture_module_interface([1; 32], "main", "TypeDependency", BTreeMap::new());
        let source = fixture_module_interface(
            [1; 32],
            "main",
            "TypeOnlySource",
            BTreeMap::from([(
                ("main".into(), "TypeDependency".into()),
                dependency.interface_sha256(),
            )]),
        );
        let compiled = crate::declaration_context::certified_product_artifact_view(
            [1; 32],
            &[],
            &[source, dependency],
            None,
        )
        .unwrap();
        let context =
            authored_interface_context(None, &compiled, [root.clone()], &admitted).unwrap();
        assert!(context.recovery_products().is_empty());
        assert!(context.lexical_graph().is_empty());
        assert!(context
            .artifact_view()
            .source_implementation_roles()
            .is_empty());
        assert!(context
            .artifact_view()
            .descriptors()
            .iter()
            .all(|descriptor| { descriptor.kind == ArtifactKind::CanonicalModuleInterface }));

        let scratch = tempfile::tempdir().unwrap();
        let artifacts = context.materialize_scratch(&scratch).unwrap().artifacts;
        assert_eq!(artifacts.len(), 2);
        assert!(artifacts.iter().all(|artifact| artifact.product.is_none()));
        let source = artifacts
            .iter()
            .find(|artifact| artifact.interface.module == "TypeOnlySource")
            .unwrap();
        assert_eq!(
            std::fs::read(&source.interface.path).unwrap(),
            b"TypeOnlySource"
        );
        assert_eq!(
            source.interface.requirements,
            vec![("main".into(), "TypeDependency".into())]
        );

        let available = |context: &ExactDeclarationContext| {
            context
                .artifact_view()
                .descriptors()
                .into_iter()
                .map(|descriptor| descriptor.owner)
                .chain(std::iter::once(root.clone()))
                .collect()
        };
        inherited_source_lexical_imports(
            &root,
            &admitted,
            &[],
            &BTreeMap::new(),
            &available(&context),
        )
        .unwrap();

        let missing = authored_interface_context(
            None,
            &ArtifactInventory::default().empty_view(),
            [root.clone()],
            &admitted,
        )
        .unwrap();
        assert!(inherited_source_lexical_imports(
            &root,
            &admitted,
            &[],
            &BTreeMap::new(),
            &available(&missing)
        )
        .is_err());
        let wrong = crate::declaration_context::certified_product_artifact_view(
            [1; 32],
            &[],
            &[fixture_module_interface(
                [1; 32],
                "other",
                "TypeOnlySource",
                BTreeMap::new(),
            )],
            None,
        )
        .unwrap();
        let wrong = authored_interface_context(None, &wrong, [root.clone()], &admitted).unwrap();
        assert!(inherited_source_lexical_imports(
            &root,
            &admitted,
            &[],
            &BTreeMap::new(),
            &available(&wrong)
        )
        .is_err());
    }

    #[test]
    fn authored_interface_context_excludes_changed_transient_consumers() {
        use crate::certified_products::{
            fixture_module_interface, fixture_source_module_interface,
        };

        let original = module("Original");
        let source_only = module("TypeOnlySource");
        let probe = module(crate::artifacts::AUTHORED_PRODUCT_PROBE_MODULE);
        let consumer = module("LaterWholeCheck");
        let admitted = BTreeMap::from([
            (original.clone(), vec![source_only.clone()]),
            (source_only.clone(), vec![]),
            (probe.clone(), vec![original.clone()]),
            (consumer.clone(), vec![original.clone()]),
        ]);
        let offer = |source_sha256| {
            crate::declaration_context::certified_product_artifact_view(
                [1; 32],
                &[],
                &[
                    fixture_module_interface([1; 32], "main", "Original", BTreeMap::new()),
                    fixture_module_interface([1; 32], "main", "TypeOnlySource", BTreeMap::new()),
                    fixture_source_module_interface(
                        [1; 32],
                        "main",
                        &probe.module,
                        source_sha256,
                        BTreeMap::new(),
                        None,
                    ),
                    fixture_source_module_interface(
                        [1; 32],
                        "main",
                        &consumer.module,
                        source_sha256,
                        BTreeMap::new(),
                        None,
                    ),
                ],
                None,
            )
            .unwrap()
        };
        let first_offer = offer([1; 32]);
        let second_offer = offer([2; 32]);
        assert!(first_offer.merge(&second_offer).is_err());
        let first =
            authored_interface_context(None, &first_offer, [original.clone()], &admitted).unwrap();
        let second =
            authored_interface_context(Some(&first), &second_offer, [original.clone()], &admitted)
                .unwrap();
        let owners = second
            .artifact_view()
            .descriptors()
            .into_iter()
            .map(|descriptor| descriptor.owner)
            .collect::<BTreeSet<_>>();
        assert_eq!(owners, BTreeSet::from([original, source_only]));
        assert!(!owners.contains(&probe));
        assert!(!owners.contains(&consumer));
        assert!(second.recovery_products().is_empty());
        assert!(second.lexical_graph().is_empty());
    }

    #[test]
    fn authored_closure_reuses_owned_witnesses_and_preserves_legacy_validation() {
        use crate::certified_products::{
            tests::{original_witness_fixture, recovered_witness_fixtures},
            PendingImportOwner, ORIGINAL_PRODUCT_DECODES,
        };
        use tidepool_repr::execution_schema::testing;

        let source = |owner| PendingImportOwner::Source {
            binder: testing::identity("B", "entry"),
            owner,
            original_ordinal: 7,
        };
        let retained = PendingImportOwner::Retained {
            identity: testing::identity("Value", "live"),
            generation: 11,
        };
        let packages = BTreeMap::new();
        let b = original_witness_fixture("B", Some(retained.clone()), 7, &packages);
        let a = original_witness_fixture("A", Some(source(b.owner().clone())), 7, &packages);
        let legacy = vec![a, b];
        let recovered = recovered_witness_fixtures(&legacy);
        let products = recovered
            .iter()
            .map(|row| row.product.clone())
            .collect::<Vec<_>>();
        let root = tempfile::tempdir().unwrap();
        let evidence = products
            .iter()
            .map(|product| crate::cache::ModuleEvidence {
                unit: product.owner().unit.clone(),
                module: product.owner().module.clone(),
                boot: false,
                source: root.path().join(format!("{}.hs", product.owner().module)),
                imports: vec![],
                product: ProductAvailability::Ready,
            })
            .collect::<Vec<_>>();
        let owner = ExactModuleIdentity {
            unit: "fixture".into(),
            module: "A".into(),
        };
        let admit = |selected: &[CertifiedRecoveryProduct]| {
            admit_authored_artifact_closure(
                selected,
                &owner,
                [1; 32],
                &evidence,
                None,
                None,
                &[],
                &[root.path().to_path_buf()],
            )
        };
        let before = ORIGINAL_PRODUCT_DECODES.with(std::cell::Cell::get);
        let (_scratch, artifacts, _, _, _) = admit(&products).unwrap();
        assert_eq!(artifacts.len(), products.len());
        for (artifact, product) in artifacts.iter().zip(&products) {
            assert_eq!(
                std::fs::read(&artifact.interface.path).unwrap(),
                product.interface_bytes()
            );
            assert_eq!(artifact.interface.sha256, sha256(product.interface_bytes()));
            let snapshot = artifact.product.as_ref().unwrap();
            assert_eq!(
                std::fs::read(&snapshot.path).unwrap(),
                product.product_bytes()
            );
            assert_eq!(snapshot.sha256, sha256(product.product_bytes()));
        }
        assert_eq!(ORIGINAL_PRODUCT_DECODES.with(std::cell::Cell::get), before);
        assert!(
            admit(&products[..1]).is_err(),
            "selected source closure must be complete"
        );
        assert!(admit(&[
            products[0].clone(),
            products[0].clone(),
            products[1].clone()
        ])
        .is_err());
        let wrong_version = original_witness_fixture("B", Some(retained), 8, &packages);
        assert!(admit(&[products[0].clone(), wrong_version]).is_err());
        let legacy_before = ORIGINAL_PRODUCT_DECODES.with(std::cell::Cell::get);
        admit(&legacy).unwrap();
        assert!(
            ORIGINAL_PRODUCT_DECODES.with(std::cell::Cell::get) > legacy_before,
            "uncaptured originals still use the full byte validator"
        );
    }

    #[test]
    fn authored_closure_refuses_package_drift_and_zero_group_home_downgrade() {
        use crate::certified_products::{
            tests::{original_witness_fixture, recovered_witness_fixtures},
            PackageInterfaceWitness, PendingImportOwner,
        };
        use tidepool_repr::execution_schema::testing;

        let root = tempfile::tempdir().unwrap();
        let package_path = root.path().join("External.hi");
        std::fs::write(&package_path, [0x43]).unwrap();
        let digest = Sha256::digest([0x43]).into();
        let packages = BTreeMap::from([(
            ("fixture".into(), "External".into()),
            PackageInterfaceWitness {
                selected_path: package_path.clone(),
                sha256: digest,
            },
        )]);
        let consumer = original_witness_fixture(
            "Consumer",
            Some(PendingImportOwner::Package {
                unit: "fixture".into(),
                module: "External".into(),
                binder: testing::identity("External", "entry"),
                interface_digest: digest,
            }),
            7,
            &packages,
        );
        let empty = original_witness_fixture("External", None, 7, &BTreeMap::new());
        let recovered = recovered_witness_fixtures(&[consumer, empty]);
        let products = recovered
            .iter()
            .map(|row| row.product.clone())
            .collect::<Vec<_>>();
        let evidence = products
            .iter()
            .map(|product| crate::cache::ModuleEvidence {
                unit: product.owner().unit.clone(),
                module: product.owner().module.clone(),
                boot: false,
                source: root.path().join(format!("{}.hs", product.owner().module)),
                imports: vec![],
                product: ProductAvailability::Ready,
            })
            .collect::<Vec<_>>();
        let owner = ExactModuleIdentity {
            unit: "fixture".into(),
            module: "Consumer".into(),
        };
        let admit = |selected: &[CertifiedRecoveryProduct]| {
            admit_authored_artifact_closure(
                selected,
                &owner,
                [1; 32],
                &evidence,
                None,
                None,
                &[],
                &[root.path().to_path_buf()],
            )
        };
        admit(&products[..1]).unwrap();
        assert!(
            admit(&products).is_err(),
            "zero-group home cannot be treated as a package"
        );
        std::fs::write(&package_path, [0x44]).unwrap();
        assert!(
            admit(&products[..1]).is_err(),
            "a later admission must reread live package bytes"
        );
    }

    #[test]
    fn planned_source_closure_uses_admitted_retained_owner_adjacency() {
        let root = module("Authored");
        let retained = module("CheckedHomeValue");
        let child = module("CurrentSourceChild");
        let mut imports = vec![ExactInterfaceOwner {
            owner: root.clone(),
            requirements: vec![retained.clone()],
        }];
        let admitted = BTreeMap::from([
            (root.clone(), vec![retained.clone()]),
            (retained.clone(), vec![child.clone()]),
            (child.clone(), vec![]),
        ]);
        let available = [root.clone(), retained.clone(), child.clone()]
            .into_iter()
            .collect();

        merge_admitted_source_imports(&mut imports, &admitted, &available).unwrap();

        let by_owner = imports
            .into_iter()
            .map(|row| (row.owner, row.requirements))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(by_owner[&retained], vec![child.clone()]);
        assert_eq!(by_owner[&child], Vec::<ExactModuleIdentity>::new());
        let lexical = source_lexical_surface(&[root], &by_owner, &[], &BTreeMap::new()).unwrap();
        assert!(lexical.lexical.iter().any(|node| node.owner == retained));
        assert!(lexical.lexical.iter().any(|node| node.owner == child));
    }

    #[test]
    fn planned_certificate_carries_only_reachable_authenticated_inherited_rows() {
        let root = module("Tidepool.Session.Lib.G3");
        let retained = module("CheckedHomeValue");
        let child = module("InheritedChild");
        let unrelated = module("UnrelatedRetainedValue");
        let admitted = BTreeMap::from([(root.clone(), vec![retained.clone()])]);
        let inherited = vec![
            ExactLexicalNode {
                owner: retained.clone(),
                imports: vec![child.clone()],
            },
            ExactLexicalNode {
                owner: child.clone(),
                imports: vec![],
            },
            ExactLexicalNode {
                owner: unrelated.clone(),
                imports: vec![],
            },
        ];
        let implementations = BTreeMap::from([(
            root.clone(),
            crate::artifact_inventory::ArtifactKind::OriginalModule,
        )]);
        let available = [
            root.clone(),
            retained.clone(),
            child.clone(),
            unrelated.clone(),
        ]
        .into_iter()
        .collect();

        let closure = inherited_source_lexical_imports(
            &root,
            &admitted,
            &inherited,
            &implementations,
            &available,
        )
        .unwrap();

        assert_eq!(
            closure,
            vec![
                ExactLexicalNode {
                    owner: retained.clone(),
                    imports: vec![module("InheritedChild")],
                },
                ExactLexicalNode {
                    owner: child,
                    imports: vec![],
                },
            ]
        );
        assert!(!closure.iter().any(|node| node.owner == unrelated));

        let missing =
            inherited_source_lexical_imports(&root, &admitted, &[], &implementations, &available);
        assert!(
            missing.is_err(),
            "an artifact without its lexical proof is insufficient"
        );

        let wrong_unit = ExactModuleIdentity {
            unit: "other".into(),
            module: retained.module.clone(),
        };
        let wrong_adjacency = BTreeMap::from([(root.clone(), vec![wrong_unit.clone()])]);
        let wrong_available = [root.clone(), wrong_unit].into_iter().collect();
        assert!(inherited_source_lexical_imports(
            &root,
            &wrong_adjacency,
            &inherited,
            &implementations,
            &wrong_available,
        )
        .is_err());

        let conflicting = BTreeMap::from([
            (root.clone(), vec![retained.clone()]),
            (retained.clone(), vec![]),
        ]);
        assert!(inherited_source_lexical_imports(
            &root,
            &conflicting,
            &inherited,
            &implementations,
            &available,
        )
        .is_err());
    }

    #[test]
    fn planned_source_closure_retains_same_program_imports_without_public_selection() {
        let root = module("Authored");
        let retained = module("CheckedHomeValue");
        let dependency = module("SelectedDependency");
        let unrelated = module("UnselectedDependency");
        let baseline = vec![];
        let program = vec![
            ExactLexicalNode {
                owner: retained.clone(),
                imports: vec![dependency.clone()],
            },
            ExactLexicalNode {
                owner: dependency.clone(),
                imports: vec![],
            },
            ExactLexicalNode {
                owner: unrelated.clone(),
                imports: vec![],
            },
        ];
        let inherited = merge_program_source_lexical(&baseline, &program).unwrap();
        let admitted = BTreeMap::from([(root.clone(), vec![retained.clone()])]);
        let available = [
            root.clone(),
            retained.clone(),
            dependency.clone(),
            unrelated,
        ]
        .into_iter()
        .collect();
        let imports = inherited_source_lexical_imports(
            &root,
            &admitted,
            &inherited,
            &BTreeMap::new(),
            &available,
        )
        .unwrap();
        assert_eq!(imports.len(), 2);
        assert!(imports.iter().any(|node| node.owner == retained));
        assert!(imports.iter().any(|node| node.owner == dependency));
        assert!(
            baseline.is_empty(),
            "request selection leaves baseline unchanged"
        );
        assert!(merge_program_source_lexical(
            &[ExactLexicalNode {
                owner: retained,
                imports: vec![],
            }],
            &program,
        )
        .is_err());
    }

    #[test]
    fn planned_source_closure_still_refuses_missing_or_conflicting_adjacency() {
        let root = module("Authored");
        let retained = module("CheckedHomeValue");
        let authored = ExactInterfaceOwner {
            owner: root.clone(),
            requirements: vec![retained.clone()],
        };
        let available = [root.clone(), retained.clone()].into_iter().collect();

        let mut missing = vec![authored.clone()];
        merge_admitted_source_imports(&mut missing, &BTreeMap::new(), &available).unwrap();
        let missing = missing
            .into_iter()
            .map(|row| (row.owner, row.requirements))
            .collect();
        assert!(source_lexical_surface(&[root.clone()], &missing, &[], &BTreeMap::new()).is_err());

        let mut conflicting = vec![authored];
        let admitted = BTreeMap::from([(root, vec![])]);
        assert!(merge_admitted_source_imports(&mut conflicting, &admitted, &available).is_err());
    }

    #[test]
    fn planned_source_closure_ignores_adjacency_without_an_exact_artifact_owner() {
        let root = module("Authored");
        let retained = module("CheckedHomeValue");
        let absent = module("UnretainedSource");
        let mut imports = vec![ExactInterfaceOwner {
            owner: root.clone(),
            requirements: vec![retained.clone()],
        }];
        let admitted = BTreeMap::from([
            (root.clone(), vec![retained.clone()]),
            (retained.clone(), vec![absent.clone()]),
            (absent.clone(), vec![]),
        ]);
        let available = [root.clone(), retained.clone()].into_iter().collect();

        merge_admitted_source_imports(&mut imports, &admitted, &available).unwrap();
        let by_owner = imports
            .into_iter()
            .map(|row| (row.owner, row.requirements))
            .collect::<BTreeMap<_, _>>();
        assert!(source_lexical_surface(&[root], &by_owner, &[], &BTreeMap::new()).is_err());
    }

    fn identity(module: &str, namespace: ExportNamespace, occurrence: &str) -> ExportIdentity {
        ExportIdentity {
            unit: "main".into(),
            module: module.into(),
            namespace,
            occurrence: occurrence.into(),
            record_parent: None,
        }
    }

    fn inventory() -> PlannedInventory {
        let mut field = identity("Lib.G7", ExportNamespace::Field, "value");
        field.record_parent = Some("Box".into());
        PlannedInventory {
            original_unit: "main".into(),
            original_module: "Lib.G7".into(),
            interface_fingerprint: "1234567890abcdef1234567890abcdef".into(),
            selection: PlannedSelection {
                exports: vec![DeclarationExport {
                    kind: DeclarationKind::Type,
                    head: identity("Lib.G7", ExportNamespace::Type, "Box"),
                    children: vec![
                        identity("Lib.G7", ExportNamespace::Constructor, "Box"),
                        field,
                    ],
                }],
                instances: InstanceInventory {
                    classes: vec![ClassInstanceEvidence {
                        dfun: identity("Lib.G7", ExportNamespace::Value, "$fEqBox"),
                        class: ExportIdentity {
                            unit: "ghc-internal".into(),
                            module: "GHC.Internal.Classes".into(),
                            namespace: ExportNamespace::Type,
                            occurrence: "Eq".into(),
                            record_parent: None,
                        },
                        selected_axioms: vec![identity(
                            "Lib.G7",
                            ExportNamespace::Type,
                            "D:R:ResultBox",
                        )],
                    }],
                    families: vec![identity("Lib.G7", ExportNamespace::Type, "D:R:ResultBox")],
                },
                family_closure: vec![
                    identity("Lib.G7", ExportNamespace::Type, "D:R:ResultBox"),
                    identity("Retained", ExportNamespace::Type, "D:R:OtherInt"),
                ],
            },
        }
    }

    #[test]
    fn original_inventory_refuses_foreign_exports_and_incomplete_family_closure() {
        let original = ExactModuleIdentity {
            unit: "main".into(),
            module: "Lib.G7".into(),
        };
        let interfaces = vec![
            ExactInterfaceOwner {
                owner: original.clone(),
                requirements: vec![],
            },
            ExactInterfaceOwner {
                owner: ExactModuleIdentity {
                    unit: "main".into(),
                    module: "Retained".into(),
                },
                requirements: vec![],
            },
        ];
        inventory().validate(&original, &interfaces).unwrap();
        let mut changed = inventory();
        changed.original_module = "Lib.G8".into();
        assert!(changed.validate(&original, &interfaces).is_err());
        let mut changed = inventory();
        changed.selection.exports[0].children[0].module = "Old".into();
        assert!(changed.validate(&original, &interfaces).is_err());
        let mut changed = inventory();
        changed.selection.exports[0].children[1].record_parent = Some(String::new());
        assert!(changed.validate(&original, &interfaces).is_err());
        let mut changed = inventory();
        changed.selection.instances.classes[0].dfun.module = "Old".into();
        assert!(changed.validate(&original, &interfaces).is_err());
        let mut changed = inventory();
        changed.selection.family_closure.remove(0);
        assert!(changed.validate(&original, &interfaces).is_err());
        assert!(inventory().validate(&original, &interfaces[..1]).is_err());
        let mut changed = inventory();
        changed
            .selection
            .family_closure
            .push(changed.selection.family_closure[0].clone());
        assert!(changed.validate(&original, &interfaces).is_err());
    }
}
