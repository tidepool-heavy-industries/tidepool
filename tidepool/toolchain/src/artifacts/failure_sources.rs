//! Bounded diagnostic custody of declared compiler inputs, never admission.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::cache::{DependencyEvidence, SourceEvidence};

const BYTE_LIMIT: u64 = 128 << 20;
const ENTRY_LIMIT: usize = 4096;
const DEPTH_LIMIT: usize = 16;

#[derive(Serialize, Debug, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
enum Capture {
    Matching { retained_path: PathBuf },
    Missing,
    Drift { observed_sha256: String },
    Unavailable { reason: String },
    Bound,
}

#[derive(Serialize)]
struct SourceCapture {
    original_path: PathBuf,
    expected_sha256: String,
    inventories: Vec<PathBuf>,
    #[serde(flatten)]
    capture: Capture,
}

#[derive(Serialize)]
struct Manifest {
    scope: &'static str,
    complete: bool,
    byte_limit: u64,
    bytes_read: u64,
    inventories_discovered: usize,
    inventories_decoded: usize,
    issues: Vec<String>,
    sources: Vec<SourceCapture>,
}

struct Retention {
    remaining: u64,
    entries: usize,
    declarations: usize,
    inventories_discovered: usize,
    inventories_decoded: usize,
    issues: Vec<String>,
    sources: BTreeMap<(PathBuf, String), SourceCapture>,
}

#[derive(Debug)]
enum ReadFailure {
    Bound,
    Io(io::Error),
}

impl From<io::Error> for ReadFailure {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl std::fmt::Display for ReadFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bound => f.write_str("source diagnostics exceed byte bound"),
            Self::Io(error) => std::fmt::Display::fmt(error, f),
        }
    }
}

