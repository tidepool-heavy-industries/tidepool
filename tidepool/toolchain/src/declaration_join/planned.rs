//! Same-offer original declaration certification. The owning offer binds the
//! worker inventory to its request, final source, and sealed interface bytes.

use super::*;
use crate::checked_cell::hex;
use crate::declaration_context::{ExactProductAdmission, ExactSourceAdmission};
use std::collections::{BTreeMap, BTreeSet};

/// Retain issued native roles in the selected authored source closure and their
/// required type interfaces. Other whole-check consumers stay transient.
pub(super) fn authored_interface_context(
    baseline: Option<&Arc<ExactDeclarationContext>>,
    output: &crate::declaration_context::IssuedOriginalOutput,
    original_owners: impl IntoIterator<Item = ExactModuleIdentity>,
    source_imports: &BTreeMap<ExactModuleIdentity, Vec<ExactModuleIdentity>>,
) -> Result<Arc<ExactDeclarationContext>, CompileError> {
    let support = crate::artifact_inventory::CompilerSourceSupport::close_complete_output(
        output.compiler_inputs(),
    )?;
    authored_support_context(baseline, &support, original_owners, source_imports)
}

fn authored_support_context(
    baseline: Option<&Arc<ExactDeclarationContext>>,
    support: &crate::artifact_inventory::CompilerSourceSupport,
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
    let context = match baseline {
        Some(context) => context.as_ref().clone(),
        None => ExactDeclarationContext::empty(),
    };
    let support = support.source_support(&owners)?;
    Ok(Arc::new(context.extend_issued_compiler_support(&support)?))
}

/// Validated authored facts retain the actual issuing owners. Paths and
/// temporary files exist only if the raw inventory consumer requests them.
/// SOURCE visibility is issued separately from this artifact dependency closure.
pub(super) struct ValidatedAuthoredClosure<'a, 'validation> {
    originals: &'a crate::certified_products::SelectedOriginalClosure,
    checked: crate::recovery_artifacts::ValidatedCertifiedProducts<'a, 'validation>,
    pub(super) interfaces: Vec<ExactInterfaceOwner>,
    pub(super) original_imports: Vec<ExactInterfaceOwner>,
}

pub(super) struct AuthoredClosureObservation {
    pub(super) directory: Arc<tempfile::TempDir>,
    pub(super) artifacts: Vec<DeclarationArtifact>,
    pub(super) original_imports: Vec<ExactInterfaceOwner>,
}

impl<'a, 'validation> ValidatedAuthoredClosure<'a, 'validation> {
    pub(super) fn admit(
        originals: &'a crate::certified_products::SelectedOriginalClosure,
        toolchain_identity_sha256: [u8; 32],
        evidence: &[crate::cache::ModuleEvidence],
        source_admission: Option<&ExactSourceAdmission>,
        source_imports: &BTreeMap<ExactModuleIdentity, Vec<ExactModuleIdentity>>,
        context: Option<&Arc<ExactDeclarationContext>>,
        includes: &[PathBuf],
        validation: &'validation mut crate::recovery_artifacts::PackageInterfaceValidation,
    ) -> Result<Self, CompileError> {
        let started = std::time::Instant::now();
        let products = originals.products();
        let mut checked = crate::recovery_artifacts::ValidatedCertifiedProducts::admit(
            toolchain_identity_sha256,
            products,
            validation,
        )
        .map_err(|error| contract(format!("authored artifact closure rejected: {error}")))?;
        originals
            .validate_with(checked.validation())
            .map_err(|error| contract(format!("authored artifact closure rejected: {error}")))?;
        let mut interfaces = Vec::with_capacity(products.len());
        let mut original_imports = Vec::new();
        for product in products {
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
                    let exact = admitted.exact_imports().get(&identity).ok_or_else(|| {
                        contract("fresh authored module lacks exact import witness")
                    })?;
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
            interfaces.push(ExactInterfaceOwner {
                owner: identity,
                requirements,
            });
        }
        {
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
            merge_admitted_source_imports(
                &mut original_imports,
                source_imports,
                &available_owners,
            )?;
        }
        if let Some(context) = context {
            checked
                .include_interfaces(
                    context.toolchain_identity_sha256(),
                    context.inherited_declaration_interface_entries(products)?,
                )
                .map_err(|error| {
                    contract(format!("authored interface closure rejected: {error}"))
                })?;
        }
        interfaces.extend(
            checked
                .interface_entries()
                .map(|entry| ExactInterfaceOwner {
                    owner: entry.descriptor.owner.clone(),
                    requirements: entry.requirements.clone(),
                }),
        );
        crate::timing::record_stage_with_owners(
            crate::timing::NO_NODE,
            crate::timing::NO_ROUND,
            "products.authored_closure_admission",
            started.elapsed(),
            products
                .iter()
                .flat_map(|product| product.original_byte_anchors())
                .map(|bytes| bytes.len() as u64)
                .sum(),
            products.len(),
        );
        Ok(Self {
            originals,
            checked,
            interfaces,
            original_imports,
        })
    }

    pub(super) fn materialize(self) -> Result<AuthoredClosureObservation, CompileError> {
        let directory = Arc::new(tempfile::tempdir()?);
        let materialized = self
            .checked
            .materialize(
                directory.path(),
                crate::recovery_artifacts::MaterializationMode::Scratch,
            )
            .map_err(|error| contract(format!("authored artifact closure rejected: {error}")))?;
        let mut artifacts = Vec::with_capacity(self.interfaces.len());
        for ((product, reference), interface) in self
            .originals
            .products()
            .iter()
            .zip(materialized.products)
            .zip(&self.interfaces)
        {
            artifacts.push(DeclarationArtifact {
                interface: ExactIfaceArtifact {
                    unit: interface.owner.unit.clone(),
                    module: interface.owner.module.clone(),
                    path: directory.path().join(reference.interface_path),
                    sha256: hex(&product.owner().skinny_iface_sha256),
                    requirements: interface
                        .requirements
                        .iter()
                        .map(|owner| (owner.unit.clone(), owner.module.clone()))
                        .collect(),
                },
                product: Some(ModuleSnapshot {
                    module: product.owner().module.clone(),
                    path: directory.path().join(reference.product_path),
                    sha256: hex(&product.owner().product_sha256),
                }),
            });
        }
        for materialized in materialized.interfaces {
            let entry = materialized.entry;
            artifacts.push(DeclarationArtifact {
                interface: ExactIfaceArtifact {
                    unit: entry.descriptor.owner.unit.clone(),
                    module: entry.descriptor.owner.module.clone(),
                    path: directory.path().join(materialized.files.interface_path),
                    sha256: hex(&materialized.files.skinny_iface_sha256),
                    requirements: entry
                        .requirements
                        .iter()
                        .map(|owner| (owner.unit.clone(), owner.module.clone()))
                        .collect(),
                },
                product: None,
            });
        }
        Ok(AuthoredClosureObservation {
            directory,
            artifacts,
            original_imports: self.original_imports,
        })
    }
}

