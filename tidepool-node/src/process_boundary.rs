//! A small mount boundary for external interactive actor processes.
//!
//! This is operational write containment, not a hardened container. The host
//! filesystem, environment, network, credentials, and process namespace stay
//! available. Bubblewrap only makes selected repository roots read-only and
//! then re-exposes one narrower actor workspace as writable at a stable
//! model-visible project path. The composition root may also expose shared Git
//! metadata writable for native linked-worktree semantics. The boundary keeps
//! actor working files separate without requiring one project-trust entry per
//! generated checkout.

use std::path::{Path, PathBuf};

pub const BUBBLEWRAP_PROGRAM: &str = "bwrap";

/// An exact executable plus argv, ready for a process launcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInvocation {
    pub program: String,
    pub args: Vec<String>,
}

/// Validated repository mounts for one process incarnation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessMountBoundary {
    cwd: PathBuf,
    project_root: PathBuf,
    read_only_roots: Vec<PathBuf>,
    writable_roots: Vec<PathBuf>,
}

impl ProcessMountBoundary {
    pub fn new(
        cwd: impl AsRef<Path>,
        read_only_roots: impl IntoIterator<Item = PathBuf>,
        writable_roots: impl IntoIterator<Item = PathBuf>,
    ) -> Result<Self, ProcessBoundaryError> {
        let cwd = canonicalize("working directory", cwd.as_ref())?;
        let mut read_only_roots = read_only_roots
            .into_iter()
            .map(|path| canonicalize("read-only root", &path))
            .collect::<Result<Vec<_>, _>>()?;
        let mut writable_roots = writable_roots
            .into_iter()
            .map(|path| canonicalize("writable root", &path))
            .collect::<Result<Vec<_>, _>>()?;
        read_only_roots.sort();
        read_only_roots.dedup();
        writable_roots.sort();
        writable_roots.dedup();

        for writable in &writable_roots {
            if !read_only_roots
                .iter()
                .any(|protected| writable.starts_with(protected))
            {
                return Err(ProcessBoundaryError::WritableOutsideProtectedRoot {
                    path: writable.clone(),
                });
            }
        }
        if !read_only_roots
            .iter()
            .chain(writable_roots.iter())
            .any(|root| cwd.starts_with(root))
        {
            return Err(ProcessBoundaryError::WorkingDirectoryOutsideBoundary { path: cwd });
        }

        Ok(Self {
            project_root: cwd.clone(),
            cwd,
            read_only_roots,
            writable_roots,
        })
    }

    /// Present the real process workspace at one stable model-visible path.
    /// Distinct mount namespaces may reuse the same slot concurrently.
    pub fn with_project_root(
        mut self,
        project_root: impl AsRef<Path>,
    ) -> Result<Self, ProcessBoundaryError> {
        self.project_root = canonicalize("model-visible project root", project_root.as_ref())?;
        Ok(self)
    }

    /// Wrap `command` with Bubblewrap. Broad read-only mounts are emitted
    /// first and narrower writable overrides last, so mount order implements
    /// the boundary directly rather than relying on filesystem permissions.
    pub fn wrap(
        &self,
        bubblewrap: impl Into<String>,
        command: ProcessInvocation,
    ) -> ProcessInvocation {
        let mut args = vec![
            "--bind".into(),
            "/".into(),
            "/".into(),
            "--dev-bind".into(),
            "/dev".into(),
            "/dev".into(),
        ];
        for path in &self.read_only_roots {
            let path = path.to_string_lossy().into_owned();
            args.extend(["--ro-bind".into(), path.clone(), path]);
        }
        for path in &self.writable_roots {
            let path = path.to_string_lossy().into_owned();
            args.extend(["--bind".into(), path.clone(), path]);
        }
        if self.project_root != self.cwd {
            let writable = self
                .writable_roots
                .iter()
                .any(|root| self.cwd.starts_with(root));
            args.push(if writable { "--bind" } else { "--ro-bind" }.into());
            args.push(self.cwd.to_string_lossy().into_owned());
            args.push(self.project_root.to_string_lossy().into_owned());
        }
        args.extend([
            "--chdir".into(),
            self.project_root.to_string_lossy().into_owned(),
            "--die-with-parent".into(),
            "--".into(),
            command.program,
        ]);
        args.extend(command.args);
        ProcessInvocation {
            program: bubblewrap.into(),
            args,
        }
    }
}

