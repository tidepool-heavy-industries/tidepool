//! Opt-in, case-owned diagnostics that outlive a successful compiler transaction.
//! Copies are observations only: original paths and certificates are never restamped.

use std::io::{self, Read};
use std::path::{Path, PathBuf};

use tidepool_extract_cmd::ExtractCmd;

const BYTE_LIMIT: u64 = 128 << 20;
const ENTRY_LIMIT: usize = 4096;
const DEPTH_LIMIT: usize = 16;

/// Create compiler scratch with its ordinary production lifetime, or with the
/// isolated case's explicit diagnostic lifetime. Original compiler paths remain
/// unchanged through execution and hard process termination; only the runner
/// removes a diagnostic case after outcome and cleanup are both confirmed.
pub fn compiler_scratch_directory() -> io::Result<tempfile::TempDir> {
    match diagnostic_root()? {
        None => tempfile::TempDir::new(),
        Some(root) => {
            let transactions = root.join("compiler-transactions");
            std::fs::create_dir_all(&transactions)?;
            let mut directory = tempfile::TempDir::new_in(transactions)?;
            directory.disable_cleanup(true);
            Ok(directory)
        }
    }
}

fn diagnostic_root() -> io::Result<Option<PathBuf>> {
    if std::env::var("TIDEPOOL_TEST_DIAGNOSTIC_SCOPE").as_deref() != Ok("1") {
        return Ok(None);
    }
    let root = std::env::var_os("TIDEPOOL_TEST_ARTIFACT_ROOT")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other("diagnostic scope lacks its case root"))?;
    if !root.is_absolute() || !root.is_dir() {
        return Err(io::Error::other(
            "diagnostic case root must be an existing absolute directory",
        ));
    }
    Ok(Some(root))
}

/// Optional, bounded observations of a case-owned original compiler directory.
/// Ordinary production compilation performs no capture or additional copying.
/// Request evidence is written before compiler execution. Completed transactions
/// retain bounded consumed-source copies and a bounded hash inventory alongside
/// their original outputs before later native execution can fail. Compiler
/// output files are never copied, relocated, restamped, or used as diagnostics
/// to grant authority. Missing evidence remains explicit in diagnostic reports.
pub struct CompilerDiagnosticCapture {
    directory: Option<PathBuf>,
}

impl CompilerDiagnosticCapture {
    pub fn start(source: &Path, command: &ExtractCmd) -> Self {
        let directory = match diagnostic_root() {
            Ok(Some(root)) if source.starts_with(root.join("compiler-transactions")) => {
                Some(source.to_path_buf())
            }
            Ok(_) => None,
            Err(error) => {
                tracing::warn!(%error, "could not start compiler execution diagnostics");
                None
            }
        };
        let capture = Self { directory };
        capture.write(source, command, None);
        capture
    }

    pub fn completed(&self, source: &Path, command: &ExtractCmd, success: bool, stderr: &[u8]) {
        self.write(source, command, Some((success, stderr)));
    }

    fn write(&self, source: &Path, command: &ExtractCmd, result: Option<(bool, &[u8])>) {
        let Some(directory) = &self.directory else {
            return;
        };
        if let Err(error) = self.write_inner(directory, source, command, result) {
            tracing::warn!(%error, path = %directory.display(), "compiler execution diagnostics are incomplete");
        }
    }

    fn write_inner(
        &self,
        directory: &Path,
        source: &Path,
        command: &ExtractCmd,
        result: Option<(bool, &[u8])>,
    ) -> io::Result<()> {
        std::fs::write(
            directory.join("compiler-request.bin"),
            command.request_bytes(),
        )?;
        std::fs::write(
            directory.join("compiler-cwd.bin"),
            std::env::current_dir()?.as_os_str().as_encoded_bytes(),
        )?;
        let mut snapshot = Snapshot {
            remaining: BYTE_LIMIT,
            entries: ENTRY_LIMIT,
            files: Vec::new(),
            issues: Vec::new(),
        };
        if let Err(error) = snapshot.inspect(source, 0) {
            snapshot.issues.push(error.to_string());
        }
        if let Some((_, stderr)) = result {
            let limit = 4 << 20;
            std::fs::write(
                directory.join("compiler.stderr"),
                &stderr[..stderr.len().min(limit)],
            )?;
            if stderr.len() > limit {
                snapshot
                    .issues
                    .push("compiler stderr exceeds 4 MiB diagnostic bound".into());
            }
            if let Err(error) = super::failure_sources::retain(directory, snapshot.issues.clone()) {
                snapshot
                    .issues
                    .push(format!("consumed source diagnostics: {error}"));
            }
        }
        let report = serde_json::json!({
            "schema": 1, "kind": "compiler-execution-diagnostics", "authority": false,
            "original_directory": source, "phase": if result.is_some() { "compiler_completed" } else { "compiler_started" },
            "compiler_process_success": result.map(|(success, _)| success),
            "artifact_byte_limit": BYTE_LIMIT, "artifact_entry_limit": ENTRY_LIMIT,
            "artifact_bytes": BYTE_LIMIT - snapshot.remaining, "files": snapshot.files,
            "issues": snapshot.issues,
        });
        let pending = directory.join("transaction.json.pending");
        std::fs::write(&pending, serde_json::to_vec_pretty(&report)?)?;
        std::fs::rename(pending, directory.join("transaction.json"))
    }
}

