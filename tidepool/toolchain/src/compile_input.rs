//! Input continuity is distinct from native inventory identity. The digest
//! excludes compiler-assigned output identifiers; its private proof binds the
//! entire output bundle that consumed those inputs in this process.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use sha2::{Digest, Sha256};
use tidepool_repr::{execution_schema::PreparedProgram, DataConTable};

use crate::artifacts::YieldSite;
use crate::cache::{DependencyEvidence, ModuleImportEvidence, ResolutionEvidence};
use crate::certified_products::{
    CertifiedTargetPackageInterfaces, PackageInterfaceWitness, PendingCertifiedGroup,
    PendingImportOwner,
};
use crate::CompileError;

/// Compiler-issued continuity of complete, admitted compilation inputs.
/// Cloning this proof does not authorize a different native output bundle.
#[derive(Debug, Clone)]
pub struct SealedCompileInputIdentity {
    identity: String,
    target: Arc<PreparedProgram>,
    groups: Arc<[PendingCertifiedGroup]>,
    target_owners: Arc<[PendingImportOwner]>,
    package_interfaces: CertifiedTargetPackageInterfaces,
    table: DataConTable,
    sites: Arc<[YieldSite]>,
}

impl SealedCompileInputIdentity {
    /// Versioned serialization for source-continuity intent. This string alone
    /// is not executable authority and cannot reconstruct the private proof.
    pub fn compile_input_identity(&self) -> &str {
        &self.identity
    }

    pub fn matches_bundle(
        &self,
        prepared: &PreparedProgram,
        groups: &[PendingCertifiedGroup],
        target_owners: &[PendingImportOwner],
        package_interfaces: &CertifiedTargetPackageInterfaces,
        table: &DataConTable,
        yield_sites: &[YieldSite],
    ) -> bool {
        (std::ptr::eq(self.target.as_ref(), prepared) || self.target.as_ref() == prepared)
            && self.groups.as_ref() == groups
            && self.target_owners.as_ref() == target_owners
            && self
                .package_interfaces
                .matches_bundle(package_interfaces, prepared)
            && &self.table == table
            && same_sites(&self.sites, yield_sites)
    }
}

fn same_sites(left: &[YieldSite], right: &[YieldSite]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut left: Vec<_> = left.iter().collect();
    let mut right: Vec<_> = right.iter().collect();
    left.sort_by_key(|site| site.site);
    right.sort_by_key(|site| site.site);
    left == right
}

#[derive(Serialize)]
struct InputRecipe<'a> {
    version: &'static str,
    producer: &'a [u8],
    source: &'a str,
    target: &'a str,
    include: &'a [PathBuf],
    sources: Vec<&'a crate::cache::SourceEvidence>,
    modules: Vec<ModuleInput<'a>>,
    resolutions: Vec<&'a ResolutionEvidence>,
    packages: Vec<&'a str>,
    package_interfaces: Vec<(&'a (String, String), &'a Path, [u8; 32])>,
}

#[derive(Serialize)]
struct ModuleInput<'a> {
    unit: &'a str,
    module: &'a str,
    boot: bool,
    source: &'a Path,
    imports: Vec<&'a ModuleImportEvidence>,
}

fn canonical_rows<T: Serialize>(mut rows: Vec<T>) -> Vec<T> {
    // These closed evidence types serialize without fallible values.
    rows.sort_by_cached_key(|row| serde_json::to_vec(row).expect("closed input evidence"));
    rows
}

