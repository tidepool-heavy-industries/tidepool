//! Complete named physical output for native and portable compiler consumers.

use std::collections::{BTreeMap, BTreeSet};
use std::marker::PhantomData;
use std::path::{Component, Path, PathBuf};
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
    CertifiedProducts,
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
            Self::CertifiedProducts => "certified-products.cbor".into(),
            Self::PortableReceipt => "portable-output.mode".into(),
            Self::Dependencies => "dependencies.json".into(),
        }
    }
}

pub(crate) struct NativeOutput;
pub(crate) struct PortableOutput;
pub(crate) struct OriginalOutput;

pub(crate) struct InvocationOutputLayout<P = NativeOutput> {
    artifacts: Vec<Artifact>,
    purpose: PhantomData<P>,
}

impl InvocationOutputLayout<NativeOutput> {
    pub(crate) fn new(targets: &[&str]) -> Result<Self, CompileError> {
        Self::for_members(
            targets,
            [
                Artifact::ModuleProducts,
                Artifact::ModulePackageImports,
                Artifact::CertifiedProducts,
            ],
        )
    }
}

impl InvocationOutputLayout<OriginalOutput> {
    /// Original product sidecars are acquired by their one semantic issuer.
    pub(crate) fn original(targets: &[&str]) -> Result<Self, CompileError> {
        Self::for_members(targets, [Artifact::CertifiedProducts])
    }
}

impl InvocationOutputLayout<PortableOutput> {
    pub(crate) fn portable(targets: &[&str]) -> Result<Self, CompileError> {
        Self::for_members(targets, [Artifact::PortableReceipt, Artifact::Dependencies])
    }
}

impl<P> InvocationOutputLayout<P> {
    fn for_members<const N: usize>(
        targets: &[&str],
        members: [Artifact; N],
    ) -> Result<Self, CompileError> {
        if targets.is_empty() {
            return Err(CompileError::ExtractFailed(
                "invocation output requires a target".into(),
            ));
        }
        let mut artifacts = members
            .iter()
            .filter(|artifact| matches!(artifact, Artifact::PortableReceipt))
            .cloned()
            .collect::<Vec<_>>();
        artifacts.push(Artifact::Metadata);
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
        artifacts.extend(
            members
                .into_iter()
                .filter(|artifact| !matches!(artifact, Artifact::PortableReceipt)),
        );
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

    fn len(&self) -> usize {
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
    ) -> Result<InvocationOutputBundle<P>, CompileError> {
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
            let bytes =
                certified_products::read_bounded_with_operation(&path, limit as u64, &operation)
                    .map_err(|error| match &error {
                        certified_products::CertificationError::EvidenceRead {
                            failure: certified_products::EvidenceReadFailure::Io { error, .. },
                            ..
                        } if error.kind() == std::io::ErrorKind::NotFound => {
                            CompileError::MissingOutput(path)
                        }
                        _ => compiler_evidence_failure(error),
                    })?;
            if matches!(artifact, Artifact::PortableReceipt) && bytes != b"TPPORTABLEOUTPUT1\n" {
                return Err(CompileError::ExtractFailed(
                    "portable output mode receipt differs".into(),
                ));
            }
            Ok(bytes)
        })?;
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

/// A borrow of the immutable bytes and provenance held by a completed native
/// compiler operation. Its variants are private: paths and digests cannot mint
/// this authority independently of their production output owner.
pub(crate) struct CapturedNativeOutput<'a> {
    owner: NativeOutputOwner<'a>,
}

