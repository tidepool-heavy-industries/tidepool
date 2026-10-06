//! Complete original source proofs retained independently of native products.
//! Only an admitted compiler transaction issues a graph; recovery authenticates
//! the same immutable bytes without requiring authored files to remain present.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::de::{SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tidepool_repr::execution_schema::{CachedHomeOwner, ModuleVersion};
use tidepool_repr::SessionModule;

use crate::cache::{
    DependencyEvidence, ImportQualifier, ModuleEvidence, ModuleImportEvidence, ProductAvailability,
    ResolutionEvidence, SourceEvidence,
};
use crate::certified_products::PackageInterfaceWitness;
use crate::declaration_join::ExactModuleIdentity;
use crate::CompileError;

pub(crate) const GRAPH_BYTES_LIMIT: usize = 64 << 20;
const SOURCE_BYTES_LIMIT: usize = 32 << 20;
const OWNER_LIMIT: usize = 4096;
const EDGE_LIMIT: usize = 65536;
const PROFILE: &str = "tidepool-ghc-pipeline-v1";

/// Untrusted canonical source-selection claims carry no native execution owner.
pub(crate) struct SourceSelectedOriginalClaim {
    pub(crate) owner: ExactModuleIdentity,
    pub(crate) certificate_sha256: [u8; 32],
    pub(crate) interface_sha256: [u8; 32],
    pub(crate) source_sha256: [u8; 32],
}

#[derive(Clone, Debug)]
pub(crate) struct SourceSelectedOriginal {
    interface: crate::certified_products::CertifiedModuleInterface,
    imports: Vec<ExactModuleIdentity>,
}

impl SourceSelectedOriginal {
    pub(crate) fn interface(&self) -> &crate::certified_products::CertifiedModuleInterface {
        &self.interface
    }
    pub(crate) fn imports(&self) -> &[ExactModuleIdentity] {
        &self.imports
    }
}

pub(crate) struct SourceSelectionContext<'a> {
    pub(crate) producer: [u8; 32],
    pub(crate) interfaces: &'a [crate::certified_products::CertifiedModuleInterface],
    pub(crate) exact_interfaces: &'a BTreeMap<ExactModuleIdentity, [u8; 32]>,
    pub(crate) include: &'a [PathBuf],
    pub(crate) fresh: &'a DependencyEvidence,
    pub(crate) roots: &'a [(ExactModuleIdentity, ImportQualifier, ExactModuleIdentity)],
    pub(crate) independent: &'a BTreeSet<ExactModuleIdentity>,
}

// Resolution candidate paths have their own aggregate edge budget, independent
// of resolution rows, module imports and exact import edges.
fn resolution_candidates_fit(resolutions: &[ResolutionEvidence]) -> bool {
    resolutions.len() <= EDGE_LIMIT
        && resolutions
            .iter()
            .all(|resolution| resolution.candidates.len() <= OWNER_LIMIT)
        && resolutions.iter().fold(0usize, |total, resolution| {
            total.saturating_add(resolution.candidates.len())
        }) <= EDGE_LIMIT
}

/// Validate current selection separately from the fresh-source cache evidence.
/// The ordered search roots come from the request, never from its receipt.
pub(crate) fn validate_source_selected_originals(
    claims: Vec<SourceSelectedOriginalClaim>,
    evidence: &DependencyEvidence,
    context: SourceSelectionContext<'_>,
) -> Result<BTreeMap<ExactModuleIdentity, SourceSelectedOriginal>, CompileError> {
    validate_source_selected_originals_with_validation(
        claims,
        evidence,
        context,
        &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
    )
}

pub(crate) fn validate_source_selected_originals_with_validation(
    claims: Vec<SourceSelectedOriginalClaim>,
    evidence: &DependencyEvidence,
    context: SourceSelectionContext<'_>,
    package_validation: &mut crate::recovery_artifacts::PackageInterfaceValidation,
) -> Result<BTreeMap<ExactModuleIdentity, SourceSelectedOriginal>, CompileError> {
    let refused = |detail: &str| failure(&format!("current source selection: {detail}"));
    if claims.is_empty()
        || claims.len() > OWNER_LIMIT
        || evidence.version != 4
        || !evidence.cache_safe
        || !evidence.selection_complete
        || evidence.sources.len() != claims.len()
        || evidence.modules.len() != claims.len()
        || !resolution_candidates_fit(&evidence.resolutions)
        || evidence.packages.len() > OWNER_LIMIT
        || context.include.len() > OWNER_LIMIT
        || context.include.iter().any(|root| !root.is_absolute())
        || evidence
            .modules
            .iter()
            .any(|module| module.imports.len() > OWNER_LIMIT)
        || evidence
            .modules
            .iter()
            .map(|module| module.imports.len())
            .sum::<usize>()
            > EDGE_LIMIT
    {
        return Err(refused("invalid or incomplete selection evidence"));
    }
    let interfaces = context
        .interfaces
        .iter()
        .map(|interface| {
            (
                ExactModuleIdentity {
                    unit: interface.unit().into(),
                    module: interface.module().into(),
                },
                interface,
            )
        })
        .collect::<BTreeMap<_, _>>();
    if interfaces.len() != context.interfaces.len() {
        return Err(refused("ambiguous canonical owner"));
    }
    let sources = evidence
        .sources
        .iter()
        .map(|source| (&source.path, &source.sha256))
        .collect::<BTreeMap<_, _>>();
    let modules = evidence
        .modules
        .iter()
        .map(|node| {
            (
                ExactModuleIdentity {
                    unit: node.unit.clone(),
                    module: node.module.clone(),
                },
                node,
            )
        })
        .collect::<BTreeMap<_, _>>();
    if sources.len() != evidence.sources.len() || modules.len() != evidence.modules.len() {
        return Err(refused("duplicate source evidence"));
    }
    let mut selected = BTreeMap::new();
    let mut paths = BTreeSet::new();
    let mut packages = BTreeSet::new();
    let mut expected_resolutions = BTreeMap::new();
    let mut record_resolution =
        |qualifier: &ImportQualifier, module: &str, path: Option<PathBuf>| {
            let key = (String::from(qualifier.clone()), module.to_owned(), false);
            if expected_resolutions
                .insert(key, path.clone())
                .is_some_and(|old| old != path)
            {
                Err(refused("conflicting current import resolutions"))
            } else {
                Ok(())
            }
        };
    for claim in claims {
        let key = claim.owner;
        let interface = interfaces
            .get(&key)
            .ok_or_else(|| refused("selected owner lacks a canonical interface"))?;
        if key.unit.is_empty()
            || key.module.is_empty()
            || SessionModule::is_reserved_name(&key.module)
            || selected.contains_key(&key)
            || interface.source_imports().is_none()
            || interface.producer_sha256() != context.producer
            || interface.interface_sha256() != claim.interface_sha256
            || interface.source_sha256() != claim.source_sha256
            || <[u8; 32]>::from(Sha256::digest(interface.certificate_bytes()))
                != claim.certificate_sha256
            || context.exact_interfaces.get(&key) != Some(&claim.interface_sha256)
        {
            return Err(refused(
                "selected canonical identity, origin or producer changed",
            ));
        }
        for ((unit, module), sha) in interface.requirements() {
            if context.exact_interfaces.get(&ExactModuleIdentity {
                unit: unit.clone(),
                module: module.clone(),
            }) != Some(sha)
            {
                return Err(refused("canonical interface dependency missing or changed"));
            }
        }
        let node = modules
            .get(&key)
            .ok_or_else(|| refused("selected owner lacks current source"))?;
        if node.boot
            || node.product != ProductAvailability::InterfaceOnly
            || !paths.insert(node.source.clone())
            || sources.get(&node.source).copied() != Some(&hex(&claim.source_sha256))
            || !node.source.is_absolute()
        {
            return Err(refused("current source owner, path or digest changed"));
        }
        let file = std::fs::File::open(&node.source)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() > SOURCE_BYTES_LIMIT as u64 {
            return Err(refused("selected source exceeds its bound"));
        }
        package_validation
            .inventory
            .charge(metadata.len() as usize + 1)
            .map_err(|error| failure(&error.to_string()))?;
        let mut bytes = Vec::with_capacity(metadata.len() as usize + 1);
        file.take(metadata.len() + 1).read_to_end(&mut bytes)?;
        use std::os::unix::fs::MetadataExt;
        let current_metadata = std::fs::metadata(&node.source)?;
        if metadata.dev() != current_metadata.dev() || metadata.ino() != current_metadata.ino() {
            return Err(refused("selected source path changed during capture"));
        }
        if bytes.len() as u64 != metadata.len()
            || <[u8; 32]>::from(Sha256::digest(&bytes)) != claim.source_sha256
        {
            return Err(refused("selected original source changed"));
        }
        let actual_imports = node
            .imports
            .iter()
            .map(|edge| crate::certified_products::CanonicalSourceImport {
                qualifier: edge.qualifier.clone(),
                module: edge.module.clone(),
                boot: edge.boot,
                home_unit: edge.selected.as_ref().map(|_| key.unit.clone()),
            })
            .map(|edge| edge.key())
            .collect::<BTreeSet<_>>();
        let original_imports = interface
            .source_imports()
            .expect("checked source origin")
            .iter()
            .map(|edge| edge.key())
            .collect::<BTreeSet<_>>();
        if actual_imports.len() != node.imports.len() || actual_imports != original_imports {
            return Err(refused(
                "current source import adjacency differs from its original certificate",
            ));
        }
        let package_proof =
            crate::recovery_artifacts::validate_package_import_evidence_with_validation(
                interface.package_imports_bytes(),
                interface.unit(),
                interface.module(),
                &claim.interface_sha256,
                Path::new("canonical-source-selection"),
                package_validation,
            )
            .map_err(|_| refused("canonical package interface changed"))?;
        let mut imports = BTreeSet::new();
        let mut edges = BTreeSet::new();
        for edge in &node.imports {
            if edge.boot
                || !edges.insert((
                    String::from(edge.qualifier.clone()),
                    edge.module.clone(),
                    edge.selected.clone(),
                ))
            {
                return Err(refused("duplicate or unsupported source import"));
            }
            if edge.selected.is_some() {
                if !matches!(&edge.qualifier, ImportQualifier::Unqualified)
                    && !matches!(&edge.qualifier, ImportQualifier::ThisUnit(unit) if unit == &key.unit)
                {
                    return Err(refused("home source import has another package qualifier"));
                }
                imports.insert(ExactModuleIdentity {
                    unit: key.unit.clone(),
                    module: edge.module.clone(),
                });
            } else {
                if !package_proof.roots().keys().any(|(unit, module)| {
                    module == &edge.module
                        && match &edge.qualifier {
                            ImportQualifier::OtherUnit(wanted) => unit == wanted,
                            _ => true,
                        }
                }) && !(edge.module == "GHC.Prim"
                    && !package_proof.compiler_provided().is_empty())
                {
                    return Err(refused("current package import lacks canonical evidence"));
                }
                packages.insert(edge.module.clone());
            }
            record_resolution(&edge.qualifier, &edge.module, edge.selected.clone())?;
        }
        selected.insert(
            key,
            SourceSelectedOriginal {
                interface: (*interface).clone(),
                imports: imports.into_iter().collect(),
            },
        );
    }
    if modules.keys().ne(selected.keys()) {
        return Err(refused("selection has another current owner"));
    }
    let fresh = context
        .fresh
        .modules
        .iter()
        .filter(|node| !node.boot)
        .map(|node| ExactModuleIdentity {
            unit: node.unit.clone(),
            module: node.module.clone(),
        })
        .collect::<BTreeSet<_>>();
    let mut pending = Vec::new();
    for (source, qualifier, owner) in context.roots {
        if selected.contains_key(owner) {
            if !fresh.contains(source)
                || source.unit != owner.unit
                || !matches!(qualifier, ImportQualifier::Unqualified)
                    && !matches!(qualifier,ImportQualifier::ThisUnit(unit) if unit == &owner.unit)
            {
                return Err(refused(
                    "selected root leaves the actual fresh import graph",
                ));
            }
            record_resolution(
                qualifier,
                &owner.module,
                Some(modules[owner].source.clone()),
            )?;
            pending.push(owner.clone());
        }
    }
    let mut reachable = BTreeSet::new();
    while let Some(owner) = pending.pop() {
        if !reachable.insert(owner.clone()) {
            continue;
        }
        for imported in selected[&owner].imports() {
            let edge = modules[&owner]
                .imports
                .iter()
                .find(|edge| edge.module == imported.module)
                .unwrap();
            if let Some(child) = modules.get(imported) {
                if edge.selected.as_ref() != Some(&child.source) {
                    return Err(refused("selected dependency source changed"));
                }
                pending.push(imported.clone());
            } else if !context.independent.contains(imported) && !fresh.contains(imported) {
                return Err(refused("selected source closure is incomplete"));
            }
        }
    }
    if reachable.iter().ne(selected.keys()) {
        return Err(refused(
            "selected owners are unreachable from actual imports",
        ));
    }
    if evidence.packages.len() != packages.len()
        || evidence.packages.iter().collect::<BTreeSet<_>>()
            != packages.iter().collect::<BTreeSet<_>>()
    {
        return Err(refused("current package selection changed"));
    }
    let mut observed = BTreeSet::new();
    for resolution in &evidence.resolutions {
        let key = (
            String::from(resolution.qualifier.clone()),
            resolution.module.clone(),
            resolution.boot,
        );
        if !observed.insert(key.clone())
            || expected_resolutions.get(&key) != Some(&resolution.selected)
        {
            return Err(refused(
                "resolution witness leaves the selected source graph",
            ));
        }
        if resolution.candidates
            != source_search_candidates(
                context.include,
                &resolution.qualifier,
                &resolution.module,
                resolution.boot,
                resolution.selected.as_deref(),
            )?
        {
            return Err(refused(
                "resolution omits or reorders current search candidates",
            ));
        }
        for path in &resolution.candidates {
            if Some(path) != resolution.selected.as_ref() {
                validate_absent_source(path)?;
            }
        }
    }
    if observed.len() != expected_resolutions.len() {
        return Err(refused("selection omits resolution evidence"));
    }
    Ok(selected)
}