/// Add authenticated compiler HOME adjacency without issuing SOURCE visibility.
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
    originals: &crate::certified_products::SelectedOriginalClosure,
    output: &crate::declaration_context::IssuedOriginalOutput,
    admission: &ExactProductAdmission<'_>,
    includes: &[PathBuf],
    inventory_json: &[u8],
) -> Result<CertifiedAuthoredDeclaration, CompileError> {
    validate_planned_declaration_source(
        &module,
        normalized_source,
        producer_identity,
        admission,
        includes,
    )?;
    let source_path = admission.source().witness().source_path();
    if std::fs::read(source_path)? != normalized_source.as_bytes() {
        return Err(contract(
            "planned original source differs from its exact admission",
        ));
    }
    certify_admitted_planned_declaration(
        module,
        normalized_source,
        producer_identity,
        originals,
        output,
        admission,
        includes,
        inventory_json,
    )
}

/// Consume an actual checked authored stage and its already-issued Original
/// closure. The captured source owner replaces the finite path's live reread.
#[allow(clippy::too_many_arguments)]
pub(crate) fn certify_world_stage_planned_declaration(
    program: &crate::compiler_world::CheckedWorldCellProgram,
    stage: &Arc<crate::compiler_world::CheckedWorldCellStage>,
    normalized_source: &str,
    originals: &crate::certified_products::SelectedOriginalClosure,
    output: &crate::declaration_context::IssuedOriginalOutput,
    admission: &ExactProductAdmission<'_>,
    includes: &[PathBuf],
) -> Result<CertifiedAuthoredDeclaration, CompileError> {
    let authored = NativeAuthoredDeclarationAdmission::from_world_stage(
        program,
        stage,
        admission.source(),
        normalized_source,
    )?;
    let [ordinal] = stage.ordinals() else {
        return Err(contract(
            "authored World stage has multiple execution items",
        ));
    };
    let item = program
        .items()
        .get(*ordinal)
        .ok_or_else(|| contract("authored World stage omits its checked item"))?;
    if !Arc::ptr_eq(item.proof().stage(), stage) {
        return Err(contract("authored World item belongs to another stage"));
    }
    let proof = match item.kind() {
        crate::compiler_world::WorldItemProofKind::Declaration(proof)
        | crate::compiler_world::WorldItemProofKind::Prologue(proof) => proof,
        crate::compiler_world::WorldItemProofKind::Executable(_) => {
            return Err(contract("authored World stage has an executable item"));
        }
    };
    let selected = originals
        .products()
        .iter()
        .find(|product| {
            product.owner().unit == authored.owner.unit
                && product.owner().module == authored.owner.module
        })
        .ok_or_else(|| contract("authored World item has no issued Original"))?;
    let interface = selected
        .module_interface()
        .ok_or_else(|| contract("authored World Original lacks canonical interface"))?;
    if proof.owner() != &authored.owner
        || interface.interface_sha256() != *proof.iface_sha256()
        || selected.interface_bytes() != interface.interface_bytes()
    {
        return Err(contract(
            "authored World item differs from its sealed Original",
        ));
    }
    let module = SessionModule::lib(tidepool_repr::Generation(authored.generation));
    validate_planned_declaration_source(
        &module,
        normalized_source,
        program.producer_identity_bytes(),
        admission,
        includes,
    )?;
    certify_admitted_planned_declaration(
        module,
        normalized_source,
        program.producer_identity_bytes(),
        originals,
        output,
        admission,
        includes,
        proof.inventory_json().as_bytes(),
    )
}

fn validate_planned_declaration_source(
    module: &SessionModule,
    normalized_source: &str,
    producer_identity: &[u8],
    admission: &ExactProductAdmission<'_>,
    includes: &[PathBuf],
) -> Result<(), CompileError> {
    let witness = admission.source().witness();
    if module.kind != SessionModuleKind::Lib
        || module.gen.0 == 0
        || producer_identity.is_empty()
        || includes.is_empty()
        || !witness.source_path().is_absolute()
        || !witness.matches_source(witness.source_path(), normalized_source)
    {
        return Err(contract(
            "planned original source differs from its exact admission",
        ));
    }
    Ok(())
}