impl Retention {
    fn read(&mut self, path: &Path, limit: u64) -> Result<Vec<u8>, ReadFailure> {
        use std::os::unix::fs::OpenOptionsExt;
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(io::Error::other("declared input is not a regular file").into());
        }
        let limit = limit.min(self.remaining);
        if metadata.len() > limit {
            return Err(ReadFailure::Bound);
        }
        let mut bytes = Vec::new();
        let result = file.take(limit + 1).read_to_end(&mut bytes);
        self.remaining = self.remaining.saturating_sub(bytes.len() as u64);
        result?;
        if bytes.len() as u64 > limit {
            return Err(ReadFailure::Bound);
        }
        Ok(bytes)
    }

    fn capture(&mut self, root: &Path, inventory: &Path, source: SourceEvidence) -> io::Result<()> {
        self.declarations = self
            .declarations
            .checked_sub(1)
            .ok_or_else(|| io::Error::other("source diagnostic declarations exceed 65536"))?;
        if source.path == Path::new("@generated-source") {
            // The unchanged graph retains the generated target's actual text.
            return Ok(());
        }
        let key = (source.path.clone(), source.sha256.clone());
        if let Some(previous) = self.sources.get_mut(&key) {
            if !previous.inventories.iter().any(|path| path == inventory) {
                previous.inventories.push(inventory.to_owned());
            }
            return Ok(());
        }
        if self.sources.len() >= ENTRY_LIMIT {
            return Err(io::Error::other("declared source inventory exceeds 4096"));
        }
        let capture = if !source.path.is_absolute() {
            Capture::Unavailable {
                reason: "declared source path is not absolute".into(),
            }
        } else {
            match self.read(&source.path, BYTE_LIMIT) {
                Ok(bytes) => {
                    let observed_sha256 = crate::checked_cell::hash(&bytes);
                    if observed_sha256 != source.sha256 {
                        Capture::Drift { observed_sha256 }
                    } else {
                        let retained_path =
                            PathBuf::from("consumed-sources").join(&observed_sha256);
                        match fs::write(root.join(&retained_path), bytes) {
                            Ok(()) => Capture::Matching { retained_path },
                            Err(error) => Capture::Unavailable {
                                reason: error.to_string(),
                            },
                        }
                    }
                }
                Err(ReadFailure::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                    Capture::Missing
                }
                Err(ReadFailure::Bound) => Capture::Bound,
                Err(error) => Capture::Unavailable {
                    reason: error.to_string(),
                },
            }
        };
        self.sources.insert(
            key,
            SourceCapture {
                original_path: source.path,
                expected_sha256: source.sha256,
                inventories: vec![inventory.to_owned()],
                capture,
            },
        );
        Ok(())
    }

    fn inventory(&mut self, root: &Path, path: &Path) -> io::Result<()> {
        let relative = path.strip_prefix(root).expect("retained artifact child");
        let graph = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                name == "execution-source.cbor"
                    || (name.starts_with("execution-") && name.ends_with(".cbor"))
            });
        let dependencies = path.file_name() == Some(std::ffi::OsStr::new("dependencies.json"))
            || (relative.parent() == Some(Path::new("selected-candidate-evidence"))
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("evidence-") && name.ends_with(".json")));
        if !graph && !dependencies {
            return Ok(());
        }
        self.inventories_discovered += 1;
        let limit = if graph {
            crate::execution_source::GRAPH_BYTES_LIMIT as u64
        } else {
            4 << 20
        };
        let sources = self.read(path, limit).and_then(|bytes| {
            if graph {
                crate::execution_source::diagnostic_source_inventory(&bytes)
                    .map_err(|error| ReadFailure::Io(io::Error::other(error)))
            } else {
                let evidence: DependencyEvidence =
                    serde_json::from_slice(&bytes).map_err(io::Error::other)?;
                if evidence.sources.len() > ENTRY_LIMIT {
                    return Err(io::Error::other("declared source inventory exceeds 4096").into());
                }
                Ok(evidence.sources)
            }
        });
        match sources {
            Ok(sources) => {
                self.inventories_decoded += 1;
                for source in sources {
                    self.capture(root, relative, source)?;
                }
            }
            Err(error) => self.issues.push(format!("{}: {error}", relative.display())),
        }
        Ok(())
    }

    fn walk(&mut self, root: &Path, directory: &Path, depth: usize) -> io::Result<()> {
        if depth > DEPTH_LIMIT {
            return Err(io::Error::other("source diagnostic depth exceeds 16"));
        }
        let mut children = fs::read_dir(directory)?
            .take(self.entries + 1)
            .collect::<Result<Vec<_>, _>>()?;
        self.entries = self
            .entries
            .checked_sub(children.len())
            .ok_or_else(|| io::Error::other("source diagnostic entries exceed 4096"))?;
        children.sort_by_key(fs::DirEntry::file_name);
        for child in children {
            // Only the already retained request tree is visited; links are not
            // traversed and no source workspace directory is searched.
            if child.file_name() == "consumed-sources" {
                continue;
            }
            let kind = child.file_type()?;
            if kind.is_dir() {
                self.walk(root, &child.path(), depth + 1)?;
            } else if kind.is_file() {
                self.inventory(root, &child.path())?;
            }
        }
        Ok(())
    }
}

pub(super) fn retain(root: &Path, prior_issues: Vec<String>) -> io::Result<()> {
    retain_with_limit(root, BYTE_LIMIT, prior_issues)
}