fn validate_absent_source(path: &Path) -> Result<(), CompileError> {
    if !path.is_absolute() {
        return Err(failure(
            "current source selection: negative candidate is not absolute",
        ));
    }
    match std::fs::metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        _ => Err(failure(
            "current source selection: negative source candidate is present or unavailable",
        )),
    }
}

fn source_search_candidates(
    include: &[PathBuf],
    qualifier: &ImportQualifier,
    module: &str,
    boot: bool,
    selected: Option<&Path>,
) -> Result<Vec<PathBuf>, CompileError> {
    if matches!(qualifier, ImportQualifier::OtherUnit(_)) {
        return if selected.is_none() {
            Ok(vec![])
        } else {
            Err(failure(
                "current source selection: package import selected a home source",
            ))
        };
    }
    if module.split('.').any(|part| {
        part.is_empty()
            || !part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'\'')
    }) {
        return Err(failure(
            "current source selection: unsupported module search convention",
        ));
    }
    let extensions: &[&str] = if boot {
        &["hs-boot", "lhs-boot"]
    } else {
        &["hs", "lhs", "hsig", "lhsig"]
    };
    let relative = module.replace('.', "/");
    let mut candidates = Vec::new();
    for root in include {
        for extension in extensions {
            let candidate = root.join(format!("{relative}.{extension}"));
            if !candidates.contains(&candidate) {
                candidates.push(candidate);
            }
        }
    }
    if let Some(selected) = selected {
        let index = candidates
            .iter()
            .position(|candidate| candidate == selected)
            .ok_or_else(|| {
                failure("current source selection: selected source leaves trusted import roots")
            })?;
        candidates.truncate(index + 1);
    }
    Ok(candidates)
}

#[derive(Debug)]
pub(crate) enum ExecutionSourceAdmission {
    Available(Arc<CertifiedExecutionSourceGraph>),
    Unavailable(ExecutionSourceUnavailable),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExecutionSourceUnavailable {
    UnsupportedShape,
    LimitExceeded,
    UnavailableIncludeRoot,
    UnsupportedPath,
}

/// Inputs already checked against a successful original compiler transaction.
pub(crate) struct ExecutionSourceGraphInput<'a> {
    pub producer: crate::artifact_inventory::CanonicalProducerIdentity,
    pub semantic_sha256: Option<[u8; 32]>,
    pub include: &'a [PathBuf],
    pub source_path: &'a Path,
    pub source: &'a str,
    pub evidence: &'a DependencyEvidence,
    pub exact_imports: &'a BTreeMap<ExactModuleIdentity, Vec<ExactModuleIdentity>>,
    pub owners: &'a [CachedHomeOwner],
    pub fresh_owners: &'a BTreeSet<ExactModuleIdentity>,
    pub retained_sources: &'a BTreeMap<ExactModuleIdentity, [u8; 32]>,
    pub packages: &'a BTreeMap<(String, String), PackageInterfaceWitness>,
}