fn input_identity(
    producer: &[u8],
    include: &[PathBuf],
    evidence: &DependencyEvidence,
    packages: &BTreeMap<(String, String), PackageInterfaceWitness>,
    source: &str,
    target: &str,
) -> Result<String, CompileError> {
    let mut package_units: Vec<_> = evidence.packages.iter().map(String::as_str).collect();
    package_units.sort_unstable();
    package_units.dedup();
    // Map tuple keys cannot be serialized as a JSON object. Preserve the full
    // authenticated closure, including instance/family-only package imports.
    let recipe = InputRecipe {
        version: "tidepool-compile-input-v1",
        producer,
        source,
        target,
        include,
        sources: canonical_rows(evidence.sources.iter().collect()),
        modules: canonical_rows(
            evidence
                .modules
                .iter()
                .map(|module| ModuleInput {
                    unit: &module.unit,
                    module: &module.module,
                    boot: module.boot,
                    source: &module.source,
                    imports: canonical_rows(module.imports.iter().collect()),
                })
                .collect(),
        ),
        resolutions: canonical_rows(evidence.resolutions.iter().collect()),
        packages: package_units,
        package_interfaces: packages
            .iter()
            .map(|(owner, witness)| (owner, witness.selected_path.as_path(), witness.sha256))
            .collect(),
    };
    let bytes = serde_json::to_vec(&recipe)
        .map_err(|error| CompileError::ExtractFailed(format!("compile input recipe: {error}")))?;
    Ok(format!(
        "tidepool-compile-input-v1:{:x}",
        Sha256::digest(bytes)
    ))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn seal(
    producer: &[u8],
    include: &[PathBuf],
    evidence: &DependencyEvidence,
    packages: &BTreeMap<(String, String), PackageInterfaceWitness>,
    source: &str,
    target: &str,
    prepared: &Arc<PreparedProgram>,
    groups: &Arc<[PendingCertifiedGroup]>,
    target_owners: &[PendingImportOwner],
    package_interfaces: &CertifiedTargetPackageInterfaces,
    table: DataConTable,
    sites: Vec<YieldSite>,
) -> Result<Option<SealedCompileInputIdentity>, CompileError> {
    // The initial contract covers ordinary startup compilation. Mutable
    // resident Val/Lib interfaces require a distinct context recipe.
    if target_owners
        .iter()
        .chain(groups.iter().flat_map(PendingCertifiedGroup::imports))
        .any(|owner| {
            matches!(
                owner,
                PendingImportOwner::Retained { .. } | PendingImportOwner::RetainedPackage { .. }
            )
        })
        || !evidence.cache_safe
        || evidence.modules.iter().any(|module| {
            module.module.starts_with("Tidepool.Session.")
                || module
                    .imports
                    .iter()
                    .any(|import| import.module.starts_with("Tidepool.Session."))
        })
    {
        return Ok(None);
    }
    if producer.is_empty()
        || !evidence.valid(source)
        || !package_interfaces.matches_target(prepared)
    {
        return Err(CompileError::ExtractFailed(
            "compile input identity lacks validated input or output ownership".into(),
        ));
    }
    crate::artifacts::YieldSites::from_sites(sites.clone())
        .map_err(|error| CompileError::Asks(error.to_string()))?;
    let include = include
        .iter()
        .map(std::path::absolute)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(SealedCompileInputIdentity {
        identity: input_identity(producer, &include, evidence, packages, source, target)?,
        target: prepared.clone(),
        groups: groups.clone(),
        target_owners: target_owners.to_vec().into(),
        package_interfaces: package_interfaces.clone(),
        table,
        sites: sites.into(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::{ImportQualifier, ModuleEvidence, ProductAvailability, SourceEvidence};

    fn evidence() -> DependencyEvidence {
        DependencyEvidence {
            version: 4,
            cache_safe: true,
            selection_complete: true,
            sources: vec![
                SourceEvidence {
                    path: "@generated-source".into(),
                    sha256: "a".repeat(64),
                },
                SourceEvidence {
                    path: "/source/Driver.hs".into(),
                    sha256: "b".repeat(64),
                },
            ],
            modules: vec![ModuleEvidence {
                unit: "main".into(),
                module: "Root".into(),
                boot: false,
                source: "@generated-source".into(),
                product: ProductAvailability::Ready,
                imports: vec![ModuleImportEvidence {
                    qualifier: ImportQualifier::Unqualified,
                    module: "Driver".into(),
                    boot: false,
                    selected: Some("/source/Driver.hs".into()),
                }],
            }],
            resolutions: vec![ResolutionEvidence {
                qualifier: ImportQualifier::Unqualified,
                module: "Driver".into(),
                boot: false,
                selected: Some("/source/Driver.hs".into()),
                candidates: vec!["/negative/Driver.hs".into(), "/source/Driver.hs".into()],
            }],
            packages: vec!["base".into(), "text".into()],
        }
    }

    #[test]
    fn compile_input_generated_source_relocation_is_stable() {
        let source = "module Root where root = 42";
        let input_at = |root: &Path| {
            let input = root.join("Root.hs");
            std::fs::write(&input, source).unwrap();
            let evidence = DependencyEvidence {
                version: 4,
                cache_safe: true,
                selection_complete: true,
                sources: vec![SourceEvidence {
                    path: input.clone(),
                    sha256: format!("{:x}", Sha256::digest(source)),
                }],
                modules: vec![ModuleEvidence {
                    unit: "main".into(),
                    module: "Root".into(),
                    boot: false,
                    source: input.clone(),
                    imports: vec![],
                    product: ProductAvailability::Ready,
                }],
                resolutions: vec![],
                packages: vec![],
            };
            let bytes = serde_json::to_vec(&evidence).unwrap();
            DependencyEvidence::from_worker(&bytes, &input, source).unwrap()
        };
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let first = input_at(first.path());
        let second = input_at(second.path());
        assert_eq!(
            input_identity(b"producer", &[], &first, &BTreeMap::new(), source, "root").unwrap(),
            input_identity(b"producer", &[], &second, &BTreeMap::new(), source, "root").unwrap()
        );
    }

    #[test]
    fn compile_input_canonical_order_preserves_source_and_resolution_identity() {
        let evidence = evidence();
        let include = vec!["/negative".into(), "/source".into()];
        let packages = BTreeMap::from([(
            ("base".into(), "Instances".into()),
            PackageInterfaceWitness {
                selected_path: "/package/Instances.hi".into(),
                sha256: [3; 32],
            },
        )]);
        let identity = |evidence: &DependencyEvidence,
                        include: &[PathBuf],
                        producer: &[u8],
                        packages: &BTreeMap<_, _>| {
            input_identity(
                producer,
                include,
                evidence,
                packages,
                "complete source",
                "root",
            )
            .unwrap()
        };
        let expected = identity(&evidence, &include, b"producer", &packages);
        let mut reordered = evidence.clone();
        reordered.sources.reverse();
        reordered.packages.reverse();
        reordered.modules[0].product = ProductAvailability::InterfaceOnly;
        assert_eq!(
            expected,
            identity(&reordered, &include, b"producer", &packages)
        );
        let mut changed = evidence.clone();
        changed.sources[1].sha256 = "c".repeat(64);
        assert_ne!(
            expected,
            identity(&changed, &include, b"producer", &packages)
        );
        let mut changed = evidence.clone();
        changed.resolutions[0].candidates.reverse();
        assert_ne!(
            expected,
            identity(&changed, &include, b"producer", &packages)
        );
        let mut changed_include = include.clone();
        changed_include.reverse();
        assert_ne!(
            expected,
            identity(&evidence, &changed_include, b"producer", &packages)
        );
        assert_ne!(
            expected,
            identity(&evidence, &include, b"other producer", &packages)
        );
        let mut changed_packages = packages.clone();
        changed_packages.values_mut().next().unwrap().sha256 = [4; 32];
        assert_ne!(
            expected,
            identity(&evidence, &include, b"producer", &changed_packages)
        );
    }
}
