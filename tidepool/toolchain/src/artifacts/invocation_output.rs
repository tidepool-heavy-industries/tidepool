//! Complete named compiler output, shared by physical acquisition and memo IO.

use std::collections::BTreeSet;
use std::path::{Component, Path};
use std::sync::Arc;

use tidepool_repr::execution_schema::{DecodeLimits, InventoryOperation};

use super::{certified_products, compiler_evidence_failure, CompileError, RawTargetOutput};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Artifact {
    Metadata,
    Prepared(String),
    YieldSites(Option<String>),
    ModuleProducts,
    ModulePackageImports,
}

impl Artifact {
    fn filename(&self) -> String {
        match self {
            Self::Metadata => "meta.cbor".into(),
            Self::Prepared(target) => super::prepared_artifact_name(target),
            Self::YieldSites(Some(target)) => format!("{target}.asks.json"),
            Self::YieldSites(None) => "asks.json".into(),
            Self::ModuleProducts => "module-products.cbor".into(),
            Self::ModulePackageImports => "module-package-imports.cbor".into(),
        }
    }
}

pub(crate) struct InvocationOutputLayout {
    artifacts: Vec<Artifact>,
}

impl InvocationOutputLayout {
    pub(crate) fn new(targets: &[&str]) -> Result<Self, CompileError> {
        if targets.is_empty() {
            return Err(CompileError::ExtractFailed(
                "invocation output requires a target".into(),
            ));
        }
        let mut artifacts = vec![Artifact::Metadata];
        let mut target_names = BTreeSet::new();
        for target in targets {
            if target.is_empty() || !target_names.insert(*target) {
                return Err(CompileError::ExtractFailed(format!(
                    "duplicate invocation target {target:?}"
                )));
            }
            artifacts.push(Artifact::Prepared((*target).into()));
            artifacts.push(Artifact::YieldSites(
                (targets.len() > 1).then(|| (*target).into()),
            ));
        }
        artifacts.extend([Artifact::ModuleProducts, Artifact::ModulePackageImports]);
        let mut filenames = BTreeSet::new();
        for artifact in &artifacts {
            let filename = artifact.filename();
            let mut components = Path::new(&filename).components();
            if !matches!(components.next(), Some(Component::Normal(_)))
                || components.next().is_some()
            {
                return Err(CompileError::ExtractFailed(format!(
                    "invalid invocation output name {filename:?}"
                )));
            }
            if !filenames.insert(filename) {
                return Err(CompileError::ExtractFailed(
                    "duplicate invocation output name".into(),
                ));
            }
        }
        Ok(Self { artifacts })
    }

    pub(crate) fn len(&self) -> usize {
        self.artifacts.len()
    }

    /// The reader must supply every required member. Its typed identity stays
    /// attached to its bytes through persistence and downstream interpretation.
    pub(crate) fn capture<E>(
        &self,
        operation: Arc<InventoryOperation>,
        mut read: impl FnMut(&str, bool) -> Result<Vec<u8>, E>,
    ) -> Result<InvocationOutputBundle, E> {
        let mut artifacts = Vec::with_capacity(self.artifacts.len());
        for artifact in &self.artifacts {
            let bytes = read(
                &artifact.filename(),
                matches!(artifact, Artifact::Prepared(_)),
            )?;
            artifacts.push((artifact.clone(), Arc::new(bytes)));
        }
        Ok(InvocationOutputBundle {
            artifacts,
            operation,
        })
    }

    pub(crate) fn read(
        &self,
        root: &Path,
        operation: Arc<InventoryOperation>,
    ) -> Result<InvocationOutputBundle, CompileError> {
        operation
            .reserve::<(Artifact, Arc<Vec<u8>>)>(self.len())
            .map_err(|error| compiler_evidence_failure(error.into()))?;
        self.capture(operation.clone(), |name, prepared| {
            let path = root.join(name);
            let limit = if prepared {
                DecodeLimits::default().max_bytes
            } else {
                operation.limits().max_bytes
            };
            certified_products::read_bounded_with_operation(&path, limit as u64, &operation)
                .map_err(|error| match &error {
                    certified_products::CertificationError::EvidenceRead {
                        failure: certified_products::EvidenceReadFailure::Io { error, .. },
                        ..
                    } if error.kind() == std::io::ErrorKind::NotFound => {
                        CompileError::MissingOutput(path)
                    }
                    _ => compiler_evidence_failure(error),
                })
        })
    }
}

pub(crate) struct InvocationOutputBundle {
    artifacts: Vec<(Artifact, Arc<Vec<u8>>)>,
    operation: Arc<InventoryOperation>,
}

impl InvocationOutputBundle {
    fn get(&self, selected: &Artifact) -> &Arc<Vec<u8>> {
        &self
            .artifacts
            .iter()
            .find(|(artifact, _)| artifact == selected)
            .expect("layout captured every required artifact")
            .1
    }

    pub(crate) fn metadata(&self) -> &[u8] {
        self.get(&Artifact::Metadata)
    }

    pub(crate) fn products(&self) -> &[u8] {
        self.get(&Artifact::ModuleProducts)
    }

    pub(crate) fn package_imports(&self) -> &[u8] {
        self.get(&Artifact::ModulePackageImports)
    }

    pub(crate) fn operation(&self) -> &Arc<InventoryOperation> {
        &self.operation
    }

    pub(crate) fn targets(&self) -> Vec<RawTargetOutput> {
        let multi = self
            .artifacts
            .iter()
            .filter(|(artifact, _)| matches!(artifact, Artifact::Prepared(_)))
            .count()
            > 1;
        self.artifacts
            .iter()
            .filter_map(|(artifact, prepared_bytes)| {
                let Artifact::Prepared(target) = artifact else {
                    return None;
                };
                Some(RawTargetOutput {
                    target: target.clone(),
                    asks_bytes: self
                        .get(&Artifact::YieldSites(multi.then(|| target.clone())))
                        .clone(),
                    prepared_bytes: prepared_bytes.clone(),
                })
            })
            .collect()
    }

    pub(crate) fn named_artifacts(&self) -> impl Iterator<Item = (String, &[u8])> {
        self.artifacts
            .iter()
            .map(|(artifact, bytes)| (artifact.filename(), bytes.as_slice()))
    }

    pub(crate) fn bytes_len(&self) -> u64 {
        self.artifacts
            .iter()
            .map(|(_, bytes)| bytes.len() as u64)
            .sum()
    }
}
