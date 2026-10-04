//! Prepare one complete workspace before deferred actor/native startup.

use super::overlay_resource::{SourceManifest, SourceSelection, SourceStamp};
use super::*;

use exomonad_node::MountNamespace;
use exomonad_worktree::PreparedSourceWorktree;

use std::ffi::OsString;
use std::io;
use tidepool_bridge_effects::WtWorktreeHandle;

const SOURCE_CAPTURE_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// Bound on how long a fork will queue for the parent workspace's publication
/// gate before giving up. A stuck publication (held by another fork that
/// never settles) would otherwise block every sibling fork forever.
const PUBLICATION_WAIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

pub(super) struct AdmittedWorkspace {
    pub(super) handle: WtWorktreeHandle,
    pub(super) workspace: Arc<PreparedWorkspace>,
    pub(super) notice: Option<String>,
}

#[derive(Debug)]
enum SourceFallback {
    Busy,
    Unavailable(String),
    ImportFailed(String),
}

impl SourceFallback {
    fn notice(&self) -> String {
        let reason = match self {
            Self::Busy => "source is busy".to_owned(),
            Self::Unavailable(detail) => format!("source capture unavailable: {detail}"),
            Self::ImportFailed(detail) => format!("source import failed: {detail}"),
        };
        format!(
            "Working files were not inherited ({reason}). This checkout starts at the source's committed HEAD; build-cache inheritance is independent."
        )
    }
}

struct CapturedSource {
    git: PreparedSourceWorktree,
    source: Option<OverlayResourceLease>,
    fallback: Option<SourceFallback>,
}

pub(super) struct RootImport {
    inventory: std::collections::BTreeMap<PathBuf, SourceStamp>,
    exclusions: Vec<OsString>,
    selection: SourceSelection,
    manifest: SourceManifest,
    snapshot: OverlaySnapshot,
}

enum Activation {
    Prepared,
    Activated,
}

pub(super) struct PreparedWorkspace {
    manager: WorktreeManager,
    activation: Mutex<Activation>,
    pub(super) host_path: PathBuf,
    pub(super) worktree: Option<WorktreeId>,
    pub(super) helper_draft: PathBuf,
    pub(super) view: exomonad_node::MountNamespace,
    pub(super) source: Option<SharedOverlayResource>,
    pub(super) build: Option<SharedOverlayResource>,
    source_preserved_mounts: Vec<PathBuf>,
    pub(super) owns_source: bool,
    pub(super) publication: Arc<tokio::sync::Mutex<()>>,
}

pub(super) struct ActiveWorkspace {
    prepared: Arc<PreparedWorkspace>,
    pub(super) view: exomonad_node::MountNamespace,
}

impl std::ops::Deref for ActiveWorkspace {
    type Target = PreparedWorkspace;
    fn deref(&self) -> &Self::Target {
        &self.prepared
    }
}

fn step_error(step: &str, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{step}: {error}"))
}

impl PreparedWorkspace {
    /// Called only with exact native/process and hosted cleanup established.
    pub(super) async fn retire(&self, active: &MountNamespace) -> io::Result<()> {
        let _publication = self.publication.lock().await;
        let manager = self.manager.clone();
        let worktree = self.worktree.clone();
        let retained_layers = match (&worktree, &self.source) {
            (Some(_), Some(source)) => source.retain_source_layers().await?,
            _ => None,
        };
        let active = active.clone();
        let prepared = self.view.clone();
        tidepool_runtime::spawn_blocking_in_span(move || -> io::Result<()> {
            if let Some(id) = worktree {
                match retained_layers {
                    Some(layers) => manager
                        .retain_retired_view(&id, &active, Path::new(ACTOR_PROJECT_ROOT), layers)
                        .map_err(io::Error::other)?,
                    None if manager
                        .registry()
                        .get(&id)
                        .map_err(io::Error::other)?
                        .is_some_and(|receipt| {
                            receipt.status == exomonad_worktree::WorktreeRecordStatus::Retained
                        }) => {}
                    None => manager
                        .release_host_view(&id, &active, Path::new(ACTOR_PROJECT_ROOT))
                        .map_err(io::Error::other)?,
                }
            }
            active
                .detach_retired_tree(Path::new(ACTOR_PROJECT_ROOT))
                .map_err(|error| step_error("detaching the active view", error))?;
            prepared
                .detach_retired_tree(Path::new(ACTOR_PROJECT_ROOT))
                .map_err(|error| step_error("detaching the prepared view", error))
        })
        .await
        .map_err(io::Error::other)??;
        for resource in self.source.iter().chain(self.build.iter()) {
            resource
                .retire()
                .await
                .map_err(|error| step_error("retiring an overlay resource", error))?;
        }
        Ok(())
    }

    pub(super) fn activate(
        self: Arc<Self>,
        worktrees: &WorktreeManager,
        view: exomonad_node::MountNamespace,
    ) -> io::Result<Arc<ActiveWorkspace>> {
        let mut activation = self.activation.lock();
        if !matches!(*activation, Activation::Prepared) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "workspace already activated",
            ));
        }
        if let Some(id) = &self.worktree {
            worktrees
                .activate_worktree(id, &self.view, view.clone(), Path::new(ACTOR_PROJECT_ROOT))
                .map_err(io::Error::other)?;
        } else {
            let git = worktrees.git();
            let expected = exomonad_worktree::git::inspect::git_dir(
                &git.with_mount_namespace(self.view.clone()),
                Path::new(ACTOR_PROJECT_ROOT),
            )
            .map_err(io::Error::other)?;
            let observed = exomonad_worktree::git::inspect::git_dir(
                &git.with_mount_namespace(view.clone()),
                Path::new(ACTOR_PROJECT_ROOT),
            )
            .map_err(io::Error::other)?;
            if expected != observed {
                return Err(io::Error::other(
                    "activated root Git identity differs from preparation",
                ));
            }
        }
        *activation = Activation::Activated;
        drop(activation);
        Ok(Arc::new(ActiveWorkspace {
            prepared: self,
            view,
        }))
    }
}

