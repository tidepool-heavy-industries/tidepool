//! Opt-in, case-owned diagnostics that outlive a successful compiler transaction.
//! Copies are observations only: original paths and certificates are never restamped.

use std::io::{self, Read};
use std::path::{Path, PathBuf};

use tidepool_extract_cmd::ExtractCmd;

const BYTE_LIMIT: u64 = 128 << 20;
const ENTRY_LIMIT: usize = 4096;
const DEPTH_LIMIT: usize = 16;

/// Diagnostic copies of already admitted input bytes. In particular, a sealed
/// transport path may cease to exist when its transaction closes; it is never
/// reopened to reconstruct the input observed by the compiler.
pub(crate) struct InputArtifactDiagnostics {
    directory: PathBuf,
    remaining: u64,
    saved: std::collections::BTreeSet<String>,
}

impl InputArtifactDiagnostics {
    pub(crate) fn new(directory: PathBuf) -> io::Result<Self> {
        std::fs::create_dir_all(&directory)?;
        Ok(Self {
            directory,
            remaining: BYTE_LIMIT,
            saved: Default::default(),
        })
    }

    pub(crate) fn bytes_written(&self) -> u64 {
        BYTE_LIMIT - self.remaining
    }

    fn bytes(&mut self, extension: &str, bytes: &[u8]) -> io::Result<serde_json::Value> {
        let sha256 = crate::checked_cell::hash(bytes);
        let name = format!("{sha256}.{extension}");
        if !self.saved.contains(&name) {
            if bytes.len() as u64 > self.remaining || self.saved.len() >= ENTRY_LIMIT {
                return Err(io::Error::other(format!(
                    "input artifact diagnostics exceed capture bound: attempted {} bytes, {} bytes remaining, {} of {} entries retained",
                    bytes.len(),
                    self.remaining,
                    self.saved.len(),
                    ENTRY_LIMIT,
                )));
            }
            std::fs::write(self.directory.join(&name), bytes)?;
            self.remaining -= bytes.len() as u64;
            self.saved.insert(name.clone());
        }
        Ok(serde_json::json!({"file": name, "sha256": sha256, "bytes": bytes.len()}))
    }

    pub(crate) fn canonical(
        &mut self,
        interface: &crate::certified_products::CertifiedModuleInterface,
    ) -> io::Result<serde_json::Value> {
        Ok(serde_json::json!({
            "unit": interface.unit(), "module": interface.module(),
            "interface": self.bytes("hi", interface.interface_bytes())?,
            "packages": self.bytes("packages", interface.package_imports_bytes())?,
            "certificate": self.bytes("certificate", interface.certificate_bytes())?,
            "core": interface.core_bytes().map(|bytes| self.bytes("core", bytes)).transpose()?,
        }))
    }

    pub(crate) fn payload(
        &mut self,
        payload: &crate::artifact_inventory::ArtifactPayload,
    ) -> io::Result<serde_json::Value> {
        use crate::artifact_inventory::ArtifactPayload;
        match payload {
            ArtifactPayload::Canonical(interface) => self.canonical(interface),
            ArtifactPayload::Original(product) => Ok(serde_json::json!({
                "interface": self.bytes("hi", product.interface_bytes())?,
                "packages": self.bytes("packages", product.package_imports_bytes())?,
                "certificate": self.bytes("certificate", product.certification_bytes())?,
                "canonical": product.module_interface().map(|interface| self.canonical(interface)).transpose()?,
            })),
            ArtifactPayload::Interface(interface, _) => Ok(serde_json::json!({
                "interface": self.bytes("hi", interface.interface_bytes())?,
                "packages": self.bytes("packages", interface.package_imports_bytes())?,
            })),
        }
    }
}

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

enum DiagnosticStage<'a> {
    Preparing,
    Prepared,
    Refused(&'a crate::CompileError),
    Started,
    Completed { success: bool, stderr: &'a [u8] },
}

impl DiagnosticStage<'_> {
    fn phase(&self) -> &'static str {
        match self {
            Self::Preparing => "preparing",
            Self::Prepared => "prepared",
            Self::Refused(_) => "preparation_refused",
            Self::Started => "compiler_started",
            Self::Completed { .. } => "compiler_completed",
        }
    }
}

impl CompilerDiagnosticCapture {
    fn new(source: &Path) -> Self {
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
        Self { directory }
    }

    pub fn start(source: &Path, command: &ExtractCmd) -> Self {
        let capture = Self::new(source);
        capture.write(source, command, DiagnosticStage::Started);
        capture
    }

    pub fn preparing(source: &Path, command: &ExtractCmd) -> Self {
        let capture = Self::new(source);
        capture.write(source, command, DiagnosticStage::Preparing);
        capture
    }

    pub fn prepared(&self, source: &Path, command: &ExtractCmd) {
        self.write(source, command, DiagnosticStage::Prepared);
    }

    pub fn admission_refused(
        &self,
        source: &Path,
        command: &ExtractCmd,
        error: &crate::CompileError,
    ) {
        self.write(source, command, DiagnosticStage::Refused(error));
    }

    pub fn completed(&self, source: &Path, command: &ExtractCmd, success: bool, stderr: &[u8]) {
        self.write(
            source,
            command,
            DiagnosticStage::Completed { success, stderr },
        );
    }

    fn write(&self, source: &Path, command: &ExtractCmd, stage: DiagnosticStage<'_>) {
        let Some(directory) = &self.directory else {
            return;
        };
        if let Err(error) = self.write_inner(directory, source, command, stage) {
            tracing::warn!(%error, path = %directory.display(), "compiler execution diagnostics are incomplete");
        }
    }

