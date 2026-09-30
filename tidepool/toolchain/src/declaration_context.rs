//! Exact declaration inputs retain original products independently of source
//! lookup, while their explicit virtual graph owns lexical visibility.

use ciborium::value::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::certified_products::{
    certify_inherited_products, InheritedProductInput, PendingCertifiedGroup,
};
use crate::declaration_join::{
    AcceptedJoin, CertifiedAuthoredDeclaration, DeclarationArtifact, ExactIfaceArtifact,
    ExactInterfaceOwner, ExactLexicalNode, ExactModuleIdentity, ModuleSnapshot,
};
use crate::recovery_artifacts::{
    self, CertifiedJoinedInterface, CertifiedRecoveryProduct, RecoveryArtifactRef, RecoveryJoinRef,
};
use crate::CompileError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExactDeclarationContext {
    producer: [u8; 32],
    products: Vec<CertifiedRecoveryProduct>,
    joins: Vec<CertifiedJoinedInterface>,
    interfaces: Vec<ExactInterfaceOwner>,
    lexical: Vec<ExactLexicalNode>,
}

/// The paths live under the caller's owned artifact directory. Their content
/// and requirements are always derived from the protected context.
pub struct MaterializedExactDeclarationContext {
    pub artifacts: Vec<DeclarationArtifact>,
    pub lexical: Vec<ExactLexicalNode>,
}

pub(crate) struct ExactCompilationRequest {
    pub(crate) context: Arc<ExactDeclarationContext>,
    pub(crate) manifest: PathBuf,
    pub(crate) request_sha256: String,
    pub(crate) semantic_sha256: [u8; 32],
    pub(crate) artifacts: Vec<DeclarationArtifact>,
    pub(crate) groups: Vec<PendingCertifiedGroup>,
}

/// A successful compiler transaction's actual generated source, bound to its
/// immutable declaration context. It is not ordinary source-cache evidence.
#[derive(Clone, Debug)]
pub struct ExactSourceWitness {
    source_path: PathBuf,
    source_sha256: [u8; 32],
}

impl ExactSourceWitness {
    pub fn source_path(&self) -> &Path {
        &self.source_path
    }
    pub fn source_sha256(&self) -> &[u8; 32] {
        &self.source_sha256
    }
    pub fn matches_source(&self, path: &Path, source: &str) -> bool {
        use sha2::Digest;
        self.source_path == path
            && self.source_sha256 == <[u8; 32]>::from(sha2::Sha256::digest(source.as_bytes()))
    }
}

pub(crate) struct ExactSourceAdmission {
    pub(crate) witness: ExactSourceWitness,
    pub(crate) evidence: crate::cache::DependencyEvidence,
    pub(crate) evidence_bytes: Vec<u8>,
    pub(crate) exact_imports: BTreeMap<ExactModuleIdentity, Vec<ExactModuleIdentity>>,
}

pub(crate) struct ExactProductAdmission<'a> {
    pub(crate) request: &'a ExactCompilationRequest,
    pub(crate) source: &'a ExactSourceAdmission,
}

impl ExactSourceAdmission {
    pub(crate) fn validate_ineligible_evidence(&self, bytes: &[u8]) -> Result<(), CompileError> {
        let mut expected: crate::cache::DependencyEvidence =
            serde_json::from_slice(&self.evidence_bytes).map_err(failure)?;
        expected.cache_safe = false;
        expected.selection_complete = false;
        let actual: crate::cache::DependencyEvidence =
            serde_json::from_slice(bytes).map_err(failure)?;
        if serde_json::to_value(expected).map_err(failure)?
            != serde_json::to_value(actual).map_err(failure)?
        {
            return Err(failure(
                "source-cache-ineligible evidence differs from exact fresh proof",
            ));
        }
        Ok(())
    }
}

