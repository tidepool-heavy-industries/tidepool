//! Source-free certification of a retained declaration tip.

use super::*;

/// Durable selectors are checked against the retained interface by the bound
/// compiler. They carry no execution certificate on their own.
pub struct RecoveryDeclarationSelection {
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
    context: Arc<ExactDeclarationContext>,
    root: ExactModuleIdentity,
    interface_sha256: String,
    exports: Vec<DeclarationExport>,
    instances: InstanceInventory,
    family_closure: Vec<ExportIdentity>,
}

impl RecoveredDeclarationTip {
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
    joins: &[crate::recovery_artifacts::RecoveryJoinRef],
    selection: RecoveryDeclarationSelection,
    includes: &[PathBuf],
) -> Result<RecoveredDeclarationTip, CompileError> {
    let context = Arc::new(ExactDeclarationContext::capture_recovery(
        recovery_root,
        products,
        joins,
        selection.lexical,
    )?);
    if context.toolchain_identity_sha256() == [0; 32]
        || !context
            .lexical_graph()
            .iter()
            .any(|node| node.owner == selection.root)
    {
        return Err(contract("recovered root lacks a certified lexical owner"));
    }
    let scratch = tempfile::tempdir()?;
    let materialized = context.materialize(scratch.path())?;
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