/// Authenticate declaration facts against the same source/product admission.
/// Physical source checks belong to the source owner before this shared step.
#[allow(clippy::too_many_arguments)]
fn certify_admitted_planned_declaration(
    module: SessionModule,
    normalized_source: &str,
    producer_identity: &[u8],
    originals: &crate::certified_products::SelectedOriginalClosure,
    output: &crate::declaration_context::IssuedOriginalOutput,
    admission: &ExactProductAdmission<'_>,
    includes: &[PathBuf],
    inventory_json: &[u8],
) -> Result<CertifiedAuthoredDeclaration, CompileError> {
    let source_admission = admission.source();
    let module_name = module.module_name();
    let source_sha256: [u8; 32] = Sha256::digest(normalized_source.as_bytes()).into();
    let toolchain_identity_sha256 =
        crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(
            producer_identity,
        )
        .sha256();
    let source_scope = admission.original_output_source_scope(output.compiler_inputs())?;
    let baseline = admission.context();
    if baseline.toolchain_identity_sha256() != [0; 32]
        && baseline.toolchain_identity_sha256() != toolchain_identity_sha256
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
    let evidence = &source_admission.evidence().modules;
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
    let source_roots = source_admission
        .source_lexical_imports(&original_owner)
        .ok_or_else(|| contract("planned declaration lacks its checked source links"))?
        .into_iter()
        .filter(|owner| source_scope.admits(owner))
        .collect::<Vec<_>>();
    let source_scope = source_scope.project_roots(&source_roots)?;
    let products = originals.products();
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
        Some(baseline),
        output,
        products.iter().map(|product| ExactModuleIdentity {
            unit: product.owner().unit.clone(),
            module: product.owner().module.clone(),
        }),
        source_admission.consumed_home_adjacency(),
    )?;
    let artifact_context = Arc::new(
        artifact_context
            .as_ref()
            .clone()
            .retain_issued_native_inputs(output.compiler_inputs())?,
    );
    let mut validation = crate::recovery_artifacts::PackageInterfaceValidation::default();
    let ValidatedAuthoredClosure {
        interfaces,
        original_imports,
        ..
    } = ValidatedAuthoredClosure::admit(
        originals,
        toolchain_identity_sha256,
        evidence,
        Some(source_admission),
        source_admission.consumed_home_adjacency(),
        Some(&artifact_context),
        includes,
        &mut validation,
    )?;
    inventory.validate(&original_owner, &interfaces)?;
    let compiler_support =
        crate::declaration_context::certified_artifact_view(&originals, artifact_context.as_ref())?;
    Ok(CertifiedAuthoredDeclaration {
        product: selected.clone(),
        compiler_support,
        lexical_exports: inventory.selection.exports.clone(),
        introduced_exports: inventory.selection.exports,
        instances: inventory.selection.instances,
        family_closure: inventory.selection.family_closure,
        source_sha256,
        toolchain_identity_sha256,
        original_imports,
        source_scope,
        source_roots,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authored_interface_census_preserves_inherited_native_interfaces_as_types() {
        use crate::certified_products::{
            fixture_finalized_product,
            tests::{original_groups_fixture_with_interface, recovered_witness_fixtures},
        };
        let legacy = ["Authored", "Schema", "FreshDependency"].map(|module| {
            fixture_finalized_product(
                original_groups_fixture_with_interface(
                    module,
                    vec![(7, vec![])],
                    7,
                    &BTreeMap::new(),
                    module.as_bytes().to_vec(),
                ),
                [1; 32],
            )
        });
        let products = recovered_witness_fixtures(&legacy)
            .into_iter()
            .map(|row| row.product)
            .collect::<Vec<_>>();
        let durable = tempfile::tempdir().unwrap();
        let references = crate::recovery_artifacts::materialize_certified_products(
            durable.path(),
            [1; 32],
            &products,
        )
        .unwrap();
        let context = ExactDeclarationContext::capture_recovery(
            durable.path(),
            &references,
            &[],
            &[],
            vec![],
        )
        .unwrap();
        let before = context.artifact_view().selected_native_groups();
        // Held immutable custody survives its original files. A fresh acquisition
        // must still authenticate those files and reject their corruption.
        std::fs::write(
            durable.path().join(&references[1].product_path),
            b"corrupt native product",
        )
        .unwrap();
        assert!(ExactDeclarationContext::capture_recovery(
            durable.path(),
            &references,
            &[],
            &[],
            vec![]
        )
        .is_err());
        let context = Arc::new(context);
        let scratch = Arc::new(tempfile::tempdir().unwrap());
        let mut validation = crate::recovery_artifacts::PackageInterfaceValidation::default();
        let mut checked = crate::recovery_artifacts::ValidatedCertifiedProducts::admit(
            [1; 32],
            &[],
            &mut validation,
        )
        .unwrap();
        checked
            .include_interfaces(
                context.toolchain_identity_sha256(),
                context
                    .inherited_declaration_interface_entries(&products[..1])
                    .unwrap(),
            )
            .unwrap();
        let types = checked
            .materialize(
                scratch.path(),
                crate::recovery_artifacts::MaterializationMode::Scratch,
            )
            .unwrap()
            .interfaces;
        assert_eq!(
            types
                .iter()
                .map(|artifact| artifact.entry.descriptor.owner.module.as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["Schema", "FreshDependency"])
        );
        for artifact in types {
            let expected = artifact.entry.descriptor.owner.module.as_bytes();
            assert_eq!(
                std::fs::read(scratch.path().join(&artifact.files.interface_path)).unwrap(),
                expected
            );
            assert_eq!(hex(&artifact.files.skinny_iface_sha256), sha256(expected));
        }
        let scratch_files = std::fs::read_dir(scratch.path().join("artifacts"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(
            scratch_files.len(),
            4,
            "two inherited interfaces and their package witnesses"
        );
        assert!(scratch_files
            .iter()
            .all(|name| name.to_str().unwrap().ends_with(".hi")
                || name.to_str().unwrap().ends_with(".hi.packages")));
        assert_eq!(context.artifact_view().selected_native_groups(), before);
    }

    #[test]
    fn authored_certification_preserves_historical_canonical_custody() {
        use crate::artifact_inventory::{
            ArtifactEntry, ArtifactInventory, CompilerInputProjection, IssuedNativeOffer,
        };
        use crate::certified_products::{
            fixture_finalized_product,
            tests::{original_groups_fixture_with_interface, recovered_witness_fixtures},
        };
        let product = |name: &str, version: u8, interface: &[u8]| {
            let value = fixture_finalized_product(
                original_groups_fixture_with_interface(
                    name,
                    vec![(7, vec![]), (11, vec![])],
                    version,
                    &BTreeMap::new(),
                    interface.to_vec(),
                ),
                [1; 32],
            );
            recovered_witness_fixtures(&[value]).remove(0).product
        };
        let old = product("PublicationType", 1, b"historical canonical");
        let current = product("PublicationType", 2, b"selected canonical");
        let authored = product("AuthoredConsumer", 3, b"authored canonical");
        let inventory = ArtifactInventory::default();
        let old_entry = Arc::new(ArtifactEntry::original([1; 32], old).unwrap());
        let old_view = inventory
            .admit_shared(&inventory.empty_view(), vec![old_entry.clone()])
            .unwrap();
        let offer = IssuedNativeOffer::issue(old_entry.descriptor.id, &old_view, None).unwrap();
        let current_entry = Arc::new(ArtifactEntry::original([1; 32], current.clone()).unwrap());
        let current_view = inventory
            .admit_shared(&inventory.empty_view(), vec![current_entry.clone()])
            .unwrap();
        let projection = CompilerInputProjection::from_issued_entries(&[current_entry.clone()])
            .unwrap()
            .interface_only();
        let current_support = crate::declaration_context::recover_fixture_compiler_support(
            projection.clone(),
            current_view,
            [],
        )
        .unwrap();
        let current_view = current_support
            .selected_interfaces()
            .unwrap()
            .select_owners(vec![current_entry.descriptor.owner.clone()])
            .unwrap();
        let baseline = ExactDeclarationContext::new(&[], &[], vec![])
            .unwrap()
            .extend_issued_compiler_support(
                &crate::declaration_context::recover_fixture_compiler_support(
                    projection.clone(),
                    current_view,
                    [offer],
                )
                .unwrap(),
            )
            .unwrap();
        let compiled = crate::declaration_context::certified_product_artifact_view(
            [1; 32],
            &[current.clone(), authored.clone()],
            &[],
            None,
        )
        .unwrap();
        let products = [current, authored.clone()];
        let issued = CompilerInputProjection::from_issued_entries(
            &compiled
                .entries()
                .into_iter()
                .filter(|entry| {
                    matches!(
                        &entry.payload,
                        crate::artifact_inventory::ArtifactPayload::Original(_)
                    )
                })
                .collect::<Vec<_>>(),
        )
        .unwrap();
        let selection =
            crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                &issued,
                &compiled.metadata_snapshot(),
                &tidepool_repr::execution_schema::InventoryOperation::new(Default::default()),
            )
            .unwrap();
        let originals = selection.selected_original_closure(&compiled).unwrap();
        let original_graph = originals.native_closure().capture_graph_selection();
        let available = issued
            .roles()
            .iter()
            .filter_map(|role| role.original())
            .map(|original| {
                let proof =
                    crate::artifact_inventory::NativeOriginalRequirement::from_selected_view(
                        original, &compiled,
                    )
                    .unwrap()
                    .unwrap();
                IssuedNativeOffer::issue(original, &compiled, Some(proof)).unwrap()
            })
            .collect::<Vec<_>>();
        let support = crate::declaration_context::recover_fixture_compiler_support(
            issued.clone(),
            compiled.clone(),
            [],
        )
        .unwrap();
        let narrowed = support
            .select_support_roots(issued.roles().iter().map(|role| role.interface()).collect())
            .unwrap()
            .finalize_namespace()
            .unwrap();
        let scoped = compiled
            .select_issued(
                narrowed
                    .projection()
                    .roles()
                    .iter()
                    .flat_map(|role| std::iter::once(role.interface()).chain(role.original()))
                    .collect(),
                &crate::artifact_inventory::ExactArtifactSelection::capture(narrowed.artifacts()),
            )
            .unwrap();
        assert!(scoped.selected_native_groups().is_empty());
        let baseline = authored_support_context(
            Some(&Arc::new(baseline)),
            &crate::declaration_context::recover_fixture_compiler_support(
                issued.clone(),
                scoped.clone(),
                [],
            )
            .unwrap(),
            products.iter().map(|product| ExactModuleIdentity {
                unit: product.owner().unit.clone(),
                module: product.owner().module.clone(),
            }),
            &BTreeMap::new(),
        )
        .unwrap();
        let baseline = baseline
            .as_ref()
            .clone()
            .retain_issued_native_offers(available)
            .unwrap();
        let support =
            crate::declaration_context::certified_artifact_view(&originals, &baseline).unwrap();
        let view = support.artifacts();
        let graph = view.capture_graph_selection();
        for root in &original_graph.roots {
            if matches!(
                root,
                crate::artifact_inventory::ArtifactBindingNode::Group(_)
            ) {
                assert!(
                    graph.roots.contains(root),
                    "certification lost an issued complete group root"
                );
            }
        }
        for scope in &original_graph.selections {
            assert!(
                graph.selections.contains(scope),
                "certification changed an issued dependency scope"
            );
        }
        assert!(view.artifact_ids().contains(&old_entry.descriptor.id));
        let roles = support.projection();
        assert_eq!(
            roles
                .snapshot_roles()
                .get(&current_entry.descriptor.owner)
                .unwrap()
                .interface(),
            projection.roles()[0].interface()
        );
        assert_eq!(
            roles
                .snapshot_roles()
                .get(&current_entry.descriptor.owner)
                .unwrap()
                .original(),
            Some(current_entry.descriptor.id)
        );
        let authored_entry = ArtifactEntry::original([1; 32], authored).unwrap();
        assert_eq!(
            roles
                .snapshot_roles()
                .get(&authored_entry.descriptor.owner)
                .unwrap()
                .original(),
            Some(authored_entry.descriptor.id)
        );
        assert!(originals
            .native_closure()
            .binding_dependencies()
            .iter()
            .all(|edge| view.binding_dependencies().contains(edge)));
    }

    fn issued_originals(
        products: &[CertifiedRecoveryProduct],
    ) -> Result<crate::certified_products::SelectedOriginalClosure, CompileError> {
        let view = crate::declaration_context::certified_product_artifact_view(
            [1; 32],
            products,
            &[],
            None,
        )?;
        let entries = view
            .entries()
            .into_iter()
            .filter(|entry| {
                matches!(
                    &entry.payload,
                    crate::artifact_inventory::ArtifactPayload::Original(_)
                )
            })
            .collect::<Vec<_>>();
        let projection =
            crate::artifact_inventory::CompilerInputProjection::from_issued_entries(&entries)?;
        let selection =
            crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                &projection,
                &view.metadata_snapshot(),
                &tidepool_repr::execution_schema::InventoryOperation::new(Default::default()),
            )
            .map_err(|error| contract(error.to_string()))?;
        selection
            .selected_original_closure(&view)
            .map_err(|error| contract(error.to_string()))
    }

    fn module(module: &str) -> ExactModuleIdentity {
        ExactModuleIdentity {
            unit: "main".into(),
            module: module.into(),
        }
    }

    #[test]
    fn authored_source_closure_preserves_issued_native_roles_and_refuses_other_versions() {
        use crate::artifact_inventory::{ArtifactPayload, CompilerInputProjection};
        use crate::certified_products::{
            fixture_finalized_product, fixture_finalized_product_with_requirements,
            tests::{original_groups_fixture_with_interface, recovered_witness_fixtures},
        };
        let product = |name: &str, version| {
            let legacy = fixture_finalized_product(
                original_groups_fixture_with_interface(
                    name,
                    vec![(7, vec![]), (11, vec![])],
                    version,
                    &BTreeMap::new(),
                    name.as_bytes().to_vec(),
                ),
                [1; 32],
            );
            recovered_witness_fixtures(&[legacy]).remove(0).product
        };
        let private_type = product("PrivateType", 1);
        let authored = fixture_finalized_product_with_requirements(
            product("Authored", 1),
            [1; 32],
            Some(BTreeMap::from([(
                (
                    private_type.owner().unit.clone(),
                    private_type.owner().module.clone(),
                ),
                private_type.module_interface().unwrap().interface_sha256(),
            )])),
        );
        let authored = recovered_witness_fixtures(&[authored]).remove(0).product;
        let products = [
            authored,
            product("NativeA", 1),
            product("NativeB", 1),
            product("Transient", 1),
            private_type,
        ];
        let module = |name: &str| ExactModuleIdentity {
            unit: products[0].owner().unit.clone(),
            module: name.into(),
        };
        let issued_view = |products: &[CertifiedRecoveryProduct]| {
            let view = crate::declaration_context::certified_product_artifact_view(
                [1; 32],
                products,
                &[],
                None,
            )
            .unwrap();
            let entries = view
                .entries()
                .into_iter()
                .filter(|entry| matches!(&entry.payload, ArtifactPayload::Original(_)))
                .collect::<Vec<_>>();
            let projection = CompilerInputProjection::from_issued_entries(&entries).unwrap();
            (view, projection)
        };
        for reverse in [false, true] {
            let mut ordered = products.to_vec();
            if reverse {
                ordered.reverse();
            }
            let (full, issued) = issued_view(&ordered);
            let support = crate::declaration_context::recover_fixture_compiler_support(
                issued.clone(),
                full.clone(),
                [],
            )
            .unwrap();
            let projected = support
                .selected_interfaces()
                .unwrap()
                .select_owners(issued.snapshot_roles().keys().cloned().collect())
                .unwrap();
            let interfaces = full
                .select_issued(
                    projected
                        .capture_graph_selection()
                        .namespace
                        .iter()
                        .map(|binding| binding.artifact)
                        .collect(),
                    &crate::artifact_inventory::ExactArtifactSelection::capture(&projected),
                )
                .unwrap();
            let view = interfaces.retain_custody(&full).unwrap();
            assert!(view.selected_native_groups().is_empty());
            for dependency in ["NativeA", "NativeB"] {
                let root = module("Authored");
                let dependency = module(dependency);
                let imports = BTreeMap::from([
                    (root.clone(), vec![dependency.clone()]),
                    (dependency.clone(), vec![]),
                    (
                        module("Transient"),
                        vec![module("NativeA"), module("NativeB")],
                    ),
                ]);
                let context = authored_support_context(
                    None,
                    &crate::declaration_context::recover_fixture_compiler_support(
                        issued.clone(),
                        view.clone(),
                        [],
                    )
                    .unwrap(),
                    [root.clone()],
                    &imports,
                )
                .unwrap();
                assert_eq!(
                    context
                        .compiler_input_projection()
                        .snapshot_roles()
                        .keys()
                        .cloned()
                        .collect::<BTreeSet<_>>(),
                    BTreeSet::from([root.clone(), dependency.clone(), module("PrivateType")])
                );
                assert_eq!(
                    context
                        .recovery_products()
                        .iter()
                        .map(|product| {
                            ExactModuleIdentity {
                                unit: product.owner().unit.clone(),
                                module: product.owner().module.clone(),
                            }
                        })
                        .collect::<BTreeSet<_>>(),
                    BTreeSet::from([root.clone(), dependency.clone()]),
                    "unrelated whole-check native products must remain transient"
                );
                let persistent =
                    issued.for_source_owners(&BTreeSet::from([root.clone(), dependency.clone()]));
                persistent.validate(&view).unwrap();
                let roles = context.compiler_input_projection().snapshot_roles();
                for (owner, role) in roles {
                    assert_eq!(
                        persistent.snapshot_roles().get(owner),
                        Some(role),
                        "bounded support and persistent policy must agree on retained roles"
                    );
                }
                assert!(persistent.snapshot_roles()[&module("Transient")]
                    .original()
                    .is_none());
                assert!(!roles.contains_key(&module("Transient")),
                    "persistent type visibility and bounded support have different retention scopes");
                assert!(issued.snapshot_roles()[&module("Transient")].original().is_some(),
                    "the original private issuer keeps implementation choices independently of lexical publication");
                assert!(roles[&root].original().is_some());
                assert!(roles[&dependency].original().is_some());
                assert!(
                    roles[&module("PrivateType")].original().is_none(),
                    "native proof type dependencies do not receive a body reuse offer"
                );
                assert_eq!(
                    context.artifact_view().selected_native_groups(),
                    view.selected_native_groups(),
                    "compiler role transfer cannot demand custody-only native groups"
                );
                assert!(
                    context.lexical_graph().is_empty(),
                    "private compiler roles do not publish source imports"
                );
                let native = view
                    .entries()
                    .into_iter()
                    .find(|entry| {
                        matches!(&entry.payload, ArtifactPayload::Original(product)
                        if product.owner().module == dependency.module)
                    })
                    .unwrap();
                // This control selects a persisted graph prefix, independent of
                // target observations or executable input permission.
                let first = view
                    .inventory()
                    .admit_recovery_selection(
                        &view,
                        vec![Arc::clone(&native)],
                        &BTreeSet::from([crate::artifact_inventory::NativeGroupKey {
                            artifact: native.descriptor.id,
                            original_ordinal: 7,
                        }]),
                    )
                    .unwrap();
                let first = first.retain_custody(&first).unwrap();
                let support = crate::declaration_context::recover_fixture_compiler_support(
                    issued.clone(),
                    first.clone(),
                    [],
                )
                .unwrap()
                .finalize_namespace()
                .unwrap();
                let first = first
                    .select_issued(
                        issued
                            .roles()
                            .iter()
                            .flat_map(|role| {
                                std::iter::once(role.interface()).chain(role.original())
                            })
                            .collect(),
                        &crate::artifact_inventory::ExactArtifactSelection::capture(
                            support.artifacts(),
                        ),
                    )
                    .unwrap();
                let before = first.selected_native_groups();
                assert_eq!(before.len(), 1);
                assert!(before.iter().all(|group| group.original_ordinal == 7));
                let narrowed = authored_support_context(
                    None,
                    &crate::declaration_context::recover_fixture_compiler_support(
                        issued.clone(),
                        first.clone(),
                        [],
                    )
                    .unwrap(),
                    [root.clone()],
                    &imports,
                )
                .unwrap();
                assert_eq!(
                    narrowed.artifact_view().selected_native_groups(),
                    before,
                    "inherited first-group demand must survive without demanding later groups"
                );
                let historical = product(&dependency.module, 2);
                let (conflicting, conflict_selection) = issued_view(&[historical]);
                assert!(
                    authored_support_context(
                        Some(&context),
                        &crate::declaration_context::recover_fixture_compiler_support(
                            conflict_selection.clone(),
                            conflicting.clone(),
                            []
                        )
                        .unwrap(),
                        [dependency.clone()],
                        &BTreeMap::from([(dependency, vec![])])
                    )
                    .is_err(),
                    "another native version cannot replace the selected owner"
                );
            }
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
            &[source.clone(), dependency.clone()],
            None,
        )
        .unwrap();
        let context = authored_support_context(
            None,
            &crate::declaration_context::recover_fixture_compiler_support(
                crate::artifact_inventory::CompilerInputProjection::from_issued_entries(&[
                    Arc::new(crate::artifact_inventory::ArtifactEntry::canonical(source)),
                    Arc::new(crate::artifact_inventory::ArtifactEntry::canonical(
                        dependency,
                    )),
                ])
                .unwrap(),
                compiled.clone(),
                [],
            )
            .unwrap(),
            [root.clone()],
            &admitted,
        )
        .unwrap();
        assert!(context.recovery_products().is_empty());
        assert!(context.lexical_graph().is_empty());
        assert!(context
            .artifact_view()
            .retained_source_implementations()
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
            let interfaces = [
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
            ];
            let entries = interfaces
                .iter()
                .cloned()
                .map(|interface| {
                    Arc::new(crate::artifact_inventory::ArtifactEntry::canonical(
                        interface,
                    ))
                })
                .collect::<Vec<_>>();
            let roles =
                crate::artifact_inventory::CompilerInputProjection::from_issued_entries(&entries)
                    .unwrap();
            let view = crate::declaration_context::certified_product_artifact_view(
                [1; 32],
                &[],
                &interfaces,
                None,
            )
            .unwrap();
            (view, roles)
        };
        let (first_offer, first_roles) = offer([1; 32]);
        let (second_offer, second_roles) = offer([2; 32]);
        assert!(first_offer.merge_fixture_selections(&second_offer).is_err());
        let first = authored_support_context(
            None,
            &crate::declaration_context::recover_fixture_compiler_support(
                first_roles.clone(),
                first_offer.clone(),
                [],
            )
            .unwrap(),
            [original.clone()],
            &admitted,
        )
        .unwrap();
        let second = authored_support_context(
            Some(&first),
            &crate::declaration_context::recover_fixture_compiler_support(
                second_roles.clone(),
                second_offer.clone(),
                [],
            )
            .unwrap(),
            [original.clone()],
            &admitted,
        )
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
    fn authored_closure_reuses_owned_witnesses_and_validates_uncaptured_products() {
        use crate::certified_products::{
            fixture_finalized_product,
            tests::{original_groups_fixture_with_interface, recovered_witness_fixtures},
            PendingImportOwner, ORIGINAL_PRODUCT_DECODES,
        };
        use tidepool_repr::execution_schema::testing;

        let source = |owner| PendingImportOwner::Source {
            binder: testing::identity("B", "entry"),
            owner,
            original_ordinal: 7,
        };
        let packages = BTreeMap::new();
        let b = fixture_finalized_product(
            original_groups_fixture_with_interface(
                "B",
                vec![(7, vec![])],
                7,
                &packages,
                vec![0x42],
            ),
            [1; 32],
        );
        let a = fixture_finalized_product(
            original_groups_fixture_with_interface(
                "A",
                vec![(7, vec![source(b.owner().clone())])],
                7,
                &packages,
                vec![0x41],
            ),
            [1; 32],
        );
        let legacy = vec![a, b];
        assert!(legacy
            .iter()
            .all(|product| product.module_interface().is_some()
                && product.original_native().is_none()));
        let cold_before = ORIGINAL_PRODUCT_DECODES.with(std::cell::Cell::get);
        let recovered = recovered_witness_fixtures(&legacy);
        assert!(
            ORIGINAL_PRODUCT_DECODES.with(std::cell::Cell::get) > cold_before,
            "cold recovery authenticates the uncaptured original bytes"
        );
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
            let originals = issued_originals(selected)?;
            let mut validation = crate::recovery_artifacts::PackageInterfaceValidation::default();
            ValidatedAuthoredClosure::admit(
                &originals,
                [1; 32],
                &evidence,
                None,
                &BTreeMap::from([(owner.clone(), vec![])]),
                None,
                &[root.path().to_path_buf()],
                &mut validation,
            )?
            .materialize()
        };
        let before = ORIGINAL_PRODUCT_DECODES.with(std::cell::Cell::get);
        let originals = issued_originals(&products).unwrap();
        let mut validation = crate::recovery_artifacts::PackageInterfaceValidation::default();
        let ((interfaces, facts_work), (_, facts_copied)) =
            crate::recovery_artifacts::observe_native_materialization(|| {
                let mut facts = ValidatedAuthoredClosure::admit(
                    &originals,
                    [1; 32],
                    &evidence,
                    None,
                    &BTreeMap::from([(owner.clone(), vec![])]),
                    None,
                    &[root.path().to_path_buf()],
                    &mut validation,
                )
                .unwrap();
                let work = facts.checked.validation().work();
                (facts.interfaces, work)
            });
        assert_eq!(
            facts_work.written_bytes, 0,
            "metadata admission writes no scratch files"
        );
        assert_eq!(
            facts_copied, 0,
            "native custody is validated without copying it"
        );
        assert_eq!(
            interfaces
                .iter()
                .map(|row| row.owner.module.as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["A", "B"])
        );
        assert_eq!(
            interfaces,
            vec![
                ExactInterfaceOwner {
                    owner: owner.clone(),
                    requirements: vec![]
                },
                ExactInterfaceOwner {
                    owner: ExactModuleIdentity {
                        unit: "fixture".into(),
                        module: "B".into()
                    },
                    requirements: vec![],
                },
            ],
            "native group dependencies do not invent source interface imports"
        );
        let b_owner = ExactModuleIdentity {
            unit: "fixture".into(),
            module: "B".into(),
        };
        let mut imported_evidence = evidence.clone();
        imported_evidence
            .iter_mut()
            .find(|row| row.module == "A")
            .unwrap()
            .imports
            .push(crate::cache::ModuleImportEvidence {
                qualifier: crate::cache::ImportQualifier::Unqualified,
                module: "B".into(),
                boot: false,
                selected: Some(root.path().join("B.hs")),
            });
        let imports = BTreeMap::from([
            (owner.clone(), vec![b_owner.clone()]),
            (b_owner.clone(), vec![]),
        ]);
        let mut imported_validation =
            crate::recovery_artifacts::PackageInterfaceValidation::default();
        let imported = ValidatedAuthoredClosure::admit(
            &originals,
            [1; 32],
            &imported_evidence,
            None,
            &imports,
            None,
            &[root.path().to_path_buf()],
            &mut imported_validation,
        )
        .unwrap();
        assert_eq!(
            imported.interfaces,
            vec![
                ExactInterfaceOwner {
                    owner: owner.clone(),
                    requirements: vec![b_owner.clone()]
                },
                ExactInterfaceOwner {
                    owner: b_owner,
                    requirements: vec![]
                },
            ],
            "exact selected source-import evidence adds its required interface owner"
        );
        let (observation, (_, physical_copied)) =
            crate::recovery_artifacts::observe_native_materialization(|| admit(&products));
        let observation = observation.unwrap();
        assert!(
            physical_copied > 0,
            "the raw inspection control really materializes native inputs"
        );
        let artifacts = &observation.artifacts;
        assert_eq!(
            interfaces,
            artifacts
                .iter()
                .map(|artifact| ExactInterfaceOwner {
                    owner: ExactModuleIdentity {
                        unit: artifact.interface.unit.clone(),
                        module: artifact.interface.module.clone()
                    },
                    requirements: artifact
                        .interface
                        .requirements
                        .iter()
                        .map(|(unit, module)| ExactModuleIdentity {
                            unit: unit.clone(),
                            module: module.clone()
                        })
                        .collect(),
                })
                .collect::<Vec<_>>()
        );
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
        let repeated = issued_originals(&[
            products[0].clone(),
            products[0].clone(),
            products[1].clone(),
        ])
        .unwrap();
        assert_eq!(
            repeated.products().len(),
            products.len(),
            "repeated custody cannot issue duplicate compiler roles"
        );
        let wrong_version = fixture_finalized_product(
            original_groups_fixture_with_interface(
                "B",
                vec![(7, vec![])],
                8,
                &packages,
                vec![0x42],
            ),
            [1; 32],
        );
        let wrong_version = recovered_witness_fixtures(&[wrong_version])
            .remove(0)
            .product;
        assert_eq!(
            wrong_version.module_interface(),
            products[1].module_interface()
        );
        assert!(admit(&[products[0].clone(), wrong_version]).is_err());
        let cold_before = ORIGINAL_PRODUCT_DECODES.with(std::cell::Cell::get);
        let finalized = issued_originals(&legacy).unwrap();
        assert_eq!(finalized.products().len(), legacy.len());
        assert!(finalized
            .products()
            .iter()
            .all(|product| product.original_native().is_some()));
        assert!(
            ORIGINAL_PRODUCT_DECODES.with(std::cell::Cell::get) > cold_before,
            "inventory admission authenticates uncaptured original bytes"
        );
        let original = &legacy[1];
        let mut changed_bytes = original.product_bytes().to_vec();
        changed_bytes.push(0);
        let changed = CertifiedRecoveryProduct::from_certification(
            original.owner().clone(),
            original.interface_bytes().to_vec(),
            changed_bytes,
            original.package_imports_bytes().to_vec(),
            original.certification_bytes().to_vec(),
        )
        .with_module_interface(original.module_interface().unwrap().clone())
        .unwrap();
        assert!(issued_originals(&[legacy[0].clone(), changed]).is_err());
    }

    #[test]
    fn authored_closure_preserves_package_custody_and_refuses_zero_group_home_downgrade() {
        use crate::certified_products::{
            encode_home_certification, fixture_finalized_product,
            tests::{original_groups_fixture_with_interface, recovered_witness_fixtures},
            PackageInterfaceWitness, PendingImportOwner,
        };
        use tidepool_repr::execution_schema::{
            testing, CachedHomeOwner, ModuleVersion, RawModuleProduct, SymbolIdentity,
        };

        let root = tempfile::tempdir().unwrap();
        let external_unit = "external-package";
        let package_path = root.path().join("External.hi");
        std::fs::write(&package_path, [0x43]).unwrap();
        let digest = Sha256::digest([0x43]).into();
        let packages = BTreeMap::from([(
            (external_unit.into(), "External".into()),
            PackageInterfaceWitness {
                selected_path: package_path.clone(),
                sha256: digest,
            },
        )]);
        let consumer = original_groups_fixture_with_interface(
            "Consumer",
            vec![(
                7,
                vec![PendingImportOwner::Package {
                    unit: external_unit.into(),
                    module: "External".into(),
                    binder: SymbolIdentity {
                        unit: external_unit.into(),
                        ..testing::identity("External", "entry")
                    },
                    interface_digest: digest,
                }],
            )],
            7,
            &packages,
            vec![0x42],
        );
        let interface = vec![0x45];
        let product_bytes =
            tidepool_test_data::prepared_encode::encode_module_products(&[RawModuleProduct {
                unit: external_unit.into(),
                module: "External".into(),
                interface: interface.clone(),
                groups: vec![],
            }]);
        let empty_owner = CachedHomeOwner {
            unit: external_unit.into(),
            module: "External".into(),
            module_version: ModuleVersion([7; 32]),
            skinny_iface_sha256: Sha256::digest(&interface).into(),
            product_sha256: Sha256::digest(&product_bytes).into(),
        };
        let certification = encode_home_certification(&empty_owner, &[], &BTreeMap::new()).unwrap();
        let package_bytes = crate::module_candidates::tests::package_imports_with_roots(
            &empty_owner.unit,
            &empty_owner.module,
            &interface,
            Vec::new(),
        );
        let empty = CertifiedRecoveryProduct::from_certification(
            empty_owner,
            interface,
            product_bytes,
            package_bytes,
            certification,
        );
        let consumer = recovered_witness_fixtures(&[fixture_finalized_product(consumer, [1; 32])])
            .remove(0)
            .product;
        let empty = recovered_witness_fixtures(&[fixture_finalized_product(empty, [1; 32])])
            .remove(0)
            .product;
        issued_originals(std::slice::from_ref(&empty)).unwrap();
        let products = vec![consumer, empty];
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
            let originals = issued_originals(selected)?;
            let mut validation = crate::recovery_artifacts::PackageInterfaceValidation::default();
            ValidatedAuthoredClosure::admit(
                &originals,
                [1; 32],
                &evidence,
                None,
                &BTreeMap::from([(owner.clone(), vec![])]),
                None,
                &[root.path().to_path_buf()],
                &mut validation,
            )?
            .materialize()
        };
        admit(&products[..1]).unwrap();
        assert!(
            admit(&products).is_err(),
            "zero-group home cannot be treated as a package"
        );
        let originals = issued_originals(&products[..1]).unwrap();
        let captured = originals.products()[0]
            .original_native()
            .unwrap()
            .package_interfaces()
            .get(&(external_unit.into(), "External".into()))
            .unwrap()
            .clone();
        assert_eq!(captured.sha256(), &digest);
        assert_eq!(captured.bytes().as_ref(), &[0x43]);
        std::fs::write(&package_path, [0x44]).unwrap();
        assert_eq!(std::fs::read(&package_path).unwrap(), [0x44]);
        // Recovered originals own immutable package interfaces. Their original
        // selection paths do not replace that custody during later admission.
        let mut validation = crate::recovery_artifacts::PackageInterfaceValidation::default();
        let retained = ValidatedAuthoredClosure::admit(
            &originals,
            [1; 32],
            &evidence,
            None,
            &BTreeMap::from([(owner.clone(), vec![])]),
            None,
            &[root.path().to_path_buf()],
            &mut validation,
        )
        .expect("held package custody survives ambient selection-path drift")
        .materialize()
        .unwrap();
        assert_eq!(captured.sha256(), &digest);
        assert_eq!(captured.bytes().as_ref(), &[0x43]);
        let interfaces = std::fs::read_dir(retained.directory.path().join("package-code"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "hi"))
            .collect::<Vec<_>>();
        assert_eq!(interfaces.len(), 1);
        assert_eq!(
            std::fs::read(&interfaces[0]).unwrap().as_slice(),
            captured.bytes().as_ref()
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

    proptest::proptest! {
        #![proptest_config({
            let mut config = proptest::test_runner::Config::default();
            if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
                config.failure_persistence = Some(Box::new(
                    proptest::test_runner::FileFailurePersistence::Direct(path),
                ));
            }
            config
        })]

        #[test]
        fn completed_source_adjacency_retains_exact_type_only_chains(length in 1usize..9) {
            let root = module("Authored");
            let sources = (0..length)
                .map(|index| module(&format!("TypeOnly{index}")))
                .collect::<Vec<_>>();
            let expected = sources.iter().enumerate().map(|(index, owner)| ExactLexicalNode {
                owner: owner.clone(),
                imports: sources.get(index + 1).cloned().into_iter().collect(),
            }).collect::<Vec<_>>();
            let mut admitted = expected.iter()
                .map(|node| (node.owner.clone(), node.imports.clone()))
                .collect::<BTreeMap<_, _>>();
            admitted.insert(root.clone(), vec![sources[0].clone()]);
            let transient = module("UnretainedProbe");
            admitted.insert(transient.clone(), vec![]);
            let available: BTreeSet<_> = sources.iter().cloned().chain([root.clone()]).collect();
            let original = ExactInterfaceOwner {
                owner: root.clone(),
                requirements: vec![sources[0].clone()],
            };
            let surface = |rows: Vec<ExactInterfaceOwner>| {
                let imports = rows.into_iter().map(|row| (row.owner, row.requirements)).collect();
                original_source_lexical_surface(&root, &imports, &[], &BTreeMap::new())
            };
            let mut rows = vec![original.clone()];
            merge_admitted_source_imports(&mut rows, &admitted, &available).unwrap();
            proptest::prop_assert!(rows.iter().all(|row| row.owner != transient));
            proptest::prop_assert_eq!(surface(rows).unwrap().lexical, expected);

            let mut missing = admitted.clone();
            missing.remove(sources.last().unwrap());
            let mut rows = vec![original.clone()];
            merge_admitted_source_imports(&mut rows, &missing, &available).unwrap();
            proptest::prop_assert!(matches!(surface(rows), Err(CompileError::ExtractFailed(_))));

            let mut wrong_unit = available.clone();
            let tail = sources.last().unwrap();
            wrong_unit.remove(tail);
            wrong_unit.insert(ExactModuleIdentity { unit: "other".into(), module: tail.module.clone() });
            let mut rows = vec![original.clone()];
            merge_admitted_source_imports(&mut rows, &admitted, &wrong_unit).unwrap();
            proptest::prop_assert!(matches!(surface(rows), Err(CompileError::ExtractFailed(_))));

            let mut conflicting = admitted.clone();
            conflicting.insert(root.clone(), vec![]);
            let mut rows = vec![original.clone()];
            proptest::prop_assert!(matches!(
                merge_admitted_source_imports(&mut rows, &conflicting, &available),
                Err(CompileError::ExtractFailed(_))
            ));
            let mut duplicate = vec![original.clone(), original];
            proptest::prop_assert!(matches!(
                merge_admitted_source_imports(&mut duplicate, &admitted, &available),
                Err(CompileError::ExtractFailed(_))
            ));
        }
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
