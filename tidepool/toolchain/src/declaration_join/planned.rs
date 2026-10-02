//! Same-offer original declaration certification. The owning offer binds the
//! worker inventory to its request, final source, and sealed interface bytes.

use super::*;
use crate::artifacts::SealedTurnProducts;
use crate::declaration_context::ExactSourceAdmission;
use std::collections::BTreeMap;

pub(super) fn admit_authored_artifact_closure(
    products: &[CertifiedRecoveryProduct],
    toolchain_identity_sha256: [u8; 32],
    evidence: &[crate::cache::ModuleEvidence],
    source_admission: Option<&ExactSourceAdmission>,
    context: Option<&Arc<ExactDeclarationContext>>,
    includes: &[PathBuf],
) -> Result<
    (
        tempfile::TempDir,
        Vec<DeclarationArtifact>,
        Vec<ExactInterfaceOwner>,
        Vec<crate::recovery_artifacts::CertifiedJoinedInterface>,
    ),
    CompileError,
> {
    let scratch = tempfile::tempdir()?;
    let references = crate::recovery_artifacts::materialize_certified_products_with_validation(
        scratch.path(),
        toolchain_identity_sha256,
        products,
        &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
        crate::recovery_artifacts::MaterializationMode::Scratch,
    )
    .map_err(|error| contract(format!("authored artifact closure rejected: {error}")))?;
    let verified = references
        .iter()
        .map(|reference| {
            crate::recovery_artifacts::verify_materialized_ref(scratch.path(), reference)
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| contract(format!("authored artifact closure rejected: {error}")))?;
    crate::certified_products::certify_inherited_products(
        &verified
            .iter()
            .map(|artifact| crate::certified_products::InheritedProductInput { artifact })
            .collect::<Vec<_>>(),
        &[],
    )
    .map_err(|error| contract(format!("authored artifact closure rejected: {error}")))?;
    let mut artifacts = Vec::with_capacity(products.len());
    let mut original_imports = Vec::new();
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
            let inherited = context
                .map(|context| {
                    context
                        .lexical_graph()
                        .iter()
                        .map(|node| node.owner.clone())
                        .collect::<std::collections::BTreeSet<_>>()
                })
                .unwrap_or_default();
            let available_owner_keys = available_owners.iter().take(64).collect::<Vec<_>>();
            let inherited_owner_keys = inherited.iter().take(64).collect::<Vec<_>>();
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
                home_import_owner_count = source_imports.len(),
                home_import_owners = ?source_imports.keys().take(64).collect::<Vec<_>>(),
                home_import_owners_omitted = source_imports.len().saturating_sub(64),
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
    }
    let joined_interfaces =
        context.map_or_else(Vec::new, |context| context.joined_interfaces().to_vec());
    if let Some(context) = context {
        let inherited = context.materialize_scratch(&scratch)?;
        artifacts.extend(
            inherited
                .artifacts
                .into_iter()
                .filter(|artifact| artifact.product.is_none()),
        );
    }
    Ok((scratch, artifacts, original_imports, joined_interfaces))
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

fn bounded_source_path(path: &Path) -> String {
    let rendered = path.to_string_lossy();
    if rendered.chars().count() <= 256 {
        rendered.into_owned()
    } else {
        format!("{}…", rendered.chars().take(256).collect::<String>())
    }
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
        use std::collections::BTreeSet;
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
    source_admission: &ExactSourceAdmission,
    includes: &[PathBuf],
    baseline: Option<&Arc<ExactDeclarationContext>>,
    inventory_json: &[u8],
) -> Result<CertifiedAuthoredDeclaration, CompileError> {
    use std::collections::BTreeSet;
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
    // Later whole-check modules are transient consumers, not original owners.
    // Retain only the original graph and its already admitted baseline closure.
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
            })
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
    let (_scratch, artifacts, original_imports, joined_interfaces) =
        admit_authored_artifact_closure(
            &products,
            toolchain_identity_sha256,
            evidence,
            Some(source_admission),
            baseline,
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
        baseline.map(Arc::as_ref),
    )?;
    Ok(CertifiedAuthoredDeclaration {
        product: selected.clone(),
        artifacts,
        lexical_exports: inventory.selection.exports.clone(),
        introduced_exports: inventory.selection.exports,
        instances: inventory.selection.instances,
        family_closure: inventory.selection.family_closure,
        source_sha256,
        toolchain_identity_sha256,
        original_imports,
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