impl ExactCompilationRequest {
    pub(crate) fn admit_source(
        &self,
        source_path: &Path,
        source: &str,
    ) -> Result<ExactSourceAdmission, CompileError> {
        use sha2::Digest;
        let expected: [u8; 32] = sha2::Sha256::digest(source.as_bytes()).into();
        self.validate_outputs(
            source_path
                .parent()
                .ok_or_else(|| failure("source has no directory"))?,
        )?
        .into_iter()
        .find(|admitted| {
            admitted.witness.source_path() == source_path
                && admitted.witness.source_sha256() == &expected
        })
        .ok_or_else(|| failure("source lacks its exact consumed source receipt"))
    }
    pub(crate) fn validate_outputs(
        &self,
        root: &Path,
    ) -> Result<Vec<ExactSourceAdmission>, CompileError> {
        self.validate_outputs_with_planned(root, None)
    }

    pub(crate) fn validate_outputs_with_planned(
        &self,
        root: &Path,
        planned: Option<&ExactModuleIdentity>,
    ) -> Result<Vec<ExactSourceAdmission>, CompileError> {
        self.context.validate_artifacts(&self.artifacts)?;
        if sha256(&std::fs::read(&self.manifest)?) != self.request_sha256 {
            return Err(failure("scope request changed during compilation"));
        }
        let directory = root.join(".exact-compilations");
        let mut receipts = std::fs::read_dir(&directory)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<Vec<_>, _>>()?;
        receipts.sort();
        if receipts.is_empty() || receipts.len() > 4096 {
            return Err(failure("missing or excessive successful compile receipts"));
        }
        receipts
            .iter()
            .map(|path| self.validate_receipt(&path.join("receipt.cbor"), planned))
            .collect()
    }

    fn validate_receipt(
        &self,
        path: &Path,
        planned: Option<&ExactModuleIdentity>,
    ) -> Result<ExactSourceAdmission, CompileError> {
        use sha2::Digest;
        let bytes = bounded_read(path, 4 * 1024 * 1024)?;
        let mut cursor = std::io::Cursor::new(&bytes);
        let value: Value = ciborium::de::from_reader(&mut cursor).map_err(failure)?;
        if cursor.position() != bytes.len() as u64 {
            return Err(failure("compile receipt has trailing bytes"));
        }
        let header = row(&value, 9)?;
        if string(&header[0])? != "TPEXACTCOMPILE"
            || string(&header[1])? != "1"
            || string(&header[2])? != self.request_sha256
            || string(&header[3])? != hex(&self.semantic_sha256)
        {
            return Err(failure(
                "compile receipt belongs to another context or version",
            ));
        }
        let source_path = PathBuf::from(string(&header[4])?);
        let snapshot = PathBuf::from(string(&header[6])?);
        if !source_path.is_absolute()
            || !snapshot.is_absolute()
            || snapshot
                != path
                    .parent()
                    .ok_or_else(|| failure("compile receipt has no owner directory"))?
                    .join("source.hs")
        {
            return Err(failure("compile source snapshot has another owner"));
        }
        let source_bytes = bounded_read(&snapshot, 32 * 1024 * 1024)?;
        let source_sha256: [u8; 32] = sha2::Sha256::digest(&source_bytes).into();
        if string(&header[5])? != hex(&source_sha256) {
            return Err(failure("compile source snapshot changed"));
        }
        let source = std::str::from_utf8(&source_bytes).map_err(failure)?;
        let evidence_bytes = string(&header[7])?.as_bytes().to_vec();
        let evidence =
            crate::cache::DependencyEvidence::from_worker(&evidence_bytes, &source_path, source)
                .ok_or_else(|| {
                    failure("fresh compilation lacks complete tracked source evidence")
                })?;
        let mut exact_owners: BTreeSet<_> = self
            .artifacts
            .iter()
            .map(|artifact| {
                (
                    artifact.interface.unit.as_str(),
                    artifact.interface.module.as_str(),
                )
            })
            .collect();
        if let Some(planned) = planned {
            exact_owners.insert((planned.unit.as_str(), planned.module.as_str()));
        }
        let source_owners: BTreeSet<_> = evidence
            .modules
            .iter()
            .map(|module| (module.unit.as_str(), module.module.as_str(), module.boot))
            .collect();
        if source_owners
            .iter()
            .any(|(unit, module, _)| exact_owners.contains(&(*unit, *module)))
        {
            return Err(failure("fresh module replaced an admitted exact owner"));
        }
        let mut selected: BTreeSet<_> = self
            .context
            .lexical
            .iter()
            .map(|node| (node.owner.unit.as_str(), node.owner.module.as_str()))
            .collect();
        if let Some(planned) = planned {
            selected.insert((planned.unit.as_str(), planned.module.as_str()));
        }
        let edges = list(&header[8], 4096)?;
        let mut seen = BTreeSet::new();
        let mut exact_imports = BTreeMap::new();
        for module in edges {
            let module = row(module, 4)?;
            let owner = (
                string(&module[0])?,
                string(&module[1])?,
                boolean(&module[2])?,
            );
            if !source_owners.contains(&owner) || !seen.insert(owner) {
                return Err(failure(
                    "exact import witness has another fresh source owner",
                ));
            }
            let mut imported = BTreeSet::new();
            let mut resolved = BTreeSet::new();
            for edge in list(&module[3], 4096)? {
                let edge = row(edge, 4)?;
                let qualifier = string(&edge[0])?;
                let name = string(&edge[1])?;
                let boot = boolean(&edge[2])?;
                let unit = string(&edge[3])?;
                if boot
                    || !selected.contains(&(unit, name))
                    || (qualifier != "none" && qualifier != format!("this:{unit}"))
                    || !imported.insert((qualifier, name, boot, unit))
                {
                    return Err(failure(
                        "exact import witness leaves selected lexical graph",
                    ));
                }
                resolved.insert(identity(unit, name));
            }
            exact_imports.insert(identity(owner.0, owner.1), resolved.into_iter().collect());
        }
        if seen != source_owners {
            return Err(failure("exact import witness omits a fresh source module"));
        }
        Ok(ExactSourceAdmission {
            witness: ExactSourceWitness {
                source_path,
                source_sha256,
            },
            evidence,
            evidence_bytes,
            exact_imports,
        })
    }
}