enum NativeOutputOwner<'a> {
    World(&'a crate::compiler_world::CheckedWorldCellProgram),
    Finite(&'a FiniteNativeOutput),
}

struct FiniteNativeOutput {
    root: PathBuf,
    issuing_root: PathBuf,
    producer: [u8; 32],
    profile: [u8; 32],
    operation: Arc<InventoryOperation>,
    files: BTreeMap<PathBuf, Arc<[u8]>>,
}

impl<'a> CapturedNativeOutput<'a> {
    pub(crate) fn from_world(program: &'a crate::compiler_world::CheckedWorldCellProgram) -> Self {
        Self {
            owner: NativeOutputOwner::World(program),
        }
    }

    pub(crate) fn directory(&self) -> &Path {
        match &self.owner {
            NativeOutputOwner::World(program) => program.directory().path(),
            NativeOutputOwner::Finite(output) => &output.issuing_root,
        }
    }

    pub(crate) fn physical_directory(&self) -> &Path {
        match &self.owner {
            NativeOutputOwner::World(program) => program.directory().path(),
            NativeOutputOwner::Finite(output) => &output.root,
        }
    }

    pub(crate) fn producer_sha256(&self) -> &[u8; 32] {
        match &self.owner {
            NativeOutputOwner::World(program) => program.producer_sha256(),
            NativeOutputOwner::Finite(output) => &output.producer,
        }
    }

    pub(crate) fn profile_sha256(&self) -> &[u8; 32] {
        match &self.owner {
            NativeOutputOwner::World(program) => program.logical().profile_sha256(),
            NativeOutputOwner::Finite(output) => &output.profile,
        }
    }

    pub(crate) fn operation(&self) -> &Arc<InventoryOperation> {
        match &self.owner {
            NativeOutputOwner::World(program) => &program.validation().inventory,
            NativeOutputOwner::Finite(output) => &output.operation,
        }
    }

    pub(crate) fn validate_interfaces(
        &self,
        receipt: &certified_products::CertifiedReceipt,
        validation: &mut crate::recovery_artifacts::PackageInterfaceValidation,
    ) -> Result<(), CompileError> {
        if !Arc::ptr_eq(self.operation(), &validation.inventory) {
            return Err(CompileError::ExtractFailed(
                "native output has another inventory owner".into(),
            ));
        }
        for witness in receipt.packages.values() {
            let relative = witness
                .selected_path
                .strip_prefix(self.directory())
                .map_err(|_| {
                    CompileError::ExtractFailed(
                        "package interface is outside completed output".into(),
                    )
                })?;
            validation
                .verify_captured(
                    &witness.selected_path,
                    &self.physical_directory().join(relative),
                    &witness.sha256,
                    self.bytes(relative)?,
                )
                .map_err(|error| {
                    compiler_evidence_failure(certified_products::package_validation_error(error))
                })?;
        }
        Ok(())
    }

    pub(crate) fn bytes(&self, relative: &Path) -> Result<&Arc<[u8]>, CompileError> {
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(CompileError::ExtractFailed(
                "invalid native output path".into(),
            ));
        }
        match &self.owner {
            NativeOutputOwner::World(program) => {
                crate::checked_cell::world_output_bytes(program, relative)
            }
            NativeOutputOwner::Finite(output) => output
                .files
                .get(relative)
                .ok_or_else(|| CompileError::MissingOutput(output.root.join(relative))),
        }
    }
}

/// This owner can only be returned after successful compiler completion and
/// acquisition of its exact named output. Portable output has no such issuer.
pub(super) struct CompletedNativeOutputBundle<P> {
    pub(super) bundle: InvocationOutputBundle<P>,
    pub(super) receipt: certified_products::CertifiedReceipt,
    output: FiniteNativeOutput,
}

impl<P> CompletedNativeOutputBundle<P> {
    pub(super) fn record_captured_members(
        &self,
        validation: &mut crate::recovery_artifacts::PackageInterfaceValidation,
    ) -> Result<(), CompileError> {
        validation
            .record_captured_bytes(
                &self.output.root.join("certified-products.cbor"),
                self.bundle.get(&Artifact::CertifiedProducts),
            )
            .map_err(|error| {
                compiler_evidence_failure(certified_products::package_validation_error(error))
            })?;
        for (relative, bytes) in &self.output.files {
            validation
                .record_captured_bytes(&self.output.root.join(relative), bytes)
                .map_err(|error| {
                    compiler_evidence_failure(certified_products::package_validation_error(error))
                })?;
        }
        Ok(())
    }

