//! Per-program process isolation for the STG corpus. A native crash or watchdog
//! expiry is evidence for one row, never permission to omit the remaining rows.

use std::collections::BTreeSet;
use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::{Command as ProcessCommand, ExitStatus};

use tidepool_prepared_corpus::{
    Classification, Expectation, Expectations, OracleScope, Outcome, ProgramRecord,
    ProjectionManifest, ProjectionOutcome, Stage,
};
use tidepool_repr::serial::read_metadata;

const REPORT_VERSION: u32 = 2;

/// Arguments are paths owned by the corpus verification recipe. The child
/// receives a manifest index, never a command string derived from a program.
enum Command {
    EffectsCore {
        include: PathBuf,
    },
    AuditOperations {
        manifest: PathBuf,
        output: PathBuf,
    },
    Run {
        manifest: PathBuf,
        expectations: PathBuf,
        metadata: PathBuf,
        output: PathBuf,
    },
    Child {
        manifest: PathBuf,
        expectations: PathBuf,
        metadata: PathBuf,
        output: PathBuf,
        index: usize,
    },
}

#[derive(serde::Serialize)]
struct CorpusReport {
    version: u32,
    source_targets: Vec<tidepool_prepared_corpus::SourceTargetMapping>,
    source_mapped: usize,
    source_unmapped: usize,
    stg_programs: usize,
    programs: Vec<ProgramRecord>,
    stage_totals: Vec<StageTotal>,
}

#[derive(Debug, Clone, Copy)]
struct ManifestSummary {
    source_mapped: usize,
    source_unmapped: usize,
}

#[derive(serde::Serialize)]
struct StageTotal {
    stage: Stage,
    passed: usize,
    failed: usize,
    not_closed: usize,
    no_finite_observation: usize,
    function_valued: usize,
    missing_expectation: usize,
    no_oracle: usize,
    running: usize,
    not_reached: usize,
}

#[derive(serde::Serialize)]
struct OperationAuditReport {
    version: u32,
    manifest_programs: usize,
    projected_programs: usize,
    decoded_programs: usize,
    projection_omissions: Vec<AuditOmission>,
    decode_omissions: Vec<AuditOmission>,
    operations: Vec<AuditedOperation>,
}

#[derive(serde::Serialize)]
struct AuditOmission {
    program: String,
    reason: String,
}

#[derive(serde::Serialize)]
struct AuditedOperation {
    identity: String,
    signature: String,
    supported: bool,
    programs: Vec<String>,
}

struct OperationAccumulator {
    identity: tidepool_repr::execution_schema::OperationIdentity,
    signature: tidepool_repr::execution_schema::Signature,
    supported: bool,
    programs: Vec<String>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let command = parse_arguments()?;
    match command {
        Command::EffectsCore { include } => {
            let module = include.join("Tidepool/Effects/Core.hs");
            if !module.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("generated effect surface is missing {}", module.display()),
                )
                .into());
            }
            println!("{}", include.display());
            Ok(())
        }
        Command::AuditOperations { manifest, output } => audit_operations(manifest, output),
        Command::Run {
            manifest,
            expectations,
            metadata,
            output,
        } => run_corpus(manifest, expectations, metadata, output),
        Command::Child {
            manifest,
            expectations,
            metadata,
            output,
            index,
        } => run_one(manifest, expectations, metadata, output, index),
    }
}

fn parse_arguments() -> Result<Command, Box<dyn Error>> {
    parse_values(std::env::args_os().skip(1).collect())
}

fn parse_values(values: Vec<OsString>) -> Result<Command, Box<dyn Error>> {
    if let [mode, include] = values.as_slice() {
        if mode == "effects-core" {
            return Ok(Command::EffectsCore {
                include: PathBuf::from(include),
            });
        }
    }
    if let [mode, manifest, output] = values.as_slice() {
        if mode == "audit-operations" {
            return Ok(Command::AuditOperations {
                manifest: PathBuf::from(manifest),
                output: PathBuf::from(output),
            });
        }
    }
    let [mode, manifest, expectations, metadata, output, rest @ ..] = values.as_slice() else {
        return Err(usage().into());
    };
    let paths = || {
        (
            PathBuf::from(manifest),
            PathBuf::from(expectations),
            PathBuf::from(metadata),
            PathBuf::from(output),
        )
    };
    match (mode.to_str(), rest) {
        (Some("run"), []) => {
            let (manifest, expectations, metadata, output) = paths();
            Ok(Command::Run {
                manifest,
                expectations,
                metadata,
                output,
            })
        }
        (Some("child"), [index]) => {
            let index = index
                .to_str()
                .ok_or_else(usage)?
                .parse()
                .map_err(|_| usage())?;
            let (manifest, expectations, metadata, output) = paths();
            Ok(Command::Child {
                manifest,
                expectations,
                metadata,
                output,
                index,
            })
        }
        _ => Err(usage().into()),
    }
}

fn usage() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "usage: prepared-corpus effects-core GENERATED_INCLUDE | prepared-corpus audit-operations MANIFEST OUTPUT | prepared-corpus run MANIFEST EXPECTATIONS METADATA OUTPUT | prepared-corpus child MANIFEST EXPECTATIONS METADATA OUTPUT INDEX",
    )
}