fn canonicalize(kind: &'static str, path: &Path) -> Result<PathBuf, ProcessBoundaryError> {
    path.canonicalize()
        .map_err(|source| ProcessBoundaryError::InvalidPath {
            kind,
            path: path.to_path_buf(),
            source,
        })
}

#[derive(Debug, thiserror::Error)]
pub enum ProcessBoundaryError {
    #[error("cannot resolve {kind} {}: {source}", .path.display())]
    InvalidPath {
        kind: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("writable root {} is not inside a protected read-only root", .path.display())]
    WritableOutsideProtectedRoot { path: PathBuf },
    #[error("working directory {} is outside the process mount boundary", .path.display())]
    WorkingDirectoryOutsideBoundary { path: PathBuf },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broad_read_only_mount_precedes_narrow_writable_workspace() {
        let root = tempfile::tempdir().unwrap();
        let workers = root.path().join("workers");
        let actor = workers.join("actor");
        std::fs::create_dir_all(&actor).unwrap();
        let boundary =
            ProcessMountBoundary::new(&actor, [workers.clone()], [actor.clone()]).unwrap();
        let wrapped = boundary.wrap(
            "bwrap",
            ProcessInvocation {
                program: "codex".into(),
                args: vec!["--sandbox".into(), "danger-full-access".into()],
            },
        );
        let read_only = wrapped
            .args
            .windows(3)
            .position(|args| args[0] == "--ro-bind" && args[1] == workers.to_string_lossy())
            .unwrap();
        let writable = wrapped
            .args
            .windows(3)
            .position(|args| args[0] == "--bind" && args[1] == actor.to_string_lossy())
            .unwrap();
        assert!(read_only < writable);
        assert_eq!(wrapped.program, "bwrap");
        assert!(wrapped.args.ends_with(&[
            "codex".into(),
            "--sandbox".into(),
            "danger-full-access".into()
        ]));
    }

    #[test]
    fn writable_exception_must_be_nested_under_a_protected_root() {
        let root = tempfile::tempdir().unwrap();
        let protected = root.path().join("protected");
        let outside = root.path().join("outside");
        std::fs::create_dir_all(&protected).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        assert!(matches!(
            ProcessMountBoundary::new(&outside, [protected], [outside.clone()]),
            Err(ProcessBoundaryError::WritableOutsideProtectedRoot { .. })
        ));
    }

    #[test]
    fn private_workspace_is_mounted_at_one_stable_project_root() {
        let root = tempfile::tempdir().unwrap();
        let workers = root.path().join("workers");
        let actor = workers.join("actor");
        let project_root = root.path().join("actor-project");
        std::fs::create_dir_all(&actor).unwrap();
        std::fs::create_dir_all(&project_root).unwrap();
        let boundary = ProcessMountBoundary::new(&actor, [workers.clone()], [actor.clone()])
            .unwrap()
            .with_project_root(&project_root)
            .unwrap();
        let wrapped = boundary.wrap(
            "bwrap",
            ProcessInvocation {
                program: "pwd".into(),
                args: Vec::new(),
            },
        );

        assert!(wrapped.args.windows(3).any(|args| {
            args[0] == "--bind"
                && args[1] == actor.to_string_lossy()
                && args[2] == project_root.to_string_lossy()
        }));
        assert!(wrapped
            .args
            .windows(2)
            .any(|args| { args[0] == "--chdir" && args[1] == project_root.to_string_lossy() }));
    }
}