struct Snapshot {
    remaining: u64,
    entries: usize,
    files: Vec<serde_json::Value>,
    issues: Vec<String>,
}

impl Snapshot {
    fn inspect(&mut self, source: &Path, depth: usize) -> io::Result<()> {
        if depth > DEPTH_LIMIT {
            return Err(io::Error::other(
                "compiler diagnostics exceed directory depth bound",
            ));
        }
        let mut children = std::fs::read_dir(source)?
            .take(self.entries + 1)
            .collect::<Result<Vec<_>, _>>()?;
        if children.len() > self.entries {
            return Err(io::Error::other("compiler diagnostics exceed entry bound"));
        }
        children.sort_by_key(std::fs::DirEntry::file_name);
        self.entries -= children.len();
        for entry in children {
            let kind = entry.file_type()?;
            if kind.is_dir() {
                if let Err(error) = self.inspect(&entry.path(), depth + 1) {
                    self.issues
                        .push(format!("{}: {error}", entry.path().display()));
                }
            } else if kind.is_file() {
                if [
                    "compiler-request.bin",
                    "compiler-cwd.bin",
                    "transaction.json",
                    "transaction.json.pending",
                    "compiler.stderr",
                    "consumed-sources.json",
                ]
                .iter()
                .any(|name| entry.file_name() == *name)
                {
                    continue;
                }
                let length = entry.metadata()?.len();
                if length > self.remaining {
                    self.issues.push(format!(
                        "{} exceeds remaining artifact byte bound",
                        entry.path().display()
                    ));
                    continue;
                }
                // A bounded read also handles a file growing during snapshotting.
                let mut bytes = Vec::new();
                std::fs::File::open(entry.path())?
                    .take(self.remaining + 1)
                    .read_to_end(&mut bytes)?;
                if bytes.len() as u64 > self.remaining {
                    self.issues.push(format!(
                        "{} grew beyond artifact byte bound",
                        entry.path().display()
                    ));
                    continue;
                }
                self.remaining -= bytes.len() as u64;
                self.files.push(
                    serde_json::json!({"original_path": entry.path(), "retained_path": entry.path(),
                    "bytes": bytes.len(), "sha256": crate::checked_cell::hash(&bytes)}),
                );
            } else {
                self.issues.push(format!(
                    "{} is not a regular file or directory",
                    entry.path().display()
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_transaction_diagnostics_survive_later_execution_failure() {
        let scratch = tempfile::tempdir().unwrap();
        let original = scratch.path().to_path_buf();
        let retained = scratch.keep();
        std::fs::write(retained.join("cell.txt"), "error runtimeFailure").unwrap();
        std::fs::write(
            retained.join("result.prepared.cbor"),
            b"prepared diagnostic bytes",
        )
        .unwrap();
        let command = ExtractCmd::with_bin(
            tidepool_extract_cmd::ResolvedExtractBin::assume_resolved("/diagnostic/compiler"),
        );
        let capture = CompilerDiagnosticCapture {
            directory: Some(retained.clone()),
        };
        capture.completed(&original, &command, true, b"compiler succeeded");
        let failure = std::panic::catch_unwind(|| panic!("injected later execution failure"));
        assert!(failure.is_err());
        assert_eq!(
            std::fs::read(retained.join("result.prepared.cbor")).unwrap(),
            b"prepared diagnostic bytes"
        );
        let report: serde_json::Value =
            serde_json::from_slice(&std::fs::read(retained.join("transaction.json")).unwrap())
                .unwrap();
        assert_eq!(report["compiler_process_success"], true);
        assert_eq!(report["authority"], false);
        assert!(retained.join("compiler-request.bin").is_file());
        std::fs::remove_dir_all(retained).unwrap();
    }

    #[test]
    fn bounded_snapshot_reports_omissions_without_following_symlinks() {
        let scratch = tempfile::tempdir().unwrap();
        let retained = tempfile::tempdir().unwrap();
        std::fs::write(scratch.path().join("large"), b"too large").unwrap();
        std::os::unix::fs::symlink("/does/not/exist", scratch.path().join("linked")).unwrap();
        let mut snapshot = Snapshot {
            remaining: 1,
            entries: 10,
            files: Vec::new(),
            issues: Vec::new(),
        };
        snapshot.inspect(scratch.path(), 0).unwrap();
        assert_eq!(snapshot.issues.len(), 2);
        assert!(!retained.path().join("large").exists());
        assert!(!retained.path().join("linked").exists());
    }
}