fn audit_operations(manifest_path: PathBuf, output: PathBuf) -> Result<(), Box<dyn Error>> {
    let manifest = read_manifest(&manifest_path)?;
    validate_manifest(&manifest)?;
    let requirements = tidepool_toolchain::prepared_artifact::production_requirements()?;
    let mut projected_programs = 0;
    let mut decoded_programs = 0;
    let mut projection_omissions = Vec::new();
    let mut decode_omissions = Vec::new();
    let mut operations = Vec::new();

    for row in &manifest.programs {
        match &row.projection {
            ProjectionOutcome::Rejected { reason } => projection_omissions.push(AuditOmission {
                program: row.name.clone(),
                reason: reason.clone(),
            }),
            ProjectionOutcome::Projected { artifact, .. } => {
                projected_programs += 1;
                let parsed = manifest_artifact_path(&manifest_path, artifact)
                    .and_then(|path| fs::read(path).map_err(|error| error.into()))
                    .and_then(|bytes| {
                        tidepool_repr::execution_schema::parse_program(
                            &bytes,
                            &requirements,
                            tidepool_repr::execution_schema::DecodeLimits::default(),
                        )
                        .map_err(|error| error.into())
                    });
                match parsed {
                    Ok(program) => {
                        decoded_programs += 1;
                        record_operations(&mut operations, &row.name, &program);
                    }
                    Err(error) => decode_omissions.push(AuditOmission {
                        program: row.name.clone(),
                        reason: error.to_string(),
                    }),
                }
            }
        }
    }

    let operations = operations
        .into_iter()
        .map(|operation: OperationAccumulator| AuditedOperation {
            identity: format!("{:?}", operation.identity),
            signature: format!("{:?}", operation.signature),
            supported: operation.supported,
            programs: operation.programs,
        })
        .collect();
    write_json(
        &output,
        &OperationAuditReport {
            version: 1,
            manifest_programs: manifest.programs.len(),
            projected_programs,
            decoded_programs,
            projection_omissions,
            decode_omissions,
            operations,
        },
    )
}

fn record_operations(
    accumulated: &mut Vec<OperationAccumulator>,
    program_name: &str,
    program: &tidepool_repr::execution_schema::PreparedProgram,
) {
    for declaration in program.operations() {
        let signature = &program.signatures()[declaration.signature.0 as usize];
        if let Some(existing) = accumulated.iter_mut().find(|existing| {
            existing.identity == declaration.identity && existing.signature == *signature
        }) {
            if existing
                .programs
                .last()
                .is_none_or(|name| name != program_name)
            {
                existing.programs.push(program_name.to_owned());
            }
            continue;
        }
        accumulated.push(OperationAccumulator {
            identity: declaration.identity.clone(),
            signature: signature.clone(),
            supported: tidepool_codegen::prepared_program::supports_operation(
                declaration,
                signature,
            ),
            programs: vec![program_name.to_owned()],
        });
    }
}

fn run_corpus(
    manifest: PathBuf,
    expectations: PathBuf,
    metadata: PathBuf,
    output: PathBuf,
) -> Result<(), Box<dyn Error>> {
    let executable = std::env::current_exe()?;
    run_corpus_with(&executable, manifest, expectations, metadata, output)
}

fn run_corpus_with(
    executable: &Path,
    manifest_path: PathBuf,
    expectations: PathBuf,
    metadata: PathBuf,
    output: PathBuf,
) -> Result<(), Box<dyn Error>> {
    let manifest = read_manifest(&manifest_path)?;
    let summary = validate_manifest(&manifest)?;
    // A declared oracle domain must describe this manifest. An unreadable
    // expectation file is left to each child, which records it as that row's
    // validation failure.
    if let Ok(declared) = read_json::<Expectations>(&expectations) {
        validate_oracle_domain(&manifest, &declared)?;
    }

    let mut programs = Vec::with_capacity(manifest.programs.len());
    for (index, row) in manifest.programs.iter().enumerate() {
        let child_output = child_report_path(&output, index);
        remove_stale_child_report(&child_output)?;
        #[allow(
            clippy::disallowed_methods,
            reason = "corpus CLI fan-out: spawns one short-lived child per manifest row and blocks on its exit status; not a resident process this crate leaves running"
        )]
        let status = ProcessCommand::new(executable)
            .arg("child")
            .arg(&manifest_path)
            .arg(&expectations)
            .arg(&metadata)
            .arg(&child_output)
            .arg(index.to_string())
            .status();
        let mut report = match &status {
            Ok(status) => read_child_report(&child_output, &row.name, *status),
            Err(error) => {
                failed_without_report(&row.name, format!("could not start child process: {error}"))
            }
        };
        if let Ok(status) = status {
            if !status.success() {
                fail_abnormal(&mut report, format!("child exited abnormally: {status}"));
            }
        }
        programs.push(report);
    }

    let report = CorpusReport {
        version: REPORT_VERSION,
        source_targets: manifest.source_targets,
        source_mapped: summary.source_mapped,
        source_unmapped: summary.source_unmapped,
        stg_programs: programs.len(),
        stage_totals: stage_totals(&programs),
        programs,
    };
    write_json(&output, &report)?;
    Ok(())
}