fn bounded_read(path: &Path, limit: u64) -> Result<Vec<u8>, CompileError> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(failure("exact artifact exceeds byte bound"));
    }
    Ok(bytes)
}
fn row(value: &Value, length: usize) -> Result<&[Value], CompileError> {
    let values = list(value, length)?;
    if values.len() != length {
        return Err(failure("invalid exact compile row"));
    }
    Ok(values)
}
fn list(value: &Value, limit: usize) -> Result<&[Value], CompileError> {
    match value {
        Value::Array(values) if values.len() <= limit => Ok(values),
        _ => Err(failure("invalid exact compile inventory")),
    }
}
fn string(value: &Value) -> Result<&str, CompileError> {
    match value {
        Value::Text(value) => Ok(value),
        _ => Err(failure("invalid exact compile text")),
    }
}
fn boolean(value: &Value) -> Result<bool, CompileError> {
    match value {
        Value::Bool(value) => Ok(*value),
        _ => Err(failure("invalid exact compile boolean")),
    }
}

fn failure(message: impl std::fmt::Display) -> CompileError {
    CompileError::ExtractFailed(format!("exact declaration context: {message}"))
}

fn identity(unit: &str, module: &str) -> ExactModuleIdentity {
    ExactModuleIdentity {
        unit: unit.to_owned(),
        module: module.to_owned(),
    }
}

impl ExactDeclarationContext {
    pub fn new(
        authored: &[Arc<CertifiedAuthoredDeclaration>],
        joins: &[Arc<AcceptedJoin>],
        lexical: Vec<ExactLexicalNode>,
    ) -> Result<Self, CompileError> {
        Self {
            producer: [0; 32],
            products: Vec::new(),
            joins: Vec::new(),
            interfaces: Vec::new(),
            lexical: Vec::new(),
        }
        .extend(authored, joins, lexical)
    }

