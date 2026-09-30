//! Exact declaration inputs retain original products independently of source
//! lookup, while their explicit virtual graph owns lexical visibility.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
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
    pub fn recovery_products(&self) -> &[CertifiedRecoveryProduct] {
        &self.products
    }
    pub fn joined_interfaces(&self) -> &[CertifiedJoinedInterface] {
        &self.joins
    }
    pub fn lexical_graph(&self) -> &[ExactLexicalNode] {
        &self.lexical
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
}

fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(bytes))
}