fn run_one(
    manifest_path: PathBuf,
    expectations_path: PathBuf,
    metadata_path: PathBuf,
    output: PathBuf,
    index: usize,
) -> Result<(), Box<dyn Error>> {
    let manifest = read_manifest(&manifest_path)?;
    validate_manifest(&manifest)?;
    let row = manifest.programs.get(index).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("manifest has no program at index {index}"),
        )
    })?;
    tidepool_prepared_corpus::watchdog::arm();
    let _watchdog = tidepool_prepared_corpus::watchdog::begin(&row.name);

    match &row.projection {
        ProjectionOutcome::Rejected { reason } => {
            let mut record = ProgramRecord::new(row.name.clone());
            record.record(
                Stage::Projection,
                Outcome::Failed {
                    reason: reason.clone(),
                },
            );
            write_json(&output, &record)?;
        }
        ProjectionOutcome::Projected { artifact, .. } => {
            let mut record = ProgramRecord::new(row.name.clone());
            record.record(Stage::Projection, Outcome::Passed);
            record.record(Stage::Validation, Outcome::Running);
            persist_or_exit(&output, &record);
            let artifact_path = manifest_artifact_path(&manifest_path, artifact)?;
            let bytes = match fs::read(artifact_path) {
                Ok(bytes) => bytes,
                Err(error) => return fail_active_stage(&output, &mut record, error),
            };
            let expectations: Expectations = match read_json(&expectations_path) {
                Ok(expectations) => expectations,
                Err(error) => return fail_active_stage(&output, &mut record, error),
            };
            let metadata = match fs::read(metadata_path) {
                Ok(metadata) => metadata,
                Err(error) => return fail_active_stage(&output, &mut record, error),
            };
            let constructors = match read_metadata(&metadata) {
                Ok((constructors, _)) => constructors,
                Err(error) => return fail_active_stage(&output, &mut record, error),
            };
            let requirements =
                match tidepool_toolchain::prepared_artifact::production_requirements() {
                    Ok(requirements) => requirements,
                    Err(error) => return fail_active_stage(&output, &mut record, error),
                };
            let expected = expected_for(row, &expectations);
            let scope = OracleScope::of(row.expectation_key.as_deref(), &expectations);
            let persist = |driver_record: &ProgramRecord| {
                merge_driver_record(&mut record, driver_record);
                persist_or_exit(&output, &record);
            };
            let _record = tidepool_prepared_corpus::run_prepared_artifact(
                row.name.clone(),
                &bytes,
                &requirements,
                expected,
                scope,
                &constructors,
                persist,
            );
        }
    }
    Ok(())
}

fn fail_active_stage<E: std::fmt::Display>(
    output: &Path,
    record: &mut ProgramRecord,
    error: E,
) -> Result<(), Box<dyn Error>> {
    let reason = error.to_string();
    eprintln!(
        "prepared-corpus {} validation failed: {reason}",
        record.name
    );
    record.record(Stage::Validation, Outcome::Failed { reason });
    write_json(output, record)
}

/// The manifest is the authority that GHC projected this row. The artifact
/// driver owns decoding and later stages, so map its decode failure onto the
/// already-running validation stage instead of erasing manifest evidence.
fn merge_driver_record(record: &mut ProgramRecord, driver_record: &ProgramRecord) {
    for driver_stage in &driver_record.stages {
        match (&driver_stage.stage, &driver_stage.outcome) {
            (Stage::Projection, Outcome::Failed { reason }) => record.record(
                Stage::Validation,
                Outcome::Failed {
                    reason: reason.clone(),
                },
            ),
            (Stage::Projection, _) => {}
            (stage, outcome) => record.record(*stage, outcome.clone()),
        }
    }
}

fn read_manifest(path: &Path) -> Result<ProjectionManifest, Box<dyn Error>> {
    read_json(path)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, Box<dyn Error>> {
    let bytes = fs::read(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn validate_manifest(manifest: &ProjectionManifest) -> Result<ManifestSummary, Box<dyn Error>> {
    if manifest.version != REPORT_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "unsupported projection manifest version {}",
                manifest.version
            ),
        )
        .into());
    }
    let mut names = BTreeSet::new();
    let mut expectation_keys = BTreeSet::new();
    for row in &manifest.programs {
        if row.name.is_empty() || !names.insert(row.name.clone()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "projection manifest has a duplicate or empty name {:?}",
                    row.name
                ),
            )
            .into());
        }
        match &row.projection {
            ProjectionOutcome::Projected { artifact, identity } => {
                validate_relative_artifact(artifact)?;
                if canonical_identity(identity) != row.name {
                    return Err(invalid_manifest(format!(
                        "program name {:?} is not the canonical identity of {:?}",
                        row.name, identity
                    )));
                }
                validate_expectation_key(row, identity, &mut expectation_keys)?;
            }
            ProjectionOutcome::Rejected { .. } => {
                if row.expectation_key.is_some() {
                    return Err(invalid_manifest(format!(
                        "rejected program {:?} cannot carry an oracle key",
                        row.name
                    )));
                }
            }
        }
    }
    let mut source_names = BTreeSet::new();
    let mut source_mapped = 0;
    let mut source_unmapped = 0;
    for target in &manifest.source_targets {
        if target.source_name.is_empty() || !source_names.insert(target.source_name.clone()) {
            return Err(invalid_manifest(format!(
                "source target names must be unique and non-empty: {:?}",
                target.source_name
            )));
        }
        match &target.identity {
            None => source_unmapped += 1,
            Some(identity) => {
                if !is_external_identity(identity) {
                    return Err(invalid_manifest(format!(
                        "source target {:?} maps to an internal identity {:?}",
                        target.source_name, identity
                    )));
                }
                if target.source_name != identity.occurrence {
                    return Err(invalid_manifest(format!(
                        "source target {:?} is not the exact external occurrence {:?}",
                        target.source_name, identity.occurrence
                    )));
                }
                let canonical = canonical_identity(identity);
                if !names.contains(&canonical) {
                    return Err(invalid_manifest(format!(
                        "source target {:?} maps to missing program {:?}",
                        target.source_name, canonical
                    )));
                }
                source_mapped += 1;
            }
        }
    }
    Ok(ManifestSummary {
        source_mapped,
        source_unmapped,
    })
}