fn retain_with_limit(root: &Path, byte_limit: u64, prior_issues: Vec<String>) -> io::Result<()> {
    fs::create_dir(root.join("consumed-sources"))?;
    let mut retention = Retention {
        remaining: byte_limit,
        entries: ENTRY_LIMIT,
        declarations: 65536,
        inventories_discovered: 0,
        inventories_decoded: 0,
        issues: prior_issues,
        sources: BTreeMap::new(),
    };
    if let Err(error) = retention.walk(root, root, 0) {
        retention.issues.push(error.to_string());
    }
    if retention.inventories_discovered == 0 {
        retention
            .issues
            .push("no declared source inventory discovered".into());
    }
    let complete = retention.issues.is_empty()
        && retention
            .sources
            .values()
            .all(|source| matches!(source.capture, Capture::Matching { .. }));
    let manifest = Manifest {
        scope: "declared compiler inputs; diagnostic only; never compiler authority",
        complete,
        byte_limit,
        bytes_read: byte_limit - retention.remaining,
        inventories_discovered: retention.inventories_discovered,
        inventories_decoded: retention.inventories_decoded,
        issues: retention.issues,
        sources: retention.sources.into_values().collect(),
    };
    struct BoundedWriter {
        file: fs::File,
        remaining: usize,
    }
    impl io::Write for BoundedWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > self.remaining {
                return Err(io::Error::other(
                    "source diagnostic manifest exceeds eight MiB",
                ));
            }
            let written = io::Write::write(&mut self.file, bytes)?;
            self.remaining -= written;
            Ok(written)
        }
        fn flush(&mut self) -> io::Result<()> {
            io::Write::flush(&mut self.file)
        }
    }
    let path = root.join("consumed-sources.json");
    let writer = BoundedWriter {
        file: fs::File::create(&path)?,
        remaining: 8 << 20,
    };
    if let Err(error) = serde_json::to_writer_pretty(writer, &manifest) {
        // Keep a valid explicit partial report even if metadata itself exceeds
        // its bound; the original retained inventory files remain unchanged.
        return serde_json::to_writer(
            fs::File::create(path)?,
            &serde_json::json!({
                "scope": manifest.scope,
                "complete": false,
                "issues": [format!("source capture manifest unavailable: {error}")],
                "captured_source_rows": manifest.sources.len(),
                "byte_limit": byte_limit,
            "bytes_read": manifest.bytes_read,
            "inventories_discovered": manifest.inventories_discovered,
            "inventories_decoded": manifest.inventories_decoded,
            }),
        )
        .map_err(io::Error::other);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_inventory_is_distinct_from_declared_empty_inventory() {
        let absent = tempfile::tempdir().unwrap();
        retain(absent.path(), vec![]).unwrap();
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(absent.path().join("consumed-sources.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["complete"], false);
        assert_eq!(manifest["inventories_discovered"], 0);
        assert_eq!(
            manifest["issues"],
            serde_json::json!(["no declared source inventory discovered"])
        );

        let empty = tempfile::tempdir().unwrap();
        let evidence = DependencyEvidence {
            version: 4,
            cache_safe: true,
            selection_complete: true,
            sources: vec![],
            resolutions: vec![],
            packages: vec![],
            modules: vec![],
        };
        fs::write(
            empty.path().join("dependencies.json"),
            serde_json::to_vec(&evidence).unwrap(),
        )
        .unwrap();
        retain(empty.path(), vec![]).unwrap();
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(empty.path().join("consumed-sources.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["complete"], true);
        assert_eq!(manifest["inventories_discovered"], 1);
        assert_eq!(manifest["inventories_decoded"], 1);
        assert_eq!(manifest["sources"], serde_json::json!([]));
    }

    #[test]
    fn nested_failure_sources_survive_cleanup_with_explicit_drift_and_missing() {
        use std::os::unix::fs::symlink;
        let inputs = tempfile::tempdir().unwrap();
        let retained = tempfile::tempdir().unwrap();
        let (graph, _) = crate::execution_source::test_graph(inputs.path());
        let scope = retained.path().join("exact-scope/artifacts");
        fs::create_dir_all(&scope).unwrap();
        graph.capture_descriptor(&scope).unwrap();
        let item = retained.path().join("item-0");
        fs::create_dir(&item).unwrap();
        let missing = inputs.path().join("Gone.hs");
        let changed = inputs.path().join("Changed.hs");
        fs::write(&changed, "changed").unwrap();
        let shared = SourceEvidence {
            path: inputs.path().join("A.hs"),
            sha256: crate::checked_cell::hash(&fs::read(inputs.path().join("A.hs")).unwrap()),
        };
        let mut evidence = DependencyEvidence {
            version: 4,
            cache_safe: true,
            selection_complete: true,
            sources: vec![
                shared.clone(),
                SourceEvidence {
                    path: missing.clone(),
                    sha256: crate::checked_cell::hash(b"missing"),
                },
                SourceEvidence {
                    path: changed.clone(),
                    sha256: crate::checked_cell::hash(b"original"),
                },
            ],
            resolutions: vec![],
            packages: vec![],
            modules: vec![],
        };
        fs::write(
            item.join("dependencies.json"),
            serde_json::to_vec(&evidence).unwrap(),
        )
        .unwrap();
        let candidates = retained.path().join("selected-candidate-evidence");
        fs::create_dir(&candidates).unwrap();
        evidence.sources = vec![shared.clone()];
        fs::write(
            candidates.join("evidence-shared.json"),
            serde_json::to_vec(&evidence).unwrap(),
        )
        .unwrap();
        let unrelated = tempfile::tempdir().unwrap();
        fs::write(unrelated.path().join("dependencies.json"), b"not selected").unwrap();
        symlink(unrelated.path(), retained.path().join("unrelated-link")).unwrap();
        retain(retained.path(), vec![]).unwrap();
        drop(inputs);
        let manifest: serde_json::Value = serde_json::from_slice(
            &fs::read(retained.path().join("consumed-sources.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest["complete"], false);
        assert_eq!(manifest["issues"], serde_json::json!([]));
        let rows = manifest["sources"].as_array().unwrap();
        assert_eq!(rows.len(), 4);
        assert_eq!(
            rows.iter()
                .find(|row| row["original_path"] == shared.path.to_str().unwrap())
                .unwrap()["inventories"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        for row in rows.iter().filter(|row| row["status"] == "matching") {
            let bytes =
                fs::read(retained.path().join(row["retained_path"].as_str().unwrap())).unwrap();
            assert_eq!(crate::checked_cell::hash(&bytes), row["expected_sha256"]);
            assert!(!Path::new(row["original_path"].as_str().unwrap()).exists());
        }
        assert_eq!(
            rows.iter()
                .find(|row| row["original_path"] == missing.to_str().unwrap())
                .unwrap()["status"],
            "missing"
        );
        let drift = rows
            .iter()
            .find(|row| row["original_path"] == changed.to_str().unwrap())
            .unwrap();
        assert_eq!(drift["status"], "drift");
        assert_eq!(
            drift["observed_sha256"],
            crate::checked_cell::hash(b"changed")
        );
    }

    #[test]
    fn source_diagnostics_share_one_byte_budget() {
        let inputs = tempfile::tempdir().unwrap();
        let retained = tempfile::tempdir().unwrap();
        let path = inputs.path().join("TooLarge.hs");
        let bytes = vec![b'x'; 1024];
        fs::write(&path, &bytes).unwrap();
        let evidence = DependencyEvidence {
            version: 4,
            cache_safe: true,
            selection_complete: true,
            sources: vec![SourceEvidence {
                path,
                sha256: crate::checked_cell::hash(&bytes),
            }],
            resolutions: vec![],
            packages: vec![],
            modules: vec![],
        };
        let encoded = serde_json::to_vec(&evidence).unwrap();
        fs::write(retained.path().join("dependencies.json"), &encoded).unwrap();
        retain_with_limit(retained.path(), encoded.len() as u64 + 8, vec![]).unwrap();
        let manifest: serde_json::Value = serde_json::from_slice(
            &fs::read(retained.path().join("consumed-sources.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest["complete"], false);
        assert_eq!(manifest["sources"][0]["status"], "bound");
        assert_eq!(manifest["bytes_read"], encoded.len());
        assert_eq!(
            fs::read_dir(retained.path().join("consumed-sources"))
                .unwrap()
                .count(),
            0
        );
    }
}