#[cfg(test)]
pub(crate) fn test_graph(
    root: &Path,
) -> (Arc<CertifiedExecutionSourceGraph>, Vec<CachedHomeOwner>) {
    let source = "module Input where\nvalue = 1\n";
    let input = root.join("Input.hs");
    std::fs::write(&input, source).unwrap();
    let mut sources = vec![SourceEvidence {
        path: "@generated-source".into(),
        sha256: hex(&Sha256::digest(source.as_bytes()).into()),
    }];
    let mut modules = vec![];
    let mut owners = vec![];
    for (index, module) in ["A", "B", "Input"].iter().enumerate() {
        let path = if *module == "Input" {
            PathBuf::from("@generated-source")
        } else {
            let path = root.join(format!("{module}.hs"));
            let text = format!("module {module} where\nvalue = 1\n");
            std::fs::write(&path, &text).unwrap();
            sources.push(SourceEvidence {
                path: path.clone(),
                sha256: hex(&Sha256::digest(text.as_bytes()).into()),
            });
            path
        };
        modules.push(ModuleEvidence {
            unit: "main".into(),
            module: (*module).into(),
            boot: false,
            source: path,
            imports: vec![],
            product: ProductAvailability::Ready,
        });
        owners.push(CachedHomeOwner {
            unit: "main".into(),
            module: (*module).into(),
            module_version: ModuleVersion([index as u8 + 1; 32]),
            skinny_iface_sha256: Sha256::digest(b"iface").into(),
            product_sha256: Sha256::digest(b"product").into(),
        });
    }
    let evidence = DependencyEvidence {
        version: 4,
        cache_safe: true,
        selection_complete: true,
        sources,
        resolutions: vec![],
        packages: vec![],
        modules,
    };
    let exact = BTreeMap::from([(
        ExactModuleIdentity {
            unit: "main".into(),
            module: "Input".into(),
        },
        vec![ExactModuleIdentity {
            unit: "main".into(),
            module: "Tidepool.Session.Val.G1".into(),
        }],
    )]);
    let graph = CertifiedExecutionSourceGraph::admit(ExecutionSourceGraphInput {
        producer: crate::artifact_inventory::CanonicalProducerIdentity::from_test_sha256([7; 32]),
        semantic_sha256: Some([8; 32]),
        include: &[],
        source_path: &input,
        source,
        evidence: &evidence,
        exact_imports: &exact,
        owners: &owners,
        packages: &BTreeMap::new(),
        fresh_owners: &owners
            .iter()
            .map(|owner| ExactModuleIdentity {
                unit: owner.unit.clone(),
                module: owner.module.clone(),
            })
            .collect(),
        retained_sources: &BTreeMap::new(),
    })
    .unwrap();
    let ExecutionSourceAdmission::Available(graph) = graph else {
        panic!("test graph is unavailable");
    };
    (graph, owners)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issued_recipe_preserves_source_package_closure_and_rejects_native_projection_or_tampering() {
        let root = tempfile::tempdir().unwrap();
        let (base, owners) = test_graph(root.path());
        let package = root.path().join("SourceOnly.hi");
        std::fs::write(&package, b"source import package interface").unwrap();
        let issued = test_graph_with_package_witness(&base, &package);
        let wire = GraphWire::decode(issued.bytes()).unwrap();
        let source_packages = BTreeMap::from([(
            ("package-unit".into(), "Package.Module".into()),
            PackageInterfaceWitness {
                selected_path: package,
                sha256: wire.packages[0].sha256,
            },
        )]);
        let native_packages = BTreeMap::new();
        let imports = wire
            .exact_imports
            .iter()
            .map(|row| (row.owner.clone(), row.imports.clone()))
            .collect::<BTreeMap<_, _>>();
        let fresh = owners
            .iter()
            .map(|owner| ExactModuleIdentity {
                unit: owner.unit.clone(),
                module: owner.module.clone(),
            })
            .collect();
        let retained = BTreeMap::new();
        let input = || ExecutionSourceGraphInput {
            producer: crate::artifact_inventory::CanonicalProducerIdentity::from_test_sha256(
                wire.producer_sha256,
            ),
            semantic_sha256: wire.semantic_sha256,
            include: &wire.include,
            source_path: &wire.source_path,
            source: &wire.source,
            evidence: &wire.evidence,
            exact_imports: &imports,
            owners: &owners,
            fresh_owners: &fresh,
            retained_sources: &retained,
            packages: &source_packages,
        };
        let bytes = Arc::clone(&issued.bytes);
        let admitted = CertifiedExecutionSourceGraph::admit_issued(
            Arc::clone(&bytes),
            issued.digest(),
            input(),
        )
        .unwrap();
        assert_eq!(admitted.digest(), issued.digest());
        assert!(
            Arc::ptr_eq(&admitted.bytes, &bytes),
            "retain the actual issued capsule"
        );
        assert!(CertifiedExecutionSourceGraph::admit_issued(
            Arc::clone(&bytes),
            issued.digest(),
            ExecutionSourceGraphInput {
                packages: &native_packages,
                ..input()
            }
        )
        .is_err());
        assert!(
            CertifiedExecutionSourceGraph::admit_issued(Arc::clone(&bytes), [0; 32], input())
                .is_err()
        );
        let inherited_owner = ExactModuleIdentity {
            unit: wire.owners[1].unit.clone(),
            module: wire.owners[1].module.clone(),
        };
        let mut selected_fresh = fresh.clone();
        selected_fresh.remove(&inherited_owner);
        let retained_recipe = BTreeMap::from([(inherited_owner, base.digest())]);
        let inherited_input = || ExecutionSourceGraphInput {
            fresh_owners: &selected_fresh,
            retained_sources: &retained_recipe,
            ..input()
        };
        let mut withheld = GraphWire::decode(&bytes).unwrap();
        withheld.owners[1].fresh = false;
        let encoded = withheld.encode().unwrap();
        let digest = Sha256::digest(&encoded).into();
        CertifiedExecutionSourceGraph::admit_issued(encoded.into(), digest, inherited_input())
            .unwrap();
        withheld.owners[1].original_graph_sha256 = Some(base.digest());
        let encoded = withheld.encode().unwrap();
        let digest = Sha256::digest(&encoded).into();
        CertifiedExecutionSourceGraph::admit_issued(encoded.into(), digest, inherited_input())
            .unwrap();
        withheld.owners[1].original_graph_sha256 = Some([99; 32]);
        let encoded = withheld.encode().unwrap();
        let digest = Sha256::digest(&encoded).into();
        assert!(CertifiedExecutionSourceGraph::admit_issued(
            encoded.into(),
            digest,
            inherited_input()
        )
        .is_err());
        let mut altered = GraphWire::decode(&bytes).unwrap();
        altered.owners[0].product_sha256[0] ^= 1;
        let altered = altered.encode().unwrap();
        let altered_digest = Sha256::digest(&altered).into();
        assert!(CertifiedExecutionSourceGraph::admit_issued(
            altered.into(),
            altered_digest,
            input()
        )
        .is_err());
        let mut altered = GraphWire::decode(&bytes).unwrap();
        altered.packages.clear();
        let altered = altered.encode().unwrap();
        let altered_digest = Sha256::digest(&altered).into();
        assert!(CertifiedExecutionSourceGraph::admit_issued(
            altered.into(),
            altered_digest,
            input()
        )
        .is_err());
    }

    #[test]
    fn canonical_source_selection_needs_no_native_recipe_and_refuses_drift() {
        let root = tempfile::tempdir().unwrap();
        let owner = ExactModuleIdentity {
            unit: "main".into(),
            module: "A".into(),
        };
        let consumer = ExactModuleIdentity {
            unit: "main".into(),
            module: "Consumer".into(),
        };
        let source = root.path().join("A.hs");
        let bytes = b"module A where\ntype Answer = Int\n";
        std::fs::write(&source, bytes).unwrap();
        let source_sha: [u8; 32] = Sha256::digest(bytes).into();
        let interface = crate::certified_products::fixture_source_module_interface(
            [2; 32],
            "main",
            "A",
            source_sha,
            BTreeMap::new(),
            None,
        );
        let exact = BTreeMap::from([(owner.clone(), interface.interface_sha256())]);
        let interfaces = [interface.clone()];
        let include = [root.path().to_path_buf()];
        let fresh = DependencyEvidence {
            version: 4,
            cache_safe: true,
            selection_complete: true,
            sources: vec![],
            resolutions: vec![],
            packages: vec![],
            modules: vec![ModuleEvidence {
                unit: "main".into(),
                module: "Consumer".into(),
                boot: false,
                source: root.path().join("Consumer.hs"),
                imports: vec![],
                product: ProductAvailability::Ready,
            }],
        };
        let evidence = DependencyEvidence {
            version: 4,
            cache_safe: true,
            selection_complete: true,
            sources: vec![SourceEvidence {
                path: source.clone(),
                sha256: hex(&source_sha),
            }],
            modules: vec![ModuleEvidence {
                unit: "main".into(),
                module: "A".into(),
                boot: false,
                source: source.clone(),
                imports: vec![],
                product: ProductAvailability::InterfaceOnly,
            }],
            packages: vec![],
            resolutions: vec![ResolutionEvidence {
                qualifier: ImportQualifier::Unqualified,
                module: "A".into(),
                boot: false,
                selected: Some(source.clone()),
                candidates: vec![source.clone()],
            }],
        };
        let roots = [(consumer, ImportQualifier::Unqualified, owner.clone())];
        let independent = BTreeSet::new();
        let context = || SourceSelectionContext {
            producer: [2; 32],
            interfaces: &interfaces,
            exact_interfaces: &exact,
            include: &include,
            fresh: &fresh,
            roots: &roots,
            independent: &independent,
        };
        let claim = || SourceSelectedOriginalClaim {
            owner: owner.clone(),
            source_sha256: source_sha,
            certificate_sha256: Sha256::digest(interface.certificate_bytes()).into(),
            interface_sha256: interface.interface_sha256(),
        };
        assert!(validate_source_selected_originals(vec![claim()], &evidence, context()).is_ok());
        // A duplicated alias/preamble edge retains the same actual source demand.
        let duplicate = [roots[0].clone(), roots[0].clone()];
        assert!(validate_source_selected_originals(
            vec![claim()],
            &evidence,
            SourceSelectionContext {
                roots: &duplicate,
                ..context()
            }
        )
        .is_ok());
        for field in 0..3 {
            let mut forged = claim();
            match field {
                0 => forged.certificate_sha256[0] ^= 1,
                1 => forged.interface_sha256[0] ^= 1,
                _ => forged.source_sha256[0] ^= 1,
            };
            assert!(
                validate_source_selected_originals(vec![forged], &evidence, context()).is_err()
            );
        }
        assert!(validate_source_selected_originals(
            vec![claim()],
            &evidence,
            SourceSelectionContext {
                producer: [9; 32],
                ..context()
            }
        )
        .is_err());
        assert!(validate_source_selected_originals(
            vec![claim()],
            &evidence,
            SourceSelectionContext {
                roots: &[],
                ..context()
            }
        )
        .is_err());
        std::fs::write(&source, b"module A where\ntype Answer = Bool\n").unwrap();
        assert!(validate_source_selected_originals(vec![claim()], &evidence, context()).is_err());
        std::fs::remove_file(&source).unwrap();
        assert!(validate_source_selected_originals(vec![claim()], &evidence, context()).is_err());
        std::fs::write(&source, bytes).unwrap();
        let earlier = root.path().join("earlier");
        std::fs::create_dir(&earlier).unwrap();
        let include_earlier = [earlier.clone(), root.path().to_path_buf()];
        assert!(validate_source_selected_originals(
            vec![claim()],
            &evidence,
            SourceSelectionContext {
                include: &include_earlier,
                ..context()
            }
        )
        .is_err());
        let alias_root = root.path().join("active");
        std::os::unix::fs::symlink(root.path(), &alias_root).unwrap();
        let alias_source = alias_root.join("A.hs");
        let mut alias_evidence = evidence.clone();
        alias_evidence.sources[0].path = alias_source.clone();
        alias_evidence.modules[0].source = alias_source.clone();
        alias_evidence.resolutions[0].selected = Some(alias_source.clone());
        alias_evidence.resolutions[0].candidates = vec![alias_source];
        let alias_include = [alias_root];
        assert!(validate_source_selected_originals(
            vec![claim()],
            &alias_evidence,
            SourceSelectionContext {
                include: &alias_include,
                ..context()
            }
        )
        .is_ok());
        let package = root.path().join("package.hi");
        std::fs::write(&package, b"canonical package interface").unwrap();
        let packaged = crate::certified_products::fixture_source_module_interface(
            [2; 32],
            "main",
            "A",
            source_sha,
            BTreeMap::new(),
            Some(&package),
        );
        let package_claim = || SourceSelectedOriginalClaim {
            owner: owner.clone(),
            source_sha256: source_sha,
            certificate_sha256: Sha256::digest(packaged.certificate_bytes()).into(),
            interface_sha256: packaged.interface_sha256(),
        };
        let packaged_interfaces = [packaged.clone()];
        assert!(validate_source_selected_originals(
            vec![package_claim()],
            &evidence,
            SourceSelectionContext {
                interfaces: &packaged_interfaces,
                ..context()
            }
        )
        .is_ok());
        std::fs::write(&package, b"changed package interface").unwrap();
        assert!(validate_source_selected_originals(
            vec![package_claim()],
            &evidence,
            SourceSelectionContext {
                interfaces: &packaged_interfaces,
                ..context()
            }
        )
        .is_err());
        // A failed selection cannot contaminate a later valid request.
        assert!(validate_source_selected_originals(vec![claim()], &evidence, context()).is_ok());
    }

    #[test]
    fn canonical_source_selection_traverses_authored_edges_without_promoting_type_requirements() {
        let root = tempfile::tempdir().unwrap();
        let key = |name: &str| ExactModuleIdentity {
            unit: "main".into(),
            module: name.into(),
        };
        let a = root.path().join("A.hs");
        let b = root.path().join("B.hs");
        let a_bytes = b"module A where\nimport B\ntype Answer = B.Answer\n";
        let b_bytes = b"module B where\ntype Answer = Int\n";
        std::fs::write(&a, a_bytes).unwrap();
        std::fs::write(&b, b_bytes).unwrap();
        let b_interface = crate::certified_products::fixture_source_module_interface(
            [2; 32],
            "main",
            "B",
            Sha256::digest(b_bytes).into(),
            BTreeMap::new(),
            None,
        );
        let a_interface = crate::certified_products::fixture_source_module_interface(
            [2; 32],
            "main",
            "A",
            Sha256::digest(a_bytes).into(),
            BTreeMap::from([(("main".into(), "B".into()), b_interface.interface_sha256())]),
            None,
        );
        let a_interface = crate::certified_products::fixture_module_source_imports(
            a_interface,
            vec![crate::certified_products::CanonicalSourceImport {
                qualifier: ImportQualifier::Unqualified,
                module: "B".into(),
                boot: false,
                home_unit: Some("main".into()),
            }],
        );
        let interfaces = [a_interface.clone(), b_interface.clone()];
        let exact = interfaces
            .iter()
            .map(|interface| (key(interface.module()), interface.interface_sha256()))
            .collect::<BTreeMap<_, _>>();
        let evidence = DependencyEvidence {
            version: 4,
            cache_safe: true,
            selection_complete: true,
            sources: vec![
                SourceEvidence {
                    path: a.clone(),
                    sha256: hex(&a_interface.source_sha256()),
                },
                SourceEvidence {
                    path: b.clone(),
                    sha256: hex(&b_interface.source_sha256()),
                },
            ],
            modules: vec![
                ModuleEvidence {
                    unit: "main".into(),
                    module: "A".into(),
                    boot: false,
                    source: a.clone(),
                    product: ProductAvailability::InterfaceOnly,
                    imports: vec![ModuleImportEvidence {
                        qualifier: ImportQualifier::Unqualified,
                        module: "B".into(),
                        boot: false,
                        selected: Some(b.clone()),
                    }],
                },
                ModuleEvidence {
                    unit: "main".into(),
                    module: "B".into(),
                    boot: false,
                    source: b.clone(),
                    product: ProductAvailability::InterfaceOnly,
                    imports: vec![],
                },
            ],
            packages: vec![],
            resolutions: vec![
                ResolutionEvidence {
                    qualifier: ImportQualifier::Unqualified,
                    module: "A".into(),
                    boot: false,
                    selected: Some(a.clone()),
                    candidates: vec![a.clone()],
                },
                ResolutionEvidence {
                    qualifier: ImportQualifier::Unqualified,
                    module: "B".into(),
                    boot: false,
                    selected: Some(b.clone()),
                    candidates: vec![b.clone()],
                },
            ],
        };
        let fresh = DependencyEvidence {
            modules: vec![ModuleEvidence {
                unit: "main".into(),
                module: "Consumer".into(),
                boot: false,
                source: root.path().join("Consumer.hs"),
                product: ProductAvailability::Ready,
                imports: vec![],
            }],
            ..evidence.clone()
        };
        let roots = [(key("Consumer"), ImportQualifier::Unqualified, key("A"))];
        let includes = [root.path().to_path_buf()];
        let independent = BTreeSet::new();
        let context = || SourceSelectionContext {
            producer: [2; 32],
            interfaces: &interfaces,
            exact_interfaces: &exact,
            include: &includes,
            fresh: &fresh,
            roots: &roots,
            independent: &independent,
        };
        let claims = || {
            interfaces
                .iter()
                .map(|interface| SourceSelectedOriginalClaim {
                    owner: key(interface.module()),
                    certificate_sha256: Sha256::digest(interface.certificate_bytes()).into(),
                    interface_sha256: interface.interface_sha256(),
                    source_sha256: interface.source_sha256(),
                })
                .collect::<Vec<_>>()
        };
        assert!(validate_source_selected_originals(claims(), &evidence, context()).is_ok());
        let missing = BTreeMap::from([(key("A"), a_interface.interface_sha256())]);
        assert!(validate_source_selected_originals(
            claims(),
            &evidence,
            SourceSelectionContext {
                exact_interfaces: &missing,
                ..context()
            }
        )
        .is_err());
        let changed = BTreeMap::from([
            (key("A"), a_interface.interface_sha256()),
            (key("B"), [9; 32]),
        ]);
        assert!(validate_source_selected_originals(
            claims(),
            &evidence,
            SourceSelectionContext {
                exact_interfaces: &changed,
                ..context()
            }
        )
        .is_err());
        let mut inventory_only = evidence.clone();
        inventory_only.modules[0].imports.clear();
        inventory_only.resolutions.pop();
        assert!(validate_source_selected_originals(claims(), &inventory_only, context()).is_err());
        let mut missing_child = evidence.clone();
        missing_child.modules.pop();
        missing_child.sources.pop();
        let mut only_a = claims();
        only_a.pop();
        assert!(validate_source_selected_originals(only_a, &missing_child, context()).is_err());
        // Drop both the reported edge and its claimed child. Canonical type
        // requirements still have B available, but cannot authenticate this lie.
        let mut omitted = evidence.clone();
        omitted.modules[0].imports.clear();
        omitted.modules.pop();
        omitted.sources.pop();
        omitted.resolutions.pop();
        let mut omitted_claims = claims();
        omitted_claims.pop();
        assert!(validate_source_selected_originals(omitted_claims, &omitted, context()).is_err());
        // A type requirement alone cannot manufacture an authored import.
        let type_only_a =
            crate::certified_products::fixture_module_source_imports(a_interface.clone(), vec![]);
        let type_only_interfaces = [type_only_a.clone(), b_interface.clone()];
        let mut promoted_claims = claims();
        promoted_claims[0].certificate_sha256 =
            Sha256::digest(type_only_a.certificate_bytes()).into();
        assert!(validate_source_selected_originals(
            promoted_claims,
            &evidence,
            SourceSelectionContext {
                interfaces: &type_only_interfaces,
                ..context()
            }
        )
        .is_err());
        let mut packages = evidence.clone();
        packages.modules[1].imports.push(ModuleImportEvidence {
            qualifier: ImportQualifier::Unqualified,
            module: "Data.List".into(),
            boot: false,
            selected: None,
        });
        assert!(validate_source_selected_originals(claims(), &packages, context()).is_err());
        assert!(validate_source_selected_originals(claims(), &evidence, context()).is_ok());
    }

    #[test]
    fn canonical_source_selection_allows_authenticated_edges_to_fresh_or_independent_owners() {
        let root = tempfile::tempdir().unwrap();
        let key = |name: &str| ExactModuleIdentity {
            unit: "main".into(),
            module: name.into(),
        };
        let a = root.path().join("A.hs");
        let b = root.path().join("B.hs");
        let bytes = b"module A where\nimport B ()\ntype Answer = Int\n";
        std::fs::write(&a, bytes).unwrap();
        std::fs::write(&b, b"module B where\nvalue = 43\n").unwrap();
        // This is a Rust boundary fixture, not a claim that GHC omits usage
        // for this syntax. The real compiler fixture inspects its own usages.
        let interface = crate::certified_products::fixture_module_source_imports(
            crate::certified_products::fixture_source_module_interface(
                [2; 32],
                "main",
                "A",
                Sha256::digest(bytes).into(),
                BTreeMap::new(),
                None,
            ),
            vec![crate::certified_products::CanonicalSourceImport {
                qualifier: ImportQualifier::Unqualified,
                module: "B".into(),
                boot: false,
                home_unit: Some("main".into()),
            }],
        );
        let interfaces = [interface.clone()];
        let exact = BTreeMap::from([(key("A"), interface.interface_sha256())]);
        let node = |name: &str, source: PathBuf| ModuleEvidence {
            unit: "main".into(),
            module: name.into(),
            boot: false,
            source,
            product: ProductAvailability::Ready,
            imports: vec![],
        };
        let evidence = DependencyEvidence {
            version: 4,
            cache_safe: true,
            selection_complete: true,
            sources: vec![SourceEvidence {
                path: a.clone(),
                sha256: hex(&interface.source_sha256()),
            }],
            modules: vec![ModuleEvidence {
                product: ProductAvailability::InterfaceOnly,
                imports: vec![ModuleImportEvidence {
                    qualifier: ImportQualifier::Unqualified,
                    module: "B".into(),
                    boot: false,
                    selected: Some(b.clone()),
                }],
                ..node("A", a.clone())
            }],
            packages: vec![],
            resolutions: [(&a, "A"), (&b, "B")]
                .into_iter()
                .map(|(path, name)| ResolutionEvidence {
                    qualifier: ImportQualifier::Unqualified,
                    module: name.into(),
                    boot: false,
                    selected: Some(path.clone()),
                    candidates: vec![path.clone()],
                })
                .collect(),
        };
        let fresh = DependencyEvidence {
            modules: vec![
                node("Consumer", root.path().join("Consumer.hs")),
                node("B", b),
            ],
            ..evidence.clone()
        };
        let roots = [(key("Consumer"), ImportQualifier::Unqualified, key("A"))];
        let includes = [root.path().to_path_buf()];
        let independent = BTreeSet::new();
        let context = || SourceSelectionContext {
            producer: [2; 32],
            interfaces: &interfaces,
            exact_interfaces: &exact,
            include: &includes,
            fresh: &fresh,
            roots: &roots,
            independent: &independent,
        };
        let claims = || {
            vec![SourceSelectedOriginalClaim {
                owner: key("A"),
                certificate_sha256: Sha256::digest(interface.certificate_bytes()).into(),
                interface_sha256: interface.interface_sha256(),
                source_sha256: interface.source_sha256(),
            }]
        };
        assert!(validate_source_selected_originals(claims(), &evidence, context()).is_ok());
        let mut without_b = fresh.clone();
        without_b.modules.pop();
        assert!(validate_source_selected_originals(
            claims(),
            &evidence,
            SourceSelectionContext {
                fresh: &without_b,
                ..context()
            }
        )
        .is_err());
        let admitted = BTreeSet::from([key("B")]);
        assert!(validate_source_selected_originals(
            claims(),
            &evidence,
            SourceSelectionContext {
                fresh: &without_b,
                independent: &admitted,
                ..context()
            }
        )
        .is_ok());
        let mut omitted = evidence.clone();
        omitted.modules[0].imports.clear();
        omitted.resolutions.pop();
        assert!(validate_source_selected_originals(claims(), &omitted, context()).is_err());
    }

    fn valid_wire() -> GraphWire {
        let root = tempfile::tempdir().unwrap();
        let (graph, _) = test_graph(root.path());
        let wire = GraphWire::decode(graph.bytes()).unwrap();
        wire.validate().unwrap();
        wire
    }

    fn assert_diagnostic(wire: &GraphWire, expected: &str) {
        let CompileError::ExtractFailed(message) = wire.validate().unwrap_err() else {
            panic!("unexpected refusal boundary");
        };
        assert_eq!(
            message,
            format!("original execution source proof: {expected}")
        );
    }

    #[test]
    fn fresh_owner_with_retained_graph_has_a_provenance_refusal() {
        let mut wire = valid_wire();
        wire.owners[0].fresh = true;
        wire.owners[0].original_graph_sha256 = Some([7; 32]);
        assert!(matches!(
            wire.validate_original_owners(),
            Err(OriginalOwnerRefusal::FreshWithRetainedGraph(_))
        ));
        assert_diagnostic(
            &wire,
            "original owner main:A: fresh source also names a retained original graph",
        );
    }

    #[test]
    fn duplicate_owner_has_a_key_refusal() {
        let mut wire = valid_wire();
        wire.owners
            .insert(1, OwnerWire::from(&wire.owners[0].owner()));
        assert!(matches!(
            wire.validate_original_owners(),
            Err(OriginalOwnerRefusal::Duplicate(_))
        ));
        assert_diagnostic(&wire, "original owner main:A: duplicate unit/module key");
    }

    #[test]
    fn unordered_owner_has_an_order_refusal() {
        let mut wire = valid_wire();
        wire.owners.swap(0, 1);
        assert!(matches!(
            wire.validate_original_owners(),
            Err(OriginalOwnerRefusal::Unordered(_))
        ));
        assert_diagnostic(
            &wire,
            "original owner main:A: unit/module keys are not strictly ordered",
        );
    }

    #[test]
    fn empty_owner_identity_has_an_identity_refusal() {
        let mut wire = valid_wire();
        wire.owners[0].unit.clear();
        assert!(matches!(
            wire.validate_original_owners(),
            Err(OriginalOwnerRefusal::InvalidIdentity(_))
        ));
        assert_diagnostic(&wire, "original owner :A: empty unit or module identity");
    }

    #[test]
    fn zero_retained_graph_digest_has_a_retained_source_refusal() {
        let mut wire = valid_wire();
        wire.owners[0].fresh = false;
        wire.owners[0].original_graph_sha256 = Some([0; 32]);
        assert!(matches!(
            wire.validate_original_owners(),
            Err(OriginalOwnerRefusal::InvalidRetainedGraphDigest(_))
        ));
        assert_diagnostic(
            &wire,
            "original owner main:A: retained original graph has a zero digest",
        );
    }

    #[test]
    fn session_native_owners_remain_in_graph_without_source_replay_capability() {
        let root = tempfile::tempdir().unwrap();
        let (graph, owners) = test_graph_with_local_source_dependency(root.path());
        for name in [
            "Tidepool.Session.Lib.G1",
            "Tidepool.Session.Val.G1",
            "Tidepool.Session.Lib.G18446744073709551616",
            "Tidepool.Session.Lib.Gbad",
        ] {
            let mut wire = GraphWire::decode(graph.bytes()).unwrap();
            let mut native_owner = owners[1].clone();
            native_owner.module = name.into();
            wire.owners
                .iter_mut()
                .find(|owner| owner.module == "B")
                .unwrap()
                .module = name.into();
            wire.owners.sort_by(|left, right| {
                (&left.unit, &left.module).cmp(&(&right.unit, &right.module))
            });
            for module in &mut wire.evidence.modules {
                if module.module == "B" {
                    module.module = name.into();
                }
                for imported in &mut module.imports {
                    if imported.module == "B" {
                        imported.module = name.into();
                    }
                }
            }
            for resolution in &mut wire.evidence.resolutions {
                if resolution.module == "B" {
                    resolution.module = name.into();
                }
            }
            let bytes = wire.encode().unwrap();
            let retained = CertifiedExecutionSourceGraph::recover(bytes.clone()).unwrap();
            assert_eq!(retained.bytes(), bytes);
            assert!(retained.matches_owner(&native_owner));
            assert_eq!(
                retained.direct_source_owners(&owners[0]).unwrap(),
                vec![&native_owner]
            );
            assert!(!retained.eligible_source_replay_root(&native_owner));
            assert!(
                !retained.eligible_source_replay_root(&owners[0]),
                "replaying a support module must not reconstruct a session dependency"
            );
        }
        assert!(graph.eligible_source_replay_root(&owners[0]));
        assert!(graph.eligible_source_replay_root(&owners[1]));
    }

    #[test]
    fn source_graph_roundtrip_scopes_eligibility_to_original_root() {
        let root = tempfile::tempdir().unwrap();
        let (graph, owners) = test_graph(root.path());
        assert!(graph.eligible_source_replay_root(&owners[0]));
        assert!(graph.eligible_source_replay_root(&owners[1]));
        assert!(
            !graph.eligible_source_replay_root(&owners[2]),
            "unrelated generated/Val target is ineligible"
        );
        let recovered = CertifiedExecutionSourceGraph::recover(graph.bytes().to_vec()).unwrap();
        assert_eq!(recovered, graph);
        std::fs::remove_file(root.path().join("A.hs")).unwrap();
        assert_eq!(
            CertifiedExecutionSourceGraph::recover(graph.bytes().to_vec()).unwrap(),
            graph,
            "authored source absence cannot discard native/interface evidence"
        );
        let mut other = owners[0].clone();
        other.module_version = ModuleVersion([99; 32]);
        assert!(!graph.eligible_source_replay_root(&other));
    }

    #[test]
    fn source_graph_refuses_noncanonical_incomplete_and_malformed_proofs() {
        let root = tempfile::tempdir().unwrap();
        let (graph, _) = test_graph(root.path());
        let mut trailing = graph.bytes().to_vec();
        trailing.push(0);
        assert!(CertifiedExecutionSourceGraph::recover(trailing).is_err());
        let mut wire = GraphWire::decode(graph.bytes()).unwrap();
        wire.evidence.cache_safe = false;
        assert!(CertifiedExecutionSourceGraph::recover(wire.encode().unwrap()).is_err());
        wire.evidence.cache_safe = true;
        wire.version = 2;
        assert!(CertifiedExecutionSourceGraph::recover(wire.encode().unwrap()).is_err());
        assert!(CertifiedExecutionSourceGraph::recover(b"malformed".to_vec()).is_err());
    }

    #[test]
    fn source_graph_refuses_transitive_mutable_exact_import_and_cached_backfill() {
        let root = tempfile::tempdir().unwrap();
        let (graph, owners) = test_graph(root.path());
        let mut wire = GraphWire::decode(graph.bytes()).unwrap();
        wire.exact_imports.push(ExactImportsWire {
            owner: ExactModuleIdentity {
                unit: "main".into(),
                module: "A".into(),
            },
            imports: vec![ExactModuleIdentity {
                unit: "main".into(),
                module: "B".into(),
            }],
        });
        wire.exact_imports.push(ExactImportsWire {
            owner: ExactModuleIdentity {
                unit: "main".into(),
                module: "B".into(),
            },
            imports: vec![ExactModuleIdentity {
                unit: "main".into(),
                module: "Tidepool.Session.Val.G1".into(),
            }],
        });
        let recovered = CertifiedExecutionSourceGraph::recover(wire.encode().unwrap()).unwrap();
        assert!(
            !recovered.eligible_source_replay_root(&owners[0]),
            "fresh exact dependencies require their full closure"
        );
        wire.exact_imports.pop();
        wire.owners
            .iter_mut()
            .find(|owner| owner.module == "B")
            .unwrap()
            .fresh = false;
        let recovered = CertifiedExecutionSourceGraph::recover(wire.encode().unwrap()).unwrap();
        assert!(
            !recovered.eligible_source_replay_root(&owners[0]),
            "cached owners cannot receive a consumer source recipe"
        );
        wire.owners
            .iter_mut()
            .find(|owner| owner.module == "B")
            .unwrap()
            .original_graph_sha256 = Some([91; 32]);
        let recovered = CertifiedExecutionSourceGraph::recover(wire.encode().unwrap()).unwrap();
        assert!(
            recovered.eligible_source_replay_root(&owners[0]),
            "retained graph demand is resolved through selected original artifacts"
        );
        assert!(
            !recovered.eligible_source_replay_root(&owners[1]),
            "the inherited owner uses its own graph"
        );
    }

    #[test]
    fn source_graph_optional_unavailability_preserves_primary_proof_errors() {
        let root = tempfile::tempdir().unwrap();
        let (graph, owners) = test_graph(root.path());
        let wire = GraphWire::decode(graph.bytes()).unwrap();
        let fresh = owners
            .iter()
            .map(|owner| ExactModuleIdentity {
                unit: owner.unit.clone(),
                module: owner.module.clone(),
            })
            .collect();
        let exact = BTreeMap::new();
        let retained = BTreeMap::new();
        let packages = BTreeMap::new();
        let invoke = |include: &[PathBuf], owners: &[CachedHomeOwner]| {
            CertifiedExecutionSourceGraph::admit(ExecutionSourceGraphInput {
                producer: crate::artifact_inventory::CanonicalProducerIdentity::from_test_sha256(
                    wire.producer_sha256,
                ),
                semantic_sha256: wire.semantic_sha256,
                include,
                source_path: &wire.source_path,
                source: &wire.source,
                evidence: &wire.evidence,
                exact_imports: &exact,
                owners,
                fresh_owners: &fresh,
                retained_sources: &retained,
                packages: &packages,
            })
        };
        assert!(matches!(
            invoke(&[root.path().join("absent-unused-root")], &owners).unwrap(),
            ExecutionSourceAdmission::Unavailable(
                ExecutionSourceUnavailable::UnavailableIncludeRoot
            )
        ));
        assert!(matches!(
            invoke(&vec![root.path().to_path_buf(); OWNER_LIMIT + 1], &owners).unwrap(),
            ExecutionSourceAdmission::Unavailable(ExecutionSourceUnavailable::LimitExceeded)
        ));
        assert!(matches!(
            invoke(&[], &[]).unwrap(),
            ExecutionSourceAdmission::Unavailable(ExecutionSourceUnavailable::UnsupportedShape)
        ));
        std::fs::write(root.path().join("A.hs"), "changed consumed source").unwrap();
        assert!(
            invoke(&[], &owners).is_err(),
            "stale primary proof cannot be optional unavailability"
        );
    }

    #[test]
    fn resolution_candidate_paths_share_an_aggregate_admission_budget() {
        let root = tempfile::tempdir().unwrap();
        let (graph, owners) = test_graph(root.path());
        let mut wire = GraphWire::decode(graph.bytes()).unwrap();
        let missing = root.path().join("missing-candidate.hs");
        wire.evidence.resolutions = (0..16)
            .map(|index| ResolutionEvidence {
                qualifier: ImportQualifier::Unqualified,
                module: format!("Missing{index}"),
                boot: false,
                selected: None,
                candidates: vec![missing.clone(); OWNER_LIMIT],
            })
            .collect();
        assert!(resolution_candidates_fit(&wire.evidence.resolutions));
        wire.validate().unwrap();
        let boundary_bytes = wire.encode().unwrap();
        CertifiedExecutionSourceGraph::recover(boundary_bytes.clone()).unwrap();

        wire.evidence.resolutions.push(ResolutionEvidence {
            qualifier: ImportQualifier::Unqualified,
            module: "Missing16".into(),
            boot: false,
            selected: None,
            candidates: vec![missing.clone()],
        });
        assert!(wire.evidence.resolutions.len() < EDGE_LIMIT);
        assert!(wire
            .evidence
            .resolutions
            .iter()
            .all(|row| row.candidates.len() <= OWNER_LIMIT));
        assert!(!resolution_candidates_fit(&wire.evidence.resolutions));
        assert!(wire.validate().is_err());
        assert!(
            wire.encode().is_err(),
            "bound before serialization allocation"
        );
        let fresh = owners
            .iter()
            .map(|owner| ExactModuleIdentity {
                unit: owner.unit.clone(),
                module: owner.module.clone(),
            })
            .collect();
        assert!(matches!(
            CertifiedExecutionSourceGraph::admit(ExecutionSourceGraphInput {
                producer: crate::artifact_inventory::CanonicalProducerIdentity::from_test_sha256(
                    wire.producer_sha256,
                ),
                semantic_sha256: wire.semantic_sha256,
                include: &wire.include,
                source_path: &wire.source_path,
                source: &wire.source,
                evidence: &wire.evidence,
                exact_imports: &BTreeMap::new(),
                owners: &owners,
                fresh_owners: &fresh,
                retained_sources: &BTreeMap::new(),
                packages: &BTreeMap::new(),
            })
            .unwrap(),
            ExecutionSourceAdmission::Unavailable(ExecutionSourceUnavailable::LimitExceeded)
        ));

        // Retained bytes still need independent recovery admission. Mutate only
        // the candidate inventory using the existing generic CBOR encoder.
        use ciborium::value::Value;
        let mut packet: Value = ciborium::de::from_reader(boundary_bytes.as_slice()).unwrap();
        let fields = packet.as_array_mut().unwrap();
        let evidence = fields[7].as_array_mut().unwrap();
        evidence[3].as_array_mut().unwrap().push(Value::Array(vec![
            Value::Text(String::from(ImportQualifier::Unqualified)),
            Value::Text("Missing16".into()),
            Value::Bool(false),
            Value::Null,
            Value::Array(vec![Value::Text(missing.to_str().unwrap().into())]),
        ]));
        let mut excessive_bytes = Vec::new();
        ciborium::ser::into_writer(&packet, &mut excessive_bytes).unwrap();
        assert!(excessive_bytes.len() < GRAPH_BYTES_LIMIT);
        let decoded = GraphWire::decode(&excessive_bytes).unwrap();
        assert!(!resolution_candidates_fit(&decoded.evidence.resolutions));
        assert!(decoded.validate().is_err());
        assert!(CertifiedExecutionSourceGraph::recover(excessive_bytes).is_err());
    }

    #[test]
    fn graph_descriptor_capture_preserves_immutable_bytes_and_refuses_substitution() {
        let root = tempfile::tempdir().unwrap();
        let capture = tempfile::tempdir().unwrap();
        let (graph, _) = test_graph(root.path());
        let path = graph.capture_descriptor(capture.path()).unwrap();
        assert_eq!(path.parent(), Some(capture.path()));
        assert_eq!(std::fs::read(&path).unwrap(), graph.bytes());
        assert_eq!(graph.capture_descriptor(capture.path()).unwrap(), path);

        std::fs::write(&path, b"substituted graph").unwrap();
        assert!(graph.capture_descriptor(capture.path()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"substituted graph");
        std::fs::write(&path, graph.bytes()).unwrap();
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(b"x").unwrap();
        drop(file);
        assert!(graph.capture_descriptor(capture.path()).is_err());
        std::fs::write(&path, graph.bytes()).unwrap();
        assert_eq!(graph.capture_descriptor(capture.path()).unwrap(), path);
    }

    #[test]
    fn source_graph_bounds_decoder_allocation_and_refuses_required_source_edges() {
        let root = tempfile::tempdir().unwrap();
        let (graph, owners) = test_graph(root.path());
        let mut wire = GraphWire::decode(graph.bytes()).unwrap();
        wire.include = vec![root.path().to_path_buf(); OWNER_LIMIT + 1];
        assert!(CertifiedExecutionSourceGraph::recover(wire.encode().unwrap()).is_err());
        assert!(matches!(
            validate_cbor_budget(&[0x9a, 0x00, 0x40, 0x00, 0x00]),
            Err(CborBudgetError::LimitExceeded)
        ));
        wire.include.clear();
        let mut boot = wire
            .evidence
            .modules
            .iter()
            .find(|module| module.module == "B")
            .unwrap()
            .clone();
        boot.boot = true;
        boot.product = ProductAvailability::Boot;
        boot.imports.push(ModuleImportEvidence {
            qualifier: ImportQualifier::Unqualified,
            module: "A".into(),
            boot: false,
            selected: Some(root.path().join("A.hs")),
        });
        let selected = boot.source.clone();
        wire.evidence.modules.push(boot);
        wire.evidence
            .modules
            .iter_mut()
            .find(|module| module.module == "A")
            .unwrap()
            .imports
            .push(ModuleImportEvidence {
                qualifier: ImportQualifier::Unqualified,
                module: "B".into(),
                boot: true,
                selected: Some(selected),
            });
        let recovered = CertifiedExecutionSourceGraph::recover(wire.encode().unwrap()).unwrap();
        assert!(
            !recovered.eligible_source_replay_root(&owners[0]),
            "SOURCE imports have no first-slice execution recipe"
        );
        assert!(
            recovered.eligible_source_replay_root(&owners[1]),
            "unrelated boot evidence preserves regular originals"
        );
        assert!(
            wire.source_imports()[&("main".into(), "B".into())].is_empty(),
            "unrelated boot imports cannot add graph requirements to a regular original"
        );
    }

    #[test]
    fn source_graph_refuses_transitive_interface_only_or_missing_original() {
        let root = tempfile::tempdir().unwrap();
        let (graph, owners) = test_graph(root.path());
        let mut wire = GraphWire::decode(graph.bytes()).unwrap();
        let selected = wire
            .evidence
            .modules
            .iter()
            .find(|node| node.module == "B")
            .unwrap()
            .source
            .clone();
        wire.evidence
            .modules
            .iter_mut()
            .find(|node| node.module == "A")
            .unwrap()
            .imports
            .push(ModuleImportEvidence {
                qualifier: ImportQualifier::Unqualified,
                module: "B".into(),
                boot: false,
                selected: Some(selected),
            });
        wire.evidence
            .modules
            .iter_mut()
            .find(|node| node.module == "B")
            .unwrap()
            .product = ProductAvailability::InterfaceOnly;
        let recovered = CertifiedExecutionSourceGraph::recover(wire.encode().unwrap()).unwrap();
        assert!(
            !recovered.eligible_source_replay_root(&owners[0]),
            "a selected interface-only dependency cannot grant execution"
        );
        wire.owners.retain(|owner| owner.module != "B");
        let recovered = CertifiedExecutionSourceGraph::recover(wire.encode().unwrap()).unwrap();
        assert!(
            !recovered.eligible_source_replay_root(&owners[0]),
            "selected source without a retained native original cannot grant execution"
        );
    }
}

#[cfg(test)]
pub(crate) fn test_graph_with_large_origin(
    graph: &Arc<CertifiedExecutionSourceGraph>,
    length: usize,
) -> Arc<CertifiedExecutionSourceGraph> {
    let mut wire = GraphWire::decode(graph.bytes()).unwrap();
    wire.source = "x".repeat(length);
    wire.evidence
        .sources
        .iter_mut()
        .find(|source| source.path == Path::new("@generated-source"))
        .unwrap()
        .sha256 = hex(&Sha256::digest(wire.source.as_bytes()).into());
    CertifiedExecutionSourceGraph::recover(wire.encode().unwrap()).unwrap()
}

#[cfg(test)]
pub(crate) fn test_graph_with_package_witness(
    graph: &Arc<CertifiedExecutionSourceGraph>,
    path: &Path,
) -> Arc<CertifiedExecutionSourceGraph> {
    let mut wire = GraphWire::decode(graph.bytes()).unwrap();
    wire.packages.push(PackageWire {
        unit: "package-unit".into(),
        module: "Package.Module".into(),
        selected_path: path.to_path_buf(),
        sha256: Sha256::digest(std::fs::read(path).unwrap()).into(),
    });
    CertifiedExecutionSourceGraph::recover(wire.encode().unwrap()).unwrap()
}

#[cfg(test)]
pub(crate) fn test_graph_requiring_original(
    graph: &Arc<CertifiedExecutionSourceGraph>,
    required: &CachedHomeOwner,
    digest: [u8; 32],
) -> Arc<CertifiedExecutionSourceGraph> {
    let mut wire = GraphWire::decode(graph.bytes()).unwrap();
    let original = wire
        .owners
        .iter_mut()
        .find(|owner| owner.module == required.module)
        .unwrap();
    *original = OwnerWire::from(required);
    original.original_graph_sha256 = Some(digest);
    wire.exact_imports.push(ExactImportsWire {
        owner: ExactModuleIdentity {
            unit: "main".into(),
            module: "A".into(),
        },
        imports: vec![ExactModuleIdentity {
            unit: required.unit.clone(),
            module: required.module.clone(),
        }],
    });
    CertifiedExecutionSourceGraph::recover(wire.encode().unwrap()).unwrap()
}

#[cfg(test)]
pub(crate) fn test_graph_with_local_source_dependency(
    root: &Path,
) -> (Arc<CertifiedExecutionSourceGraph>, Vec<CachedHomeOwner>) {
    let (graph, owners) = test_graph(root);
    let mut wire = GraphWire::decode(graph.bytes()).unwrap();
    let selected = root.join("B.hs");
    wire.evidence
        .modules
        .iter_mut()
        .find(|module| module.module == "A")
        .unwrap()
        .imports
        .push(ModuleImportEvidence {
            qualifier: ImportQualifier::Unqualified,
            module: "B".into(),
            boot: false,
            selected: Some(selected.clone()),
        });
    wire.evidence.resolutions.push(ResolutionEvidence {
        qualifier: ImportQualifier::Unqualified,
        module: "B".into(),
        boot: false,
        selected: Some(selected.clone()),
        candidates: vec![selected],
    });
    let graph = CertifiedExecutionSourceGraph::recover(wire.encode().unwrap()).unwrap();
    (graph, owners)
}

/// One shared proof for every original issued by the same source transaction.
/// Its constructor and recovery decoder remain inside the toolchain owner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CertifiedExecutionSourceGraph {
    bytes: Arc<[u8]>,
    digest: [u8; 32],
    producer_sha256: [u8; 32],
    semantic_sha256: Option<[u8; 32]>,
    owners: BTreeMap<(String, String), CachedHomeOwner>,
    source_replay_roots: BTreeSet<(String, String)>,
    source_imports: BTreeMap<(String, String), BTreeSet<(String, String)>>,
    retained_graphs: BTreeMap<(String, String), (CachedHomeOwner, [u8; 32])>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GraphWire {
    version: u32,
    profile: String,
    producer_sha256: [u8; 32],
    semantic_sha256: Option<[u8; 32]>,
    include: Vec<PathBuf>,
    source_path: PathBuf,
    source: String,
    evidence: DependencyEvidence,
    exact_imports: Vec<ExactImportsWire>,
    owners: Vec<OwnerWire>,
    packages: Vec<PackageWire>,
}

/// Read source declarations for failure diagnostics without issuing a graph
/// capability. Physical bytes must still match each declaration before capture.
pub(crate) fn diagnostic_source_inventory(
    bytes: &[u8],
) -> Result<Vec<SourceEvidence>, CompileError> {
    if bytes.len() > GRAPH_BYTES_LIMIT {
        return Err(failure("diagnostic graph exceeds its byte bound"));
    }
    let wire = GraphWire::decode(bytes)?;
    wire.validate()?;
    if wire.encode()? != bytes {
        return Err(failure("noncanonical diagnostic graph encoding"));
    }
    Ok(wire.evidence.sources)
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnerWire {
    unit: String,
    module: String,
    module_version: [u8; 32],
    skinny_iface_sha256: [u8; 32],
    product_sha256: [u8; 32],
    fresh: bool,
    original_graph_sha256: Option<[u8; 32]>,
}

impl From<&CachedHomeOwner> for OwnerWire {
    fn from(owner: &CachedHomeOwner) -> Self {
        Self {
            unit: owner.unit.clone(),
            module: owner.module.clone(),
            module_version: owner.module_version.0,
            skinny_iface_sha256: owner.skinny_iface_sha256,
            product_sha256: owner.product_sha256,
            fresh: false,
            original_graph_sha256: None,
        }
    }
}

impl OwnerWire {
    fn owner(&self) -> CachedHomeOwner {
        CachedHomeOwner {
            unit: self.unit.clone(),
            module: self.module.clone(),
            module_version: ModuleVersion(self.module_version),
            skinny_iface_sha256: self.skinny_iface_sha256,
            product_sha256: self.product_sha256,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExactImportsWire {
    owner: ExactModuleIdentity,
    imports: Vec<ExactModuleIdentity>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageWire {
    unit: String,
    module: String,
    selected_path: PathBuf,
    sha256: [u8; 32],
}

#[derive(Serialize)]
#[serde(transparent)]
struct BoundedVec<T, const LIMIT: usize>(Vec<T>);

impl<'de, T: Deserialize<'de>, const LIMIT: usize> Deserialize<'de> for BoundedVec<T, LIMIT> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct BoundedVisitor<T, const LIMIT: usize>(std::marker::PhantomData<T>);
        impl<'de, T: Deserialize<'de>, const LIMIT: usize> Visitor<'de> for BoundedVisitor<T, LIMIT> {
            type Value = BoundedVec<T, LIMIT>;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(formatter, "an array of at most {LIMIT} entries")
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<Self::Value, A::Error> {
                if sequence.size_hint().is_some_and(|length| length > LIMIT) {
                    return Err(serde::de::Error::custom(
                        "execution source array exceeds its bound",
                    ));
                }
                let mut values = Vec::new();
                while let Some(value) = sequence.next_element()? {
                    if values.len() == LIMIT {
                        return Err(serde::de::Error::custom(
                            "execution source array exceeds its bound",
                        ));
                    }
                    values.push(value);
                }
                Ok(BoundedVec(values))
            }
        }
        deserializer.deserialize_seq(BoundedVisitor::<T, LIMIT>(std::marker::PhantomData))
    }
}
impl<T, const LIMIT: usize> From<Vec<T>> for BoundedVec<T, LIMIT> {
    fn from(values: Vec<T>) -> Self {
        Self(values)
    }
}
impl<T, const LIMIT: usize> FromIterator<T> for BoundedVec<T, LIMIT> {
    fn from_iter<I: IntoIterator<Item = T>>(values: I) -> Self {
        Self(values.into_iter().collect())
    }
}
impl<T, const LIMIT: usize> IntoIterator for BoundedVec<T, LIMIT> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

type ImportEncoding = (String, String, bool, Option<String>);
type ModuleEncoding = (
    String,
    String,
    bool,
    String,
    BoundedVec<ImportEncoding, OWNER_LIMIT>,
    ProductAvailability,
);
type ResolutionEncoding = (
    String,
    String,
    bool,
    Option<String>,
    BoundedVec<String, OWNER_LIMIT>,
);
type EvidenceEncoding = (
    bool,
    bool,
    BoundedVec<(String, String), OWNER_LIMIT>,
    BoundedVec<ResolutionEncoding, EDGE_LIMIT>,
    BoundedVec<ModuleEncoding, OWNER_LIMIT>,
    BoundedVec<String, OWNER_LIMIT>,
);
type OwnerEncoding = (String, String, String, String, String, bool, Option<String>);
type ExactEncoding = (String, String, BoundedVec<(String, String), OWNER_LIMIT>);
type PackageEncoding = (String, String, String, String);
type GraphEncoding = (
    String,
    u32,
    String,
    String,
    Option<String>,
    BoundedVec<String, OWNER_LIMIT>,
    (String, String),
    EvidenceEncoding,
    BoundedVec<OwnerEncoding, OWNER_LIMIT>,
    BoundedVec<ExactEncoding, OWNER_LIMIT>,
    BoundedVec<PackageEncoding, OWNER_LIMIT>,
);

fn path_text(path: &Path) -> Result<String, CompileError> {
    path.to_str()
        .filter(|text| text.len() <= 65536)
        .map(str::to_owned)
        .ok_or_else(|| failure("path is not bounded UTF-8"))
}
fn hex(digest: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(64);
    for byte in digest {
        text.push(HEX[usize::from(byte >> 4)] as char);
        text.push(HEX[usize::from(byte & 15)] as char);
    }
    text
}
pub(crate) fn parse_digest(text: &str) -> Result<[u8; 32], CompileError> {
    if text.len() != 64
        || !text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(failure("invalid digest"));
    }
    let mut digest = [0; 32];
    for (index, byte) in digest.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16)
            .map_err(|_| failure("invalid digest"))?;
    }
    Ok(digest)
}