    pub fn capture_recovery(
        root: &Path,
        products: &[RecoveryArtifactRef],
        joins: &[RecoveryJoinRef],
        lexical: Vec<ExactLexicalNode>,
    ) -> Result<Self, CompileError> {
        let mut context = Self {
            producer: [0; 32],
            products: Vec::new(),
            joins: Vec::new(),
            interfaces: Vec::new(),
            lexical,
        };
        let verified = products
            .iter()
            .map(|reference| recovery_artifacts::verify_materialized_ref(root, reference))
            .collect::<Result<Vec<_>, _>>()
            .map_err(failure)?;
        let inputs = verified
            .iter()
            .map(|artifact| InheritedProductInput { artifact })
            .collect::<Vec<_>>();
        certify_inherited_products(&inputs, &[]).map_err(failure)?;
        for artifact in verified {
            context.admit_producer(artifact.reference.toolchain_identity_sha256)?;
            let product = CertifiedRecoveryProduct::from_certification(
                tidepool_repr::execution_schema::CachedHomeOwner {
                    unit: artifact.reference.unit,
                    module: artifact.reference.module,
                    module_version: tidepool_repr::execution_schema::ModuleVersion(
                        artifact.reference.module_version,
                    ),
                    skinny_iface_sha256: artifact.reference.skinny_iface_sha256,
                    product_sha256: artifact.reference.product_sha256,
                },
                artifact.interface_bytes,
                artifact.product_bytes,
                artifact.package_imports_bytes,
                artifact.certification_bytes,
            );
            let requirements = crate::certified_products::certified_home_requirements(
                product.certification_bytes(),
                product.owner(),
            )
            .map_err(failure)?
            .iter()
            .map(|owner| identity(&owner.unit, &owner.module))
            .collect();
            context.interfaces.push(ExactInterfaceOwner {
                owner: identity(&product.owner().unit, &product.owner().module),
                requirements,
            });
            context.products.push(product);
        }
        for reference in joins {
            context.admit_producer(reference.toolchain_identity_sha256)?;
            let artifact =
                recovery_artifacts::verify_materialized_join(root, reference).map_err(failure)?;
            context.joins.push(
                CertifiedJoinedInterface::from_certification(
                    reference.toolchain_identity_sha256,
                    reference.unit.clone(),
                    reference.module.clone(),
                    artifact.interface_bytes,
                    artifact.package_imports_bytes,
                )
                .map_err(failure)?,
            );
            context.interfaces.push(ExactInterfaceOwner {
                owner: identity(&reference.unit, &reference.module),
                requirements: Vec::new(),
            });
        }
        context.normalize()?;
        Ok(context)
    }

    /// Merge recovered inputs with fresh certificates, replacing the complete
    /// selected lexical graph. One owner cannot acquire two implementations.
    pub fn extend(
        mut self,
        authored: &[Arc<CertifiedAuthoredDeclaration>],
        joins: &[Arc<AcceptedJoin>],
        lexical: Vec<ExactLexicalNode>,
    ) -> Result<Self, CompileError> {
        for certificate in authored {
            self.admit_producer(certificate.toolchain_identity_sha256())?;
            self.products
                .extend_from_slice(certificate.recovery_products());
            self.interfaces.extend_from_slice(&certificate.interfaces);
            self.joins.extend_from_slice(&certificate.joined_interfaces);
        }
        for join in joins {
            self.admit_producer(join.toolchain_identity_sha256())?;
            self.products
                .extend_from_slice(join.context().recovery_products());
            self.joins.extend_from_slice(&join.context().joins);
            self.joins.push(join.interface().clone());
            self.interfaces
                .extend_from_slice(&join.context().interfaces);
            self.interfaces.push(ExactInterfaceOwner {
                owner: identity(join.interface().unit(), join.interface().module()),
                requirements: join
                    .context()
                    .interfaces
                    .iter()
                    .map(|interface| interface.owner.clone())
                    .collect(),
            });
        }
        self.lexical = lexical;
        self.normalize()?;
        Ok(self)
    }