/// Initialize a run's mutable draft once. An authored helper directory owns
/// its complete contents, including intentional deletions from the seed.
pub(crate) fn initialize_helper_draft(workspace: &Path, draft: &Path) -> io::Result<()> {
    if draft.exists() {
        return Ok(());
    }
    let authored = workspace.join(".exomonad/helpers");
    let seed = workspace.join(".exomonad/workspace/seeds/helpers");
    if authored.is_dir() {
        copy_helper_draft(&authored, draft)
    } else if seed.is_dir() {
        copy_helper_draft(&seed, draft)
    } else {
        std::fs::create_dir_all(draft)
    }
}

/// Git's relative gitfiles are located at the host checkout, while actors see
/// the same checkout at a stable path. Overlay absolute pointers in the actor
/// view without changing the host's tracked or administrative bytes.
fn mount_workspace_gitfiles(
    mut boundary: ProcessMountBoundary,
    git: &exomonad_worktree::git::GitCli,
    workspace: &Path,
    visible: &Path,
    generated: &Path,
) -> io::Result<ProcessMountBoundary> {
    if !workspace.is_dir() {
        return Ok(boundary);
    }
    let mut pending = vec![(
        workspace.to_path_buf(),
        visible.to_path_buf(),
        PathBuf::new(),
    )];
    while let Some((host, target, relative)) = pending.pop() {
        let gitfile = host.join(".git");
        let metadata = match std::fs::symlink_metadata(&gitfile) {
            Ok(metadata) => metadata,
            Err(error)
                if error.kind() == io::ErrorKind::NotFound && relative.as_os_str().is_empty() =>
            {
                return Ok(boundary);
            }
            Err(error) => return Err(error),
        };
        if metadata.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "workspace .git is a symlink",
            ));
        }
        if metadata.is_file() {
            let admin =
                exomonad_worktree::git::inspect::git_dir(git, &host).map_err(io::Error::other)?;
            let common = exomonad_worktree::git::inspect::git_common_dir(git, &host)
                .map_err(io::Error::other)?;
            if common != admin {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "linked workspace Git metadata cannot be normalized for the actor mount",
                ));
            }
            let pointer = generated.join(&relative).join("gitfile");
            std::fs::create_dir_all(pointer.parent().expect("gitfile has parent"))?;
            std::fs::write(&pointer, format!("gitdir: {}\n", admin.display()))?;
            boundary = boundary
                .with_read_only_overlay(&pointer, target.join(".git"))
                .map_err(io::Error::other)?;
            // Submodule administration records core.worktree relative to its
            // host gitdir. Git honors that value even when its gitfile points
            // correctly at the administration directory, so bind a private
            // config with the actor-visible worktree as the final value.
            let host_config = admin.join("config");
            let mut config = std::fs::read(&host_config)?;
            if !config.ends_with(b"\n") {
                config.push(b'\n');
            }
            config.extend_from_slice(
                format!("[core]\n\tworktree = {}\n", target.display()).as_bytes(),
            );
            let actor_config = generated.join(&relative).join("config");
            std::fs::write(&actor_config, config)?;
            boundary = boundary
                .with_read_only_overlay(&actor_config, &host_config)
                .map_err(io::Error::other)?;
        } else if !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "workspace .git is not a file or directory",
            ));
        }
        let tracked = git
            .try_run(&host, &["ls-files", "--stage", "-z"])
            .map_err(io::Error::other)?;
        for entry in tracked.nul_fields() {
            let Some((mode, path)) = entry.split_once('\t') else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid Git index entry",
                ));
            };
            if !mode.starts_with("160000 ") {
                continue;
            }
            let nested = host.join(path);
            if nested.is_dir() {
                pending.push((nested, target.join(path), relative.join(path)));
            }
        }
    }
    Ok(boundary)
}

/// Copy a branch's mutable helper draft at a fork boundary. The caller holds
/// the parent helper publication lock; the two manifests additionally make
/// concurrent file edits fail closed instead of creating a torn child draft.
pub(crate) fn copy_helper_draft(source: &Path, destination: &Path) -> io::Result<()> {
    use super::overlay_resource::source_manifest;

    let excluded: [&std::ffi::OsStr; 0] = [];
    let before = source_manifest(source, &excluded)?;
    let parent = destination
        .parent()
        .ok_or_else(|| io::Error::other("helper draft destination has no parent"))?;
    std::fs::create_dir_all(parent)?;
    if destination.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "fork helper draft already exists",
        ));
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let staging = parent.join(format!(".helper-draft-{}-{nonce}", std::process::id()));
    let result = (|| {
        copy_helper_tree(source, &staging)?;
        if source_manifest(source, &excluded)? != before {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "helper draft changed while fork snapshot was copied",
            ));
        }
        std::fs::rename(&staging, destination)
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result
}

fn copy_helper_tree(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let metadata = std::fs::symlink_metadata(source)?;
    if metadata.is_dir() {
        std::fs::create_dir(destination)?;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            copy_helper_tree(&entry.path(), &destination.join(entry.file_name()))?;
        }
        std::fs::set_permissions(
            destination,
            std::fs::Permissions::from_mode(metadata.mode()),
        )?;
    } else if metadata.is_file() {
        std::fs::copy(source, destination)?;
        std::fs::set_permissions(
            destination,
            std::fs::Permissions::from_mode(metadata.mode()),
        )?;
    } else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "helper draft contains a non-file entry",
        ));
    }
    Ok(())
}
