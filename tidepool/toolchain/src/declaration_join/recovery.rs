//! Source-free certification of a retained declaration tip.

use super::*;

/// Durable selectors are checked against the retained interface by the bound
/// compiler. They carry no execution certificate on their own.
pub enum RecoveryDeclarationOrigin {
    Join,
    Authored {
        generation: u64,
        introduced_exports: Vec<DeclarationExport>,
    },
}

pub struct RecoveryDeclarationSelection {
    pub origin: RecoveryDeclarationOrigin,
    pub root: ExactModuleIdentity,
    pub lexical: Vec<ExactLexicalNode>,
    pub exports: Vec<DeclarationExport>,
    pub instances: InstanceInventory,
    pub family_closure: Vec<ExportIdentity>,
}

/// Exact declaration facts recovered from verified artifacts and a fresh
/// compiler inventory. No authored source or publication receipt is recreated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveredDeclarationTip {
    origin: RecoveredDeclarationOrigin,
    context: Arc<ExactDeclarationContext>,
    root: ExactModuleIdentity,
    interface_sha256: String,
    exports: Vec<DeclarationExport>,
    instances: InstanceInventory,
    family_closure: Vec<ExportIdentity>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RecoveredDeclarationOrigin {
    Join,
    Authored {
        generation: u64,
        native_artifact: crate::artifact_inventory::ArtifactId,
    },
}

impl RecoveredDeclarationTip {
    /// Present only after the original module's compiler inventory matches
    /// the complete introduced export delta for this authenticated generation.
    pub fn authored_generation(&self) -> Option<u64> {
        match self.origin {
            RecoveredDeclarationOrigin::Join => None,
            RecoveredDeclarationOrigin::Authored { generation, .. } => Some(generation),
        }
    }
    pub fn context(&self) -> &Arc<ExactDeclarationContext> {
        &self.context
    }
    pub fn root(&self) -> &ExactModuleIdentity {
        &self.root
    }
    pub fn interface_sha256(&self) -> &str {
        &self.interface_sha256
    }
    pub fn exports(&self) -> &[DeclarationExport] {
        &self.exports
    }
    pub fn instances(&self) -> &InstanceInventory {
        &self.instances
    }
    pub fn family_closure(&self) -> &[ExportIdentity] {
        &self.family_closure
    }
}

/// Recover one original or joined interface without compiling or replaying
/// declaration source. The existing inspection owner fences the current
/// compiler producer against every retained product's certified producer.
pub fn certify_recovered_declaration_tip(
    recovery_root: &Path,
    products: &[crate::recovery_artifacts::RecoveryArtifactRef],
    module_interfaces: &[crate::recovery_artifacts::RecoveryModuleInterfaceRef],
    joins: &[crate::recovery_artifacts::RecoveryJoinRef],
    selection: RecoveryDeclarationSelection,
    includes: &[PathBuf],
) -> Result<RecoveredDeclarationTip, CompileError> {
    certify_recovered_declaration_tip_with_value_interfaces(
        recovery_root,
        products,
        module_interfaces,
        joins,
        &[],
        selection,
        includes,
    )
}

pub fn certify_recovered_declaration_tip_with_value_interfaces(
    recovery_root: &Path,
    products: &[crate::recovery_artifacts::RecoveryArtifactRef],
    module_interfaces: &[crate::recovery_artifacts::RecoveryModuleInterfaceRef],
    joins: &[crate::recovery_artifacts::RecoveryJoinRef],
    values: &[crate::recovery_artifacts::RecoveryValueInterfaceRef],
    mut selection: RecoveryDeclarationSelection,
    includes: &[PathBuf],
) -> Result<RecoveredDeclarationTip, CompileError> {
    let context = Arc::new(
        ExactDeclarationContext::capture_recovery_with_value_interfaces(
            recovery_root,
            products,
            module_interfaces,
            joins,
            values,
            std::mem::take(&mut selection.lexical),
        )?,
    );
    certify_recovered_declaration_tip_in_context(context, selection, includes)
}