    pub fn toolchain_identity_sha256(&self) -> [u8; 32] {
        self.producer
    }
    pub(crate) fn interface_owners(&self) -> &[ExactInterfaceOwner] {
        &self.interfaces
    }
    pub fn recovery_products(&self) -> &[CertifiedRecoveryProduct] {
        &self.products
    }
    pub fn joined_interfaces(&self) -> &[CertifiedJoinedInterface] {
        &self.joins
    }
    pub fn lexical_graph(&self) -> &[ExactLexicalNode] {
        &self.lexical
    }

    /// Logical identity is independent of materialization paths and of the
    /// optional source bytes retained only by a fresh authored certificate.
    pub fn semantic_sha256(&self) -> [u8; 32] {
        use sha2::Digest;
        let mut lexical = self.lexical.iter().collect::<Vec<_>>();
        lexical.sort_by_key(|node| &node.owner);
        let value = Value::Array(vec![
            text("TPEXACTCONTEXT"),
            text("1"),
            text(hex(&self.producer)),
            Value::Array(
                self.products
                    .iter()
                    .map(|product| {
                        let owner = product.owner();
                        Value::Array(vec![
                            text(&owner.unit),
                            text(&owner.module),
                            text(hex(&owner.module_version.0)),
                            text(hex(&owner.skinny_iface_sha256)),
                            text(hex(&owner.product_sha256)),
                            text(sha256(product.package_imports_bytes())),
                            text(sha256(product.certification_bytes())),
                        ])
                    })
                    .collect(),
            ),
            Value::Array(
                self.joins
                    .iter()
                    .map(|join| {
                        Value::Array(vec![
                            text(join.unit()),
                            text(join.module()),
                            text(sha256(join.interface_bytes())),
                            text(sha256(join.package_imports_bytes())),
                        ])
                    })
                    .collect(),
            ),
            Value::Array(
                lexical
                    .into_iter()
                    .map(|node| {
                        let mut imports = node.imports.iter().collect::<Vec<_>>();
                        imports.sort();
                        Value::Array(vec![
                            module_value(&node.owner),
                            Value::Array(imports.into_iter().map(module_value).collect()),
                        ])
                    })
                    .collect(),
            ),
        ]);
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&value, &mut bytes).expect("owned value encoding");
        sha2::Sha256::digest(bytes).into()
    }

    pub(crate) fn validate_artifacts(
        &self,
        artifacts: &[DeclarationArtifact],
    ) -> Result<(), CompileError> {
        let mut seen = BTreeSet::new();
        for artifact in artifacts {
            let exact = &artifact.interface;
            let owner = identity(&exact.unit, &exact.module);
            if !seen.insert(owner.clone()) || !exact.path.is_absolute() {
                return Err(failure("duplicate owner or relative interface path"));
            }
            let metadata = self
                .interfaces
                .iter()
                .find(|entry| entry.owner == owner)
                .ok_or_else(|| failure("artifact is not owned by the context"))?;
            if exact.requirements.iter().cloned().collect::<BTreeSet<_>>()
                != metadata
                    .requirements
                    .iter()
                    .map(|owner| (owner.unit.clone(), owner.module.clone()))
                    .collect()
            {
                return Err(failure("artifact requirements differ from owned context"));
            }
            let (iface, packages) =
                if let Some(product) = self.products.iter().find(|product| {
                    identity(&product.owner().unit, &product.owner().module) == owner
                }) {
                    let snapshot = artifact
                        .product
                        .as_ref()
                        .ok_or_else(|| failure("original product is missing"))?;
                    if !snapshot.path.is_absolute()
                        || snapshot.module != exact.module
                        || snapshot.sha256 != hex(&product.owner().product_sha256)
                        || std::fs::read(&snapshot.path)? != product.product_bytes()
                    {
                        return Err(failure("original product differs from owned bytes"));
                    }
                    (product.interface_bytes(), product.package_imports_bytes())
                } else {
                    let join = self
                        .joins
                        .iter()
                        .find(|join| identity(join.unit(), join.module()) == owner)
                        .ok_or_else(|| failure("synthetic anchor is missing"))?;
                    if artifact.product.is_some() {
                        return Err(failure("synthetic anchor has an implementation product"));
                    }
                    (join.interface_bytes(), join.package_imports_bytes())
                };
            if sha256(iface) != exact.sha256 || std::fs::read(&exact.path)? != iface {
                return Err(failure("interface differs from owned bytes"));
            }
            let mut packages_path = exact.path.as_os_str().to_os_string();
            packages_path.push(".packages");
            if std::fs::read(std::path::PathBuf::from(packages_path))? != packages {
                return Err(failure("package witness differs from owned bytes"));
            }
        }
        if seen.len() != self.interfaces.len() {
            return Err(failure("owned artifact closure is incomplete"));
        }
        Ok(())
    }

    fn admit_producer(&mut self, producer: [u8; 32]) -> Result<(), CompileError> {
        if producer == [0; 32] || (self.producer != [0; 32] && producer != self.producer) {
            return Err(failure("producer identity differs"));
        }
        self.producer = producer;
        Ok(())
    }

    fn normalize(&mut self) -> Result<(), CompileError> {
        let mut products = BTreeMap::new();
        for product in self.products.drain(..) {
            let owner = identity(&product.owner().unit, &product.owner().module);
            if let Some(previous) = products.get(&owner) {
                let previous: &CertifiedRecoveryProduct = previous;
                if previous.owner() != product.owner()
                    || previous.interface_bytes() != product.interface_bytes()
                    || previous.product_bytes() != product.product_bytes()
                    || previous.package_imports_bytes() != product.package_imports_bytes()
                    || previous.certification_bytes() != product.certification_bytes()
                {
                    return Err(failure("one original owner has differing artifacts"));
                }
            } else {
                products.insert(owner, product);
            }
        }
        let mut joins = BTreeMap::new();
        for join in self.joins.drain(..) {
            let owner = identity(join.unit(), join.module());
            if products.contains_key(&owner) {
                return Err(failure("synthetic interface collides with original owner"));
            }
            if joins.get(&owner).is_some_and(|previous| previous != &join) {
                return Err(failure("one synthetic owner has differing artifacts"));
            }
            joins.insert(owner, join);
        }
        let owners = products
            .keys()
            .chain(joins.keys())
            .cloned()
            .collect::<BTreeSet<_>>();
        if owners
            .iter()
            .map(|owner| &owner.module)
            .collect::<BTreeSet<_>>()
            .len()
            != owners.len()
        {
            return Err(failure("same module name occurs under multiple units"));
        }
        let mut interfaces = BTreeMap::new();
        for interface in self.interfaces.drain(..) {
            if !owners.contains(&interface.owner)
                || interface
                    .requirements
                    .iter()
                    .any(|owner| !owners.contains(owner))
            {
                return Err(failure("incomplete exact interface requirements"));
            }
            interfaces
                .entry(interface.owner)
                .or_insert_with(BTreeSet::new)
                .extend(interface.requirements);
        }
        if interfaces.len() != owners.len() {
            return Err(failure("missing original interface metadata"));
        }
        let lexical_owners = self
            .lexical
            .iter()
            .map(|node| &node.owner)
            .collect::<BTreeSet<_>>();
        if lexical_owners.len() != self.lexical.len()
            || self.lexical.iter().any(|node| {
                !owners.contains(&node.owner)
                    || node
                        .imports
                        .iter()
                        .any(|owner| !lexical_owners.contains(owner))
                    || node.imports.iter().collect::<BTreeSet<_>>().len() != node.imports.len()
            })
        {
            return Err(failure("invalid selected lexical graph"));
        }
        self.products = products.into_values().collect();
        self.joins = joins.into_values().collect();
        self.interfaces = interfaces
            .into_iter()
            .map(|(owner, requirements)| ExactInterfaceOwner {
                owner,
                requirements: requirements.into_iter().collect(),
            })
            .collect();
        Ok(())
    }

    pub fn materialize(
        &self,
        root: &Path,
    ) -> Result<MaterializedExactDeclarationContext, CompileError> {
        let references =
            recovery_artifacts::materialize_certified_products(root, self.producer, &self.products)
                .map_err(failure)?;
        let joined = self
            .joins
            .iter()
            .map(|join| join.materialize(root))
            .collect::<Result<Vec<_>, _>>()
            .map_err(failure)?;
        let requirements = self
            .interfaces
            .iter()
            .map(|interface| (&interface.owner, &interface.requirements))
            .collect::<BTreeMap<_, _>>();
        let mut artifacts = Vec::new();
        for reference in references {
            let owner = identity(&reference.unit, &reference.module);
            artifacts.push(DeclarationArtifact {
                interface: ExactIfaceArtifact {
                    unit: reference.unit,
                    module: reference.module.clone(),
                    path: root.join(reference.interface_path),
                    sha256: hex(&reference.skinny_iface_sha256),
                    requirements: requirements[&owner]
                        .iter()
                        .map(|owner| (owner.unit.clone(), owner.module.clone()))
                        .collect(),
                },
                product: Some(ModuleSnapshot {
                    module: reference.module,
                    path: root.join(reference.product_path),
                    sha256: hex(&reference.product_sha256),
                }),
            });
        }
        for reference in joined {
            let owner = identity(&reference.unit, &reference.module);
            artifacts.push(DeclarationArtifact {
                interface: ExactIfaceArtifact {
                    unit: reference.unit,
                    module: reference.module,
                    path: root.join(reference.interface_path),
                    sha256: hex(&reference.skinny_iface_sha256),
                    requirements: requirements[&owner]
                        .iter()
                        .map(|owner| (owner.unit.clone(), owner.module.clone()))
                        .collect(),
                },
                product: None,
            });
        }
        Ok(MaterializedExactDeclarationContext {
            artifacts,
            lexical: self.lexical.clone(),
        })
    }

    pub(crate) fn inherited_groups(
        &self,
        root: &Path,
    ) -> Result<Vec<PendingCertifiedGroup>, CompileError> {
        let references =
            recovery_artifacts::materialize_certified_products(root, self.producer, &self.products)
                .map_err(failure)?;
        let verified = references
            .iter()
            .map(|reference| recovery_artifacts::verify_materialized_ref(root, reference))
            .collect::<Result<Vec<_>, _>>()
            .map_err(failure)?;
        certify_inherited_products(
            &verified
                .iter()
                .map(|artifact| InheritedProductInput { artifact })
                .collect::<Vec<_>>(),
            &[],
        )
        .map_err(failure)
    }

    pub(crate) fn prepare_compilation(
        self: &Arc<Self>,
        root: &Path,
        producer: &[u8],
    ) -> Result<ExactCompilationRequest, CompileError> {
        self.prepare_compilation_with_authorization(root, producer, None)
    }

    pub(crate) fn prepare_compilation_with_authorization(
        self: &Arc<Self>,
        root: &Path,
        producer: &[u8],
        authorization: Option<Value>,
    ) -> Result<ExactCompilationRequest, CompileError> {
        use sha2::Digest;
        let admitted_empty = authorization.is_some()
            && self.producer == [0; 32]
            && self.products.is_empty()
            && self.joins.is_empty()
            && self.interfaces.is_empty()
            && self.lexical.is_empty();
        if !root.is_absolute()
            || (!admitted_empty
                && (self.producer == [0; 32]
                    || self.producer != <[u8; 32]>::from(sha2::Sha256::digest(producer))))
        {
            return Err(failure(
                "compile request has a different producer or invalid root",
            ));
        }
        std::fs::create_dir_all(root)?;
        let materialized = self.materialize(root)?;
        self.validate_artifacts(&materialized.artifacts)?;
        let groups = self.inherited_groups(root)?;
        let semantic_sha256 = self.semantic_sha256();
        let mut fields = vec![
            text("TPEXACTSCOPE"),
            text(if authorization.is_some() { "2" } else { "1" }),
            text(hex(&semantic_sha256)),
            Value::Array(
                materialized
                    .artifacts
                    .iter()
                    .map(|artifact| {
                        let iface = &artifact.interface;
                        let packages = iface.path.with_extension("hi.packages");
                        Ok(Value::Array(vec![
                            text(&iface.unit),
                            text(&iface.module),
                            path_value(&iface.path)?,
                            text(&iface.sha256),
                            Value::Array(
                                iface
                                    .requirements
                                    .iter()
                                    .map(|(unit, module)| {
                                        Value::Array(vec![text(unit), text(module)])
                                    })
                                    .collect(),
                            ),
                            path_value(&packages)?,
                            text(sha256(&std::fs::read(packages)?)),
                        ]))
                    })
                    .collect::<Result<Vec<_>, CompileError>>()?,
            ),
            Value::Array(
                materialized
                    .lexical
                    .iter()
                    .map(|node| {
                        Value::Array(vec![
                            module_value(&node.owner),
                            Value::Array(node.imports.iter().map(module_value).collect()),
                        ])
                    })
                    .collect(),
            ),
            Value::Array(
                self.products
                    .iter()
                    .map(|product| {
                        let owner = product.owner();
                        let artifact = materialized
                            .artifacts
                            .iter()
                            .find(|artifact| {
                                artifact.interface.unit == owner.unit
                                    && artifact.interface.module == owner.module
                            })
                            .and_then(|artifact| artifact.product.as_ref())
                            .ok_or_else(|| failure("original product anchor is missing"))?;
                        Ok(Value::Array(vec![
                            text(&owner.unit),
                            text(&owner.module),
                            text(hex(&owner.module_version.0)),
                            text(hex(&owner.skinny_iface_sha256)),
                            text(hex(&owner.product_sha256)),
                            path_value(&artifact.path)?,
                            Value::Array(
                                groups
                                    .iter()
                                    .filter(|group| group.owner() == owner)
                                    .map(|group| {
                                        Value::Array(vec![
                                            Value::Integer(group.group().original_ordinal().into()),
                                            Value::Array(
                                                group
                                                    .group()
                                                    .binders()
                                                    .iter()
                                                    .map(symbol_value)
                                                    .collect(),
                                            ),
                                            Value::Array(
                                                group
                                                    .group()
                                                    .globals()
                                                    .iter()
                                                    .map(|global| {
                                                        Value::Array(vec![
                                                            symbol_value(&global.identity),
                                                            Value::Bool(
                                                                global
                                                                    .required_generation
                                                                    .is_none(),
                                                            ),
                                                        ])
                                                    })
                                                    .collect(),
                                            ),
                                        ])
                                    })
                                    .collect(),
                            ),
                        ]))
                    })
                    .collect::<Result<Vec<_>, CompileError>>()?,
            ),
        ];
        if let Some(authorization) = authorization {
            fields.push(authorization);
        }
        let value = Value::Array(fields);
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&value, &mut bytes).map_err(failure)?;
        if bytes.len() > 4 * 1024 * 1024 {
            return Err(failure("scope manifest exceeds four MiB"));
        }
        let manifest = root.join("exact-declaration-scope.cbor");
        use std::io::Write;
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&manifest)?;
        output.write_all(&bytes)?;
        Ok(ExactCompilationRequest {
            context: self.clone(),
            manifest,
            request_sha256: sha256(&bytes),
            semantic_sha256,
            artifacts: materialized.artifacts,
            groups,
        })
    }
}

fn text(value: impl Into<String>) -> Value {
    Value::Text(value.into())
}
fn module_value(owner: &ExactModuleIdentity) -> Value {
    Value::Array(vec![text(&owner.unit), text(&owner.module)])
}
fn path_value(path: &Path) -> Result<Value, CompileError> {
    path.to_str()
        .map(text)
        .ok_or_else(|| failure("non-UTF-8 artifact path"))
}
fn symbol_value(symbol: &tidepool_repr::execution_schema::SymbolIdentity) -> Value {
    Value::Array(vec![
        text(&symbol.unit),
        text(&symbol.module),
        text(&symbol.namespace),
        text(&symbol.occurrence),
        symbol.record_parent.as_ref().map_or(Value::Null, text),
    ])
}

fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(bytes))
}
