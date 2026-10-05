//! Diagnostic observations from the deployment compilation owner. Reports are
//! not catalog admission and never publish runtime candidates.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{certified_products, module_candidates, CompileError};
use module_candidates::deployment::NativeCatalogSourceSelection;

#[derive(Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Phase {
    CompilerBinding,
    CompilerDeploymentAdmission,
    CompilerExecution,
    CompilerOutputDecode,
    SourceEvidenceValidation,
    ProductCertification,
    TargetAdmission,
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn write(output: &Path, name: &str, value: &Value) -> Result<(), CompileError> {
    std::fs::write(
        output.join(name),
        serde_json::to_vec_pretty(value)
            .map_err(|error| CompileError::ExtractFailed(error.to_string()))?,
    )?;
    Ok(())
}

pub(super) fn request(
    output: &Path,
    source: &Path,
    targets: &[&str],
    scratch: &Path,
    selection: &NativeCatalogSourceSelection,
) -> Result<(), CompileError> {
    std::fs::create_dir(output)?;
    write(
        output,
        "request.json",
        &json!({
            "schema": 1,
            "source": source,
            "source_sha256": digest(&std::fs::read(source)?),
            "targets": targets,
            "private_scratch": scratch,
            "source_selection": selection,
            "ordered_include_roots": selection.include_roots(),
        }),
    )?;
    phase(output, Phase::CompilerBinding)
}

pub(super) fn phase(output: &Path, phase: Phase) -> Result<(), CompileError> {
    write(output, "phase.json", &json!({"phase": phase}))
}

pub(super) fn endpoint(
    output: &Path,
    producer: &[u8],
    argv: &[std::ffi::OsString],
    raw: &Path,
    stage: Phase,
) -> Result<(), CompileError> {
    write(
        output,
        "invocation.json",
        &json!({
            "producer_identity": producer,
            "canonical_producer_sha256": crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(producer).hex(),
            "argv": argv.iter().map(|value| value.to_string_lossy()).collect::<Vec<_>>(),
            "raw_output": raw,
        }),
    )?;
    phase(output, stage)
}

pub(super) fn inventory(
    output: &Path,
    selection: &NativeCatalogSourceSelection,
    evidence: &crate::cache::DependencyEvidence,
    certified: &certified_products::CertifiedProducts,
) -> Result<(), CompileError> {
    if NativeCatalogSourceSelection::capture(&selection.snapshot_root)? != *selection {
        return Err(crate::toolchain::ModulePackageError::SourceChanged.into());
    }
    let interfaces: Vec<_> = certified.module_interfaces.iter().map(|interface| json!({
        "unit": interface.unit(),
        "module": interface.module(),
        "source_sha256": interface.source_sha256(),
        "producer_sha256": interface.producer_sha256(),
        "interface_sha256": interface.interface_sha256(),
        "package_imports_sha256": interface.package_imports_sha256(),
        "core": interface.core_bytes().map(|bytes| json!({
            "bytes": bytes.len(), "sha256": digest(bytes),
        })),
        "interface_requirements": interface.requirements().iter().map(|((unit, module), hash)| json!({
            "unit": unit, "module": module, "interface_sha256": hash,
        })).collect::<Vec<_>>(),
    })).collect();
    let native_owners: Vec<_> = certified
        .recovery_products
        .iter()
        .map(|product| {
            let owner = product.owner();
            json!({
                "unit": owner.unit,
                "module": owner.module,
                "module_version": owner.module_version.0,
                "skinny_iface_sha256": owner.skinny_iface_sha256,
                "product_sha256": owner.product_sha256,
                "source_sha256": product.source_sha256(),
            })
        })
        .collect();
    write(
        output,
        "catalog-inventory.json",
        &json!({
            "schema": 1,
            "diagnostic_only": true,
            "canonical_interface_count": interfaces.len(),
            "native_owner_count": native_owners.len(),
            "native_group_count": certified.groups.len(),
            "module_interfaces": interfaces,
            "native_owners": native_owners,
            "worker_module_inventory": evidence.modules,
            "cache_safe": evidence.cache_safe,
            "selection_complete": evidence.selection_complete,
        }),
    )?;
    phase(output, Phase::TargetAdmission)
}

