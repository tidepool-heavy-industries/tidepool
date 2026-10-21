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
    Turn,
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
            Self::Turn => "turn.cbor".into(),
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
pub(super) struct TurnOutput;
struct TurnResult;

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
                Artifact::Turn => 32 << 20,
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

    pub(crate) fn bytes_len(&self) -> u64 {
        self.artifacts
            .iter()
            .map(|(_, bytes)| bytes.len() as u64)
            .sum()
    }
}

mod sidecar_purpose {
    pub trait Sealed {}
    impl Sealed for super::NativeOutput {}
    impl Sealed for super::OriginalOutput {}
    impl Sealed for super::PortableOutput {}
}

impl<P: sidecar_purpose::Sealed> InvocationOutputBundle<P> {
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
}

impl InvocationOutputBundle<TurnOutput> {
    pub(super) fn prepared(&self) -> &[u8] {
        self.get(&Artifact::Prepared("__prepared".into()))
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
                    CompileError::ExtractFailed(format!(
                        "package interface {} is outside completed output {}",
                        witness.selected_path.display(),
                        self.directory().display(),
                    ))
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

mod native_purpose {
    pub trait Sealed {}
    impl Sealed for super::NativeOutput {}
    impl Sealed for super::OriginalOutput {}
    impl Sealed for super::TurnOutput {}
}

pub(super) trait NativeOutputPurpose: native_purpose::Sealed {}
impl NativeOutputPurpose for NativeOutput {}
impl NativeOutputPurpose for OriginalOutput {}
impl NativeOutputPurpose for TurnOutput {}

/// A successful turn first yields its actual result kind. Declaration results
/// have no program sidecars; executable results complete this same acquisition.
pub(super) struct TurnAcquisition<'a> {
    bundle: InvocationOutputBundle<TurnResult>,
    deployment: &'a crate::toolchain::AdmittedCompilerDeployment,
    root: &'a Path,
}

impl TurnAcquisition<'_> {
    pub(super) fn turn_owned(&self) -> Result<Arc<[u8]>, CompileError> {
        let bytes = self.bundle.get(&Artifact::Turn);
        self.bundle
            .operation()
            .charge(bytes.len())
            .map_err(|error| compiler_evidence_failure(error.into()))?;
        Ok(bytes.as_slice().into())
    }

    pub(super) fn complete(self) -> Result<CompletedNativeOutputBundle<TurnOutput>, CompileError> {
        let layout = InvocationOutputLayout::<TurnOutput> {
            artifacts: vec![
                Artifact::Metadata,
                Artifact::Prepared("__prepared".into()),
                Artifact::CertifiedProducts,
            ],
            purpose: PhantomData,
        };
        let mut bundle = layout.read(self.root, Arc::clone(self.bundle.operation()))?;
        bundle.artifacts.extend(self.bundle.artifacts);
        let association = self
            .deployment
            .package_source_association()
            .ok_or_else(|| {
                CompileError::ExtractFailed(
                    "native output requires an admitted package service profile".into(),
                )
            })?;
        capture_native_bundle(
            bundle,
            self.root,
            self.deployment.producer_identity,
            *association.profile_id(),
            self.root,
        )
    }
}

pub(super) fn extract_turn<'a>(
    run: &tidepool_extract_cmd::ExtractRun,
    deployment: &'a crate::toolchain::AdmittedCompilerDeployment,
    root: &'a Path,
) -> Result<TurnAcquisition<'a>, CompileError> {
    let layout = InvocationOutputLayout::<TurnResult> {
        artifacts: vec![Artifact::Turn],
        purpose: PhantomData,
    };
    let bundle = super::extract_and_read(run, root, &layout, &mut |_: &str, _, _| {}, |_, _| {})?;
    Ok(TurnAcquisition {
        bundle,
        deployment,
        root,
    })
}

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
            CompileError::ExtractFailed(format!(
                "native package member {} is outside completed output {}",
                path.display(),
                issuing_root.display(),
            ))
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
    #[cfg(test)]
    capture_observation::observe(&output);
    Ok(CompletedNativeOutputBundle {
        bundle,
        receipt,
        output,
    })
}