#[derive(Clone, Copy, Debug)]
enum CborBudgetError {
    LimitExceeded,
    InvalidEncoding,
}

/// Check the total allocation fanout before serde builds any nested vectors.
/// Array visitors enforce domain counts separately; this budget also bounds
/// combinations of individually valid arrays and small decoded strings.
fn validate_cbor_budget(bytes: &[u8]) -> Result<(), CborBudgetError> {
    fn item(
        bytes: &[u8],
        position: &mut usize,
        nodes: &mut usize,
        depth: usize,
    ) -> Result<(), CborBudgetError> {
        if depth > 32 || *nodes >= EDGE_LIMIT * 32 {
            return Err(CborBudgetError::LimitExceeded);
        }
        *nodes += 1;
        let byte = *bytes
            .get(*position)
            .ok_or(CborBudgetError::InvalidEncoding)?;
        *position += 1;
        let major = byte >> 5;
        let additional = byte & 31;
        let count = match additional {
            value @ 0..=23 => u64::from(value),
            value @ 24..=27 => {
                let width = 1usize << (value - 24);
                let end = position
                    .checked_add(width)
                    .ok_or(CborBudgetError::InvalidEncoding)?;
                let encoded = bytes
                    .get(*position..end)
                    .ok_or(CborBudgetError::InvalidEncoding)?;
                *position = end;
                encoded
                    .iter()
                    .fold(0u64, |value, byte| (value << 8) | u64::from(*byte))
            }
            _ => return Err(CborBudgetError::InvalidEncoding),
        };
        match major {
            0 | 1 => Ok(()),
            3 => {
                let count = usize::try_from(count).map_err(|_| CborBudgetError::InvalidEncoding)?;
                *position = position
                    .checked_add(count)
                    .filter(|end| *end <= bytes.len())
                    .ok_or(CborBudgetError::InvalidEncoding)?;
                Ok(())
            }
            4 => {
                if count > (EDGE_LIMIT * 32) as u64 {
                    return Err(CborBudgetError::LimitExceeded);
                }
                for _ in 0..count {
                    item(bytes, position, nodes, depth + 1)?;
                }
                Ok(())
            }
            7 if matches!(additional, 20..=22) => Ok(()),
            _ => Err(CborBudgetError::InvalidEncoding),
        }
    }
    let mut position = 0;
    item(bytes, &mut position, &mut 0, 0)?;
    if position != bytes.len() {
        return Err(CborBudgetError::InvalidEncoding);
    }
    Ok(())
}

