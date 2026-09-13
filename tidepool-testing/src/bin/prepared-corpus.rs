//! Per-program process isolation for the STG corpus. A native crash or watchdog
//! expiry is evidence for one row, never permission to omit the remaining rows.

use std::collections::BTreeSet;
use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::{Command as ProcessCommand, ExitStatus};

use tidepool_repr::serial::read_metadata;
use tidepool_testing::prepared_corpus::{
    Expectations, Outcome, ProgramRecord, ProjectionManifest, ProjectionOutcome, Stage,
};

const REPORT_VERSION: u32 = 1;

/// Arguments are paths owned by the corpus verification recipe. The child
/// receives a manifest index, never a command string derived from a program.
enum Command {
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
    programs: Vec<ProgramRecord>,
    stage_totals: Vec<StageTotal>,
}

#[derive(serde::Serialize)]
struct StageTotal {
    stage: Stage,
    passed: usize,
    failed: usize,
    missing_expectation: usize,
    running: usize,
    not_reached: usize,
}

fn main() -> Result<(), Box<dyn Error>> {
    let command = parse_arguments()?;
    match command {
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
        "usage: prepared-corpus run MANIFEST EXPECTATIONS METADATA OUTPUT | prepared-corpus child MANIFEST EXPECTATIONS METADATA OUTPUT INDEX",
    )
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
    validate_manifest(&manifest)?;

    let mut programs = Vec::with_capacity(manifest.programs.len());
    for (index, row) in manifest.programs.iter().enumerate() {
        let child_output = child_report_path(&output, index);
        remove_stale_child_report(&child_output)?;
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
    tidepool_testing::watchdog::arm();
    let _watchdog = tidepool_testing::watchdog::begin(&row.name);

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
            let expected = expectations.expectations.get(&row.name);
            let persist = |driver_record: &ProgramRecord| {
                merge_driver_record(&mut record, driver_record);
                persist_or_exit(&output, &record);
            };
            let _record = tidepool_testing::prepared_corpus::run_prepared_artifact(
                row.name.clone(),
                &bytes,
                &requirements,
                expected,
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
            (stage, outcome) => record.record(*stage, clone_outcome(outcome)),
        }
    }
}

fn clone_outcome(outcome: &Outcome) -> Outcome {
    match outcome {
        Outcome::Running => Outcome::Running,
        Outcome::Passed => Outcome::Passed,
        Outcome::Failed { reason } => Outcome::Failed {
            reason: reason.clone(),
        },
        Outcome::MissingExpectation => Outcome::MissingExpectation,
        Outcome::NotReached => Outcome::NotReached,
    }
}

fn read_manifest(path: &Path) -> Result<ProjectionManifest, Box<dyn Error>> {
    read_json(path)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, Box<dyn Error>> {
    let bytes = fs::read(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn validate_manifest(manifest: &ProjectionManifest) -> Result<(), Box<dyn Error>> {
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
    for row in &manifest.programs {
        if row.name.is_empty() || !names.insert(&row.name) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "projection manifest has a duplicate or empty name {:?}",
                    row.name
                ),
            )
            .into());
        }
        if let ProjectionOutcome::Projected { artifact, .. } = &row.projection {
            validate_relative_artifact(artifact)?;
        }
    }
    Ok(())
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

fn valid_child_record(record: &ProgramRecord, name: &str) -> bool {
    record.name == name
        && [
            Stage::Projection,
            Stage::Validation,
            Stage::Admission,
            Stage::Compilation,
            Stage::Execution,
            Stage::Comparison,
        ]
        .into_iter()
        .all(|stage| {
            record
                .stages
                .iter()
                .filter(|entry| entry.stage == stage)
                .count()
                == 1
        })
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

    let native_was_reached = record
        .stages
        .iter()
        .find(|stage| stage.stage == Stage::Execution)
        .is_some_and(|stage| !matches!(stage.outcome, Outcome::NotReached));
    if native_was_reached {
        let execution = record
            .stages
            .iter_mut()
            .find(|stage| stage.stage == Stage::Execution)
            .expect("ProgramRecord always has an execution stage");
        if matches!(execution.outcome, Outcome::Passed) {
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
    [
        Stage::Projection,
        Stage::Validation,
        Stage::Admission,
        Stage::Compilation,
        Stage::Execution,
        Stage::Comparison,
    ]
    .into_iter()
    .map(|stage| {
        let mut total = StageTotal {
            stage,
            passed: 0,
            failed: 0,
            missing_expectation: 0,
            running: 0,
            not_reached: 0,
        };
        for record in programs {
            let outcome = record
                .stages
                .iter()
                .find(|entry| entry.stage == stage)
                .map(|entry| &entry.outcome)
                .expect("ProgramRecord always has the fixed stage list");
            match outcome {
                Outcome::Passed => total.passed += 1,
                Outcome::Failed { .. } => total.failed += 1,
                Outcome::MissingExpectation => total.missing_expectation += 1,
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
        r#"{"version":1,"programs":[{"name":"rejected","status":"rejected","reason":"projection is unsupported"}]}"#.into()
    }

    #[test]
    fn manifest_requires_version_unique_names_and_relative_artifacts() {
        let duplicate: ProjectionManifest = serde_json::from_str(
            r#"{"version":1,"programs":[{"name":"x","status":"rejected","reason":"no"},{"name":"x","status":"rejected","reason":"no"}]}"#,
        )
        .unwrap();
        assert!(validate_manifest(&duplicate).is_err());
        let absolute: ProjectionManifest = serde_json::from_str(
            r#"{"version":1,"programs":[{"name":"x","status":"projected","artifact":"/tmp/x.cbor","identity":{"unit":"u","module":"M","namespace":"value","occurrence":"x"}}]}"#,
        )
        .unwrap();
        assert!(validate_manifest(&absolute).is_err());
        let parent: ProjectionManifest = serde_json::from_str(
            r#"{"version":1,"programs":[{"name":"x","status":"projected","artifact":"../x.cbor","identity":{"unit":"u","module":"M","namespace":"value","occurrence":"x"}}]}"#,
        )
        .unwrap();
        assert!(validate_manifest(&parent).is_err());
    }

    #[test]
    fn cli_accepts_only_typed_run_or_child_shapes() {
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
        assert!(
            parse_values(
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
            .is_err()
        );
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
        assert!(
            record.stages[1..]
                .iter()
                .all(|stage| matches!(stage.outcome, Outcome::NotReached))
        );
        fs::remove_file(manifest).unwrap();
        fs::remove_file(output).unwrap();
    }

    #[test]
    fn projected_row_preserves_manifest_projection_evidence_on_pre_driver_failure() {
        let manifest = temporary_path("projected-manifest");
        let output = temporary_path("projected-report");
        fs::write(
            &manifest,
            r#"{"version":1,"programs":[{"name":"projected","status":"projected","artifact":"missing.prepared.cbor","identity":{"unit":"u","module":"M","namespace":"value","occurrence":"projected"}}]}"#,
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