    pub(super) fn native_output(&self) -> CapturedNativeOutput<'_> {
        CapturedNativeOutput {
            owner: NativeOutputOwner::Finite(&self.output),
        }
    }
}

pub(super) trait NativeOutputPurpose {}
impl NativeOutputPurpose for NativeOutput {}
impl NativeOutputPurpose for OriginalOutput {}

/// Keep the actual completed transaction, selected deployment, physical root,
/// decoded receipt and its captured members together at the compile front door.
pub(super) fn extract_native<P: NativeOutputPurpose>(
    run: &tidepool_extract_cmd::ExtractRun,
    deployment: &crate::toolchain::AdmittedCompilerDeployment,
    root: &Path,
    layout: &InvocationOutputLayout<P>,
    on_stage: &mut impl FnMut(&str, std::time::Duration, u64),
    log_stderr: impl FnOnce(&str, bool),
) -> Result<CompletedNativeOutputBundle<P>, CompileError> {
    let bundle = super::extract_and_read(run, root, layout, on_stage, log_stderr)?;
    let association = deployment.package_source_association().ok_or_else(|| {
        CompileError::ExtractFailed(
            "native output requires an admitted package service profile".into(),
        )
    })?;
    capture_native_bundle(
        bundle,
        root,
        deployment.producer_identity,
        *association.profile_id(),
        root,
    )
}

/// Reopening uses the entry owner's checked manifest and inventory. It does not
/// manufacture a compiler transaction or a live package-service association.
pub(super) fn capture_retained(
    entry: &super::production_entry::VerifiedEntryOutput<'_>,
    operation: Arc<InventoryOperation>,
) -> Result<CompletedNativeOutputBundle<OriginalOutput>, CompileError> {
    let layout = InvocationOutputLayout::original(&["__prepared"])?;
    let bundle = layout.read(entry.root(), operation)?;
    capture_native_bundle(
        bundle,
        entry.root(),
        entry.producer(),
        entry.profile(),
        entry.issuing_root(),
    )
}

fn capture_native_bundle<P: NativeOutputPurpose>(
    bundle: InvocationOutputBundle<P>,
    root: &Path,
    producer: [u8; 32],
    profile: [u8; 32],
    issuing_root: &Path,
) -> Result<CompletedNativeOutputBundle<P>, CompileError> {
    let root = std::fs::canonicalize(root)?;
    if !issuing_root.is_absolute()
        || issuing_root
            .components()
            .any(|part| !matches!(part, Component::Normal(_) | Component::RootDir))
    {
        return Err(CompileError::ExtractFailed(
            "invalid native issuing output root".into(),
        ));
    }
    let receipt_bytes = bundle.get(&Artifact::CertifiedProducts);
    if receipt_bytes.is_empty() {
        return Err(CompileError::ExtractFailed(
            "native output product certificate unavailable".into(),
        ));
    }
    let receipt = certified_products::decode_receipt_with_operation(
        receipt_bytes,
        Some(&root),
        bundle.operation(),
    )
    .map_err(compiler_evidence_failure)?;
    bundle
        .operation()
        .reserve::<FiniteNativeOutput>(1)
        .map_err(|error| compiler_evidence_failure(error.into()))?;
    bundle
        .operation()
        .charge(
            root.as_os_str()
                .len()
                .saturating_add(issuing_root.as_os_str().len()),
        )
        .map_err(|error| compiler_evidence_failure(error.into()))?;
    let mut files = BTreeMap::new();
    for path in receipt
        .package_products
        .iter()
        .map(|product| issuing_root.join(&product.relative_path))
        .chain(
            receipt
                .packages
                .values()
                .map(|witness| witness.selected_path.clone()),
        )
    {
        let relative = path.strip_prefix(issuing_root).map_err(|_| {
            CompileError::ExtractFailed("native package member is outside completed output".into())
        })?;
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(CompileError::ExtractFailed(
                "invalid native package member path".into(),
            ));
        }
        if files.contains_key(relative) {
            continue;
        }
        let path = root.join(relative);
        let canonical = std::fs::canonicalize(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                CompileError::MissingOutput(path.clone())
            } else {
                CompileError::Io(error)
            }
        })?;
        if canonical != path {
            return Err(CompileError::ExtractFailed(
                "native package member aliases another path".into(),
            ));
        }
        bundle
            .operation()
            .reserve::<(PathBuf, Arc<[u8]>)>(1)
            .map_err(|error| compiler_evidence_failure(error.into()))?;
        let bytes = certified_products::read_bounded_with_operation(
            &path,
            bundle.operation().limits().max_module_bytes as u64,
            bundle.operation(),
        )
        .map_err(compiler_evidence_failure)?;
        bundle
            .operation()
            .charge(relative.as_os_str().len().saturating_add(bytes.len()))
            .map_err(|error| compiler_evidence_failure(error.into()))?;
        files.insert(relative.to_path_buf(), bytes.into());
    }
    let output = FiniteNativeOutput {
        root,
        issuing_root: issuing_root.to_path_buf(),
        producer: crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(
            &producer,
        )
        .sha256(),
        profile,
        operation: Arc::clone(bundle.operation()),
        files,
    };
    Ok(CompletedNativeOutputBundle {
        bundle,
        receipt,
        output,
    })
}

