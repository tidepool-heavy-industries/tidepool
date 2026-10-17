//! Complete named physical compiler output for downstream interpretation.

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

    /// Capture every required member with its typed identity.
    fn capture<E>(
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

    pub(crate) fn bytes_len(&self) -> u64 {
        self.artifacts
            .iter()
            .map(|(_, bytes)| bytes.len() as u64)
            .sum()
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