fn invalid_manifest(message: String) -> Box<dyn Error> {
    io::Error::new(io::ErrorKind::InvalidData, message).into()
}

fn canonical_identity(identity: &tidepool_prepared_corpus::SourceIdentity) -> String {
    match &identity.record_parent {
        None => format!(
            "{}:{}:{}:{}",
            identity.unit, identity.module, identity.namespace, identity.occurrence
        ),
        Some(parent) => format!(
            "{}:{}:{}:{}:{}",
            identity.unit, identity.module, identity.namespace, parent, identity.occurrence
        ),
    }
}

fn is_external_identity(identity: &tidepool_prepared_corpus::SourceIdentity) -> bool {
    identity.namespace == "value"
        && !(identity.unit == "<interactive>" && identity.module == "<local>")
}

fn validate_expectation_key(
    row: &tidepool_prepared_corpus::ProjectionRecord,
    identity: &tidepool_prepared_corpus::SourceIdentity,
    keys: &mut BTreeSet<String>,
) -> Result<(), Box<dyn Error>> {
    match row.expectation_key.as_deref() {
        None => Ok(()),
        Some(key)
            if is_external_identity(identity)
                && key == identity.occurrence
                && keys.insert(key.to_owned()) =>
        {
            Ok(())
        }
        Some(key) if !is_external_identity(identity) => Err(invalid_manifest(format!(
            "internal program {:?} cannot carry oracle key {:?}",
            row.name, key
        ))),
        Some(key) if key != identity.occurrence => Err(invalid_manifest(format!(
            "oracle key {:?} is not the exact external occurrence {:?}",
            key, identity.occurrence
        ))),
        Some(key) => Err(invalid_manifest(format!(
            "oracle key {:?} is ambiguous",
            key
        ))),
    }
}

/// A generated oracle names manifest rows. Every source top and expectation
/// key must be a manifest expectation key, and a compiler-introduced key may
/// carry only a cyclic-observation classification, so a renamed simplifier
/// float or a stale oracle fails the run instead of becoming `no_oracle`.
fn validate_oracle_domain(
    manifest: &ProjectionManifest,
    expectations: &Expectations,
) -> Result<(), Box<dyn Error>> {
    let Some(source_tops) = &expectations.source_tops else {
        return Ok(());
    };
    let keys: BTreeSet<&str> = manifest
        .programs
        .iter()
        .filter_map(|row| row.expectation_key.as_deref())
        .collect();
    if let Some(top) = source_tops.iter().find(|top| !keys.contains(top.as_str())) {
        return Err(invalid_manifest(format!(
            "oracle source top {top:?} is not a manifest expectation key"
        )));
    }
    for (key, expectation) in &expectations.expectations {
        if !keys.contains(key.as_str()) {
            return Err(invalid_manifest(format!(
                "oracle expectation {key:?} is not a manifest expectation key"
            )));
        }
        if !source_tops.contains(key) && !matches!(expectation, Expectation::CyclicObservation) {
            return Err(invalid_manifest(format!(
                "compiler-introduced key {key:?} cannot carry a value oracle"
            )));
        }
    }
    Ok(())
}

fn expected_for<'a>(
    row: &tidepool_prepared_corpus::ProjectionRecord,
    expectations: &'a Expectations,
) -> Option<&'a tidepool_prepared_corpus::Expectation> {
    row.expectation_key
        .as_deref()
        .and_then(|key| expectations.expectations.get(key))
}

fn validate_relative_artifact(artifact: &str) -> Result<(), Box<dyn Error>> {
    let path = Path::new(artifact);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("projected artifact path must be a relative file path: {artifact:?}"),
        )
        .into());
    }
    Ok(())
}

fn manifest_artifact_path(manifest: &Path, artifact: &str) -> Result<PathBuf, Box<dyn Error>> {
    validate_relative_artifact(artifact)?;
    let parent = manifest.parent().unwrap_or_else(|| Path::new("."));
    Ok(parent.join(artifact))
}

fn child_report_path(output: &Path, index: usize) -> PathBuf {
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    let name = output.file_name().unwrap_or_default().to_string_lossy();
    parent.join(format!(".{name}.child-{index}.json"))
}

fn remove_stale_child_report(path: &Path) -> Result<(), Box<dyn Error>> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn read_child_report(path: &Path, name: &str, status: ExitStatus) -> ProgramRecord {
    match read_json(path) {
        Ok(record) if valid_child_record(&record, name) => record,
        Ok(_) => failed_without_report(
            name,
            format!("child exited {status} with a report for a different or incomplete row"),
        ),
        Err(error) => failed_without_report(
            name,
            format!("child exited {status} without a readable report: {error}"),
        ),
    }
}

