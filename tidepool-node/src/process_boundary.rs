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

#[path = "process_scope.rs"]
pub mod service_scope;

#[path = "process_boundary/view.rs"]
mod view;

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
    read_only_overlays: Vec<(PathBuf, PathBuf)>,
    writable_overlays: Vec<(PathBuf, PathBuf)>,
    overlay_views: Vec<OverlayView>,
}

/// Immutable layers are ordered oldest first, matching Bubblewrap's source order.
#[derive(Debug, Clone, PartialEq, Eq)]
struct OverlayView {
    layers: Vec<PathBuf>,
    upper: PathBuf,
    work: PathBuf,
    target: PathBuf,
}

#[derive(Clone, Copy)]
enum ViewMount<'a> {
    ReadOnly(&'a Path, &'a Path),
    Writable(&'a Path, &'a Path),
    Overlay(&'a OverlayView),
}

impl ViewMount<'_> {
    fn target(&self) -> &Path {
        match self {
            Self::ReadOnly(_, target) | Self::Writable(_, target) => target,
            Self::Overlay(view) => &view.target,
        }
    }
}

impl ProcessMountBoundary {
    /// Prepare an opt-in service scope without spawning or changing legacy wrap().
    ///
    /// Requires a host-trusted absolute bubblewrap executable compatible with
    /// 0.11.0 default namespace init/block-fd/sync-fd semantics, and Linux init
    /// exit ordering audited at v6.12.63 (namespace drain before pidfd readiness).
    /// Path validation does not attest binary compatibility or kernel ordering;
    /// deployment must establish these prerequisites before using cleanup as
    /// evidence. See the prepared scope's spawn documentation.
    pub fn prepare_service_scope(
        &self,
        bubblewrap: PathBuf,
        command: ProcessInvocation,
    ) -> Result<service_scope::PreparedServiceScope, service_scope::ServiceScopeError> {
        service_scope::PreparedServiceScope::new(self.clone(), bubblewrap, command)
    }

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
            read_only_overlays: Vec::new(),
            writable_overlays: Vec::new(),
            overlay_views: Vec::new(),
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

