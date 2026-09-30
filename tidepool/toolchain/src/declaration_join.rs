//! Compiler validation of an exact declaration join. The public version is
//! opaque: the resident session compares it on accepted and rejected outcomes.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tidepool_extract_cmd::ExtractCmd;
use tidepool_repr::{SessionModule, SessionModuleKind};

use crate::{
    cache::ProductAvailability, recovery_artifacts::CertifiedRecoveryProduct, CompileError,
};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ModuleSnapshot {
    pub module: String,
    pub path: PathBuf,
    pub sha256: String,
}

impl ModuleSnapshot {
    pub fn capture(module: String, path: PathBuf) -> Result<Self, CompileError> {
        if !path.is_absolute() {
            return Err(contract("declaration module path must be absolute"));
        }
        let sha256 = sha256(&std::fs::read(&path)?);
        Ok(Self {
            module,
            path,
            sha256,
        })
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ExportNamespace {
    Value,
    Type,
    Constructor,
    Field,
}

impl ExportNamespace {
    fn wire(self) -> &'static str {
        match self {
            Self::Value => "value",
            Self::Type => "type",
            Self::Constructor => "constructor",
            Self::Field => "field",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct ExportIdentity {
    pub unit: String,
    pub module: String,
    pub namespace: ExportNamespace,
    pub occurrence: String,
    pub record_parent: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeclarationKind {
    Value,
    Type,
    Class,
}

impl DeclarationKind {
    fn wire(self) -> &'static str {
        match self {
            Self::Value => "value",
            Self::Type => "type",
            Self::Class => "class",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeclarationExport {
    pub kind: DeclarationKind,
    pub head: ExportIdentity,
    pub children: Vec<ExportIdentity>,
}

/// A write retains the original module even when it introduced only instances.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclarationWrite {
    pub generation: u64,
    pub module: ModuleSnapshot,
    pub exports: Vec<DeclarationExport>,
    pub retractions: Vec<ExportIdentity>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct InstanceInventory {
    pub classes: Vec<ExportIdentity>,
    pub families: Vec<ExportIdentity>,
}

/// The original declaration module from the same exact-source validation
/// workflow. No worker or inventory scratch path is retained by this value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CertifiedAuthoredDeclaration {
    pub product: CertifiedRecoveryProduct,
    /// Every owned home dependency inspected with the declaration, including G.
    pub recovery_products: Vec<CertifiedRecoveryProduct>,
    pub exports: Vec<DeclarationExport>,
    pub instances: InstanceInventory,
    pub family_closure: Vec<ExportIdentity>,
    pub source_sha256: [u8; 32],
    /// SHA-256 of the bound compiler producer identity used for these bytes.
    pub toolchain_identity_sha256: [u8; 32],
}

/// Certify the reserved G module after strict GHC inspection of `exact_source`.
/// A private wrapper forces even an instance-only module into the compiler's
/// product graph. Both transactions use the same ordered include stack; the
/// certified source digest fences the extra transaction to the checked bytes.
pub fn certify_authored_declaration(
    module: SessionModule,
    source_path: &Path,
    exact_source: &str,
    includes: &[PathBuf],
    session_root: &Path,
) -> Result<CertifiedAuthoredDeclaration, CompileError> {
    if module.kind != SessionModuleKind::Lib
        || module.gen.0 == 0
        || includes.is_empty()
        || !source_path.is_absolute()
        || std::fs::canonicalize(source_path)?
            != std::fs::canonicalize(includes[0].join(module.relative_hs_path()))?
    {
        return Err(contract("authored source does not match reserved module"));
    }
    let source_sha256: [u8; 32] = Sha256::digest(exact_source.as_bytes()).into();
    if std::fs::read(source_path)? != exact_source.as_bytes() {
        return Err(contract("authored source changed before certification"));
    }
    let module_name = module.module_name();
    let probe = format!(
        "module TidepoolAuthoredProductProbe where\nimport {module_name} ()\nauthoredProductProbe = (0 :: Int)\n"
    );
    let compiled = crate::artifacts::compile_authored_products(
        &probe,
        "authoredProductProbe",
        includes,
        session_root,
    )?;
    if std::fs::read(source_path)? != exact_source.as_bytes() {
        return Err(contract("authored source changed during certification"));
    }
    let producer_identity = compiled
        .producer_identity
        .ok_or_else(|| contract("authored certification has no bound compiler identity"))?;
    let toolchain_identity_sha256: [u8; 32] = Sha256::digest(producer_identity).into();
    let evidence = compiled
        .module_inventory
        .ok_or_else(|| contract("authored certification has no final dependency graph"))?;
    let source_canonical = std::fs::canonicalize(source_path)?;
    let candidates = evidence
        .iter()
        .filter(|row| row.module == module_name)
        .collect::<Vec<_>>();
    if candidates.len() != 1
        || candidates[0].boot
        || candidates[0].product != ProductAvailability::Ready
        || std::fs::canonicalize(&candidates[0].source)? != source_canonical
    {
        return Err(contract(
            "authored module is absent or ambiguous in final graph",
        ));
    }
    let products = compiled.recovery_products;
    let matches = products
        .iter()
        .filter(|product| {
            product.owner().unit == candidates[0].unit && product.owner().module == module_name
        })
        .count();
    if matches != 1 {
        return Err(contract("authored module has no unique certified product"));
    }
    let selected = products
        .iter()
        .find(|product| {
            product.owner().unit == candidates[0].unit && product.owner().module == module_name
        })
        .expect("count checked");
    if selected.source_sha256() != Some(source_sha256) {
        return Err(contract("certified authored source digest differs"));
    }

    let scratch = tempfile::tempdir()?;
    let references = crate::recovery_artifacts::materialize_certified_products(
        scratch.path(),
        toolchain_identity_sha256,
        &products,
    )
    .map_err(|error| contract(format!("authored artifact closure rejected: {error}")))?;
    let mut artifacts = Vec::with_capacity(products.len());
    for (product, reference) in products.iter().zip(&references) {
        let owner = product.owner();
        let row = evidence
            .iter()
            .find(|row| !row.boot && row.unit == owner.unit && row.module == owner.module)
            .ok_or_else(|| contract("certified product absent from final graph"))?;
        let mut requirements = Vec::new();
        for imported in &row.imports {
            let Some(path) = &imported.selected else {
                continue;
            };
            let import_owner = evidence.iter().find(|candidate| {
                !candidate.boot && candidate.module == imported.module && candidate.source == *path
            });
            if let Some(import_owner) = import_owner {
                let key = (import_owner.unit.clone(), import_owner.module.clone());
                if !requirements.contains(&key) {
                    requirements.push(key);
                }
            } else if includes.iter().any(|root| path.starts_with(root)) {
                return Err(contract(
                    "authored home import has no exact dependency owner",
                ));
            }
        }
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
    let outcome = inspect_declaration_artifacts(&artifacts, includes, session_root)?;
    let inventories = match outcome.decision {
        JoinDecision::Accepted => outcome
            .inventories
            .ok_or_else(|| contract("accepted authored inventory is empty"))?,
        JoinDecision::Rejected { diagnostic, .. } => return Err(contract(diagnostic)),
    };
    let inventory = inventories
        .into_iter()
        .filter(|entry| {
            entry.artifact.interface.unit == selected.owner().unit
                && entry.artifact.interface.module == module_name
        })
        .collect::<Vec<_>>();
    if inventory.len() != 1 {
        return Err(contract("authored inventory has no unique G module"));
    }
    let inventory = inventory.into_iter().next().expect("count checked");
    let family_closure = outcome
        .family_closure
        .ok_or_else(|| contract("accepted authored family closure is empty"))?;
    Ok(CertifiedAuthoredDeclaration {
        product: selected.clone(),
        recovery_products: products,
        exports: inventory.exports,
        instances: inventory.instances,
        family_closure,
        source_sha256,
        toolchain_identity_sha256,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclarationJoinInput {
    pub expected_public_version: String,
    pub public_module: Option<ModuleSnapshot>,
    pub private_base: Option<ModuleSnapshot>,
    pub private_tip: Option<ModuleSnapshot>,
    pub writes: Vec<DeclarationWrite>,
    pub reserved: ReservedJoin,
    pub artifacts: Vec<DeclarationArtifact>,
    pub family_closure: Vec<ExportIdentity>,
    pub expected_exports: Vec<DeclarationExport>,
    pub expected_instances: InstanceInventory,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReservedJoin {
    pub unit: String,
    pub module: String,
    pub path: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExactIfaceArtifact {
    pub unit: String,
    pub module: String,
    pub path: PathBuf,
    pub sha256: String,
    pub requirements: Vec<(String, String)>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeclarationArtifact {
    pub interface: ExactIfaceArtifact,
    pub product: Option<ModuleSnapshot>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JoinRejection {
    ArtifactChanged,
    ExportMismatch,
    ClassInstanceConflict,
    FamilyInstanceConflict,
    InstanceMismatch,
    Unprovable,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum JoinDecision {
    Accepted,
    Rejected {
        reason: JoinRejection,
        diagnostic: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeclarationJoinOutcome {
    version: u32,
    pub expected_public_version: String,
    pub request_sha256: String,
    pub reserved: ReservedJoin,
    pub implementation_sha256: String,
    pub exports_sha256: String,
    pub instances_sha256: String,
    pub family_closure_sha256: String,
    pub artifact: Option<ModuleSnapshot>,
    pub decision: JoinDecision,
}

/// Use the existing bound compiler process boundary and diagnostic policy.
/// This operation never compiles an executable or executes authored effects.
pub fn validate_declaration_join(
    input: &DeclarationJoinInput,
    includes: &[PathBuf],
    session_root: &Path,
    inject_modules: &[String],
) -> Result<DeclarationJoinOutcome, CompileError> {
    let encoded = encode_declaration_join(input)?;
    let bytes = execute_declaration_operation(&encoded, includes, session_root, inject_modules)?;
    decode_declaration_join_outcome(input, &bytes)
}

/// Canonical definite-length CBOR arrays shared with the worker. Keeping the
/// ordered writes in this digest fences instance-only changes and retractions.
pub fn encode_declaration_join(input: &DeclarationJoinInput) -> Result<Vec<u8>, CompileError> {
    let mut writer = JoinEncoder(Vec::new());
    writer.array(12);
    writer.text("TPDJOIN");
    writer.text("2");
    writer.text(&input.expected_public_version);
    writer.optional(input.public_module.as_ref())?;
    writer.optional(input.private_base.as_ref())?;
    writer.optional(input.private_tip.as_ref())?;
    writer.array(input.writes.len());
    for write in &input.writes {
        writer.array(4);
        writer.header(0, write.generation);
        writer.snapshot(&write.module)?;
        writer.exports(&write.exports);
        writer.array(write.retractions.len());
        for identity in &write.retractions {
            writer.identity(identity);
        }
    }
    writer.reserved(&input.reserved)?;
    writer.exports(&input.expected_exports);
    writer.array(2);
    writer.array(input.expected_instances.classes.len());
    for identity in &input.expected_instances.classes {
        writer.identity(identity);
    }
    writer.array(input.expected_instances.families.len());
    for identity in &input.expected_instances.families {
        writer.identity(identity);
    }
    writer.artifacts(&input.artifacts)?;
    writer.identities(&input.family_closure);
    if writer.0.len() > 4 * 1024 * 1024 {
        return Err(contract("declaration join exceeds four MiB"));
    }
    Ok(writer.0)
}

pub fn decode_declaration_join_outcome(
    input: &DeclarationJoinInput,
    bytes: &[u8],
) -> Result<DeclarationJoinOutcome, CompileError> {
    let outcome: DeclarationJoinOutcome = serde_json::from_slice(bytes)
        .map_err(|error| contract(format!("invalid declaration join receipt: {error}")))?;
    let mut implementation = JoinEncoder(Vec::new());
    implementation.artifacts(&input.artifacts)?;
    let mut exports = JoinEncoder(Vec::new());
    exports.exports(&input.expected_exports);
    let mut instances = JoinEncoder(Vec::new());
    instances.inventory(&input.expected_instances);
    let mut families = JoinEncoder(Vec::new());
    families.identities(&input.family_closure);
    if outcome.version != 2
        || outcome.expected_public_version != input.expected_public_version
        || outcome.request_sha256 != sha256(&encode_declaration_join(input)?)
        || outcome.reserved != input.reserved
        || outcome.implementation_sha256 != sha256(&implementation.0)
        || outcome.exports_sha256 != sha256(&exports.0)
        || outcome.instances_sha256 != sha256(&instances.0)
        || outcome.family_closure_sha256 != sha256(&families.0)
    {
        return Err(contract(
            "declaration join receipt belongs to a different request",
        ));
    }
    match (&outcome.decision, &outcome.artifact) {
        (JoinDecision::Accepted, Some(artifact))
            if artifact.module == input.reserved.module
                && artifact.path == input.reserved.path
                && valid_digest(&artifact.sha256) => {}
        (JoinDecision::Rejected { .. }, None) => {}
        _ => {
            return Err(contract(
                "declaration join receipt has invalid output identity",
            ))
        }
    }
    Ok(outcome)
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn valid_digest(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn contract(message: impl Into<String>) -> CompileError {
    CompileError::ExtractFailed(message.into())
}

struct JoinEncoder(Vec<u8>);

impl JoinEncoder {
    fn header(&mut self, major: u8, value: u64) {
        match value {
            0..=23 => self.0.push(major | value as u8),
            24..=255 => self.0.extend_from_slice(&[major | 24, value as u8]),
            256..=65535 => {
                self.0.push(major | 25);
                self.0.extend_from_slice(&(value as u16).to_be_bytes());
            }
            65536..=4294967295 => {
                self.0.push(major | 26);
                self.0.extend_from_slice(&(value as u32).to_be_bytes());
            }
            _ => {
                self.0.push(major | 27);
                self.0.extend_from_slice(&value.to_be_bytes());
            }
        }
    }
    fn array(&mut self, count: usize) {
        self.header(0x80, count as u64);
    }
    fn text(&mut self, text: &str) {
        self.header(0x60, text.len() as u64);
        self.0.extend_from_slice(text.as_bytes());
    }
    fn optional(&mut self, snapshot: Option<&ModuleSnapshot>) -> Result<(), CompileError> {
        match snapshot {
            Some(snapshot) => self.snapshot(snapshot),
            None => {
                self.0.push(0xf6);
                Ok(())
            }
        }
    }
    fn snapshot(&mut self, snapshot: &ModuleSnapshot) -> Result<(), CompileError> {
        if !snapshot.path.is_absolute()
            || snapshot.module.is_empty()
            || !valid_digest(&snapshot.sha256)
        {
            return Err(contract("invalid declaration module snapshot"));
        }
        let path = snapshot
            .path
            .to_str()
            .ok_or_else(|| contract("non-UTF-8 declaration path"))?;
        self.array(3);
        self.text(&snapshot.module);
        self.text(path);
        self.text(&snapshot.sha256);
        Ok(())
    }
    fn reserved(&mut self, reserved: &ReservedJoin) -> Result<(), CompileError> {
        if reserved.unit.is_empty() || reserved.module.is_empty() || !reserved.path.is_absolute() {
            return Err(contract("invalid reserved Join identity"));
        }
        self.array(3);
        self.text(&reserved.unit);
        self.text(&reserved.module);
        self.text(
            reserved
                .path
                .to_str()
                .ok_or_else(|| contract("non-UTF-8 Join path"))?,
        );
        Ok(())
    }
    fn artifacts(&mut self, artifacts: &[DeclarationArtifact]) -> Result<(), CompileError> {
        self.array(artifacts.len());
        for artifact in artifacts {
            let iface = &artifact.interface;
            self.array(2);
            if iface.unit.is_empty()
                || iface.module.is_empty()
                || !iface.path.is_absolute()
                || !valid_digest(&iface.sha256)
            {
                return Err(contract("invalid exact interface artifact"));
            }
            self.array(5);
            self.text(&iface.unit);
            self.text(&iface.module);
            self.text(
                iface
                    .path
                    .to_str()
                    .ok_or_else(|| contract("non-UTF-8 interface path"))?,
            );
            self.text(&iface.sha256);
            self.array(iface.requirements.len());
            for (unit, module) in &iface.requirements {
                self.array(2);
                self.text(unit);
                self.text(module);
            }
            self.optional(artifact.product.as_ref())?;
        }
        Ok(())
    }
    fn identities(&mut self, identities: &[ExportIdentity]) {
        self.array(identities.len());
        for identity in identities {
            self.identity(identity);
        }
    }
    fn inventory(&mut self, inventory: &InstanceInventory) {
        self.array(2);
        self.identities(&inventory.classes);
        self.identities(&inventory.families);
    }
    fn identity(&mut self, identity: &ExportIdentity) {
        self.array(5);
        self.text(&identity.unit);
        self.text(&identity.module);
        self.text(identity.namespace.wire());
        self.text(&identity.occurrence);
        match &identity.record_parent {
            Some(parent) => self.text(parent),
            None => self.0.push(0xf6),
        }
    }
    fn exports(&mut self, exports: &[DeclarationExport]) {
        self.array(exports.len());
        for export in exports {
            self.array(3);
            self.text(export.kind.wire());
            self.identity(&export.head);
            self.array(export.children.len());
            for child in &export.children {
                self.identity(child);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> DeclarationJoinInput {
        DeclarationJoinInput {
            expected_public_version: "paired-snapshot".into(),
            public_module: None,
            private_base: None,
            private_tip: None,
            writes: Vec::new(),
            reserved: ReservedJoin {
                unit: "main".into(),
                module: "Join".into(),
                path: "/scratch/Join.hi".into(),
            },
            expected_exports: Vec::new(),
            expected_instances: InstanceInventory::default(),
            artifacts: Vec::new(),
            family_closure: Vec::new(),
        }
    }
    fn receipt(input: &DeclarationJoinInput, decision: JoinDecision) -> DeclarationJoinOutcome {
        let mut implementation = JoinEncoder(Vec::new());
        implementation.artifacts(&input.artifacts).unwrap();
        let mut exports = JoinEncoder(Vec::new());
        exports.exports(&input.expected_exports);
        let mut instances = JoinEncoder(Vec::new());
        instances.inventory(&input.expected_instances);
        let mut families = JoinEncoder(Vec::new());
        families.identities(&input.family_closure);
        DeclarationJoinOutcome {
            version: 2,
            expected_public_version: input.expected_public_version.clone(),
            request_sha256: sha256(&encode_declaration_join(input).unwrap()),
            reserved: input.reserved.clone(),
            implementation_sha256: sha256(&implementation.0),
            exports_sha256: sha256(&exports.0),
            instances_sha256: sha256(&instances.0),
            family_closure_sha256: sha256(&families.0),
            artifact: if decision == JoinDecision::Accepted {
                Some(ModuleSnapshot {
                    module: input.reserved.module.clone(),
                    path: input.reserved.path.clone(),
                    sha256: "0".repeat(64),
                })
            } else {
                None
            },
            decision,
        }
    }

    #[test]
    fn matches_haskell_manifest_and_receipt() {
        let input = input();
        assert_eq!(
            encode_declaration_join(&input).unwrap(),
            include_bytes!(
                "../../../bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-v2.cbor"
            )
        );
        let receipt = include_bytes!(
            "../../../bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-v2.json"
        );
        let accepted = decode_declaration_join_outcome(&input, receipt).unwrap();
        assert_eq!(accepted.decision, JoinDecision::Accepted);
        assert_eq!(accepted.artifact.unwrap().path, input.reserved.path);
        assert_eq!(encode_declaration_inventory(&[]).unwrap(),
            include_bytes!("../../../bridge/haskell/test-cell-splitter/fixtures/declaration-join/inventory-v2.cbor"));
        let inventory = decode_declaration_inventory_outcome(&[], include_bytes!(
            "../../../bridge/haskell/test-cell-splitter/fixtures/declaration-join/inventory-v2.json")).unwrap();
        assert_eq!(inventory.decision, JoinDecision::Accepted);
    }

    #[test]
    fn inventory_rejects_changed_artifact_provenance() {
        let artifacts = Vec::new();
        let mut encoder = JoinEncoder(Vec::new());
        encoder.artifacts(&artifacts).unwrap();
        let outcome = DeclarationInventoryOutcome {
            version: 2,
            request_sha256: sha256(&encode_declaration_inventory(&artifacts).unwrap()),
            implementation_sha256: sha256(&encoder.0),
            inventories: Some(Vec::new()),
            family_closure: Some(Vec::new()),
            decision: JoinDecision::Accepted,
        };
        let bytes = serde_json::to_vec(&outcome).unwrap();
        assert_eq!(
            decode_declaration_inventory_outcome(&artifacts, &bytes).unwrap(),
            outcome
        );
        let changed = vec![DeclarationArtifact {
            interface: ExactIfaceArtifact {
                unit: "main".into(),
                module: "Old".into(),
                path: "/scratch/Old.hi".into(),
                sha256: "0".repeat(64),
                requirements: Vec::new(),
            },
            product: None,
        }];
        assert!(decode_declaration_inventory_outcome(&changed, &bytes).is_err());
    }

    #[test]
    fn rejects_cross_request_receipts_for_both_decisions() {
        let input = input();
        for decision in [
            JoinDecision::Accepted,
            JoinDecision::Rejected {
                reason: JoinRejection::ClassInstanceConflict,
                diagnostic: "duplicate instance".into(),
            },
        ] {
            let outcome = receipt(&input, decision);
            let bytes = serde_json::to_vec(&outcome).unwrap();
            assert_eq!(
                decode_declaration_join_outcome(&input, &bytes).unwrap(),
                outcome
            );
            let mut other = input.clone();
            other.expected_public_version.push_str("-new");
            assert!(decode_declaration_join_outcome(&other, &bytes).is_err());
            let mut other = input.clone();
            other.reserved.module.push_str("New");
            assert!(decode_declaration_join_outcome(&other, &bytes).is_err());
            let mut other = input.clone();
            other.writes.push(DeclarationWrite {
                generation: 7,
                module: ModuleSnapshot {
                    module: "Old".into(),
                    path: "/scratch/Old.hi".into(),
                    sha256: "0".repeat(64),
                },
                exports: Vec::new(),
                retractions: Vec::new(),
            });
            assert!(decode_declaration_join_outcome(&other, &bytes).is_err());
            let mut corrupt = outcome.clone();
            corrupt.instances_sha256 = "f".repeat(64);
            assert!(decode_declaration_join_outcome(
                &input,
                &serde_json::to_vec(&corrupt).unwrap()
            )
            .is_err());
            let mut corrupt = outcome.clone();
            corrupt.artifact = Some(ModuleSnapshot {
                module: "Wrong".into(),
                path: input.reserved.path.clone(),
                sha256: "0".repeat(64),
            });
            assert!(decode_declaration_join_outcome(
                &input,
                &serde_json::to_vec(&corrupt).unwrap()
            )
            .is_err());
        }
    }
}

fn execute_declaration_operation(
    encoded: &[u8],
    includes: &[PathBuf],
    session_root: &Path,
    inject_modules: &[String],
) -> Result<Vec<u8>, CompileError> {
    let scratch = tempfile::tempdir()?;
    let manifest = scratch.path().join("declaration-join.cbor");
    let receipt = scratch.path().join("declaration-join.json");
    std::fs::write(&manifest, encoded)?;
    let mut command = ExtractCmd::new().map_err(|error| CompileError::Io(error.into()))?;
    command
        .input(&manifest)
        .includes(includes)
        .session_root(session_root)
        .declaration_join(&manifest)
        .declaration_join_out(&receipt);
    for module in inject_modules {
        command.inject_val(module);
    }
    let endpoint = command
        .bind()
        .map_err(|error| CompileError::Io(crate::extract_spawn_error(error.source)))?;
    crate::paths::apply_build_products_dir(&mut command, &endpoint);
    let run = endpoint
        .execute(&command)
        .map_err(|error| CompileError::Io(crate::extract_spawn_error(error.source)))?;
    crate::diag::decode_extract_result(
        run.output.status.success(),
        &run.output.stdout,
        &run.output.stderr,
    )?;
    Ok(std::fs::read(receipt)?)
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeclarationInventory {
    pub artifact: DeclarationArtifact,
    pub exports: Vec<DeclarationExport>,
    pub instances: InstanceInventory,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeclarationInventoryOutcome {
    version: u32,
    pub request_sha256: String,
    pub implementation_sha256: String,
    pub inventories: Option<Vec<DeclarationInventory>>,
    pub family_closure: Option<Vec<ExportIdentity>>,
    pub decision: JoinDecision,
}

pub fn encode_declaration_inventory(
    artifacts: &[DeclarationArtifact],
) -> Result<Vec<u8>, CompileError> {
    let mut encoder = JoinEncoder(Vec::new());
    encoder.array(3);
    encoder.text("TPDINVENTORY");
    encoder.text("2");
    encoder.artifacts(artifacts)?;
    if encoder.0.len() > 4 * 1024 * 1024 {
        return Err(contract("declaration inventory exceeds four MiB"));
    }
    Ok(encoder.0)
}

/// Query exact original interface identities before the runtime computes its
/// merge. This does not replay authored declarations or publish session state.
pub fn inspect_declaration_artifacts(
    artifacts: &[DeclarationArtifact],
    includes: &[PathBuf],
    session_root: &Path,
) -> Result<DeclarationInventoryOutcome, CompileError> {
    let encoded = encode_declaration_inventory(artifacts)?;
    let bytes = execute_declaration_operation(&encoded, includes, session_root, &[])?;
    decode_declaration_inventory_outcome(artifacts, &bytes)
}

pub fn decode_declaration_inventory_outcome(
    artifacts: &[DeclarationArtifact],
    bytes: &[u8],
) -> Result<DeclarationInventoryOutcome, CompileError> {
    let outcome: DeclarationInventoryOutcome = serde_json::from_slice(bytes)
        .map_err(|error| contract(format!("invalid declaration inventory receipt: {error}")))?;
    let mut implementation = JoinEncoder(Vec::new());
    implementation.artifacts(artifacts)?;
    if outcome.version != 2
        || outcome.request_sha256 != sha256(&encode_declaration_inventory(artifacts)?)
        || outcome.implementation_sha256 != sha256(&implementation.0)
    {
        return Err(contract(
            "declaration inventory receipt belongs to a different request",
        ));
    }
    match (
        &outcome.decision,
        &outcome.inventories,
        &outcome.family_closure,
    ) {
        (JoinDecision::Accepted, Some(inventories), Some(families)) => {
            if inventories
                .iter()
                .map(|inventory| &inventory.artifact)
                .ne(artifacts.iter())
            {
                return Err(contract(
                    "declaration inventory artifact order or identity differs",
                ));
            }
            let mut actual = inventories
                .iter()
                .flat_map(|inventory| inventory.instances.families.iter().cloned())
                .collect::<Vec<_>>();
            actual.sort();
            actual.dedup();
            if &actual != families {
                return Err(contract("declaration inventory family closure differs"));
            }
        }
        (JoinDecision::Rejected { .. }, None, None) => {}
        _ => {
            return Err(contract(
                "declaration inventory receipt has invalid outcome shape",
            ))
        }
    }
    Ok(outcome)
}

#[cfg(test)]
mod authored_tests {
    use super::*;
    use tidepool_repr::Generation;

    #[test]
    fn authored_certification_refuses_different_source_before_compilation() {
        let root = tempfile::tempdir().unwrap();
        let module = SessionModule::lib(Generation(1));
        let path = root.path().join(module.relative_hs_path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "module Tidepool.Session.Lib.G1 where\n").unwrap();
        let error = certify_authored_declaration(
            module,
            &path,
            "module Tidepool.Session.Lib.G1 where\nx = 1\n",
            &[root.path().to_path_buf()],
            root.path(),
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("source changed before certification"));
    }

    #[test]
    #[ignore = "requires the matched Haskell worker and frontend"]
    fn empty_authored_module_still_has_owned_product_and_inventory() {
        let root = tempfile::tempdir().unwrap();
        let module = SessionModule::lib(Generation(1));
        let source = "module Tidepool.Session.Lib.G1 where\n";
        let path = root.path().join(module.relative_hs_path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, source).unwrap();
        let result = certify_authored_declaration(
            module,
            &path,
            source,
            &[root.path().to_path_buf()],
            root.path(),
        )
        .unwrap();
        assert_eq!(result.product.owner().module, module.module_name());
        assert_eq!(result.product.source_sha256(), Some(result.source_sha256));
        assert!(result.exports.is_empty());
        assert!(result.instances.classes.is_empty());
        assert!(result.instances.families.is_empty());
    }

    #[test]
    #[ignore = "requires the matched Haskell worker and frontend"]
    fn authored_module_retains_owned_home_import_closure() {
        let root = tempfile::tempdir().unwrap();
        let first = SessionModule::lib(Generation(1));
        let second = SessionModule::lib(Generation(2));
        let first_source = "module Tidepool.Session.Lib.G1 where\nhelper = (41 :: Int)\n";
        let second_source = "module Tidepool.Session.Lib.G2 where\nimport Tidepool.Session.Lib.G1 (helper)\nanswer = helper + 1\n";
        for (module, source) in [(first, first_source), (second, second_source)] {
            let path = root.path().join(module.relative_hs_path());
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, source).unwrap();
        }
        let result = certify_authored_declaration(
            second,
            &root.path().join(second.relative_hs_path()),
            second_source,
            &[root.path().to_path_buf()],
            root.path(),
        )
        .unwrap();
        for module in [first, second] {
            assert!(result.recovery_products.iter().any(|product| {
                product.owner().module == module.module_name() && product.source_sha256().is_some()
            }));
        }
        assert!(result
            .exports
            .iter()
            .any(|export| export.head.occurrence == "answer"));
        let durable = tempfile::tempdir().unwrap();
        let references = crate::recovery_artifacts::materialize_certified_products(
            durable.path(),
            result.toolchain_identity_sha256,
            &result.recovery_products,
        )
        .unwrap();
        for module in [first, second] {
            assert!(references
                .iter()
                .any(|reference| reference.module == module.module_name()));
        }
    }
}