impl GraphWire {
    fn encode(&self) -> Result<Vec<u8>, CompileError> {
        let evidence = &self.evidence;
        if !resolution_candidates_fit(&evidence.resolutions) {
            return Err(failure("resolution candidates exceed their edge bound"));
        }
        let encoding: GraphEncoding = (
            "TPEXECUTIONSOURCE".into(),
            self.version,
            self.profile.clone(),
            hex(&self.producer_sha256),
            self.semantic_sha256.as_ref().map(hex),
            self.include
                .iter()
                .map(|path| path_text(path))
                .collect::<Result<_, _>>()?,
            (path_text(&self.source_path)?, self.source.clone()),
            (
                evidence.cache_safe,
                evidence.selection_complete,
                evidence
                    .sources
                    .iter()
                    .map(|source| Ok((path_text(&source.path)?, source.sha256.clone())))
                    .collect::<Result<_, CompileError>>()?,
                evidence
                    .resolutions
                    .iter()
                    .map(|row| {
                        Ok((
                            String::from(row.qualifier.clone()),
                            row.module.clone(),
                            row.boot,
                            row.selected.as_deref().map(path_text).transpose()?,
                            row.candidates
                                .iter()
                                .map(|path| path_text(path))
                                .collect::<Result<_, _>>()?,
                        ))
                    })
                    .collect::<Result<_, CompileError>>()?,
                evidence
                    .modules
                    .iter()
                    .map(|row| {
                        Ok((
                            row.unit.clone(),
                            row.module.clone(),
                            row.boot,
                            path_text(&row.source)?,
                            row.imports
                                .iter()
                                .map(|edge| {
                                    Ok((
                                        String::from(edge.qualifier.clone()),
                                        edge.module.clone(),
                                        edge.boot,
                                        edge.selected.as_deref().map(path_text).transpose()?,
                                    ))
                                })
                                .collect::<Result<_, CompileError>>()?,
                            row.product,
                        ))
                    })
                    .collect::<Result<_, CompileError>>()?,
                evidence.packages.clone().into(),
            ),
            self.owners
                .iter()
                .map(|owner| {
                    (
                        owner.unit.clone(),
                        owner.module.clone(),
                        hex(&owner.module_version),
                        hex(&owner.skinny_iface_sha256),
                        hex(&owner.product_sha256),
                        owner.fresh,
                        owner.original_graph_sha256.as_ref().map(hex),
                    )
                })
                .collect(),
            self.exact_imports
                .iter()
                .map(|row| {
                    (
                        row.owner.unit.clone(),
                        row.owner.module.clone(),
                        row.imports
                            .iter()
                            .map(|owner| (owner.unit.clone(), owner.module.clone()))
                            .collect(),
                    )
                })
                .collect(),
            self.packages
                .iter()
                .map(|row| {
                    Ok((
                        row.unit.clone(),
                        row.module.clone(),
                        path_text(&row.selected_path)?,
                        hex(&row.sha256),
                    ))
                })
                .collect::<Result<_, CompileError>>()?,
        );
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&encoding, &mut bytes).map_err(|_| failure("encoding"))?;
        Ok(bytes)
    }
    fn decode(bytes: &[u8]) -> Result<Self, CompileError> {
        validate_cbor_budget(bytes).map_err(|_| failure("CBOR allocation budget or encoding"))?;
        let mut cursor = std::io::Cursor::new(bytes);
        let (
            tag,
            version,
            profile,
            producer,
            semantic,
            include,
            (source_path, source),
            evidence,
            owners,
            exact,
            packages,
        ): GraphEncoding = ciborium::de::from_reader_with_recursion_limit(&mut cursor, 32)
            .map_err(|_| failure("encoding"))?;
        if cursor.position() != bytes.len() as u64 || tag != "TPEXECUTIONSOURCE" {
            return Err(failure("encoding or tag"));
        }
        let (cache_safe, selection_complete, sources, resolutions, modules, package_names) =
            evidence;
        let qualifier =
            |text: String| ImportQualifier::try_from(text).map_err(|_| failure("import qualifier"));
        Ok(Self {
            version,
            profile,
            producer_sha256: parse_digest(&producer)?,
            semantic_sha256: semantic.as_deref().map(parse_digest).transpose()?,
            include: include.into_iter().map(PathBuf::from).collect(),
            source_path: source_path.into(),
            source,
            evidence: DependencyEvidence {
                version: 4,
                cache_safe,
                selection_complete,
                sources: sources
                    .into_iter()
                    .map(|(path, sha256)| SourceEvidence {
                        path: path.into(),
                        sha256,
                    })
                    .collect(),
                resolutions: resolutions
                    .into_iter()
                    .map(|(q, module, boot, selected, candidates)| {
                        Ok(ResolutionEvidence {
                            qualifier: qualifier(q)?,
                            module,
                            boot,
                            selected: selected.map(PathBuf::from),
                            candidates: candidates.into_iter().map(PathBuf::from).collect(),
                        })
                    })
                    .collect::<Result<_, CompileError>>()?,
                modules: modules
                    .into_iter()
                    .map(|(unit, module, boot, source, imports, product)| {
                        Ok(ModuleEvidence {
                            unit,
                            module,
                            boot,
                            source: source.into(),
                            product,
                            imports: imports
                                .into_iter()
                                .map(|(q, module, boot, selected)| {
                                    Ok(ModuleImportEvidence {
                                        qualifier: qualifier(q)?,
                                        module,
                                        boot,
                                        selected: selected.map(PathBuf::from),
                                    })
                                })
                                .collect::<Result<_, CompileError>>()?,
                        })
                    })
                    .collect::<Result<_, CompileError>>()?,
                packages: package_names.0,
            },
            owners: owners
                .into_iter()
                .map(|(unit, module, version, iface, native, fresh, original)| {
                    Ok(OwnerWire {
                        unit,
                        module,
                        module_version: parse_digest(&version)?,
                        skinny_iface_sha256: parse_digest(&iface)?,
                        product_sha256: parse_digest(&native)?,
                        fresh,
                        original_graph_sha256: original.as_deref().map(parse_digest).transpose()?,
                    })
                })
                .collect::<Result<_, CompileError>>()?,
            exact_imports: exact
                .into_iter()
                .map(|(unit, module, imports)| ExactImportsWire {
                    owner: ExactModuleIdentity { unit, module },
                    imports: imports
                        .into_iter()
                        .map(|(unit, module)| ExactModuleIdentity { unit, module })
                        .collect(),
                })
                .collect(),
            packages: packages
                .into_iter()
                .map(|(unit, module, path, digest)| {
                    Ok(PackageWire {
                        unit,
                        module,
                        selected_path: path.into(),
                        sha256: parse_digest(&digest)?,
                    })
                })
                .collect::<Result<_, CompileError>>()?,
        })
    }
}

