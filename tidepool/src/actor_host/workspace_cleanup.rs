//! Explicit offline overlay reclamation after run and mount proof.
use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

#[derive(Debug, serde::Serialize)]
pub struct StorageReport {
    pub kind: StorageKind,
    pub path: PathBuf,
    pub allocated_bytes: u64,
    pub outcome: Outcome,
}

#[derive(Debug, serde::Serialize)]
pub enum StorageKind {
    Build,
    Source,
}

#[derive(Debug, serde::Serialize)]
pub enum Outcome {
    Reclaimable,
    Removed,
    Retained(String),
}

/// Never infer namespace death from a tmux pane or a missing PID. Include
/// namespaces pinned only by a descriptor; inability to inspect is retention.
fn mounted_reference(storage: &Path) -> io::Result<Option<String>> {
    let needle = storage
        .to_str()
        .ok_or_else(|| io::Error::other("non-UTF8 storage path"))?;
    if needle
        .bytes()
        .any(|byte| byte.is_ascii_whitespace() || matches!(byte, b'\\' | b',' | b':'))
    {
        return Err(io::Error::other(
            "cannot prove mount references for escaped storage path",
        ));
    }
    let uid = fs::metadata("/proc/self")?.uid();
    let mut namespaces = BTreeSet::new();
    let mut pinned = BTreeSet::new();
    for process in fs::read_dir("/proc")? {
        let process = process?;
        if process
            .file_name()
            .to_string_lossy()
            .parse::<u32>()
            .is_err()
        {
            continue;
        }
        let directory = process.path();
        let metadata = match fs::metadata(&directory) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if metadata.uid() != uid {
            continue;
        }
        let mut inspect = || -> io::Result<Option<String>> {
            namespaces.insert(fs::read_link(directory.join("ns/mnt"))?);
            let mounts = fs::read_to_string(directory.join("mountinfo"))?;
            if mounts
                .lines()
                .any(|line| mount_references_storage(line, needle))
            {
                return Ok(Some(format!("referenced by {}", directory.display())));
            }
            for fd in fs::read_dir(directory.join("fd"))? {
                if let Ok(target) = fs::read_link(fd?.path()) {
                    // Lazy-detached mounts can remain alive through ordinary
                    // file descriptors without appearing in mountinfo.
                    if target.starts_with(storage) || target.starts_with(super::ACTOR_PROJECT_ROOT)
                    {
                        return Ok(Some(format!(
                            "retained file descriptor in {}",
                            directory.display()
                        )));
                    }
                    if target.to_string_lossy().starts_with("mnt:[") {
                        pinned.insert(target);
                    }
                }
            }
            Ok(None)
        };
        match inspect() {
            Ok(Some(reason)) => return Ok(Some(reason)),
            Ok(None) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    if !pinned.is_subset(&namespaces) {
        return Ok(Some(
            "a descriptor retains a namespace without an inspectable process".into(),
        ));
    }
    Ok(None)
}

fn mount_references_storage(line: &str, storage: &str) -> bool {
    let fields = line.split(' ').collect::<Vec<_>>();
    let Some(separator) = fields.iter().position(|field| *field == "-") else {
        return false;
    };
    if fields.get(separator + 1) != Some(&"overlay") {
        return false;
    }
    let Some(options) = fields.get(separator + 3) else {
        return false;
    };
    options.split(',').any(|option| {
        let Some((key, value)) = option.split_once('=') else {
            return false;
        };
        matches!(
            key,
            "upperdir" | "workdir" | "lowerdir" | "lowerdir+" | "datadir+"
        ) && value.split(':').any(|path| {
            path == storage
                || path
                    .strip_prefix(storage)
                    .is_some_and(|suffix| suffix.starts_with('/'))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    #[test]
    fn mount_reference_matches_path_components_not_substrings() {
        let storage = "/tmp/resource/build";
        let line = "1 0 0:1 / /mnt rw - overlay overlay rw,lowerdir+=/tmp/resource/build/base,upperdir=/tmp/resource/build/upper";
        assert!(mount_references_storage(line, storage));
        assert!(!mount_references_storage(line, "/tmp/resource/buil"));
        assert!(!mount_references_storage(line, "/tmp/resource/build-other"));
        assert!(!mount_references_storage(
            "1 0 0:1 / /mnt rw - tmpfs tmpfs rw,lowerdir+=/tmp/resource/build/base",
            storage
        ));
    }

    fn fixture(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
        let id = uuid::Uuid::new_v4().to_string();
        let run = root.join("runs").join(&id);
        let resource = root
            .join("actor-worktrees/repo/worktrees/.resources")
            .join(id)
            .join("child");
        fs::create_dir_all(&run).unwrap();
        File::create(run.join("host-incarnation.owner.lock")).unwrap();
        fs::create_dir_all(resource.join("build/upper")).unwrap();
        fs::create_dir_all(resource.join("source")).unwrap();
        fs::write(resource.join("build/upper/artifact"), b"build cache").unwrap();
        fs::write(resource.join("source/dirty"), b"unsaved work").unwrap();
        (run, resource.join("build"), resource.join("source/dirty"))
    }

    #[test]
    fn live_run_lock_prevents_cleanup() {
        let root = tempfile::tempdir().unwrap();
        let (run, build, source) = fixture(root.path());
        let owner = File::options()
            .write(true)
            .open(run.join("host-incarnation.owner.lock"))
            .unwrap();
        owner.try_lock().unwrap();
        assert!(cleanup(&run, true, false)
            .unwrap_err()
            .to_string()
            .contains("already owns"));
        assert!(build.exists());
        assert!(source.exists());
    }

    #[test]
    fn dry_run_and_retained_descriptors_never_remove_source() {
        let root = tempfile::tempdir().unwrap();
        let (run, build, source) = fixture(root.path());
        let report = cleanup(&run, false, false).unwrap();
        assert_eq!(report.len(), 1);
        assert!(build.exists());
        let held = File::open(build.join("upper/artifact")).unwrap();
        let report = cleanup(&run, true, false).unwrap();
        assert!(matches!(report[0].outcome, Outcome::Retained(_)));
        assert!(build.exists());
        assert_eq!(fs::read(source).unwrap(), b"unsaved work");
        drop(held);
    }

    #[test]
    fn source_option_requires_a_finalized_worktree_record() {
        let root = tempfile::tempdir().unwrap();
        let (run, _, source) = fixture(root.path());
        let report = cleanup(&run, true, true).unwrap();
        assert_eq!(report.len(), 2);
        let source_report = report
            .iter()
            .find(|entry| matches!(entry.kind, StorageKind::Source))
            .unwrap();
        assert!(matches!(source_report.outcome, Outcome::Retained(_)));
        assert!(source.exists());
    }

    #[test]
    fn finalized_source_passes_checkout_gate_before_mount_proof() {
        use tidepool_worktree::{
            BranchName, GitOid, WorktreeId, WorktreeOrigin, WorktreeReceipt, WorktreeRecordStatus,
            WorktreeRegistry,
        };

        let root = tempfile::tempdir().unwrap();
        let (run, _, source) = fixture(root.path());
        let resource = source.parent().unwrap().parent().unwrap();
        let id = resource.file_name().unwrap().to_str().unwrap();
        let repository = resource.ancestors().nth(4).unwrap();
        let cwd = repository.join("worktrees").join(id);
        fs::create_dir_all(&cwd).unwrap();
        fs::write(cwd.join(".git"), "gitdir: /tmp/test\n").unwrap();
        let registry = WorktreeRegistry::open(repository.join("registry")).unwrap();
        let receipt = WorktreeReceipt {
            worktree_id: WorktreeId::from_raw(id),
            cwd,
            branch: BranchName::from_raw("test"),
            source_head: GitOid::from_raw("0000000000000000000000000000000000000000"),
            snapshot_ref: None,
            origin: WorktreeOrigin::CurrentRepository,
            source_repository: root.path().to_path_buf(),
            created_at_ms: 0,
            status: WorktreeRecordStatus::Finalized,
        };
        registry.put(&receipt).unwrap();
        assert!(source_finalized(repository, resource));
        let report = cleanup(&run, true, true).unwrap();
        assert!(report.iter().any(|entry| {
            matches!(entry.kind, StorageKind::Source)
                && !matches!(&entry.outcome, Outcome::Retained(reason) if reason.contains("finalized checkout"))
        }), "{report:?}");
    }

    #[test]
    fn symlinked_storage_is_not_followed() {
        let root = tempfile::tempdir().unwrap();
        let (run, build, _) = fixture(root.path());
        let moved = root.path().join("elsewhere");
        fs::rename(&build, &moved).unwrap();
        std::os::unix::fs::symlink(&moved, &build).unwrap();
        assert!(cleanup(&run, true, false)
            .unwrap_err()
            .to_string()
            .contains("symlinked build"));
        assert!(moved.join("upper/artifact").exists());
    }
}

fn bytes(path: &Path) -> io::Result<u64> {
    let mut pending = vec![path.to_owned()];
    let mut seen = BTreeSet::new();
    let mut bytes = 0;
    while let Some(path) = pending.pop() {
        let metadata = fs::symlink_metadata(&path)?;
        if !seen.insert((metadata.dev(), metadata.ino())) {
            continue;
        }
        bytes += metadata.blocks() * 512;
        if metadata.is_dir() {
            // Kernel work/work is mode 000 and contains no build artifacts.
            if metadata.mode() & 0o700 == 0 {
                continue;
            }
            for entry in fs::read_dir(path)? {
                pending.push(entry?.path());
            }
        }
    }
    Ok(bytes)
}

fn source_finalized(repository: &Path, resource: &Path) -> bool {
    use tidepool_worktree::{WorktreeId, WorktreeRecordStatus, WorktreeRegistry};

    let Some(id) = resource.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let registry_root = repository.join("registry");
    if !registry_root.is_dir() {
        return false;
    }
    let Ok(registry) = WorktreeRegistry::open(registry_root) else {
        return false;
    };
    let Ok(Some(receipt)) = registry.get(&WorktreeId::from_raw(id)) else {
        return false;
    };
    let (Ok(cwd), Ok(managed_root), Ok(git_file)) = (
        receipt.cwd.canonicalize(),
        repository.join("worktrees").canonicalize(),
        fs::symlink_metadata(receipt.cwd.join(".git")),
    ) else {
        return false;
    };
    receipt.status == WorktreeRecordStatus::Finalized
        && cwd.starts_with(managed_root)
        && git_file.is_file()
}

pub fn cleanup(
    run_root: &Path,
    apply: bool,
    include_source: bool,
) -> io::Result<Vec<StorageReport>> {
    let run_root = run_root.canonicalize()?;
    let run_id = run_root
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| io::Error::other("invalid run root"))?;
    uuid::Uuid::parse_str(run_id).map_err(io::Error::other)?;
    let runs = run_root
        .parent()
        .ok_or_else(|| io::Error::other("missing runs directory"))?;
    if runs.file_name().is_none_or(|name| name != "runs") {
        return Err(io::Error::other("expected a recorded Shoal runs directory"));
    }
    // The same lifetime lock excludes host restart throughout inspection/removal.
    let _lock = super::host_incarnation::HostRunLock::existing(&run_root)?;
    let repositories = runs
        .parent()
        .ok_or_else(|| io::Error::other("missing Shoal state directory"))?
        .join("actor-worktrees");
    let mut report = Vec::new();
    for repository in fs::read_dir(repositories)? {
        let repository = repository?.path();
        let resources = repository.join("worktrees/.resources").join(run_id);
        if !resources.is_dir() {
            continue;
        }
        if resources.canonicalize()? != resources {
            return Err(io::Error::other("symlinked resource root retained"));
        }
        for resource in fs::read_dir(resources)? {
            let resource = resource?.path();
            for (kind, name) in [
                (StorageKind::Build, "build"),
                (StorageKind::Source, "source"),
            ] {
                if matches!(kind, StorageKind::Source) && !include_source {
                    continue;
                }
                let path = resource.join(name);
                if !path.is_dir() {
                    continue;
                }
                if path.canonicalize()? != path {
                    return Err(io::Error::other(format!(
                        "symlinked {name} storage retained"
                    )));
                }
                let allocated_bytes = bytes(&path)?;
                let outcome = if matches!(kind, StorageKind::Source)
                    && !source_finalized(&repository, &resource)
                {
                    Outcome::Retained("working files have no finalized checkout proof".into())
                } else {
                    match mounted_reference(&path) {
                        Ok(None) if apply => {
                            super::overlay_resource::remove_unmounted_storage(&path)?;
                            Outcome::Removed
                        }
                        Ok(None) => Outcome::Reclaimable,
                        Ok(Some(reason)) => Outcome::Retained(reason),
                        Err(error) => Outcome::Retained(error.to_string()),
                    }
                };
                report.push(StorageReport {
                    kind,
                    path,
                    allocated_bytes,
                    outcome,
                });
            }
        }
    }
    Ok(report)
}