pub(super) fn outcome(
    output: &Path,
    result: &Result<(), CompileError>,
) -> Result<PathBuf, CompileError> {
    let read = |name: &str| {
        std::fs::read(output.join(name))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
    };
    let invocation = read("invocation.json");
    let worker_evidence = invocation
        .as_ref()
        .and_then(|value| value["raw_output"].as_str())
        .and_then(|raw| std::fs::read(Path::new(raw).join("dependencies.json")).ok())
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    let inventory = read("catalog-inventory.json");
    write(
        output,
        "outcome.json",
        &json!({
            "schema": 1,
            "diagnostic_only": true,
            "status": if result.is_ok() { "compiled" } else { "refused" },
            "refusal": result.as_ref().err().map(ToString::to_string),
            "phase": read("phase.json").map(|value| value["phase"].clone()),
            "request": read("request.json"),
            "invocation": invocation,
            "raw_worker_flags_unadmitted": worker_evidence.map(|evidence| json!({
                "cache_safe": evidence["cache_safe"],
                "selection_complete": evidence["selection_complete"],
            })),
            "canonical_inventory_available": inventory.is_some(),
            "canonical_interface_count": inventory.as_ref().map(|value| &value["canonical_interface_count"]),
            "native_owner_count": inventory.as_ref().map(|value| &value["native_owner_count"]),
            "native_group_count": inventory.as_ref().map(|value| &value["native_group_count"]),
        }),
    )?;
    Ok(output.join("outcome.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusal_preserves_worker_flags_without_claiming_canonical_inventory() {
        let output = tempfile::tempdir().unwrap();
        let raw = output.path().join("raw");
        std::fs::create_dir_all(raw.join("build-products")).unwrap();
        std::fs::write(raw.join("build-products/partial.hi"), b"partial").unwrap();
        let source = output.path().join("Probe.hs");
        std::fs::write(&source, "module Probe where\nvalue = ()\n").unwrap();
        let evidence = serde_json::to_vec(&json!({
            "version": 4, "cache_safe": false, "selection_complete": true,
            "sources": [], "modules": [], "packages": [], "resolutions": [],
        }))
        .unwrap();
        std::fs::write(raw.join("dependencies.json"), &evidence).unwrap();
        endpoint(
            output.path(),
            &[7; 32],
            &[],
            &raw,
            Phase::SourceEvidenceValidation,
        )
        .unwrap();
        let result = crate::artifacts::validate_prepared_fixture_sources(&evidence, &source, &[]);
        assert!(result.is_err());
        let path = outcome(output.path(), &result).unwrap();
        let report: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(report["status"], "refused");
        assert_eq!(report["phase"], "source_evidence_validation");
        assert_eq!(report["raw_worker_flags_unadmitted"]["cache_safe"], false);
        assert_eq!(
            report["raw_worker_flags_unadmitted"]["selection_complete"],
            true
        );
        assert_eq!(report["canonical_inventory_available"], false);
        for field in [
            "canonical_interface_count",
            "native_owner_count",
            "native_group_count",
        ] {
            assert!(report[field].is_null());
        }
        assert_eq!(
            std::fs::read(raw.join("build-products/partial.hi")).unwrap(),
            b"partial"
        );
    }

    #[test]
    fn undeclared_source_refusal_reports_true_flags_as_unadmitted() {
        let output = tempfile::tempdir().unwrap();
        let raw = output.path().join("raw");
        std::fs::create_dir(&raw).unwrap();
        let source = output.path().join("Probe.hs");
        let outside = output.path().join("Outside.hs");
        std::fs::write(&source, "module Probe where\nvalue = ()\n").unwrap();
        std::fs::write(&outside, "module Outside where\n").unwrap();
        let evidence = serde_json::to_vec(&json!({
            "version": 4, "cache_safe": true, "selection_complete": true,
            "sources": [{ "path": outside, "sha256": digest(&std::fs::read(&outside).unwrap()) }],
            "modules": [], "packages": [], "resolutions": [],
        }))
        .unwrap();
        std::fs::write(raw.join("dependencies.json"), &evidence).unwrap();
        endpoint(
            output.path(),
            &[7; 32],
            &[],
            &raw,
            Phase::SourceEvidenceValidation,
        )
        .unwrap();
        let result = crate::artifacts::validate_prepared_fixture_sources(&evidence, &source, &[]);
        assert!(
            matches!(&result, Err(CompileError::ExtractFailed(message)) if message.contains("undeclared source"))
        );
        let report: Value = serde_json::from_slice(
            &std::fs::read(outcome(output.path(), &result).unwrap()).unwrap(),
        )
        .unwrap();
        assert_eq!(report["status"], "refused");
        assert_eq!(report["raw_worker_flags_unadmitted"]["cache_safe"], true);
        assert_eq!(
            report["raw_worker_flags_unadmitted"]["selection_complete"],
            true
        );
        assert_eq!(report["canonical_inventory_available"], false);
        assert!(report["canonical_interface_count"].is_null());
        assert!(raw.join("dependencies.json").is_file());
    }
}
