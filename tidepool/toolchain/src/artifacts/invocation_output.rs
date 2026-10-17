//! Complete named compiler output, shared by physical acquisition and memo IO.

use sha2::{Digest, Sha256};
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
    pub(crate) fn capture<E>(
        &self,
        operation: Arc<InventoryOperation>,
        mut read: impl FnMut(&str, bool) -> Result<Vec<u8>, E>,
    ) -> Result<InvocationOutputBundle<P>, E> {
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
            purpose: PhantomData,
        })
    }

    pub(crate) fn read(
        &self,
        root: &Path,
        operation: Arc<InventoryOperation>,
    ) -> Result<InvocationOutputBundle<P>, CompileError> {
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

    pub(crate) fn receipt(&self) -> &[u8] {
        self.get(&Artifact::PortableReceipt)
    }

    pub(crate) fn validate_receipt(&self) -> Result<(), CompileError> {
        let mut reader = std::io::Cursor::new(self.receipt());
        let (magic, version, targets, members): (
            String,
            String,
            Vec<String>,
            Vec<(String, String)>,
        ) = ciborium::de::from_reader(&mut reader)
            .map_err(|error| CompileError::ExtractFailed(format!("portable receipt: {error}")))?;
        let expected_targets = self
            .artifacts
            .iter()
            .filter_map(|(artifact, _)| match artifact {
                Artifact::Prepared(target) => Some(target.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let expected_members = self
            .artifacts
            .iter()
            .filter_map(|(artifact, bytes)| {
                (!matches!(artifact, Artifact::PortableReceipt)).then(|| {
                    (
                        artifact.filename(),
                        format!("{:x}", Sha256::digest(bytes.as_slice())),
                    )
                })
            })
            .collect::<Vec<_>>();
        if reader.position() != self.receipt().len() as u64
            || magic != "TPPORTABLEOUTPUT"
            || version != "1"
            || targets != expected_targets
            || members != expected_members
        {
            return Err(CompileError::ExtractFailed(
                "portable output receipt differs from acquired members".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod portable_tests {
    use super::*;
    use std::collections::BTreeMap;

    fn portable_members() -> BTreeMap<String, Vec<u8>> {
        let mut members = BTreeMap::from([
            ("meta.cbor".into(), b"metadata".to_vec()),
            ("entry.prepared.cbor".into(), b"prepared".to_vec()),
            ("asks.json".into(), b"sites".to_vec()),
            ("dependencies.json".into(), b"dependencies".to_vec()),
        ]);
        let hashes = [
            "meta.cbor",
            "entry.prepared.cbor",
            "asks.json",
            "dependencies.json",
        ]
        .map(|name| {
            (
                name.to_owned(),
                format!("{:x}", Sha256::digest(&members[name])),
            )
        });
        let mut receipt = Vec::new();
        ciborium::ser::into_writer(&("TPPORTABLEOUTPUT", "1", ["entry"], hashes), &mut receipt)
            .unwrap();
        members.insert("portable-output.cbor".into(), receipt);
        members
    }

    #[test]
    fn portable_receipt_refuses_unpaired_output_and_wrong_target_mode() {
        let layout = InvocationOutputLayout::portable(&["entry"]).unwrap();
        let capture = |members: &BTreeMap<String, Vec<u8>>| {
            layout
                .capture(
                    Arc::new(InventoryOperation::new(Default::default())),
                    |name, _| members.get(name).cloned().ok_or_else(|| name.to_owned()),
                )
                .unwrap()
        };
        let members = portable_members();
        capture(&members).validate_receipt().unwrap();
        let mut stale = members.clone();
        stale.insert("asks.json".into(), b"other typed sites".to_vec());
        assert!(capture(&stale).validate_receipt().is_err());
        let mut trailing = members.clone();
        trailing.get_mut("portable-output.cbor").unwrap().push(0);
        assert!(capture(&trailing).validate_receipt().is_err());
        let mut wrong = members.clone();
        let mut receipt = Vec::new();
        ciborium::ser::into_writer(
            &(
                "TPPORTABLEOUTPUT",
                "0",
                ["entry"],
                Vec::<(String, String)>::new(),
            ),
            &mut receipt,
        )
        .unwrap();
        wrong.insert("portable-output.cbor".into(), receipt);
        assert!(capture(&wrong).validate_receipt().is_err());
        wrong = members.clone();
        wrong.insert("other.prepared.cbor".into(), b"prepared".to_vec());
        let other = InvocationOutputLayout::portable(&["other"])
            .unwrap()
            .capture(
                Arc::new(InventoryOperation::new(Default::default())),
                |name, _| wrong.get(name).cloned().ok_or_else(|| name.to_owned()),
            )
            .unwrap();
        assert!(other.validate_receipt().is_err());
    }

    #[test]
    fn portable_members_cannot_satisfy_native_layout() {
        let members = portable_members();
        let layout = InvocationOutputLayout::new(&["entry"]).unwrap();
        let missing = layout
            .capture(
                Arc::new(InventoryOperation::new(Default::default())),
                |name, _| members.get(name).cloned().ok_or_else(|| name.to_owned()),
            )
            .err()
            .expect("native output still requires native products");
        assert_eq!(missing, "module-products.cbor");
    }
}