fn failure(detail: &str) -> CompileError {
    CompileError::ExtractFailed(format!("original execution source proof: {detail}"))
}

impl CertifiedExecutionSourceGraph {
    pub(crate) fn admit(
        input: ExecutionSourceGraphInput<'_>,
    ) -> Result<ExecutionSourceAdmission, CompileError> {
        if !input.evidence.valid(input.source) {
            return Err(failure("source evidence is incomplete or stale"));
        }
        if !input.evidence.selection_complete || input.owners.is_empty() {
            return Ok(ExecutionSourceAdmission::Unavailable(
                ExecutionSourceUnavailable::UnsupportedShape,
            ));
        }
        let excessive = input.source.len() > SOURCE_BYTES_LIMIT
            || input.include.len() > OWNER_LIMIT
            || input.owners.len() > OWNER_LIMIT
            || input.evidence.sources.len() > OWNER_LIMIT
            || input.evidence.modules.len() > OWNER_LIMIT
            || !resolution_candidates_fit(&input.evidence.resolutions)
            || input.evidence.packages.len() > OWNER_LIMIT
            || input.exact_imports.len() > OWNER_LIMIT
            || input.packages.len() > OWNER_LIMIT
            || input
                .evidence
                .modules
                .iter()
                .any(|module| module.imports.len() > OWNER_LIMIT)
            || input
                .exact_imports
                .values()
                .any(|imports| imports.len() > OWNER_LIMIT)
            || input.evidence.modules.iter().fold(0usize, |total, module| {
                total.saturating_add(module.imports.len())
            }) > EDGE_LIMIT
            || input
                .exact_imports
                .values()
                .fold(0usize, |total, imports| total.saturating_add(imports.len()))
                > EDGE_LIMIT;
        if excessive {
            return Ok(ExecutionSourceAdmission::Unavailable(
                ExecutionSourceUnavailable::LimitExceeded,
            ));
        }
        let paths = std::iter::once(input.source_path)
            .chain(input.include.iter().map(PathBuf::as_path))
            .chain(
                input
                    .evidence
                    .sources
                    .iter()
                    .map(|source| source.path.as_path()),
            )
            .chain(
                input
                    .evidence
                    .modules
                    .iter()
                    .map(|module| module.source.as_path()),
            )
            .chain(
                input
                    .evidence
                    .resolutions
                    .iter()
                    .flat_map(|resolution| resolution.candidates.iter().map(PathBuf::as_path)),
            )
            .chain(
                input
                    .packages
                    .values()
                    .map(|package| package.selected_path.as_path()),
            );
        if paths
            .into_iter()
            .any(|path| path.to_str().is_none_or(|text| text.len() > 65536))
        {
            return Ok(ExecutionSourceAdmission::Unavailable(
                ExecutionSourceUnavailable::UnsupportedPath,
            ));
        }
        let Some(include) = crate::module_candidates::context_paths(input.include) else {
            return Ok(ExecutionSourceAdmission::Unavailable(
                ExecutionSourceUnavailable::UnavailableIncludeRoot,
            ));
        };
        let mut owners = input
            .owners
            .iter()
            .map(|owner| {
                let mut wire = OwnerWire::from(owner);
                let identity = ExactModuleIdentity {
                    unit: owner.unit.clone(),
                    module: owner.module.clone(),
                };
                wire.fresh = input.fresh_owners.contains(&identity);
                wire.original_graph_sha256 = input.retained_sources.get(&identity).copied();
                wire
            })
            .collect::<Vec<_>>();
        owners.sort_by(|left, right| (&left.unit, &left.module).cmp(&(&right.unit, &right.module)));
        let wire = GraphWire {
            version: 1,
            profile: PROFILE.into(),
            producer_sha256: input.producer.sha256(),
            semantic_sha256: input.semantic_sha256,
            include,
            source_path: input.source_path.to_path_buf(),
            source: input.source.to_owned(),
            evidence: input.evidence.clone(),
            exact_imports: input
                .exact_imports
                .iter()
                .map(|(owner, imports)| {
                    let mut imports = imports.clone();
                    imports.sort();
                    imports.dedup();
                    ExactImportsWire {
                        owner: owner.clone(),
                        imports,
                    }
                })
                .collect(),
            owners,
            packages: input
                .packages
                .iter()
                .map(|((unit, module), witness)| PackageWire {
                    unit: unit.clone(),
                    module: module.clone(),
                    selected_path: witness.selected_path.clone(),
                    sha256: witness.sha256,
                })
                .collect(),
        };
        wire.validate()?;
        let bytes = wire.encode()?;
        if bytes.len() > GRAPH_BYTES_LIMIT {
            return Ok(ExecutionSourceAdmission::Unavailable(
                ExecutionSourceUnavailable::LimitExceeded,
            ));
        }
        match validate_cbor_budget(&bytes) {
            Ok(()) => {}
            Err(CborBudgetError::LimitExceeded) => {
                return Ok(ExecutionSourceAdmission::Unavailable(
                    ExecutionSourceUnavailable::LimitExceeded,
                ))
            }
            Err(CborBudgetError::InvalidEncoding) => {
                return Err(failure("canonical encoder produced an invalid graph"))
            }
        }
        Ok(ExecutionSourceAdmission::Available(Arc::new(
            Self::from_wire(bytes, &wire, None),
        )))
    }