#[cfg(test)]
mod portable_tests {
    use super::*;

    #[test]
    fn original_acquisition_leaves_product_members_to_the_original_issuer() {
        let layout = InvocationOutputLayout::original(&["__prepared"]).unwrap();
        let mut observed = Vec::new();
        let bundle = layout
            .capture_members(
                Arc::new(InventoryOperation::new(Default::default())),
                |member| {
                    observed.push(member.filename());
                    Ok::<_, ()>(vec![1])
                },
            )
            .unwrap();
        assert_eq!(
            observed,
            [
                "meta.cbor",
                "__prepared.prepared.cbor",
                "asks.json",
                "certified-products.cbor"
            ]
        );
        assert_eq!(bundle.targets().len(), 1);
        assert_eq!(bundle.bytes_len(), 4);
    }

    #[test]
    fn portable_acquisition_refuses_wrong_mode_and_missing_members() {
        let layout = InvocationOutputLayout::portable(&["entry"]).unwrap();
        let root = tempfile::tempdir().unwrap();
        for (artifact, bytes) in [
            ("meta.cbor", b"metadata".as_slice()),
            ("entry.prepared.cbor", b"prepared"),
            ("asks.json", b"sites"),
            ("dependencies.json", b"dependencies"),
            ("portable-output.mode", b"TPPORTABLEOUTPUT1\n"),
        ] {
            std::fs::write(root.path().join(artifact), bytes).unwrap();
        }
        let operation = || Arc::new(InventoryOperation::new(Default::default()));
        layout.read(root.path(), operation()).unwrap();
        // Exactly eighteen bytes is the accepted marker; nineteen refuses at
        // the physical reader's bound before larger members are acquired.
        assert_eq!(b"TPPORTABLEOUTPUT1\n".len(), 18);
        std::fs::write(
            root.path().join("portable-output.mode"),
            b"TPPORTABLEOUTPUT1\nX",
        )
        .unwrap();
        std::fs::remove_file(root.path().join("meta.cbor")).unwrap();
        assert!(matches!(
            layout.read(root.path(), operation()),
            Err(CompileError::CompilerEvidence(error)) if matches!(*error,
                certified_products::CertificationError::EvidenceRead {
                    failure: certified_products::EvidenceReadFailure::SizeLimit { actual: 19, limit: 18 }, ..
                })
        ));
        std::fs::write(root.path().join("meta.cbor"), b"metadata").unwrap();
        std::fs::write(
            root.path().join("portable-output.mode"),
            b"TPPORTABLEOUTPUT0\n",
        )
        .unwrap();
        assert!(layout.read(root.path(), operation()).is_err());
        // Encoded-length-looking bytes are a wrong marker, never a decoder input.
        std::fs::write(
            root.path().join("portable-output.mode"),
            [0x9b, 255, 255, 255, 255, 255, 255, 255, 255],
        )
        .unwrap();
        assert!(layout.read(root.path(), operation()).is_err());
        std::fs::write(
            root.path().join("portable-output.mode"),
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
        let root = tempfile::tempdir().unwrap();
        for name in ["meta.cbor", "entry.prepared.cbor", "asks.json"] {
            std::fs::write(root.path().join(name), b"portable member").unwrap();
        }
        let layout = InvocationOutputLayout::new(&["entry"]).unwrap();
        let missing = layout.read(
            root.path(),
            Arc::new(InventoryOperation::new(Default::default())),
        );
        assert!(matches!(missing, Err(CompileError::MissingOutput(path))
            if path == root.path().join("module-products.cbor")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::execution_schema::InventoryDecodeLimits;

    // Representation-only raw files exercise acquisition, not semantic issuance.
    fn physical_outputs(root: &Path, names: &[&str]) {
        for name in names {
            std::fs::write(root.join(name), name.as_bytes()).unwrap();
        }
    }

    #[test]
    fn physical_output_requires_package_sidecar_and_preserves_ordered_targets() {
        for (targets, names) in [
            (
                vec!["result"],
                vec![
                    "meta.cbor",
                    "result.prepared.cbor",
                    "asks.json",
                    "module-products.cbor",
                    "module-package-imports.cbor",
                    "certified-products.cbor",
                ],
            ),
            (
                vec!["second", "first"],
                vec![
                    "meta.cbor",
                    "second.prepared.cbor",
                    "second.asks.json",
                    "first.prepared.cbor",
                    "first.asks.json",
                    "module-products.cbor",
                    "module-package-imports.cbor",
                    "certified-products.cbor",
                ],
            ),
        ] {
            let root = tempfile::tempdir().unwrap();
            physical_outputs(root.path(), &names);
            let layout = InvocationOutputLayout::new(&targets).unwrap();
            let read = || {
                layout.read(
                    root.path(),
                    Arc::new(InventoryOperation::new(Default::default())),
                )
            };
            let bundle = read().unwrap();
            assert_eq!(bundle.metadata(), b"meta.cbor");
            assert_eq!(bundle.products(), b"module-products.cbor");
            assert_eq!(bundle.package_imports(), b"module-package-imports.cbor");
            let raw = bundle.targets();
            assert_eq!(
                raw.iter()
                    .map(|target| target.target.as_str())
                    .collect::<Vec<_>>(),
                targets
            );
            for target in &raw {
                assert_eq!(
                    target.prepared_bytes.as_slice(),
                    format!("{}.prepared.cbor", target.target).as_bytes()
                );
                let asks = if targets.len() == 1 {
                    "asks.json".into()
                } else {
                    format!("{}.asks.json", target.target)
                };
                assert_eq!(target.asks_bytes.as_slice(), asks.as_bytes());
            }
            assert_eq!(
                bundle.bytes_len(),
                names.iter().map(|name| name.len() as u64).sum::<u64>()
            );
            // Every named output is required; package provenance cannot be omitted.
            for name in &names {
                std::fs::remove_file(root.path().join(name)).unwrap();
                assert!(
                    matches!(read(), Err(CompileError::MissingOutput(path)) if path == root.path().join(name))
                );
                std::fs::write(root.path().join(name), name.as_bytes()).unwrap();
            }
            // A failed observation is not evidence that a member is missing.
            let package = root.path().join("module-package-imports.cbor");
            std::fs::remove_file(&package).unwrap();
            std::fs::create_dir(&package).unwrap();
            assert!(matches!(read(), Err(CompileError::CompilerEvidence(_))));
        }
        for targets in [
            vec![],
            vec![""],
            vec!["same", "same"],
            vec!["../escape"],
            vec!["/absolute"],
            vec!["nested/target"],
        ] {
            assert!(InvocationOutputLayout::new(&targets).is_err());
        }
    }

    #[test]
    fn physical_output_acquisition_preserves_work_and_member_bounds() {
        let root = tempfile::tempdir().unwrap();
        let names = [
            "meta.cbor",
            "result.prepared.cbor",
            "asks.json",
            "module-products.cbor",
            "module-package-imports.cbor",
            "certified-products.cbor",
        ];
        physical_outputs(root.path(), &names);
        let layout = InvocationOutputLayout::new(&["result"]).unwrap();
        layout
            .read(
                root.path(),
                Arc::new(InventoryOperation::new(Default::default())),
            )
            .unwrap();
        let bounded = InventoryOperation::new(InventoryDecodeLimits {
            max_work: 1,
            ..Default::default()
        });
        assert!(layout.read(root.path(), Arc::new(bounded)).is_err());
        std::fs::write(root.path().join("asks.json"), [0; 128]).unwrap();
        let bounded = InventoryOperation::new(InventoryDecodeLimits {
            max_bytes: 127,
            ..Default::default()
        });
        let Err(CompileError::CompilerEvidence(error)) =
            layout.read(root.path(), Arc::new(bounded))
        else {
            panic!("required asks must respect aggregate admission byte bound");
        };
        assert!(matches!(
            *error,
            certified_products::CertificationError::EvidenceRead {
                failure: certified_products::EvidenceReadFailure::SizeLimit {
                    actual: 128,
                    limit: 127
                },
                ..
            }
        ));
    }

    #[test]
    fn cancelled_runtime_compile_refuses_before_worker_submission() {
        use tidepool_extract_cmd::{
            with_compiler_transaction_cancellable, CompilerTransactionCancellation,
            CompilerTransactionClose,
        };
        let cancellation = CompilerTransactionCancellation::new();
        cancellation.cancel();
        let mut stages = 0;
        let refused = with_compiler_transaction_cancellable(
            cancellation,
            |_| {},
            || {
                crate::artifacts::compile_invocation(
                    &crate::artifacts::CompileInvocation {
                        source: include_str!(
                            "../../tests/fixtures/invocation-output/NativeTargets.hs"
                        ),
                        targets: &["second", "first"],
                        include: &[],
                        fallback_module_name: "NativeTargets",
                    },
                    |_, _, _| stages += 1,
                    &mut |_| {},
                )
            },
        );
        assert!(
            matches!(refused.action, Err(CompileError::Io(error)) if error.kind() == std::io::ErrorKind::Interrupted)
        );
        assert_eq!(stages, 0);
        assert_eq!(refused.close, CompilerTransactionClose::NotStarted);
    }

    #[test]
    fn runtime_targets_complete_through_physical_output_with_native_products() {
        let root = tempfile::tempdir().unwrap();
        let includes = [root.path().to_owned()];
        let invocation = crate::artifacts::CompileInvocation {
            source: include_str!("../../tests/fixtures/invocation-output/NativeTargets.hs"),
            targets: &["second", "first"],
            include: &includes,
            fallback_module_name: "NativeTargets",
        };
        let mut executions = 0;
        let output =
            crate::artifacts::test_support::compile_invocation(&invocation, |stage, _, _| {
                executions += usize::from(stage == crate::timing::STAGE_EXTRACT_SPAWN);
            })
            .expect("actual runtime compile through the physical output owner");
        assert_eq!(executions, 1);
        assert_eq!(
            output
                .targets
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
        assert!(output
            .module_products
            .iter()
            .any(|product| product.module == "NativeTargets"));
        assert!(output
            .module_inventory
            .as_ref()
            .unwrap()
            .iter()
            .any(|module| module.module == "NativeTargets"
                && module.product == crate::cache::ProductAvailability::Ready));
    }
}
