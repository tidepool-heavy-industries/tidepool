//! Admission of explicitly selected integration-test compiler inputs.
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

pub fn required_executable(name: &str, value: Option<OsString>) -> Result<PathBuf, String> {
    use std::os::unix::fs::PermissionsExt;
    let path = value
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| {
            format!("{name} must select the matched integration-test compiler executable")
        })?;
    let metadata = std::fs::metadata(&path)
        .map_err(|error| format!("{name}={} is unavailable: {error}", path.display()))?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return Err(format!(
            "{name}={} is not an executable file",
            path.display()
        ));
    }
    Ok(path)
}

pub fn require_compiler_executables() -> PathBuf {
    let frontend = required_executable("TIDEPOOL_EXTRACT", std::env::var_os("TIDEPOOL_EXTRACT"))
        .unwrap_or_else(|error| panic!("compiler integration configuration: {error}"));
    required_executable(
        "TIDEPOOL_EXTRACT_WORKER",
        std::env::var_os("TIDEPOOL_EXTRACT_WORKER"),
    )
    .unwrap_or_else(|error| panic!("compiler integration configuration: {error}"));
    frontend
}

pub fn required_source(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let path = root.join(relative);
    if !path.is_file() {
        return Err(format!(
            "required compiler integration source is missing: {}",
            path.display()
        ));
    }
    Ok(path)
}

#[test]
fn missing_selected_worker_is_rejected_before_compiler_admission() {
    let failure = required_executable("TIDEPOOL_EXTRACT_WORKER", None).unwrap_err();
    assert!(
        failure.contains("TIDEPOOL_EXTRACT_WORKER must select"),
        "{failure}"
    );
}

#[test]
fn selected_nonexecutable_frontend_is_rejected_before_compiler_admission() {
    let scratch = tempfile::tempdir().unwrap();
    let path = scratch.path().join("frontend");
    std::fs::write(&path, "not an executable").unwrap();
    let failure = required_executable("TIDEPOOL_EXTRACT", Some(path.into_os_string())).unwrap_err();
    assert!(failure.contains("not an executable file"), "{failure}");
}

#[test]
fn missing_stdlib_source_is_rejected_before_compiler_admission() {
    let scratch = tempfile::tempdir().unwrap();
    let failure = required_source(scratch.path(), "Tidepool/Prelude.hs").unwrap_err();
    assert!(failure.contains("Tidepool/Prelude.hs"), "{failure}");
    let valid = scratch.path().join("Tidepool");
    std::fs::create_dir(&valid).unwrap();
    std::fs::write(valid.join("Prelude.hs"), "module Tidepool.Prelude where\n").unwrap();
    assert_eq!(
        required_source(scratch.path(), "Tidepool/Prelude.hs").unwrap(),
        valid.join("Prelude.hs")
    );
}