    /// Authenticate the one worker-issued exact recipe against the admitted
    /// transaction. Its canonical bytes remain the identity consumed by later
    /// passes; native-global package witnesses are a separate proof domain.
    pub(crate) fn admit_issued(
        bytes: Arc<[u8]>,
        digest: [u8; 32],
        input: ExecutionSourceGraphInput<'_>,
    ) -> Result<Arc<Self>, CompileError> {
        if bytes.len() > GRAPH_BYTES_LIMIT || <[u8; 32]>::from(Sha256::digest(&bytes)) != digest {
            return Err(failure("issued recipe digest or byte bound differs"));
        }
        let wire = GraphWire::decode(&bytes)?;
        wire.validate()?;
        if wire.encode()?.as_slice() != bytes.as_ref() {
            return Err(failure("noncanonical issued recipe"));
        }
        if !input.evidence.valid(input.source)
            || !input.evidence.cache_safe
            || !input.evidence.selection_complete
            || wire.producer_sha256 != input.producer.sha256()
            || wire.semantic_sha256 != input.semantic_sha256
            || wire.source_path != input.source_path
            || wire.source != input.source
            || crate::module_candidates::context_paths(input.include).as_ref()
                != Some(&wire.include)
            || &wire.evidence != input.evidence
        {
            return Err(failure("issued recipe source transaction differs"));
        }
        let owners = input
            .owners
            .iter()
            .map(|owner| ((owner.unit.as_str(), owner.module.as_str()), owner))
            .collect::<BTreeMap<_, _>>();
        if owners.len() != input.owners.len() || wire.owners.len() != owners.len() {
            return Err(failure("issued recipe original inventory differs"));
        }
        for row in &wire.owners {
            let identity = ExactModuleIdentity {
                unit: row.unit.clone(),
                module: row.module.clone(),
            };
            let fresh = input.fresh_owners.contains(&identity);
            let retained = if fresh {
                None
            } else {
                input.retained_sources.get(&identity).copied()
            };
            if owners
                .get(&(row.unit.as_str(), row.module.as_str()))
                .copied()
                != Some(&row.owner())
                || row.fresh != fresh
                || (row.original_graph_sha256.is_some()
                    && (fresh || row.original_graph_sha256 != retained))
            {
                return Err(failure("issued recipe original owner differs"));
            }
        }
        // The selected worker scope may withhold an optional inherited recipe.
        // Every advertised reference still needs the exact retained native owner.
        let imports = wire
            .exact_imports
            .iter()
            .map(|row| (row.owner.clone(), row.imports.clone()))
            .collect::<BTreeMap<_, _>>();
        if imports.len() != wire.exact_imports.len() || &imports != input.exact_imports {
            return Err(failure("issued recipe exact import closure differs"));
        }
        let packages = wire
            .packages
            .iter()
            .map(|row| {
                (
                    (row.unit.clone(), row.module.clone()),
                    PackageInterfaceWitness {
                        selected_path: row.selected_path.clone(),
                        sha256: row.sha256,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        if packages.len() != wire.packages.len() || &packages != input.packages {
            return Err(failure("issued recipe source package closure differs"));
        }
        Ok(Arc::new(Self::from_wire(bytes, &wire, Some(digest))))
    }

    #[cfg(test)]
    pub(crate) fn recover(bytes: Vec<u8>) -> Result<Arc<Self>, CompileError> {
        if bytes.len() > GRAPH_BYTES_LIMIT {
            return Err(failure("graph exceeds its byte bound"));
        }
        let wire = GraphWire::decode(&bytes)?;
        wire.validate()?;
        if wire.encode()? != bytes {
            return Err(failure("noncanonical encoding"));
        }
        Ok(Arc::new(Self::from_wire(bytes, &wire, None)))
    }

    pub(crate) fn recover_verified(
        bytes: Vec<u8>,
        digest: [u8; 32],
    ) -> Result<Arc<Self>, CompileError> {
        if bytes.len() > GRAPH_BYTES_LIMIT {
            return Err(failure("graph exceeds its byte bound"));
        }
        let wire = GraphWire::decode(&bytes)?;
        wire.validate()?;
        if wire.encode()? != bytes {
            return Err(failure("noncanonical encoding"));
        }
        Ok(Arc::new(Self::from_wire(bytes, &wire, Some(digest))))
    }

    fn from_wire(
        bytes: impl Into<Arc<[u8]>>,
        wire: &GraphWire,
        verified_digest: Option<[u8; 32]>,
    ) -> Self {
        let bytes: Arc<[u8]> = bytes.into();
        Self {
            digest: verified_digest.unwrap_or_else(|| Sha256::digest(&bytes).into()),
            producer_sha256: wire.producer_sha256,
            semantic_sha256: wire.semantic_sha256,
            bytes,
            owners: wire
                .owners
                .iter()
                .map(|owner| ((owner.unit.clone(), owner.module.clone()), owner.owner()))
                .collect(),
            source_replay_roots: wire.source_replay_roots(),
            source_imports: wire.source_imports(),
            retained_graphs: wire
                .owners
                .iter()
                .filter_map(|owner| {
                    owner.original_graph_sha256.map(|digest| {
                        (
                            (owner.unit.clone(), owner.module.clone()),
                            (owner.owner(), digest),
                        )
                    })
                })
                .collect(),
        }
    }

    pub(crate) fn digest(&self) -> [u8; 32] {
        self.digest
    }
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    /// Capture unchanged graph bytes beside a request's metadata envelope.
    /// The descriptor path transports this already authenticated digest.
    pub(crate) fn capture_descriptor(&self, root: &Path) -> std::io::Result<PathBuf> {
        let path = root.join(format!("execution-{}.cbor", hex(&self.digest)));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                use std::io::Write;
                file.write_all(self.bytes())?;
            }
            Err(failure) if failure.kind() == std::io::ErrorKind::AlreadyExists => {
                // Scope and candidate parcels may share one immutable capture.
                // Compare in bounded chunks without replacing an earlier file.
                let mut file = std::fs::File::open(&path)?;
                let mut buffer = [0; 16 * 1024];
                for chunk in self.bytes().chunks(buffer.len()) {
                    file.read_exact(&mut buffer[..chunk.len()])?;
                    if buffer[..chunk.len()] != *chunk {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "execution graph capture differs",
                        ));
                    }
                }
                if file.read(&mut buffer[..1])? != 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "execution graph capture has trailing bytes",
                    ));
                }
            }
            Err(failure) => return Err(failure),
        }
        Ok(path)
    }
    pub(crate) fn producer_sha256(&self) -> [u8; 32] {
        self.producer_sha256
    }
    pub(crate) fn semantic_sha256(&self) -> Option<[u8; 32]> {
        self.semantic_sha256
    }
    pub(crate) fn matches_owner(&self, owner: &CachedHomeOwner) -> bool {
        self.owners.get(&(owner.unit.clone(), owner.module.clone())) == Some(owner)
    }
    /// Source replay capability only. Native products and interfaces have
    /// independent authority. Replay still verifies every demanded retained
    /// original graph, source selection, interface, and package witness.
    pub(crate) fn eligible_source_replay_root(&self, owner: &CachedHomeOwner) -> bool {
        self.matches_owner(owner)
            && self
                .source_replay_roots
                .contains(&(owner.unit.clone(), owner.module.clone()))
    }

    /// Direct native dependencies authenticated by this owner's original
    /// recipe. Package dependencies retain their separate package witnesses.
    pub(crate) fn direct_source_owners(
        &self,
        owner: &CachedHomeOwner,
    ) -> Option<Vec<&CachedHomeOwner>> {
        if !self.matches_owner(owner) {
            return None;
        }
        self.source_imports
            .get(&(owner.unit.clone(), owner.module.clone()))
            .into_iter()
            .flatten()
            .map(|key| self.owners.get(key))
            .collect()
    }

    /// Exact native owners of every positive source in this root's recipe.
    /// Fresh local edges need owner equality, while retained edges additionally
    /// require their separately sealed original graph.
    pub(crate) fn required_source_owners(&self, owner: &CachedHomeOwner) -> Vec<CachedHomeOwner> {
        let mut pending = vec![(owner.unit.clone(), owner.module.clone())];
        let mut seen = BTreeSet::new();
        let mut required = Vec::new();
        while let Some(key) = pending.pop() {
            if !seen.insert(key.clone()) {
                continue;
            }
            if let Some(owner) = self.owners.get(&key) {
                required.push(owner.clone());
            }
            if !self.retained_graphs.contains_key(&key) {
                if let Some(imports) = self.source_imports.get(&key) {
                    pending.extend(imports.iter().cloned());
                }
            }
        }
        required
    }

    pub(crate) fn required_original_graphs(
        &self,
        owner: &CachedHomeOwner,
    ) -> Vec<(CachedHomeOwner, [u8; 32])> {
        let mut pending = vec![(owner.unit.clone(), owner.module.clone())];
        let mut seen = BTreeSet::new();
        let mut required = BTreeMap::new();
        while let Some(key) = pending.pop() {
            if !seen.insert(key.clone()) {
                continue;
            }
            if let Some((owner, digest)) = self.retained_graphs.get(&key) {
                required.insert(key, (owner.clone(), *digest));
            } else if let Some(imports) = self.source_imports.get(&key) {
                pending.extend(imports.iter().cloned());
            }
        }
        required.into_values().collect()
    }
}