/// A parsed report already lists every stage once in order (`StageRecords`
/// rejects anything else at parse time), so only the row identity is checked.
fn valid_child_record(record: &ProgramRecord, name: &str) -> bool {
    record.name == name
}

fn failed_without_report(name: &str, reason: String) -> ProgramRecord {
    let mut record = ProgramRecord::new(name.to_owned());
    record.record(Stage::Projection, Outcome::Failed { reason });
    record
}

fn fail_abnormal(record: &mut ProgramRecord, reason: String) {
    let mut failed_running_stage = false;
    for stage in &mut record.stages {
        if matches!(stage.outcome, Outcome::Running) {
            stage.outcome = Outcome::Failed {
                reason: reason.clone(),
            };
            failed_running_stage = true;
        }
    }
    if failed_running_stage {
        return;
    }

    let native_was_reached = !matches!(
        record.stages.get(Stage::Execution).outcome,
        Outcome::NotReached
    );
    if native_was_reached {
        let execution = record.stages.get_mut(Stage::Execution);
        if !matches!(execution.outcome, Outcome::Failed { .. }) {
            execution.outcome = Outcome::Failed { reason };
        }
        record.record(Stage::Comparison, Outcome::NotReached);
        return;
    }

    if let Some(stage) = record
        .stages
        .iter_mut()
        .find(|stage| matches!(stage.outcome, Outcome::NotReached))
    {
        stage.outcome = Outcome::Failed { reason };
    }
}

fn stage_totals(programs: &[ProgramRecord]) -> Vec<StageTotal> {
    Stage::ALL
        .into_iter()
        .map(|stage| {
            let mut total = StageTotal {
                stage,
                passed: 0,
                failed: 0,
                not_closed: 0,
                no_finite_observation: 0,
                function_valued: 0,
                missing_expectation: 0,
                no_oracle: 0,
                running: 0,
                not_reached: 0,
            };
            for record in programs {
                let outcome = &record.stages.get(stage).outcome;
                match outcome {
                    Outcome::Passed => total.passed += 1,
                    Outcome::Failed { .. } => total.failed += 1,
                    Outcome::Classified { class, .. } => match class {
                        Classification::NotClosed => total.not_closed += 1,
                        Classification::NoFiniteObservation => total.no_finite_observation += 1,
                        Classification::FunctionValued => total.function_valued += 1,
                    },
                    Outcome::MissingExpectation => total.missing_expectation += 1,
                    Outcome::NoOracle => total.no_oracle += 1,
                    Outcome::Running => total.running += 1,
                    Outcome::NotReached => total.not_reached += 1,
                }
            }
            total
        })
        .collect()
}

fn persist_or_exit(path: &Path, record: &ProgramRecord) {
    if let Err(error) = write_json(path, record) {
        eprintln!("prepared-corpus could not persist {}: {error}", record.name);
        std::process::exit(2);
    }
}