    fn write_inner(
        &self,
        directory: &Path,
        source: &Path,
        command: &ExtractCmd,
        stage: DiagnosticStage<'_>,
    ) -> io::Result<()> {
        let _span = tracing::debug_span!(target: "exomonad_harness::timing", "compiler.diagnostic_capture", inclusive = true, output_dir = %directory.display(), diagnostic_stage = stage.phase()).entered();
        let mut timing = crate::timing::InvocationTiming::start_scoped(
            "compiler.diagnostic_capture",
            "write_request_and_cwd",
        );
        std::fs::write(
            directory.join("compiler-request.bin"),
            command.request_bytes(),
        )?;
        std::fs::write(
            directory.join("compiler-cwd.bin"),
            std::env::current_dir()?.as_os_str().as_encoded_bytes(),
        )?;
        timing.advance("output_tree_snapshot_read_hash");
        let mut snapshot = Snapshot {
            remaining: BYTE_LIMIT,
            entries: ENTRY_LIMIT,
            files: Vec::new(),
            issues: Vec::new(),
        };
        if let Err(error) = snapshot.inspect(source, 0) {
            snapshot.issues.push(error.to_string());
        }
        if let DiagnosticStage::Completed { stderr, .. } = &stage {
            timing.advance("retain_stderr");
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
            timing.advance("consumed_source_retention");
            if let Err(error) = super::failure_sources::retain(directory, snapshot.issues.clone()) {
                snapshot
                    .issues
                    .push(format!("consumed source diagnostics: {error}"));
            }
        }
        timing.advance("construct_transaction_report");
        tracing::debug!(target: "exomonad_harness::timing",
            artifact_bytes = BYTE_LIMIT - snapshot.remaining,
            artifact_files = snapshot.files.len(),
            "diagnostic snapshot counts");
        let report = serde_json::json!({
            "schema": 1, "kind": "compiler-execution-diagnostics", "authority": false,
            "original_directory": source, "phase": stage.phase(),
            "submission": match stage { DiagnosticStage::Preparing | DiagnosticStage::Prepared | DiagnosticStage::Refused(_) => "not_attempted", DiagnosticStage::Started => "not_observed", DiagnosticStage::Completed { .. } => "observed" },
            "primary_error": match stage { DiagnosticStage::Refused(error) => Some(error.to_string()), _ => None },
            "compiler_process_success": match stage { DiagnosticStage::Completed { success, .. } => Some(success), _ => None },
            "artifact_byte_limit": BYTE_LIMIT, "artifact_entry_limit": ENTRY_LIMIT,
            "artifact_bytes": BYTE_LIMIT - snapshot.remaining, "files": snapshot.files,
            "issues": snapshot.issues,
        });
        timing.advance("encode_and_write_transaction_report");
        let pending = directory.join("transaction.json.pending");
        std::fs::write(&pending, serde_json::to_vec_pretty(&report)?)?;
        std::fs::rename(pending, directory.join("transaction.json"))?;
        timing.advance("temporary_report_and_snapshot_drop");
        drop(report);
        drop(snapshot);
        timing.finish(true);
        Ok(())
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
    fn input_capture_reuses_identical_bytes_and_preserves_budget_after_refusal() {
        let directory = tempfile::tempdir().unwrap();
        let mut capture = InputArtifactDiagnostics::new(directory.path().join("inputs")).unwrap();
        capture.remaining = 5;
        let first = capture.bytes("hi", b"abc").unwrap();
        assert_eq!(capture.remaining, 2);
        assert_eq!(capture.bytes("hi", b"abc").unwrap(), first);
        assert_eq!(capture.remaining, 2);
        assert!(capture.bytes("hi", b"abcd").is_err());
        assert_eq!(capture.remaining, 2);
        let last = capture.bytes("hi", b"xy").unwrap();
        assert_eq!(capture.remaining, 0);
        for (record, expected) in [(first, b"abc".as_slice()), (last, b"xy".as_slice())] {
            assert_eq!(
                std::fs::read(capture.directory.join(record["file"].as_str().unwrap())).unwrap(),
                expected
            );
        }
    }

    proptest::proptest! {
        #[test]
        fn input_capture_histories_match_unique_owned_bytes(
            history in proptest::collection::vec(
                (proptest::bool::ANY, proptest::collection::vec(0u8..8, 0..12)), 0..64
            )
        ) {
            use proptest::prelude::*;
            let directory = tempfile::tempdir().unwrap();
            let mut capture = InputArtifactDiagnostics::new(directory.path().join("inputs")).unwrap();
            capture.remaining = 64;
            let mut expected = std::collections::BTreeSet::new();
            let mut remaining = 64;
            for (kind, bytes) in history {
                let extension = if kind { "hi" } else { "certificate" };
                let key = (kind, bytes.clone());
                let already = expected.contains(&key);
                let admitted = already || bytes.len() <= remaining;
                let result = capture.bytes(extension, &bytes);
                prop_assert_eq!(result.is_ok(), admitted);
                if admitted {
                    if !already {
                        remaining -= bytes.len();
                        expected.insert(key);
                    }
                    let record = result.unwrap();
                    let actual = std::fs::read(capture.directory.join(record["file"].as_str().unwrap())).unwrap();
                    prop_assert_eq!(actual, bytes);
                }
                prop_assert_eq!(capture.remaining, remaining as u64);
                prop_assert_eq!(std::fs::read_dir(&capture.directory).unwrap().count(), expected.len());
            }
        }
    }

    #[test]
    fn diagnostic_record_retains_original_paths_and_process_outcome() {
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