#[cfg(test)]
mod capture_observation {
    use super::*;

    thread_local! {
        static ROOTS: std::cell::RefCell<Option<Vec<PathBuf>>> = const { std::cell::RefCell::new(None) };
    }

    pub(super) fn observe(output: &FiniteNativeOutput) {
        ROOTS.with(|slot| {
            if let Some(roots) = slot.borrow_mut().as_mut() {
                assert!(output.root.is_absolute());
                assert!(output.root.join("certified-products.cbor").is_file());
                let interfaces = output
                    .files
                    .keys()
                    .filter(|path| path.extension().is_some_and(|extension| extension == "hi"))
                    .collect::<Vec<_>>();
                assert!(
                    !interfaces.is_empty(),
                    "lifetime fixture must capture package interfaces"
                );
                assert!(interfaces
                    .iter()
                    .all(|path| output.root.join(path).is_file()));
                roots.push(output.root.clone());
            }
        });
    }

    pub(super) fn record<T>(action: impl FnOnce() -> T) -> (T, Vec<PathBuf>) {
        struct Restore(Option<Vec<PathBuf>>);
        impl Drop for Restore {
            fn drop(&mut self) {
                ROOTS.with(|slot| *slot.borrow_mut() = self.0.take());
            }
        }
        let _restore = Restore(ROOTS.with(|slot| slot.replace(Some(Vec::new()))));
        let result = action();
        let roots = ROOTS.with(|slot| slot.borrow_mut().take().unwrap());
        (result, roots)
    }
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
    #[ignore = "requires the rooted compiler service declared by toolchain_finite_output_tests"]
    fn admitted_turn_uses_inline_sites_and_keeps_declarations_without_programs() {
        for (kind, source, template, executable) in [
            (
                "expr",
                "41 :: Int",
                include_str!("../../tests/fixtures/invocation-output/NativeTurnExpr.hs"),
                true,
            ),
            (
                "decl",
                "answer :: Int\nanswer = 41",
                include_str!("../../tests/fixtures/invocation-output/NativeTurnDecl.hs"),
                false,
            ),
        ] {
            let scratch = tempfile::tempdir().unwrap();
            let input = scratch.path().join("turn.txt");
            let template_path = scratch.path().join("template.hs");
            std::fs::write(&input, source).unwrap();
            std::fs::write(&template_path, template).unwrap();
            let includes = [scratch.path().to_owned()];
            let mut command = tidepool_extract_cmd::ExtractCmd::new().unwrap();
            let endpoint =
                crate::toolchain::AdmittedCompilerEndpoint::from_bound(command.bind().unwrap())
                    .unwrap();
            let deployment = endpoint.deployment().clone();
            let offer = crate::artifacts::ModuleCandidateOffer::select_admitted(
                &endpoint,
                &includes,
                scratch.path(),
            )
            .unwrap();
            command
                .input(&input)
                .turn()
                .target("__prepared")
                .turn_template(kind, &template_path)
                .turn_verdict(kind)
                .turn_out(scratch.path().join("turn.cbor"))
                .output_dir(scratch.path())
                .includes(&includes)
                .session_root(scratch.path())
                .bind_gen(1);
            let applied = offer
                .prepare_admitted_turn(command, scratch.path())
                .unwrap();
            let mut closes = Vec::new();
            let output = applied
                .execute_admitted_turn(endpoint, &mut |close| closes.push(close))
                .expect("actual turn output through its completed acquisition");
            assert!(
                output.compiler_output().status.success(),
                "{}",
                String::from_utf8_lossy(&output.compiler_output().stderr)
            );
            assert!(matches!(
                closes.as_slice(),
                [tidepool_extract_cmd::CompilerTransactionClose::Clean]
            ));
            assert!(output.turn_bytes().is_some());
            assert_eq!(output.native_output().is_some(), executable);
            assert!(
                !output.directory().join("asks.json").exists(),
                "turn sites are inline"
            );
            assert_eq!(
                output
                    .directory()
                    .join("__prepared.prepared.cbor")
                    .is_file(),
                executable
            );
            if let Some(native) = output.native_output() {
                assert!(native.products().is_some());
                let captured = extract_turn(&output.run, &deployment, output.directory())
                    .unwrap()
                    .complete()
                    .unwrap();
                let owner = captured.native_output();
                let mut validation =
                    crate::recovery_artifacts::PackageInterfaceValidation::with_inventory(
                        Arc::clone(owner.operation()),
                    );
                let custody = certified_products::PackageInterfaceCustody::from_native_output(
                    &owner,
                    &captured.receipt,
                    &mut validation,
                )
                .unwrap();
                let mut missing_capture = captured.receipt.clone();
                let (key, witness) = missing_capture
                    .packages
                    .iter_mut()
                    .next()
                    .expect("actual executable turn retains TYPE package support");
                let ambient = output.directory().join("uncaptured-package-copy.hi");
                std::fs::write(&ambient, custody.get(key).unwrap().bytes()).unwrap();
                witness.selected_path = ambient.clone();
                assert!(ambient.is_file());
                assert!(
                    matches!(certified_products::PackageInterfaceCustody::from_native_output(
                    &owner, &missing_capture, &mut validation,
                ), Err(certified_products::CertificationError::OriginalClosure(error))
                    if matches!(*error, CompileError::MissingOutput(ref path) if *path == ambient)),
                    "fresh output cannot substitute an ambient file for absent captured custody"
                );
                std::fs::remove_file(ambient).unwrap();
                let operation = Arc::new(InventoryOperation::new(Default::default()));
                assert!(
                    matches!(
                        InvocationOutputLayout::original(&["__prepared"]).unwrap()
                            .read(output.directory(), operation),
                        Err(CompileError::MissingOutput(path)) if path == output.directory().join("asks.json")
                    ),
                    "standalone originals still require their separate sites"
                );
                std::fs::remove_file(output.directory().join("meta.cbor")).unwrap();
                let partial = extract_turn(&output.run, &deployment, output.directory()).unwrap();
                assert!(
                    matches!(partial.complete(), Err(CompileError::MissingOutput(path))
                    if path == output.directory().join("meta.cbor")),
                    "failed executable completion issues no native output owner"
                );
            }
        }
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
    #[ignore = "requires the rooted compiler service declared by toolchain_finite_output_tests"]
    fn finite_package_targets_reuse_exact_bindings_without_crossing_target_slots() {
        let root = tempfile::tempdir().unwrap();
        let includes = [root.path().to_owned()];
        let invocation = crate::artifacts::CompileInvocation {
            source: include_str!("../../tests/fixtures/invocation-output/NativePackageTargets.hs"),
            targets: &["second", "first"],
            include: &includes,
            fallback_module_name: "NativePackageTargets",
        };
        let mut executions = 0;
        let output =
            crate::artifacts::test_support::compile_invocation(&invocation, |stage, _, _| {
                executions += usize::from(stage == crate::timing::STAGE_EXTRACT_SPAWN);
            })
            .expect("actual finite native package output through common admission");
        assert_eq!(executions, 1);
        let completed = |target: &str| {
            Arc::clone(
                output.targets[target]
                    .native_inputs
                    .as_ref()
                    .expect("finite target has its actual native inputs")
                    .completed_selection()
                    .expect("finite target has its coissued completion"),
            )
        };
        let first = completed("first");
        let second = completed("second");
        crate::artifact_inventory::IssuedNativeSelectionCompletion::assert_collection_history(&[
            Arc::clone(first.completion()),
            Arc::clone(second.completion()),
        ]);
        for name in ["first", "second"] {
            let target = &output.targets[name];
            let selection = completed(name);
            let globals = target.prepared.prepared().globals();
            assert_eq!(
                selection.target_imports().len(),
                globals.len(),
                "each completed target keeps exactly its own ordered slots"
            );
            for (global, selected) in globals.iter().zip(selection.target_imports()) {
                match selected {
                    crate::artifact_inventory::CompletedNativeTargetImport::Package {
                        binding,
                        value,
                    } => {
                        assert_eq!(binding.exported_value(&global.identity), Some(*value));
                    }
                    crate::artifact_inventory::CompletedNativeTargetImport::Source {
                        binding,
                        value,
                    } => {
                        let row = selection
                            .completion()
                            .rows()
                            .find(|row| row.binding() == *binding)
                            .unwrap();
                        assert!(row
                            .product()
                            .group()
                            .exported_values()
                            .any(|(identity, exported)| identity == &global.identity
                                && exported == *value));
                    }
                    crate::artifact_inventory::CompletedNativeTargetImport::RetainedCapture {
                        identity,
                        generation,
                    } => {
                        assert_eq!(identity, &global.identity);
                        assert_eq!(Some(*generation), global.required_generation);
                    }
                }
            }
        }
        drop(output);
        let package_bindings = |selection: &crate::artifact_inventory::CompletedNativeSelection| {
            let mut products = BTreeMap::<_, BTreeSet<_>>::new();
            for binding in selection.completion().package_bindings() {
                let exact = binding.binding();
                products.entry(exact.artifact).or_default().insert(exact);
            }
            products
        };
        let first_packages = package_bindings(&first);
        let second_packages = package_bindings(&second);
        assert!(
            !first_packages.is_empty(),
            "reverse must use actual package code"
        );
        assert!(
            !second_packages.is_empty(),
            "the dependent target must retain actual package code"
        );
        {
            // Exercise storage with an actual compiler-issued package owner.
            let package_binding = first.completion().package_bindings().next().unwrap();
            let product = package_binding.product();
            for mode in [
                crate::recovery_artifacts::MaterializationMode::Durable,
                crate::recovery_artifacts::MaterializationMode::Scratch,
            ] {
                use crate::recovery_artifacts::{
                    PackageInterfaceValidation, RecoveryArtifactError,
                };
                use sha2::{Digest, Sha256};

                let storage = tempfile::tempdir().unwrap();
                let reference = product
                    .materialize(
                        storage.path(),
                        mode,
                        &mut PackageInterfaceValidation::default(),
                    )
                    .unwrap();
                let envelope_sha256: [u8; 32] = Sha256::digest(product.bytes()).into();
                assert_eq!(reference.envelope_sha256, envelope_sha256);
                assert_eq!(reference.product_sha256, *product.product_sha256());
                let descriptor =
                    crate::artifact_inventory::ArtifactDescriptor::from_recovery_package_code(
                        &reference,
                    );
                assert_eq!(product.descriptor(), &descriptor);
                assert_eq!(product.artifact_id(), package_binding.binding().artifact);
                let restored = certified_products::CertifiedPackageCodeProduct::restore(
                    storage.path(),
                    &reference,
                    &mut PackageInterfaceValidation::default(),
                )
                .unwrap();
                assert_eq!(restored.descriptor(), &descriptor);
                assert_eq!(restored.artifact_id(), product.artifact_id());
                let restored_storage = tempfile::tempdir().unwrap();
                assert_eq!(
                    restored
                        .materialize(
                            restored_storage.path(),
                            mode,
                            &mut PackageInterfaceValidation::default(),
                        )
                        .unwrap(),
                    reference,
                    "restoration preserves both envelope and ordered-import product identity"
                );
                let envelope_path = storage.path().join(&reference.envelope_path);
                let mut corrupt = product.bytes().to_vec();
                corrupt[0] ^= 1;
                std::fs::write(&envelope_path, corrupt).unwrap();
                for held in [product, &restored] {
                    assert!(matches!(
                        held.materialize(
                            storage.path(), mode, &mut PackageInterfaceValidation::default(),
                        ),
                        Err(RecoveryArtifactError::DigestMismatch(path)) if path == envelope_path
                    ));
                }
                assert!(matches!(
                    certified_products::CertifiedPackageCodeProduct::restore(
                        storage.path(), &reference, &mut PackageInterfaceValidation::default(),
                    ),
                    Err(certified_products::CertificationError::CapturedModulePayload(
                        RecoveryArtifactError::DigestMismatch(path)
                    )) if path == envelope_path
                ));
            }
        }
        let mut shared_products = 0;
        for (product, first_bindings) in &first_packages {
            if let Some(second_bindings) = second_packages.get(product) {
                shared_products += 1;
                assert_eq!(
                    first_bindings.len(),
                    1,
                    "one exact binding per common package product"
                );
                assert_eq!(
                    second_bindings.len(),
                    1,
                    "one exact binding per common package product"
                );
                assert_eq!(
                    first_bindings, second_bindings,
                    "every common package product keeps its exact already-issued binding"
                );
            }
        }
        assert!(
            shared_products > 0,
            "the two targets must demand overlapping actual package code"
        );
        assert!(!first.target_imports().is_empty());
        assert!(!second.target_imports().is_empty());
        let roots = first
            .target_imports()
            .iter()
            .filter_map(|import| match import {
                crate::artifact_inventory::CompletedNativeTargetImport::Package {
                    binding, ..
                } => Some(binding.binding()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(!roots.is_empty());
        let operation = InventoryOperation::new(Default::default());
        let package_only = crate::artifact_inventory::IssuedPackageCodeGraph::from_issued(
            first.completion().artifacts(),
            &roots,
            &operation,
        )
        .unwrap();
        assert_eq!(package_only.roots(), roots.as_slice());
        let package_snapshot = package_only.artifacts().capture_graph_selection();
        assert!(package_snapshot.namespace.is_empty());
        assert!(package_snapshot.roots.is_empty());
        assert!(package_snapshot.native_groups.is_empty());
        assert_eq!(
            package_snapshot
                .bindings
                .iter()
                .copied()
                .collect::<BTreeSet<_>>(),
            first_packages
                .values()
                .flat_map(|bindings| bindings.iter().copied())
                .collect::<BTreeSet<_>>()
        );
        let later = crate::artifact_inventory::CompilerSourceSupport::empty()
            .with_native_selection_completion(&first)
            .unwrap()
            .with_native_selection_completion(&second)
            .unwrap();
        let package_ids = first_packages
            .keys()
            .chain(second_packages.keys())
            .copied()
            .collect::<BTreeSet<_>>();
        drop(first);
        drop(second);
        assert_eq!(
            later
                .artifacts()
                .entries_for_artifacts(&package_ids)
                .unwrap()
                .len(),
            package_ids.len(),
            "later projection owns every actual package payload after its original parent drops"
        );
        drop(later);
        let retained_package_only = crate::artifact_inventory::ArtifactInventory::default()
            .empty_view()
            .retain_custody(package_only.artifacts())
            .unwrap();
        drop(package_only);
        let first_package_ids = first_packages.keys().copied().collect::<BTreeSet<_>>();
        assert_eq!(
            retained_package_only
                .entries_for_artifacts(&first_package_ids)
                .unwrap()
                .len(),
            first_package_ids.len(),
            "actual package-only payload custody survives later projection"
        );
    }

    #[test]
    #[ignore = "requires the rooted compiler service declared by toolchain_finite_output_tests"]
    fn finite_original_package_interfaces_survive_independent_outputs_and_cold_recovery() {
        use crate::recovery_artifacts::{PackageInterfaceValidation, RecoveryArtifactError};
        for (module, source, package_code) in [
            (
                "NativePackageTargets",
                include_str!("../../tests/fixtures/invocation-output/NativePackageTargets.hs"),
                true,
            ),
            (
                "NativeTargets",
                include_str!("../../tests/fixtures/invocation-output/NativeTargets.hs"),
                false,
            ),
        ] {
            let roots = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
            let mut originals = Vec::new();
            let mut directories = Vec::new();
            let mut identities = Vec::new();
            let mut native_counts = Vec::new();
            for root in &roots {
                let includes = [root.path().to_owned()];
                let (output, captured_roots) = capture_observation::record(|| {
                    crate::artifacts::test_support::compile_invocation(
                        &crate::artifacts::CompileInvocation {
                            source,
                            targets: &["first"],
                            include: &includes,
                            fallback_module_name: module,
                        },
                        |_, _, _| {},
                    )
                    .expect("actual independent native output")
                });
                let [directory] = captured_roots.as_slice() else {
                    panic!("one actual completed output expected");
                };
                let product = output
                    .recovery_products
                    .iter()
                    .find(|product| product.owner().module == module)
                    .unwrap()
                    .clone();
                let canonical = product.module_interface().unwrap();
                let native = product.original_native().unwrap();
                native_counts.push(native.package_interfaces().iter().count());
                if package_code {
                    assert!(
                        !native.package_interfaces().is_empty(),
                        "reverse must exercise nonempty Original package references"
                    );
                }
                let target = output.targets["first"].native_inputs.as_ref().unwrap();
                assert_eq!(
                    target
                        .completed_selection()
                        .unwrap()
                        .completion()
                        .package_bindings()
                        .len()
                        != 0,
                    package_code
                );
                if !package_code {
                    assert!(canonical.package_interfaces().iter().chain(native.package_interfaces().iter()).any(|(owner,_)| owner.1 == "GHC.Types"),
                        "constructor-only Original must keep TYPE support without native package demand");
                }
                identities.push(
                    crate::artifact_inventory::ArtifactEntry::original(
                        canonical.producer_sha256(),
                        product.clone(),
                    )
                    .unwrap()
                    .descriptor
                    .id,
                );
                directories.push(directory.clone());
                originals.push(product);
            }
            assert_ne!(
                directories[0], directories[1],
                "actual compiler outputs must have independent storage"
            );
            for (owner, token) in originals[0]
                .module_interface()
                .unwrap()
                .package_interfaces()
                .iter()
            {
                let other = originals[1]
                    .module_interface()
                    .unwrap()
                    .package_interfaces()
                    .get(owner)
                    .unwrap();
                assert_eq!(token.sha256(), other.sha256());
                assert_eq!(token.bytes(), other.bytes());
            }
            for directory in &directories {
                if directory.exists() {
                    assert!(directory.join(format!("{module}.hs")).is_file());
                    assert!(directory.join("certified-products.cbor").is_file());
                    std::fs::remove_dir_all(directory).unwrap();
                }
                assert!(!directory.exists());
            }
            let storage = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
            let mut references = Vec::new();
            let mut producers = Vec::new();
            for (index, (product, store)) in originals.iter().zip(&storage).enumerate() {
                let producer = product.module_interface().unwrap().producer_sha256();
                let before = crate::recovery_artifacts::package_interface_io();
                let reference = crate::recovery_artifacts::materialize_certified_products(
                    store.path(),
                    producer,
                    std::slice::from_ref(product),
                )
                .unwrap()
                .remove(0);
                assert_eq!(
                    crate::recovery_artifacts::package_interface_io(),
                    before,
                    "Original materialization must not reopen vanished producer paths"
                );
                assert_eq!(reference.package_interfaces.len(), native_counts[index]);
                references.push(reference);
                producers.push(producer);
            }
            drop(originals);
            for (index, (reference, store)) in references.iter().zip(&storage).enumerate() {
                let artifact =
                    crate::recovery_artifacts::verify_materialized_ref(store.path(), reference)
                        .unwrap();
                let recovered = certified_products::certify_recovery_products_with_validation(
                    vec![artifact],
                    &mut PackageInterfaceValidation::default(),
                )
                .unwrap()
                .remove(0)
                .product;
                assert_eq!(
                    crate::artifact_inventory::ArtifactEntry::original(producers[index], recovered)
                        .unwrap()
                        .descriptor
                        .id,
                    identities[index],
                    "custody restoration must preserve the issued Original identity"
                );
                let mut legacy = serde_json::to_value(reference).unwrap();
                legacy.as_object_mut().unwrap().remove("package_interfaces");
                assert!(
                    serde_json::from_value::<crate::recovery_artifacts::RecoveryArtifactRef>(
                        legacy
                    )
                    .is_err()
                );
                if !reference.package_interfaces.is_empty() {
                    let mut missing = reference.clone();
                    missing.package_interfaces.pop();
                    assert!(matches!(
                        crate::recovery_artifacts::verify_materialized_ref(store.path(), &missing),
                        Err(RecoveryArtifactError::InvalidReference)
                    ));
                    let mut foreign = reference.clone();
                    foreign.package_interfaces[0].unit.push_str("-foreign");
                    assert!(matches!(
                        crate::recovery_artifacts::verify_materialized_ref(store.path(), &foreign),
                        Err(RecoveryArtifactError::InvalidReference)
                    ));
                    let mut changed = reference.clone();
                    changed.package_interfaces[0].sha256[0] ^= 1;
                    assert!(matches!(
                        crate::recovery_artifacts::verify_materialized_ref(store.path(), &changed),
                        Err(RecoveryArtifactError::InvalidReference)
                    ));
                }
            }
        }
    }

    #[test]
    #[ignore = "requires the rooted compiler service declared by toolchain_finite_output_tests"]
    fn finite_canonical_package_interfaces_survive_output_release_and_recovery() {
        use crate::recovery_artifacts::{
            MaterializationMode, PackageInterfaceValidation, RecoveryArtifactError,
        };

        let source = tempfile::tempdir().unwrap();
        let includes = [source.path().to_owned()];
        let (output, captured_roots) = capture_observation::record(|| {
            crate::artifacts::test_support::compile_invocation(
                &crate::artifacts::CompileInvocation {
                    source: include_str!(
                        "../../tests/fixtures/invocation-output/NativePackageTargets.hs"
                    ),
                    targets: &["first"],
                    include: &includes,
                    fallback_module_name: "NativePackageTargets",
                },
                |_, _, _| {},
            )
            .expect("actual canonical package owner through the finite compiler front door")
        });
        let [producer] = captured_roots.as_slice() else {
            panic!("one actual completed output expected");
        };
        let canonical = output
            .recovery_products
            .iter()
            .find(|product| product.owner().module == "NativePackageTargets")
            .and_then(|product| product.module_interface())
            .expect("actual canonical interface paired to its Original")
            .clone();
        assert!(
            !canonical.package_interfaces().is_empty(),
            "the actual source must require package interfaces"
        );
        assert!(
            canonical.requirements().is_empty(),
            "the fixture has no HOME type dependencies"
        );
        let tokens = canonical
            .package_interfaces()
            .iter()
            .map(|(owner, token)| {
                (
                    owner.clone(),
                    *token.sha256(),
                    Arc::downgrade(token.bytes()),
                )
            })
            .collect::<Vec<_>>();
        let expected_certificate = canonical.certificate_bytes().to_vec();
        drop(output);
        if producer.exists() {
            assert!(producer.join("NativePackageTargets.hs").is_file());
            assert!(producer.join("certified-products.cbor").is_file());
            std::fs::remove_dir_all(&producer).unwrap();
        }
        assert!(
            !producer.exists(),
            "the compiler output directory must be unavailable"
        );
        assert!(tokens.iter().all(|(_, _, bytes)| bytes.upgrade().is_some()));

        let retained = tempfile::tempdir().unwrap();
        let io_before = crate::recovery_artifacts::package_interface_io();
        let reference = crate::recovery_artifacts::materialize_module_interface(
            retained.path(),
            &canonical,
            &mut PackageInterfaceValidation::default(),
            MaterializationMode::Durable,
        )
        .unwrap();
        assert_eq!(
            crate::recovery_artifacts::package_interface_io(),
            io_before,
            "canonical materialization consumes held bytes without reopening ambient package paths"
        );
        assert_eq!(reference.package_interfaces.len(), tokens.len());
        for (owner, seal, _) in &tokens {
            assert!(reference.package_interfaces.iter().any(|reference| (
                &reference.unit,
                &reference.module
            ) == (&owner.0, &owner.1)
                && &reference.sha256 == seal));
        }
        drop(canonical);
        assert!(
            tokens.iter().all(|(_, _, bytes)| bytes.upgrade().is_none()),
            "durable references do not retain the original compiler's byte owners"
        );

        let recovered = crate::recovery_artifacts::recover_module_interface(
            retained.path(),
            &reference,
            &mut PackageInterfaceValidation::default(),
        )
        .unwrap();
        assert_eq!(recovered.certificate_bytes(), expected_certificate);
        let inventory = crate::artifact_inventory::ArtifactInventory::default();
        let owner = crate::declaration_join::ExactModuleIdentity {
            unit: recovered.unit().to_owned(),
            module: recovered.module().to_owned(),
        };
        let producer_sha256 = recovered.producer_sha256();
        let entry = Arc::new(crate::artifact_inventory::ArtifactEntry::canonical(
            recovered.clone(),
        ));
        let view = inventory
            .admit_shared(&inventory.empty_view(), vec![entry.clone()])
            .unwrap();
        assert!(view.capture_graph_selection().native_groups.is_empty());
        let type_files = tempfile::tempdir().unwrap();
        let io_before = crate::recovery_artifacts::package_interface_io();
        crate::recovery_artifacts::materialize_selected_interface(
            type_files.path(),
            &owner,
            entry,
            producer_sha256,
            &mut PackageInterfaceValidation::default(),
        )
        .unwrap();
        assert_eq!(
            crate::recovery_artifacts::package_interface_io(),
            io_before,
            "later TYPE demand uses restored package byte custody"
        );

        let mut missing_reference = reference.clone();
        missing_reference.package_interfaces.pop();
        assert!(!recovered.matches_recovery_reference(&missing_reference));
        assert!(matches!(
            crate::recovery_artifacts::recover_module_interface(
                retained.path(),
                &missing_reference,
                &mut PackageInterfaceValidation::default(),
            ),
            Err(RecoveryArtifactError::InvalidReference)
        ));
        let mut wrong_reference = reference.clone();
        wrong_reference.package_interfaces[0].sha256[0] ^= 1;
        assert!(!recovered.matches_recovery_reference(&wrong_reference));
        assert!(matches!(
            crate::recovery_artifacts::recover_module_interface(
                retained.path(),
                &wrong_reference,
                &mut PackageInterfaceValidation::default(),
            ),
            Err(RecoveryArtifactError::InvalidReference)
        ));

        let package_path = retained.path().join(&reference.package_interfaces[0].path);
        let bytes = std::fs::read(&package_path).unwrap();
        std::fs::remove_file(&package_path).unwrap();
        assert!(
            matches!(crate::recovery_artifacts::recover_module_interface(
            retained.path(), &reference, &mut PackageInterfaceValidation::default(),
        ), Err(RecoveryArtifactError::Unavailable(path)) if path == package_path)
        );
        std::fs::write(&package_path, b"corrupt package interface").unwrap();
        assert!(
            matches!(crate::recovery_artifacts::recover_module_interface(
            retained.path(), &reference, &mut PackageInterfaceValidation::default(),
        ), Err(RecoveryArtifactError::DigestMismatch(path)) if path == package_path)
        );
        std::fs::write(&package_path, bytes).unwrap();
    }

    #[test]
    #[ignore = "requires the rooted compiler service declared by toolchain_finite_output_tests"]
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
