//! Complete named compiler output, shared by physical acquisition and memo IO.

use std::collections::BTreeSet;
use std::marker::PhantomData;
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
    PortableReceipt,
    Dependencies,
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
            Self::PortableReceipt => "portable-output.cbor".into(),
            Self::Dependencies => "dependencies.json".into(),
        }
    }
}

pub(crate) struct NativeOutput;
pub(crate) struct PortableOutput;

pub(crate) trait OutputPurpose: Sized {
    fn validate(bundle: &InvocationOutputBundle<Self>) -> Result<(), CompileError>;
}

impl OutputPurpose for NativeOutput {
    fn validate(_: &InvocationOutputBundle<Self>) -> Result<(), CompileError> {
        Ok(())
    }
}

impl OutputPurpose for PortableOutput {
    fn validate(bundle: &InvocationOutputBundle<Self>) -> Result<(), CompileError> {
        if bundle.get(&Artifact::PortableReceipt).as_slice() != b"TPPORTABLEOUTPUT1\n" {
            return Err(CompileError::ExtractFailed(
                "portable output mode receipt differs".into(),
            ));
        }
        Ok(())
    }
}

pub(crate) struct InvocationOutputLayout<P = NativeOutput> {
    artifacts: Vec<Artifact>,
    purpose: PhantomData<P>,
}

impl InvocationOutputLayout<NativeOutput> {
    pub(crate) fn new(targets: &[&str]) -> Result<Self, CompileError> {
        Self::for_members(
            targets,
            [Artifact::ModuleProducts, Artifact::ModulePackageImports],
        )
    }

    pub(crate) fn capture<E>(
        &self,
        operation: Arc<InventoryOperation>,
        mut read: impl FnMut(&str, bool) -> Result<Vec<u8>, E>,
    ) -> Result<InvocationOutputBundle, E> {
        self.capture_members(operation, |artifact| {
            read(
                &artifact.filename(),
                matches!(artifact, Artifact::Prepared(_)),
            )
        })
    }
}

impl InvocationOutputLayout<PortableOutput> {
    pub(crate) fn portable(targets: &[&str]) -> Result<Self, CompileError> {
        Self::for_members(targets, [Artifact::PortableReceipt, Artifact::Dependencies])
    }
}

impl<P> InvocationOutputLayout<P> {
    fn for_members(targets: &[&str], members: [Artifact; 2]) -> Result<Self, CompileError> {
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
        artifacts.extend(members);
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
        Ok(Self {
            artifacts,
            purpose: PhantomData,
        })
    }

    pub(crate) fn len(&self) -> usize {
        self.artifacts.len()
    }

    /// The reader must supply every required member. Its typed identity stays
    /// attached to its bytes through persistence and downstream interpretation.
    fn capture_members<E>(
        &self,
        operation: Arc<InventoryOperation>,
        mut read: impl FnMut(&Artifact) -> Result<Vec<u8>, E>,
    ) -> Result<InvocationOutputBundle<P>, E> {
        let mut artifacts = Vec::with_capacity(self.artifacts.len());
        for artifact in &self.artifacts {
            let bytes = read(artifact)?;
            artifacts.push((artifact.clone(), Arc::new(bytes)));
        }
        Ok(InvocationOutputBundle {
            artifacts,
            operation,
            purpose: PhantomData,
        })
    }

    pub(crate) fn read(
        &self,
        root: &Path,
        operation: Arc<InventoryOperation>,
    ) -> Result<InvocationOutputBundle<P>, CompileError>
    where
        P: OutputPurpose,
    {
        operation
            .reserve::<(Artifact, Arc<Vec<u8>>)>(self.len())
            .map_err(|error| compiler_evidence_failure(error.into()))?;
        let bundle = self.capture_members(operation.clone(), |artifact| {
            let path = root.join(artifact.filename());
            let limit = match artifact {
                Artifact::Prepared(_) => DecodeLimits::default().max_bytes,
                Artifact::PortableReceipt => b"TPPORTABLEOUTPUT1\n".len(),
                _ => operation.limits().max_bytes,
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
        })?;
        P::validate(&bundle)?;
        Ok(bundle)
    }
}

pub(crate) struct InvocationOutputBundle<P = NativeOutput> {
    artifacts: Vec<(Artifact, Arc<Vec<u8>>)>,
    operation: Arc<InventoryOperation>,
    purpose: PhantomData<P>,
}

impl<P> InvocationOutputBundle<P> {
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

impl InvocationOutputBundle<NativeOutput> {
    pub(crate) fn products(&self) -> &[u8] {
        self.get(&Artifact::ModuleProducts)
    }

    pub(crate) fn package_imports(&self) -> &[u8] {
        self.get(&Artifact::ModulePackageImports)
    }
}

impl InvocationOutputBundle<PortableOutput> {
    pub(crate) fn dependencies(&self) -> &[u8] {
        self.get(&Artifact::Dependencies)
    }
}

#[cfg(test)]
mod portable_tests {
    use super::*;

    #[test]
    fn portable_acquisition_refuses_wrong_mode_and_missing_members() {
        let layout = InvocationOutputLayout::portable(&["entry"]).unwrap();
        let root = tempfile::tempdir().unwrap();
        for (artifact, bytes) in [
            ("meta.cbor", b"metadata".as_slice()),
            ("entry.prepared.cbor", b"prepared"),
            ("asks.json", b"sites"),
            ("dependencies.json", b"dependencies"),
            ("portable-output.cbor", b"TPPORTABLEOUTPUT1\n"),
        ] {
            std::fs::write(root.path().join(artifact), bytes).unwrap();
        }
        let operation = || Arc::new(InventoryOperation::new(Default::default()));
        layout.read(root.path(), operation()).unwrap();
        std::fs::write(
            root.path().join("portable-output.cbor"),
            b"TPPORTABLEOUTPUT0\n",
        )
        .unwrap();
        assert!(layout.read(root.path(), operation()).is_err());
        // A corrupt encoded-length prefix is never decoded or allocated.
        std::fs::write(
            root.path().join("portable-output.cbor"),
            [0x9b, 255, 255, 255, 255, 255, 255, 255, 255],
        )
        .unwrap();
        assert!(layout.read(root.path(), operation()).is_err());
        std::fs::write(
            root.path().join("portable-output.cbor"),
            b"TPPORTABLEOUTPUT1\n",
        )
        .unwrap();
        std::fs::remove_file(root.path().join("entry.prepared.cbor")).unwrap();
        assert!(matches!(
            layout.read(root.path(), operation()),
            Err(CompileError::MissingOutput(_))
        ));
    }

    #[test]
    fn portable_members_cannot_satisfy_native_layout() {
        let layout = InvocationOutputLayout::new(&["entry"]).unwrap();
        let missing = layout
            .capture(
                Arc::new(InventoryOperation::new(Default::default())),
                |name, _| {
                    if matches!(name, "meta.cbor" | "entry.prepared.cbor" | "asks.json") {
                        Ok(Vec::new())
                    } else {
                        Err(name.to_owned())
                    }
                },
            )
            .err()
            .expect("native output still requires native products");
        assert_eq!(missing, "module-products.cbor");
    }
}
