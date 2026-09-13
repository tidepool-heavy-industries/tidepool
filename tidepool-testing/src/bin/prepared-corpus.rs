//! Per-program process isolation for the STG corpus. A native crash or watchdog
//! expiry is evidence for one row, never permission to omit the remaining rows.

use std::path::PathBuf;

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

fn main() -> Result<(), Box<dyn std::error::Error>> {
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

fn parse_arguments() -> Result<Command, Box<dyn std::error::Error>> {
    todo!("corpus:CLI — run MANIFEST EXPECTATIONS METADATA OUTPUT or child same paths INDEX")
}

fn run_corpus(
    _manifest: PathBuf,
    _expectations: PathBuf,
    _metadata: PathBuf,
    _output: PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    // Spawn the current executable in child mode per manifest row. Generated
    // artifacts resolve relative to the manifest, never relative to cwd.
    // Validate manifest version/unique names and relative artifact paths.
    // Every row stays in the denominator, including projection rejection and
    // missing expectations. On abnormal exit, preserve passed stages and turn
    // the Running stage into failure; absence of a report is explicit failure.
    // Write JSON rows plus stage totals; expected admission failures don't make
    // the reporting command fail, but invalid manifests/report I/O do.
    todo!("corpus:AGGREGATE")
}

fn run_one(
    _manifest: PathBuf,
    _expectations: PathBuf,
    _metadata: PathBuf,
    _output: PathBuf,
    _index: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    // Arm existing tidepool_testing::watchdog before processing a program.
    // Persist a ProgramRecord before each stage. Use the prepared_corpus owner
    // for validation/admission/compile/run/compare; never load/evaluate Core.
    // Metadata is data-only read_metadata; missing expectations are not passes.
    todo!("corpus:CHILD")
}