#[derive(Debug)]
enum OriginalOwnerRefusal<'a> {
    InvalidIdentity(&'a OwnerWire),
    Duplicate(&'a OwnerWire),
    Unordered(&'a OwnerWire),
    FreshWithRetainedGraph(&'a OwnerWire),
    InvalidRetainedGraphDigest(&'a OwnerWire),
}

impl std::fmt::Display for OriginalOwnerRefusal<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (owner, reason) = match self {
            Self::InvalidIdentity(owner) => (owner, "empty unit or module identity"),
            Self::Duplicate(owner) => (owner, "duplicate unit/module key"),
            Self::Unordered(owner) => (owner, "unit/module keys are not strictly ordered"),
            Self::FreshWithRetainedGraph(owner) => {
                (owner, "fresh source also names a retained original graph")
            }
            Self::InvalidRetainedGraphDigest(owner) => {
                (owner, "retained original graph has a zero digest")
            }
        };
        write!(
            formatter,
            "original owner {}:{}: {reason}",
            owner.unit, owner.module
        )
    }
}

impl GraphWire {
    fn validate_original_owners(
        &self,
    ) -> Result<BTreeSet<(&String, &String)>, OriginalOwnerRefusal<'_>> {
        let mut owners = BTreeSet::new();
        let mut previous = None;
        for owner in &self.owners {
            let key = (&owner.unit, &owner.module);
            if owner.unit.is_empty() || owner.module.is_empty() {
                return Err(OriginalOwnerRefusal::InvalidIdentity(owner));
            }
            if !owners.insert(key) {
                return Err(OriginalOwnerRefusal::Duplicate(owner));
            }
            if owner.fresh && owner.original_graph_sha256.is_some() {
                return Err(OriginalOwnerRefusal::FreshWithRetainedGraph(owner));
            }
            if owner.original_graph_sha256 == Some([0; 32]) {
                return Err(OriginalOwnerRefusal::InvalidRetainedGraphDigest(owner));
            }
            if previous.is_some_and(|previous| previous >= key) {
                return Err(OriginalOwnerRefusal::Unordered(owner));
            }
            previous = Some(key);
        }
        Ok(owners)
    }

    fn source_imports(&self) -> BTreeMap<(String, String), BTreeSet<(String, String)>> {
        let mut imports: BTreeMap<_, BTreeSet<_>> = BTreeMap::new();
        for module in self.evidence.modules.iter().filter(|module| !module.boot) {
            imports
                .entry((module.unit.clone(), module.module.clone()))
                .or_default()
                .extend(
                    module
                        .imports
                        .iter()
                        .filter(|edge| edge.selected.is_some())
                        .map(|edge| (module.unit.clone(), edge.module.clone())),
                );
        }
        for row in &self.exact_imports {
            imports
                .entry((row.owner.unit.clone(), row.owner.module.clone()))
                .or_default()
                .extend(
                    row.imports
                        .iter()
                        .map(|owner| (owner.unit.clone(), owner.module.clone())),
                );
        }
        imports
    }
    fn validate(&self) -> Result<(), CompileError> {
        if self.version != 1
            || self.profile != PROFILE
            || self.producer_sha256 == [0; 32]
            || !self.source_path.is_absolute()
            || self.source.len() > SOURCE_BYTES_LIMIT
            || self.include.len() > OWNER_LIMIT
            || self.include.iter().any(|root| !root.is_absolute())
            || self.owners.is_empty()
            || self.owners.len() > OWNER_LIMIT
            || self.evidence.version != 4
            || !self.evidence.cache_safe
            || !self.evidence.selection_complete
            || self.evidence.sources.is_empty()
            || self.evidence.sources.len() > OWNER_LIMIT
            || self.evidence.modules.is_empty()
            || self.evidence.modules.len() > OWNER_LIMIT
            || !resolution_candidates_fit(&self.evidence.resolutions)
            || self.exact_imports.len() > OWNER_LIMIT
            || self.packages.len() > OWNER_LIMIT
        {
            return Err(failure("invalid graph shape or incomplete source proof"));
        }
        let owners = self
            .validate_original_owners()
            .map_err(|refusal| failure(&refusal.to_string()))?;
        let source_owners = self
            .evidence
            .modules
            .iter()
            .filter(|module| !module.boot)
            .map(|module| (&module.unit, &module.module))
            .collect::<BTreeSet<_>>();
        if self.evidence.modules.iter().any(|module| {
            !module.boot
                && module.product == ProductAvailability::Ready
                && !owners.contains(&(&module.unit, &module.module))
        }) {
            return Err(failure("source product lacks an exact original owner"));
        }
        let mut edges = 0usize;
        let mut exact_owners = BTreeSet::new();
        for row in &self.exact_imports {
            edges = edges.saturating_add(row.imports.len());
            if !source_owners.contains(&(&row.owner.unit, &row.owner.module))
                || !exact_owners.insert(&row.owner)
                || row.imports.windows(2).any(|pair| pair[0] >= pair[1])
            {
                return Err(failure("invalid exact source import table"));
            }
        }
        if edges > EDGE_LIMIT
            || self
                .evidence
                .modules
                .iter()
                .map(|module| module.imports.len())
                .sum::<usize>()
                > EDGE_LIMIT
        {
            return Err(failure("source graph exceeds its edge bound"));
        }
        let generated = self
            .evidence
            .sources
            .iter()
            .filter(|source| source.path == Path::new("@generated-source"))
            .collect::<Vec<_>>();
        if generated.len() != 1
            || generated[0].sha256 != format!("{:x}", Sha256::digest(self.source.as_bytes()))
        {
            return Err(failure(
                "generated source does not match its consumed bytes",
            ));
        }
        let mut packages = BTreeSet::new();
        if self.packages.iter().any(|package| {
            package.unit.is_empty()
                || package.module.is_empty()
                || !package.selected_path.is_absolute()
                || !packages.insert((&package.unit, &package.module))
        }) {
            return Err(failure("invalid package interface proof"));
        }
        Ok(())
    }

    fn source_replay_roots(&self) -> BTreeSet<(String, String)> {
        let modules = self
            .evidence
            .modules
            .iter()
            .map(|module| {
                (
                    (module.unit.as_str(), module.module.as_str(), module.boot),
                    module,
                )
            })
            .collect::<BTreeMap<_, _>>();
        let originals = self
            .owners
            .iter()
            .map(|owner| ((owner.unit.as_str(), owner.module.as_str()), owner))
            .collect::<BTreeMap<_, _>>();
        let replay_originals = self
            .owners
            .iter()
            .filter(|owner| {
                !SessionModule::is_reserved_name(&owner.module)
                    && (owner.fresh || owner.original_graph_sha256.is_some())
            })
            .map(|owner| (owner.unit.as_str(), owner.module.as_str()))
            .collect::<BTreeSet<_>>();
        let sources = self
            .evidence
            .sources
            .iter()
            .map(|source| &source.path)
            .collect::<BTreeSet<_>>();
        let exact = self
            .exact_imports
            .iter()
            .map(|row| {
                (
                    (row.owner.unit.as_str(), row.owner.module.as_str()),
                    &row.imports,
                )
            })
            .collect::<BTreeMap<_, _>>();
        self.owners
            .iter()
            .filter_map(|owner| {
                let root = (owner.unit.as_str(), owner.module.as_str(), false);
                if !owner.fresh {
                    return None;
                }
                let root_node = modules.get(&root)?;
                if root_node.product != ProductAvailability::Ready {
                    return None;
                }
                let mut pending = vec![root];
                let mut seen = BTreeSet::new();
                while let Some(key) = pending.pop() {
                    if !seen.insert(key) {
                        continue;
                    }
                    // Native session products remain executable, but their
                    // source cannot reconstruct the retained session authority.
                    if SessionModule::is_reserved_name(key.1) {
                        return None;
                    }
                    if !key.2 {
                        if let Some(original) = originals.get(&(key.0, key.1)) {
                            if !original.fresh {
                                original.original_graph_sha256?;
                                continue;
                            }
                        }
                    }
                    let node = modules.get(&key)?;
                    if node.product != ProductAvailability::Ready
                        || !originals.contains_key(&(key.0, key.1))
                        || !node.source.is_absolute()
                        || !sources.contains(&node.source)
                    {
                        return None;
                    }
                    if node.imports.iter().any(|imported| imported.boot) {
                        return None;
                    }
                    if let Some(required) = exact.get(&(key.0, key.1)) {
                        if required.iter().any(|owner| {
                            !replay_originals
                                .contains(&(owner.unit.as_str(), owner.module.as_str()))
                        }) {
                            return None;
                        }
                        for owner in required.iter().filter(|owner| {
                            originals[&(owner.unit.as_str(), owner.module.as_str())].fresh
                        }) {
                            pending.push((owner.unit.as_str(), owner.module.as_str(), false));
                        }
                    }
                    for imported in &node.imports {
                        if let Some(path) = &imported.selected {
                            let imported_key = (key.0, imported.module.as_str(), imported.boot);
                            if modules
                                .get(&imported_key)
                                .is_none_or(|selected| &selected.source != path)
                            {
                                return None;
                            }
                            pending.push(imported_key);
                        }
                    }
                }
                Some((owner.unit.clone(), owner.module.clone()))
            })
            .collect()
    }
}