    /// Overlay one process-private directory at a namespace-visible path.
    ///
    /// This is used for launch policy assembled outside the checkout. The
    /// source is resolved on the host; the destination may be inside a view
    /// created during launch. The mount is read-only inside the actor namespace.
    pub fn with_read_only_overlay(
        mut self,
        source: impl AsRef<Path>,
        target: impl AsRef<Path>,
    ) -> Result<Self, ProcessBoundaryError> {
        let source = canonicalize("read-only overlay source", source.as_ref())?;
        let target = target.as_ref().to_path_buf();
        if !target.is_absolute()
            || target
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return Err(ProcessBoundaryError::InvalidMountTarget { path: target });
        }
        self.read_only_overlays.push((source, target));
        self.read_only_overlays.sort();
        self.read_only_overlays.dedup();
        Ok(self)
    }

    /// Overlay one process-private writable directory at an existing mount
    /// point beneath the model-visible project root.
    pub fn with_writable_overlay(
        mut self,
        source: impl AsRef<Path>,
        target: impl AsRef<Path>,
    ) -> Result<Self, ProcessBoundaryError> {
        let source = canonicalize("writable overlay source", source.as_ref())?;
        let target = target.as_ref().to_path_buf();
        if !target.is_absolute() || !target.starts_with(&self.project_root) {
            return Err(ProcessBoundaryError::OverlayOutsideProjectRoot { path: target });
        }
        self.writable_overlays.push((source, target));
        self.writable_overlays.sort();
        self.writable_overlays.dedup();
        Ok(self)
    }

    /// Mount a private writable filesystem view over retained immutable layers.
    ///
    /// The resource owner retains all backing directories until process cleanup
    /// is confirmed. This boundary protects their ordinary aliases in the worker
    /// namespace; it cannot freeze other processes' aliases to the same files.
    pub fn with_overlay_view(
        mut self,
        layers: impl IntoIterator<Item = PathBuf>,
        upper: impl AsRef<Path>,
        work: impl AsRef<Path>,
        target: impl AsRef<Path>,
    ) -> Result<Self, ProcessBoundaryError> {
        let layers = layers
            .into_iter()
            .map(|path| canonicalize("snapshot layer", &path))
            .collect::<Result<Vec<_>, _>>()?;
        let upper = canonicalize("overlay upper directory", upper.as_ref())?;
        let work = canonicalize("overlay work directory", work.as_ref())?;
        let target = target.as_ref().to_path_buf();
        if !target.starts_with(&self.project_root)
            || target
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return Err(ProcessBoundaryError::OverlayOutsideProjectRoot { path: target });
        }
        let paths: Vec<_> = layers.iter().chain([&upper, &work]).collect();
        if layers.is_empty()
            || paths.iter().any(|path| !path.is_dir())
            || paths
                .iter()
                .any(|path| target.starts_with(path) || path.starts_with(&target))
            || paths.iter().enumerate().any(|(i, a)| {
                paths
                    .iter()
                    .skip(i + 1)
                    .any(|b| a.starts_with(b) || b.starts_with(a))
            })
        {
            return Err(ProcessBoundaryError::InvalidOverlayView);
        }
        self.overlay_views.push(OverlayView {
            layers,
            upper,
            work,
            target,
        });
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
        self.wrap_with_options(bubblewrap.into(), command, &[])
    }

    fn wrap_with_options(
        &self,
        bubblewrap: String,
        command: ProcessInvocation,
        options: &[String],
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
        let mut mounts = Vec::new();
        for (source, target) in &self.read_only_overlays {
            mounts.push(ViewMount::ReadOnly(source, target));
        }
        for (source, target) in &self.writable_overlays {
            mounts.push(ViewMount::Writable(source, target));
        }
        for overlay in &self.overlay_views {
            for path in overlay.layers.iter().chain([&overlay.upper, &overlay.work]) {
                mounts.push(ViewMount::ReadOnly(path, path));
            }
            mounts.push(ViewMount::Overlay(overlay));
        }
        // Parent views precede every nested mount regardless of builder order.
        mounts.sort_by(|a, b| {
            a.target()
                .components()
                .count()
                .cmp(&b.target().components().count())
                .then_with(|| a.target().cmp(b.target()))
        });
        for mount in mounts {
            match mount {
                ViewMount::ReadOnly(source, target) | ViewMount::Writable(source, target) => {
                    args.extend([
                        if matches!(mount, ViewMount::ReadOnly(..)) {
                            "--ro-bind"
                        } else {
                            "--bind"
                        }
                        .into(),
                        source.to_string_lossy().into_owned(),
                        target.to_string_lossy().into_owned(),
                    ]);
                }
                ViewMount::Overlay(overlay) => {
                    for layer in &overlay.layers {
                        args.extend(["--overlay-src".into(), layer.to_string_lossy().into_owned()]);
                    }
                    args.extend([
                        "--overlay".into(),
                        overlay.upper.to_string_lossy().into_owned(),
                        overlay.work.to_string_lossy().into_owned(),
                        overlay.target.to_string_lossy().into_owned(),
                    ]);
                }
            }
        }
        args.extend_from_slice(options);
        args.extend([
            "--chdir".into(),
            self.project_root.to_string_lossy().into_owned(),
            "--die-with-parent".into(),
            "--".into(),
            command.program,
        ]);
        args.extend(command.args);
        ProcessInvocation {
            program: bubblewrap,
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
    #[error("mount target must be absolute and contain no parent traversal: {}", .path.display())]
    InvalidMountTarget { path: PathBuf },
    #[error("overlay view requires nonempty immutable layers and disjoint backing directories")]
    InvalidOverlayView,
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
    #[error("overlay target {} is outside the model-visible project root", .path.display())]
    OverlayOutsideProjectRoot { path: PathBuf },
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

    #[test]
    fn process_private_policy_overlay_is_applied_after_workspace_mounts() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let policy = root.path().join("policy");
        let target = root.path().join("target");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&policy).unwrap();
        std::fs::create_dir_all(&target).unwrap();

        let wrapped = ProcessMountBoundary::new(&workspace, [workspace.clone()], Vec::new())
            .unwrap()
            .with_read_only_overlay(&policy, &target)
            .unwrap()
            .wrap(
                "bwrap",
                ProcessInvocation {
                    program: "true".into(),
                    args: Vec::new(),
                },
            );

        assert!(wrapped.args.windows(3).any(|args| {
            args[0] == "--ro-bind"
                && args[1] == policy.to_string_lossy()
                && args[2] == target.to_string_lossy()
        }));
    }

    #[test]
    fn writable_resource_overlay_is_private_and_applied_last() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let project = root.path().join("project");
        let resource = root.path().join("resource");
        let target = project.join(".shoal/build/cargo");
        for path in [&workspace, &project, &resource, &target] {
            std::fs::create_dir_all(path).unwrap();
        }

        let wrapped =
            ProcessMountBoundary::new(&workspace, [workspace.clone()], [workspace.clone()])
                .unwrap()
                .with_project_root(&project)
                .unwrap()
                .with_writable_overlay(&resource, &target)
                .unwrap()
                .wrap(
                    "bwrap",
                    ProcessInvocation {
                        program: "true".into(),
                        args: Vec::new(),
                    },
                );

        let workspace_mount = wrapped
            .args
            .windows(3)
            .position(|args| args[1] == workspace.to_string_lossy())
            .unwrap();
        let resource_mount = wrapped
            .args
            .windows(3)
            .position(|args| {
                args[0] == "--bind"
                    && args[1] == resource.to_string_lossy()
                    && args[2] == target.to_string_lossy()
            })
            .unwrap();
        assert!(workspace_mount < resource_mount);
    }
}
