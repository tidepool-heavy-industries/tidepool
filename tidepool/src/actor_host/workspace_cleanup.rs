//! Explicit offline build-cache reclamation. Source and Git state stay intact.
use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

#[derive(Debug, serde::Serialize)]
pub struct BuildStorage {
    pub path: PathBuf,
    pub allocated_bytes: u64,
    pub outcome: Outcome,
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
            if mounts.contains(needle) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

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
        assert!(cleanup(&run, true)
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
        let report = cleanup(&run, false).unwrap();
        assert_eq!(report.len(), 1);
        assert!(build.exists());
        let held = File::open(build.join("upper/artifact")).unwrap();
        let report = cleanup(&run, true).unwrap();
        assert!(matches!(report[0].outcome, Outcome::Retained(_)));
        assert!(build.exists());
        assert_eq!(fs::read(source).unwrap(), b"unsaved work");
        drop(held);
    }

    #[test]
    fn symlinked_storage_is_not_followed() {
        let root = tempfile::tempdir().unwrap();
        let (run, build, _) = fixture(root.path());
        let moved = root.path().join("elsewhere");
        fs::rename(&build, &moved).unwrap();
        std::os::unix::fs::symlink(&moved, &build).unwrap();
        assert!(cleanup(&run, true)
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

pub fn cleanup(run_root: &Path, apply: bool) -> io::Result<Vec<BuildStorage>> {
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
        let resources = repository?.path().join("worktrees/.resources").join(run_id);
        if !resources.is_dir() {
            continue;
        }
        if resources.canonicalize()? != resources {
            return Err(io::Error::other("symlinked resource root retained"));
        }
        for resource in fs::read_dir(resources)? {
            let build = resource?.path().join("build");
            if !build.is_dir() {
                continue;
            }
            if build.canonicalize()? != build {
                return Err(io::Error::other("symlinked build storage retained"));
            }
            let allocated_bytes = bytes(&build)?;
            let outcome = match mounted_reference(&build) {
                Ok(None) if apply => {
                    super::overlay_resource::remove_unmounted_storage(&build)?;
                    Outcome::Removed
                }
                Ok(None) => Outcome::Reclaimable,
                Ok(Some(reason)) => Outcome::Retained(reason),
                Err(error) => Outcome::Retained(error.to_string()),
            };
            report.push(BuildStorage {
                path: build,
                allocated_bytes,
                outcome,
            });
        }
    }
    Ok(report)
}