pub fn certify_recovered_declaration_tip_with_inventory(
    recovery_root: &Path,
    products: &[crate::recovery_artifacts::RecoveryArtifactRef],
    module_interfaces: &[crate::recovery_artifacts::RecoveryModuleInterfaceRef],
    joins: &[crate::recovery_artifacts::RecoveryJoinRef],
    values: &[crate::recovery_artifacts::RecoveryValueInterfaceRef],
    descriptors: &[crate::artifact_inventory::ArtifactDescriptor],
    dependencies: &[(
        crate::artifact_inventory::ArtifactId,
        crate::artifact_inventory::ArtifactId,
        crate::artifact_inventory::ArtifactDependency,
    )],
    native_groups: &[crate::artifact_inventory::NativeGroupKey],
    compiler_roles: &[crate::artifact_inventory::CompilerInputRole],
    mut selection: RecoveryDeclarationSelection,
    includes: &[PathBuf],
) -> Result<RecoveredDeclarationTip, CompileError> {
    let context = Arc::new(ExactDeclarationContext::capture_recovery_with_inventory(
        recovery_root,
        products,
        module_interfaces,
        joins,
        values,
        descriptors,
        dependencies,
        native_groups,
        compiler_roles,
        std::mem::take(&mut selection.lexical),
    )?);
    certify_recovered_declaration_tip_in_context(context, selection, includes)
}
/// Certify durable selectors against an already authenticated scoped inventory.
/// The context retains original type evidence and exact native requirements;
/// this inspection grants no recovered live value or binding lease.
pub fn certify_recovered_declaration_tip_in_context(
    context: Arc<ExactDeclarationContext>,
    selection: RecoveryDeclarationSelection,
    includes: &[PathBuf],
) -> Result<RecoveredDeclarationTip, CompileError> {
    if context.toolchain_identity_sha256() == [0; 32]
        || !context
            .lexical_graph()
            .iter()
            .any(|node| node.owner == selection.root)
    {
        return Err(contract("recovered root lacks a certified lexical owner"));
    }
    if matches!(selection.origin, RecoveryDeclarationOrigin::Join)
        && !context
            .artifact_view()
            .descriptors()
            .iter()
            .any(|descriptor| {
                descriptor.owner == selection.root
                    && descriptor.kind == crate::artifact_inventory::ArtifactKind::LexicalJoin
            })
    {
        return Err(contract(
            "recovered join root has no certified lexical-join role",
        ));
    }
    let scratch = tempfile::tempdir()?;
    let materialized = context.materialize_scratch(&scratch)?;
    let roots = materialized
        .artifacts
        .iter()
        .filter(|artifact| {
            artifact.interface.unit == selection.root.unit
                && artifact.interface.module == selection.root.module
        })
        .collect::<Vec<_>>();
    if roots.len() != 1 {
        return Err(contract("recovered root has no unique retained interface"));
    }
    let root_interface = roots[0].interface.clone();
    context.validate_artifacts(&materialized.artifacts)?;
    let outcome = inspect_declaration_artifacts_with_producer(
        &materialized.artifacts,
        includes,
        scratch.path(),
        Some(context.toolchain_identity_sha256()),
    )?;
    context.validate_artifacts(&materialized.artifacts)?;
    if let JoinDecision::Rejected { diagnostic, .. } = outcome.decision {
        return Err(contract(format!(
            "recovery inventory rejected: {diagnostic}"
        )));
    }
    let inventories = outcome
        .inventories
        .ok_or_else(|| contract("recovery inventory has no interface facts"))?;
    let origin = match &selection.origin {
        RecoveryDeclarationOrigin::Join => RecoveredDeclarationOrigin::Join,
        RecoveryDeclarationOrigin::Authored {
            generation,
            introduced_exports,
        } => {
            let native_artifact = context.authored_native_root(*generation)?;
            let original = context
                .artifact_view()
                .entries()
                .into_iter()
                .find_map(|entry| match &entry.payload {
                    crate::artifact_inventory::ArtifactPayload::Original(product)
                        if entry.descriptor.id == native_artifact =>
                    {
                        Some(product.clone())
                    }
                    _ => None,
                })
                .ok_or_else(|| {
                    contract("recovered authored native root has no original product")
                })?;
            let mut originals = inventories.iter().filter(|inventory| {
                inventory.artifact.interface.unit == original.owner().unit
                    && inventory.artifact.interface.module == original.owner().module
            });
            let inventory = originals.next().ok_or_else(|| {
                contract("recovered authored original has no interface inventory")
            })?;
            if originals.next().is_some() {
                return Err(contract(
                    "recovered authored original has ambiguous interface inventory",
                ));
            }
            let local_exports = inventory
                .exports
                .iter()
                .filter(|export| {
                    export.head.unit == original.owner().unit
                        && export.head.module == original.owner().module
                })
                .cloned()
                .collect::<Vec<_>>();
            if exports_key(&local_exports) != exports_key(introduced_exports) {
                return Err(contract(
                    "recovered authored delta differs from its original compiler inventory",
                ));
            }
            RecoveredDeclarationOrigin::Authored {
                generation: *generation,
                native_artifact,
            }
        }
    };
    let mut roots = inventories
        .into_iter()
        .filter(|inventory| inventory.artifact.interface == root_interface)
        .collect::<Vec<_>>();
    if roots.len() != 1 {
        return Err(contract("recovery inventory has no unique root receipt"));
    }
    let inventory = roots.pop().expect("unique root checked");
    let families = outcome
        .family_closure
        .ok_or_else(|| contract("recovery inventory has no family consistency closure"))?;
    if exports_key(&inventory.exports) != exports_key(&selection.exports)
        || instances_key(&inventory.instances) != instances_key(&selection.instances)
        || identities_key(&families) != identities_key(&selection.family_closure)
    {
        return Err(contract(
            "recovered interface differs from its durable selectors",
        ));
    }
    Ok(RecoveredDeclarationTip {
        origin,
        context,
        root: selection.root,
        interface_sha256: root_interface.sha256,
        exports: inventory.exports,
        instances: inventory.instances,
        family_closure: families,
    })
}

fn identities_key(identities: &[ExportIdentity]) -> Vec<ExportIdentity> {
    let mut key = identities.to_vec();
    key.sort();
    key
}

fn exports_key(exports: &[DeclarationExport]) -> Vec<Vec<u8>> {
    let mut key = exports
        .iter()
        .cloned()
        .map(|mut export| {
            export.children.sort();
            serde_json::to_vec(&export).expect("declaration export is serializable")
        })
        .collect::<Vec<_>>();
    key.sort();
    key
}

fn instances_key(instances: &InstanceInventory) -> InstanceInventory {
    let mut key = instances.clone();
    for class in &mut key.classes {
        class.selected_axioms.sort();
    }
    key.classes.sort();
    key.families.sort();
    key
}
