use std::path::Path;
use std::process::ExitCode;

use tidepool_repr::execution_schema::DecodeLimits;
use tidepool_toolchain::prepared_artifact::PreparedArtifact;

fn check(path: &Path) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    PreparedArtifact::parse(bytes, DecodeLimits::default())
        .map(|_| ())
        .map_err(|error| format!("{}: {error}", path.display()))
}

fn main() -> ExitCode {
    let paths = std::env::args_os()
        .skip(1)
        .map(std::path::PathBuf::from)
        .collect::<Vec<_>>();
    if paths.is_empty() {
        eprintln!("usage: embedded-artifact-check PATH...");
        return ExitCode::from(2);
    }

    for path in &paths {
        if let Err(error) = check(path) {
            eprintln!("embedded prepared artifact rejected: {error}");
            return ExitCode::FAILURE;
        }
    }

    println!(
        "embedded prepared artifacts: {} accepted by the production contract",
        paths.len()
    );
    ExitCode::SUCCESS
}