fn write_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), Box<dyn Error>> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let bytes = serde_json::to_vec_pretty(value)?;
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    fs::write(&temporary, bytes)?;
    fs::rename(temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tidepool_prepared_corpus::Expectation;

    fn temporary_path(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock is after the Unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "prepared-corpus-{label}-{}-{nanos}.json",
            std::process::id()
        ))
    }

    fn rejected_manifest() -> String {
        r#"{"version":2,"source_targets":[],"programs":[{"name":"rejected","status":"rejected","reason":"projection is unsupported"}]}"#.into()
    }

    #[test]
    fn manifest_requires_version_unique_names_and_relative_artifacts() {
        let duplicate: ProjectionManifest = serde_json::from_str(
            r#"{"version":2,"source_targets":[],"programs":[{"name":"x","status":"rejected","reason":"no"},{"name":"x","status":"rejected","reason":"no"}]}"#,
        )
        .unwrap();
        assert!(validate_manifest(&duplicate).is_err());
        let absolute: ProjectionManifest = serde_json::from_str(
            r#"{"version":2,"source_targets":[],"programs":[{"name":"u:M:value:x","status":"projected","artifact":"/tmp/x.cbor","identity":{"unit":"u","module":"M","namespace":"value","occurrence":"x"}}]}"#,
        )
        .unwrap();
        assert!(validate_manifest(&absolute).is_err());
        let parent: ProjectionManifest = serde_json::from_str(
            r#"{"version":2,"source_targets":[],"programs":[{"name":"u:M:value:x","status":"projected","artifact":"../x.cbor","identity":{"unit":"u","module":"M","namespace":"value","occurrence":"x"}}]}"#,
        )
        .unwrap();
        assert!(validate_manifest(&parent).is_err());
    }

    #[test]
    fn manifest_keeps_source_mapping_separate_from_stg_rows() {
        let manifest: ProjectionManifest = serde_json::from_str(
            r#"{
                "version":2,
                "source_targets":[
                    {"source_name":"value","identity":{"unit":"u","module":"M","namespace":"value","occurrence":"value"}},
                    {"source_name":"unmapped_local","identity":null}
                ],
                "programs":[
                    {"name":"u:M:value:value","expectation_key":"value","status":"projected","artifact":"0.prepared.cbor","identity":{"unit":"u","module":"M","namespace":"value","occurrence":"value"}},
                    {"name":"u:M:value:ffi","status":"rejected","reason":"unsupported"}
                ]
            }"#,
        )
        .unwrap();
        let summary = validate_manifest(&manifest).unwrap();
        assert_eq!(summary.source_mapped, 1);
        assert_eq!(summary.source_unmapped, 1);
        assert_eq!(manifest.programs.len(), 2);
    }

    #[test]
    fn manifest_rejects_internal_or_ambiguous_oracle_keys() {
        let internal: ProjectionManifest = serde_json::from_str(
            r#"{"version":2,"source_targets":[],"programs":[{"name":"u:M:local:x","expectation_key":"x","status":"projected","artifact":"x","identity":{"unit":"u","module":"M","namespace":"local","occurrence":"x"}}]}"#,
        )
        .unwrap();
        assert!(validate_manifest(&internal).is_err());

        let ambiguous: ProjectionManifest = serde_json::from_str(
            r#"{"version":2,"source_targets":[],"programs":[
                {"name":"u:One:value:x","expectation_key":"x","status":"projected","artifact":"one","identity":{"unit":"u","module":"One","namespace":"value","occurrence":"x"}},
                {"name":"u:Two:value:x","expectation_key":"x","status":"projected","artifact":"two","identity":{"unit":"u","module":"Two","namespace":"value","occurrence":"x"}}
            ]}"#,
        )
        .unwrap();
        assert!(validate_manifest(&ambiguous).is_err());
    }

    #[test]
    fn manifest_rejects_source_identity_not_in_authoritative_rows() {
        let manifest: ProjectionManifest = serde_json::from_str(
            r#"{"version":2,"source_targets":[{"source_name":"missing","identity":{"unit":"u","module":"M","namespace":"value","occurrence":"missing"}}],"programs":[{"name":"u:M:value:present","status":"rejected","reason":"unsupported"}]}"#,
        )
        .unwrap();
        assert!(validate_manifest(&manifest).is_err());
    }

    #[test]
    fn manifest_rejects_source_suffix_aliases() {
        let manifest: ProjectionManifest = serde_json::from_str(
            r#"{"version":2,"source_targets":[{"source_name":"value_t123","identity":{"unit":"u","module":"M","namespace":"value","occurrence":"value"}}],"programs":[{"name":"u:M:value:value","status":"rejected","reason":"unsupported"}]}"#,
        )
        .unwrap();
        assert!(validate_manifest(&manifest).is_err());
    }

    #[test]
    fn oracle_lookup_uses_only_the_explicit_expectation_key() {
        let row: tidepool_prepared_corpus::ProjectionRecord = serde_json::from_str(
            r#"{"name":"u:M:value:canonical","expectation_key":"historical","status":"rejected","reason":"not run"}"#,
        )
        .unwrap();
        let expectations = Expectations {
            source_revision: "test".into(),
            source_tops: None,
            expectations: [("historical".into(), Expectation::Int(7))]
                .into_iter()
                .collect(),
        };
        assert!(matches!(
            expected_for(&row, &expectations),
            Some(Expectation::Int(7))
        ));
    }

    #[test]
    fn oracle_domain_must_name_manifest_keys_and_keep_values_on_source_tops() {
        let manifest: ProjectionManifest = serde_json::from_str(
            r#"{"version":2,"source_targets":[],"programs":[
                {"name":"u:M:value:t_swap","expectation_key":"t_swap","status":"projected","artifact":"0","identity":{"unit":"u","module":"M","namespace":"value","occurrence":"t_swap"}},
                {"name":"u:M:value:t_swap1","expectation_key":"t_swap1","status":"projected","artifact":"1","identity":{"unit":"u","module":"M","namespace":"value","occurrence":"t_swap1"}}
            ]}"#,
        )
        .unwrap();
        let oracle = |tops: &[&str], entries: Vec<(&str, Expectation)>| Expectations {
            source_revision: "test".into(),
            source_tops: Some(tops.iter().map(|top| top.to_string()).collect()),
            expectations: entries
                .into_iter()
                .map(|(key, expectation)| (key.to_string(), expectation))
                .collect(),
        };
        assert!(validate_oracle_domain(
            &manifest,
            &oracle(
                &["t_swap"],
                vec![
                    ("t_swap", Expectation::Int(1)),
                    ("t_swap1", Expectation::CyclicObservation)
                ]
            )
        )
        .is_ok());
        assert!(validate_oracle_domain(&manifest, &oracle(&["renamed"], vec![])).is_err());
        assert!(validate_oracle_domain(
            &manifest,
            &oracle(&["t_swap"], vec![("t_swap1", Expectation::Int(1))])
        )
        .is_err());
        assert!(validate_oracle_domain(
            &manifest,
            &oracle(&["t_swap"], vec![("gone", Expectation::CyclicObservation)])
        )
        .is_err());
    }

    #[test]
    fn canonical_identity_preserves_source_keys_and_distinguishes_record_fields() {
        let no_parent = tidepool_prepared_corpus::SourceIdentity {
            unit: "u".into(),
            module: "M".into(),
            namespace: "value".into(),
            occurrence: "field".into(),
            record_parent: None,
        };
        let parent = tidepool_prepared_corpus::SourceIdentity {
            record_parent: Some("Record".into()),
            ..no_parent.clone()
        };
        assert_eq!(canonical_identity(&no_parent), "u:M:value:field");
        assert_eq!(canonical_identity(&parent), "u:M:value:Record:field");
        assert_ne!(canonical_identity(&no_parent), canonical_identity(&parent));
    }

    #[test]
    fn cli_accepts_only_typed_command_shapes() {
        assert!(parse_values(vec![OsString::from("effects-core")]).is_err());
        assert!(matches!(
            parse_values(vec![
                OsString::from("effects-core"),
                OsString::from("generated")
            ])
            .unwrap(),
            Command::EffectsCore { .. }
        ));
        let run = parse_values(
            [
                "run",
                "manifest.json",
                "expectations.json",
                "metadata.cbor",
                "out.json",
            ]
            .into_iter()
            .map(OsString::from)
            .collect(),
        )
        .unwrap();
        assert!(matches!(run, Command::Run { .. }));
        let audit = parse_values(
            ["audit-operations", "manifest.json", "audit.json"]
                .into_iter()
                .map(OsString::from)
                .collect(),
        )
        .unwrap();
        assert!(matches!(audit, Command::AuditOperations { .. }));
        let child = parse_values(
            [
                "child",
                "manifest.json",
                "expectations.json",
                "metadata.cbor",
                "out.json",
                "7",
            ]
            .into_iter()
            .map(OsString::from)
            .collect(),
        )
        .unwrap();
        assert!(matches!(child, Command::Child { index: 7, .. }));
        assert!(parse_values(vec![OsString::from("run")]).is_err());
        assert!(parse_values(
            [
                "child",
                "manifest.json",
                "expectations.json",
                "metadata.cbor",
                "out.json",
                "not-an-index",
            ]
            .into_iter()
            .map(OsString::from)
            .collect(),
        )
        .is_err());
    }

    #[test]
    fn operation_audit_groups_exact_pairs_and_classifies_with_native_catalog() {
        use tidepool_repr::execution_schema::{
            testing, OperationDecl, OperationIdentity, ResultContract, RuntimeRep, Signature,
            SignatureId,
        };

        let mut wire = testing::wire_program();
        let signature = Signature {
            arguments: vec![RuntimeRep::Int(64), RuntimeRep::Int(64)],
            results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
        };
        wire.signatures.push(signature.clone());
        wire.operations.extend([
            OperationDecl {
                identity: OperationIdentity::PrimOp("+#".into()),
                signature: SignatureId(1),
            },
            OperationDecl {
                identity: OperationIdentity::PrimOp("notARealPrimOp#".into()),
                signature: SignatureId(1),
            },
        ]);
        let program = testing::prepare(wire).unwrap();
        let mut operations = Vec::new();
        record_operations(&mut operations, "first", &program);
        record_operations(&mut operations, "first", &program);
        record_operations(&mut operations, "second", &program);

        assert_eq!(operations.len(), 2);
        assert!(operations[0].supported);
        assert!(!operations[1].supported);
        assert_eq!(
            operations[0].identity,
            OperationIdentity::PrimOp("+#".into())
        );
        assert_eq!(operations[0].signature, signature);
        assert_eq!(operations[0].programs, ["first", "second"]);
        assert_eq!(operations[1].programs, ["first", "second"]);
    }

    #[test]
    fn operation_audit_reports_projection_and_decode_omissions() {
        let manifest = temporary_path("audit-manifest");
        let output = temporary_path("audit-output");
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock is after the Unix epoch")
            .as_nanos();
        let artifact_name = format!(
            "prepared-corpus-audit-artifact-{}-{nonce}.cbor",
            std::process::id()
        );
        let invalid_name = format!(
            "prepared-corpus-audit-invalid-{}-{nonce}.cbor",
            std::process::id()
        );
        let parent = manifest.parent().unwrap();
        let artifact = parent.join(&artifact_name);
        let invalid = parent.join(&invalid_name);
        fs::write(
            &artifact,
            include_bytes!(
                "../../../../bridge/haskell/test-prepared-stg/fixtures/m3-vertical.cbor"
            ),
        )
        .unwrap();
        fs::write(&invalid, b"not cbor").unwrap();
        fs::write(
            &manifest,
            serde_json::to_vec(&serde_json::json!({
                "version": 2,
                "source_targets": [],
                "programs": [
                    {
                        "name": "u:M:value:decoded",
                        "status": "projected",
                        "artifact": artifact_name,
                        "identity": {
                            "unit": "u", "module": "M", "namespace": "value",
                            "occurrence": "decoded"
                        }
                    },
                    {
                        "name": "u:M:value:invalid",
                        "status": "projected",
                        "artifact": invalid_name,
                        "identity": {
                            "unit": "u", "module": "M", "namespace": "value",
                            "occurrence": "invalid"
                        }
                    },
                    {
                        "name": "u:M:value:rejected",
                        "status": "rejected",
                        "reason": "projection unsupported"
                    }
                ]
            }))
            .unwrap(),
        )
        .unwrap();

        audit_operations(manifest.clone(), output.clone()).unwrap();
        let report: serde_json::Value = read_json(&output).unwrap();
        assert_eq!(report["manifest_programs"], 3);
        assert_eq!(report["projected_programs"], 2);
        assert_eq!(report["decoded_programs"], 1);
        assert_eq!(report["projection_omissions"].as_array().unwrap().len(), 1);
        assert_eq!(report["decode_omissions"].as_array().unwrap().len(), 1);
        let operations = report["operations"].as_array().unwrap();
        let double_to_int = operations
            .iter()
            .find(|operation| operation["identity"] == "PrimOp(\"double2Int#\")")
            .expect("the current cross-language fixture retains double2Int#");
        assert_eq!(
            double_to_int["signature"],
            "Signature { arguments: [Float(64)], results: Returns([Int(64)]) }"
        );
        assert_eq!(double_to_int["supported"], true);
        assert_eq!(
            double_to_int["programs"],
            serde_json::json!(["u:M:value:decoded"])
        );

        fs::remove_file(manifest).unwrap();
        fs::remove_file(output).unwrap();
        fs::remove_file(artifact).unwrap();
        fs::remove_file(invalid).unwrap();
    }

    #[test]
    fn rejected_row_writes_a_stage_record_without_loading_or_executing_an_artifact() {
        let manifest = temporary_path("manifest");
        let output = temporary_path("child-report");
        fs::write(&manifest, rejected_manifest()).unwrap();
        run_one(
            manifest.clone(),
            PathBuf::from("does-not-need-expectations.json"),
            PathBuf::from("does-not-need-metadata.cbor"),
            output.clone(),
            0,
        )
        .unwrap();
        let record: ProgramRecord = read_json(&output).unwrap();
        assert_eq!(record.name, "rejected");
        assert!(matches!(record.stages[0].outcome, Outcome::Failed { .. }));
        assert!(record.stages[1..]
            .iter()
            .all(|stage| matches!(stage.outcome, Outcome::NotReached)));
        fs::remove_file(manifest).unwrap();
        fs::remove_file(output).unwrap();
    }

    #[test]
    fn projected_row_preserves_manifest_projection_evidence_on_pre_driver_failure() {
        let manifest = temporary_path("projected-manifest");
        let output = temporary_path("projected-report");
        fs::write(
            &manifest,
            r#"{"version":2,"source_targets":[],"programs":[{"name":"u:M:value:projected","status":"projected","artifact":"missing.prepared.cbor","identity":{"unit":"u","module":"M","namespace":"value","occurrence":"projected"}}]}"#,
        )
        .unwrap();
        run_one(
            manifest.clone(),
            PathBuf::from("does-not-need-expectations.json"),
            PathBuf::from("does-not-need-metadata.cbor"),
            output.clone(),
            0,
        )
        .unwrap();
        let record: ProgramRecord = read_json(&output).unwrap();
        assert!(matches!(record.stages[0].outcome, Outcome::Passed));
        assert!(matches!(record.stages[1].outcome, Outcome::Failed { .. }));
        fs::remove_file(manifest).unwrap();
        fs::remove_file(output).unwrap();
    }

    #[test]
    fn subprocess_parent_retains_a_projection_rejection_when_the_child_aborts() {
        let manifest = temporary_path("subprocess-manifest");
        let output = temporary_path("subprocess-report");
        fs::write(&manifest, rejected_manifest()).unwrap();
        run_corpus_with(
            Path::new("/usr/bin/false"),
            manifest.clone(),
            PathBuf::from("does-not-need-expectations.json"),
            PathBuf::from("does-not-need-metadata.cbor"),
            output.clone(),
        )
        .unwrap();
        let report: serde_json::Value = read_json(&output).unwrap();
        assert_eq!(report["programs"].as_array().unwrap().len(), 1);
        assert_eq!(report["programs"][0]["name"], "rejected");
        assert_eq!(
            report["programs"][0]["stages"][0]["outcome"]["status"],
            "failed"
        );
        fs::remove_file(&manifest).unwrap();
        fs::remove_file(&output).unwrap();
        let child_output = child_report_path(&output, 0);
        if child_output.exists() {
            fs::remove_file(child_output).unwrap();
        }
    }

    #[test]
    fn abnormal_child_preserves_its_last_running_stage_as_a_failure() {
        let mut record = ProgramRecord::new("rejected".into());
        record.record(Stage::Projection, Outcome::Passed);
        record.record(Stage::Validation, Outcome::Running);
        fail_abnormal(&mut record, "child exited abnormally: signal 6".into());
        assert!(matches!(record.stages[0].outcome, Outcome::Passed));
        assert!(matches!(record.stages[1].outcome, Outcome::Failed { .. }));
    }

    #[test]
    fn abnormal_child_after_classification_is_an_execution_failure() {
        for class in [
            Classification::NotClosed,
            Classification::NoFiniteObservation,
            Classification::FunctionValued,
        ] {
            let mut record = ProgramRecord::new("classified".into());
            record.record(
                Stage::Execution,
                Outcome::Classified {
                    class,
                    reason: "typed classification before cleanup".into(),
                },
            );
            fail_abnormal(&mut record, "child exited abnormally: signal 6".into());
            assert!(matches!(
                &record.stages[4].outcome,
                Outcome::Failed { reason } if reason.contains("signal 6")
            ));
            assert!(matches!(record.stages[5].outcome, Outcome::NotReached));
        }
    }

    #[test]
    fn abnormal_child_after_native_execution_cannot_leave_an_all_passed_row() {
        let mut record = ProgramRecord::new("completed".into());
        for stage in [
            Stage::Projection,
            Stage::Validation,
            Stage::Admission,
            Stage::Compilation,
            Stage::Execution,
            Stage::Comparison,
        ] {
            record.record(stage, Outcome::Passed);
        }
        fail_abnormal(&mut record, "child exited abnormally: signal 6".into());
        assert!(matches!(record.stages[4].outcome, Outcome::Failed { .. }));
        assert!(matches!(record.stages[5].outcome, Outcome::NotReached));
    }
}
