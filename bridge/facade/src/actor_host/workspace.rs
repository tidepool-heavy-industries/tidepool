//! Prepare one complete workspace before deferred actor/native startup.

use super::overlay_resource::{
    selected_inventory, selected_manifest, source_inventory, source_manifest, SourceManifest,
    SourceSelection, SourceStamp,
};
use super::workspace_publication::WorkspacePublication;
use super::*;
use exomonad_node::MountNamespace;
use exomonad_worktree::{PreparedSourceWorktree, WorktreeSource};
use std::ffi::OsString;
use std::io;
use tidepool_bridge_effects::WtWorktreeHandle;
use tidepool_handlers::handlers::worktree::{handle_to_wire, AuthorizedForkWorkspace};
use workspace_publication::Admission;

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
        format!("Working files were not inherited ({reason}). This checkout starts at the source's committed HEAD; build-cache inheritance is independent.")
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

#[derive(Clone)]
pub(super) struct WorkspaceLayout {
    pub(super) run_namespace: String,
    pub(super) source_root: PathBuf,
    pub(super) source_exclude: Vec<String>,
    pub(super) source_import: crate::exomonad::SourceImportPolicy,
    pub(super) root_imports: Arc<Mutex<std::collections::BTreeMap<PathBuf, Arc<RootImport>>>>,
    pub(super) worktrees: WorktreeManager,
    pub(super) base_prompt: FrozenBasePrompt,
    pub(super) backend: Arc<dyn InteractiveAgentBackend>,
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
    pub(super) publication: Arc<tokio::sync::Mutex<WorkspacePublication>>,
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
        let publication = self.publication.lock().await;
        if publication.is_pending() {
            return Err(io::Error::other("workspace publication remains pending"));
        }
        let manager = self.manager.clone();
        let worktree = self.worktree.clone();
        let active = active.clone();
        let prepared = self.view.clone();
        tidepool_runtime::spawn_blocking_in_span(move || -> io::Result<()> {
            if let Some(id) = worktree {
                manager
                    .materialize_retired_view(&id, &active, Path::new(ACTOR_PROJECT_ROOT))
                    .map_err(io::Error::other)?;
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

impl WorkspaceLayout {
    fn helper_root(&self) -> PathBuf {
        self.worktrees
            .managed_root()
            .join(".resources")
            .join(&self.run_namespace)
            .join("helpers")
    }

    fn helper_draft(&self, branch: &str) -> PathBuf {
        self.helper_root().join("drafts").join(branch)
    }

    fn inherit_helper_branch(&self, parent_draft: &Path, child_branch: &str) -> io::Result<()> {
        let parent_branch = parent_draft
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| io::Error::other("parent helper branch has no name"))?;
        let helper_root = self.helper_root();
        let parent_layer =
            crate::exomonad::source::SourceLayer::helpers(&helper_root, parent_branch);
        let _helper_revision = parent_layer
            .lock_helpers()
            .map_err(|error| io::Error::other(error.to_string()))?;
        let child_draft = self.helper_draft(child_branch);
        copy_helper_draft(parent_draft, &child_draft)?;
        let child_layer = crate::exomonad::source::SourceLayer::helpers(&helper_root, child_branch);
        let domain = format!("helper-fork:{}", self.run_namespace);
        let seed = helper_root.join("empty");
        std::fs::create_dir_all(&seed)?;
        parent_layer
            .ensure_active_from(&domain, &[seed])
            .map_err(|error| io::Error::other(error.to_string()))?;
        child_layer
            .inherit_active_from(&parent_layer, std::slice::from_ref(&child_draft))
            .map_err(|error| io::Error::other(error.to_string()))?;
        Ok(())
    }

    fn reusable_import(&self, source: &Path, excluded: &[OsString]) -> Option<OverlaySnapshot> {
        let candidate = self.root_imports.lock().get(source).cloned()?;
        if candidate.exclusions != excluded {
            return None;
        }
        let selection = self.source_selection(source, excluded).ok()?;
        if selection != candidate.selection {
            return None;
        }
        let before = selected_inventory(source, &selection).ok()?;
        if before != candidate.inventory {
            return None;
        }
        let manifest = selected_manifest(source, &selection).ok()?;
        let after = selected_inventory(source, &selection).ok()?;
        (before == after && manifest == candidate.manifest).then(|| candidate.snapshot.clone())
    }

    fn remember_import(
        &self,
        source_path: &Path,
        excluded: &[OsString],
        selection: &SourceSelection,
        source: &OverlayResourceLease,
    ) -> io::Result<()> {
        let before = selected_inventory(source_path, selection)?;
        let original = match selected_manifest(source_path, selection) {
            Ok(manifest) => manifest,
            Err(error) => {
                tracing::debug!(%error, "source manifest unavailable; import will not be reused");
                return Ok(());
            }
        };
        let after = selected_inventory(source_path, selection)?;
        if before != after {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "source changed while verifying imported base",
            ));
        }
        let (base, snapshot) = source.imported_base()?;
        let copied = match selected_manifest(base, selection) {
            Ok(manifest) => manifest,
            Err(error) => {
                tracing::debug!(%error, "imported-base manifest unavailable; import will not be reused");
                return Ok(());
            }
        };
        if original != copied {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "imported base differs from source",
            ));
        }
        self.root_imports.lock().insert(
            source_path.to_owned(),
            Arc::new(RootImport {
                inventory: after,
                exclusions: excluded.to_vec(),
                selection: selection.clone(),
                manifest: original,
                snapshot,
            }),
        );
        Ok(())
    }

    fn source_exclusions(
        &self,
        source: &Path,
        files: &Path,
    ) -> io::Result<Vec<std::ffi::OsString>> {
        let mut excluded = vec![".git".into(), ".exomonad".into()];
        let git = self.worktrees.git();
        for name in &self.source_exclude {
            if crate::exomonad::source_directory_has_tracked(git, source, name)? {
                return Err(io::Error::other(format!(
                    "configured source exclusion {name:?} contains tracked files"
                )));
            }
            excluded.push(name.into());
        }
        for entry in std::fs::read_dir(files)? {
            let entry = entry?;
            let name = entry.file_name();
            if excluded.iter().any(|excluded| excluded == &name) || !entry.file_type()?.is_dir() {
                continue;
            }
            let Ok(tag) = std::fs::read(entry.path().join("CACHEDIR.TAG")) else {
                continue;
            };
            if !tag.starts_with(b"Signature: 8a477f597d28d172789f06886806bc55") {
                continue;
            }
            let Some(name_text) = name.to_str() else {
                continue;
            };
            if !crate::exomonad::source_directory_has_tracked(git, source, name_text)? {
                excluded.push(name);
            }
        }
        excluded.sort();
        Ok(excluded)
    }

    fn source_selection(
        &self,
        source: &Path,
        excluded: &[OsString],
    ) -> io::Result<SourceSelection> {
        fn collect(
            git: &exomonad_worktree::GitCli,
            root: &Path,
            repo: &Path,
            prefix: &Path,
            excluded: &[OsString],
            leaves: &mut Vec<PathBuf>,
        ) -> io::Result<()> {
            for entry in git.source_working_paths(repo).map_err(io::Error::other)? {
                let path = entry.path;
                if prefix.as_os_str().is_empty()
                    && excluded.iter().any(|name| {
                        path.components()
                            .next()
                            .is_some_and(|part| part.as_os_str() == name)
                    })
                {
                    continue;
                }
                let relative = prefix.join(&path);
                let absolute = root.join(&relative);
                let metadata = match std::fs::symlink_metadata(&absolute) {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error),
                };
                if metadata.is_dir() {
                    if entry.tracked {
                        // Indexed directories are submodules.
                        collect(git, root, &absolute, &relative, &[], leaves)?;
                    } else {
                        return Err(io::Error::other(format!(
                            "untracked nested repository {} needs a Git ignore rule or source exclusion",
                            relative.display()
                        )));
                    }
                } else if metadata.is_file() || metadata.file_type().is_symlink() {
                    leaves.push(relative);
                } else {
                    return Err(io::Error::other(format!(
                        "unsupported source entry {}",
                        absolute.display()
                    )));
                }
            }
            Ok(())
        }
        let mut leaves = Vec::new();
        collect(
            self.worktrees.git(),
            source,
            source,
            Path::new(""),
            excluded,
            &mut leaves,
        )?;
        SourceSelection::from_leaves(source, leaves)
    }

    pub(super) fn resource_root(&self, key: &str) -> PathBuf {
        // Actor IDs restart in each run; retained resources belong to that run.
        self.worktrees
            .managed_root()
            .join(".resources")
            .join(&self.run_namespace)
            .join(key)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn prepare(
        &self,
        host_path: PathBuf,
        worktree: Option<WorktreeId>,
        key: &str,
        root: bool,
        policy: exomonad_actor::ForkWorkspacePolicy,
        mut source: Option<OverlayResourceLease>,
        inherited_build: Option<OverlaySnapshot>,
        helper_branch: Option<String>,
    ) -> io::Result<Arc<PreparedWorkspace>> {
        let visible = PathBuf::from(ACTOR_PROJECT_ROOT);
        self.worktrees
            .git()
            .ensure_exomonad_local_exclude(&host_path)
            .map_err(io::Error::other)?;
        let common =
            exomonad_worktree::git::inspect::git_common_dir(self.worktrees.git(), &host_path)
                .map_err(io::Error::other)?;
        // Distinct from the managed root below, which stays read-only: only the
        // worktrees the ROOT allocated for itself are writable to it.
        let root_worktrees = self.worktrees.root_allocations();
        let roots = writable_repository_roots(
            root,
            policy.workspace,
            &self.source_root,
            worktree.as_ref().map(|_| host_path.as_path()),
            &common,
            root.then(|| root_worktrees.managed_root()),
        );
        let resource_root = self.resource_root(key);
        let helper_branch = helper_branch.unwrap_or_else(|| {
            worktree
                .as_ref()
                .map(|id| id.as_str().to_owned())
                .unwrap_or_else(|| "run".to_owned())
        });
        let helper_draft = self.helper_draft(&helper_branch);
        if root {
            initialize_helper_draft(&self.source_root, &helper_draft)?;
        }
        std::fs::create_dir_all(&helper_draft)?;
        let helper_mountpoint = host_path.join(".exomonad/helpers");
        std::fs::create_dir_all(&helper_mountpoint)?;
        let native_policy = native_tool_policy(policy.native_tools);
        let mounts = self
            .backend
            .prepare_native_tool_policy(native_policy, &resource_root.join("native-policy"))
            .map_err(io::Error::other)?;
        let mut boundary = ProcessMountBoundary::new(
            &host_path,
            [
                self.source_root.clone(),
                self.worktrees.managed_root().to_owned(),
                common,
            ],
            roots,
        )
        .and_then(|boundary| boundary.with_project_root(&visible))
        .and_then(|boundary| {
            boundary
                .with_read_only_overlay(self.base_prompt.directory(), self.base_prompt.directory())
        })
        .map_err(io::Error::other)?;
        if let Some(source) = &mut source {
            source.prepare_root_metadata()?;
            boundary = source.mount(boundary, &visible).map_err(io::Error::other)?;
            boundary = boundary
                .with_read_only_overlay(host_path.join(".git"), visible.join(".git"))
                .map_err(io::Error::other)?;
        }
        let canonical = if root {
            self.source_root.join(".exomonad")
        } else {
            host_path.join(".exomonad")
        };
        if canonical.is_dir() {
            boundary =
                if root || policy.workspace == exomonad_actor::WorkspaceAccess::WritableBound {
                    boundary.with_writable_overlay(&canonical, visible.join(".exomonad"))
                } else {
                    boundary.with_read_only_overlay(&canonical, visible.join(".exomonad"))
                }
                .map_err(io::Error::other)?;
        }
        if root {
            boundary = mount_workspace_gitfiles(
                boundary,
                self.worktrees.git(),
                &self.source_root.join(".exomonad/workspace"),
                &visible.join(".exomonad/workspace"),
                &resource_root.join("workspace-gitfiles"),
            )?;
        }
        boundary = boundary
            .with_read_only_overlay(&helper_draft, &helper_draft)
            .and_then(|boundary| {
                if root || policy.workspace == exomonad_actor::WorkspaceAccess::WritableBound {
                    boundary.with_writable_overlay(&helper_draft, visible.join(".exomonad/helpers"))
                } else {
                    boundary
                        .with_read_only_overlay(&helper_draft, visible.join(".exomonad/helpers"))
                }
            })
            .map_err(io::Error::other)?;
        for InteractivePolicyMount { source, target } in mounts {
            boundary = boundary
                .with_read_only_overlay(source, target)
                .map_err(io::Error::other)?;
        }
        let mut build = if native_policy == InteractiveNativeToolPolicy::InspectionOnly {
            None
        } else {
            let build =
                OverlayResourceLease::allocate_path(resource_root.join("build"), inherited_build)?;
            if source.is_none() {
                std::fs::create_dir_all(host_path.join(ACTOR_BUILD_TARGET))?;
            }
            boundary = build
                .mount(boundary, &visible.join(ACTOR_BUILD_TARGET))
                .map_err(io::Error::other)?;
            Some(build)
        };
        if !root && policy.workspace != exomonad_actor::WorkspaceAccess::WritableBound {
            boundary = boundary.with_read_only_project();
        }
        // The short bootstrap may acquire mounts even if its receipt is lost.
        for resource in source.iter_mut().chain(build.iter_mut()) {
            resource.process_may_exist();
        }
        let source_preserved_mounts = boundary.preserved_mounts_under(&visible);
        let view = boundary.prepare_view(
            BUBBLEWRAP_PROGRAM,
            std::time::Instant::now() + PROCESS_OPERATION_TIMEOUT,
        )?;
        for resource in source.iter_mut().chain(build.iter_mut()) {
            resource.record_bootstrap_upper()?;
        }
        if source.is_none() {
            if let Some(id) = &worktree {
                self.worktrees
                    .mount_worktree(id, view.clone(), &visible)
                    .map_err(io::Error::other)?;
            }
        }
        Ok(Arc::new(PreparedWorkspace {
            manager: self.worktrees.clone(),
            activation: Mutex::new(Activation::Prepared),
            host_path,
            worktree,
            helper_draft,
            view,
            source: source.map(SharedOverlayResource::new),
            build: build.map(SharedOverlayResource::new),
            source_preserved_mounts,
            owns_source: root || policy.workspace == exomonad_actor::WorkspaceAccess::WritableBound,
            publication: Arc::new(tokio::sync::Mutex::new(WorkspacePublication::default())),
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

impl NativeForkAdmission {
    pub(super) async fn prepare_workspace(
        &self,
        creator: ActorRef,
        authorized: AuthorizedForkWorkspace,
        policy: exomonad_actor::ForkWorkspacePolicy,
    ) -> io::Result<AdmittedWorkspace> {
        let layout = self
            .layout
            .clone()
            .ok_or_else(|| io::Error::other("workspace layout unavailable"))?;
        let build = self.build_snapshot(creator, policy.native_tools).await;
        let creator_helper_draft = self
            .owners
            .lock()
            .get(&creator)
            .filter(|owner| owner.terminal.is_none())
            .and_then(|owner| owner.creator_workspace.as_ref())
            .map(|workspace| workspace.workspace.helper_draft.clone());
        let explicit_ref = matches!(authorized.source(), WorktreeSource::Ref(_));
        let parent = if explicit_ref {
            None
        } else {
            let owners = self.owners.lock();
            let mut matches = owners
                .iter()
                .filter(|(_, owner)| owner.terminal.is_none())
                .filter_map(|(actor, owner)| {
                    owner
                        .creator_workspace
                        .as_ref()
                        .map(|bound| (*actor, bound))
                })
                .filter(|(_, bound)| bound.workspace.owns_source)
                .filter(|(_, bound)| match authorized.source() {
                    WorktreeSource::CurrentRepository => {
                        bound.workspace.worktree.is_none()
                            && bound.workspace.host_path == layout.source_root
                    }
                    WorktreeSource::Worktree(id) => bound.workspace.worktree.as_ref() == Some(id),
                    WorktreeSource::Ref(_) => false,
                });
            let first = matches.next().map(|(actor, bound)| (actor, bound.clone()));
            if matches.next().is_some() {
                None
            } else {
                first
            }
        };
        let Some((source_owner, parent)) = parent else {
            let reason = (!explicit_ref)
                .then(|| SourceFallback::Unavailable("no unique live source owner".into()));
            return tidepool_runtime::spawn_blocking_in_span(move || {
                layout.prepare_committed(
                    authorized,
                    policy,
                    build,
                    reason,
                    None,
                    creator_helper_draft,
                )
            })
            .await
            .map_err(io::Error::other)?;
        };
        // Sibling forks queue on the same source publication. Contention here
        // says nothing about native writers or the source's availability.
        // A caller cancelled while waiting has not begun an operation.
        let wait_started = std::time::Instant::now();
        let mut publication = match tokio::time::timeout(
            PUBLICATION_WAIT_TIMEOUT,
            parent.workspace.publication.clone().lock_owned(),
        )
        .await
        {
            Ok(publication) => publication,
            Err(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "waited {}s on the parent workspace publication held by another fork; \
                         the fork was not started",
                        PUBLICATION_WAIT_TIMEOUT.as_secs()
                    ),
                ));
            }
        };
        tracing::info!(
            publication_wait_ms = wait_started.elapsed().as_millis() as u64,
            "workspace publication gate acquired"
        );
        let source_still_owned = self.owners.lock().get(&source_owner).is_some_and(|owner| {
            owner.terminal.is_none()
                && owner
                    .creator_workspace
                    .as_ref()
                    .is_some_and(|bound| Arc::ptr_eq(&bound.workspace, &parent.workspace))
        });
        if !source_still_owned {
            drop(publication);
            return tidepool_runtime::spawn_blocking_in_span(move || {
                layout.prepare_committed(
                    authorized,
                    policy,
                    build,
                    Some(SourceFallback::Unavailable(
                        "source owner retired while publication was queued".into(),
                    )),
                    None,
                    creator_helper_draft,
                )
            })
            .await
            .map_err(io::Error::other)?;
        }
        // The operation task retains its gate and resources even when its caller
        // abandons the await. Host death ends the wave instead of replaying it.
        let backend = self.backend.clone();
        let fallback_helper_draft = creator_helper_draft.clone();
        tokio::spawn(async move {
            let donor_view = parent.workspace.view.clone();
            if publication.is_pending() {
                parent
                    .settle_publication(&mut publication, backend.as_ref())
                    .await?;
            }
            let admission = publication.begin(backend.as_ref(), &parent.thread).await;
            let namespace = match admission {
                Ok(Admission::Ready(namespace)) => {
                    match parent.workspace.view.bind_live_view(namespace) {
                        Ok(namespace) => namespace,
                        Err(error) => {
                            parent
                                .settle_publication(&mut publication, backend.as_ref())
                                .await?;
                            return Err(error);
                        }
                    }
                }
                Ok(Admission::Busy) => {
                    return tidepool_runtime::spawn_blocking_in_span(move || {
                        layout.prepare_committed(
                            authorized,
                            policy,
                            build,
                            Some(SourceFallback::Busy),
                            Some(donor_view),
                            fallback_helper_draft.clone(),
                        )
                    })
                    .await
                    .map_err(io::Error::other)?
                }
                Ok(Admission::Unavailable(detail)) => {
                    return tidepool_runtime::spawn_blocking_in_span(move || {
                        layout.prepare_committed(
                            authorized,
                            policy,
                            build,
                            Some(SourceFallback::Unavailable(detail)),
                            Some(donor_view),
                            fallback_helper_draft.clone(),
                        )
                    })
                    .await
                    .map_err(io::Error::other)?
                }
                Err(error) => {
                    // If identity is known this may immediately finish; a lost
                    // begin reply remains owned for the fleet's next retry.
                    match parent
                        .settle_publication(&mut publication, backend.as_ref())
                        .await
                    {
                        Ok(()) => {}
                        Err(settle_error) => {
                            tracing::error!(
                                creator = ?creator,
                                source_owner = ?source_owner,
                                begin_error = %error,
                                settle_error = %settle_error,
                                "workspace publication admission failed and settling it afterward also failed"
                            );
                            return Err(io::Error::new(
                                error.kind(),
                                format!(
                                    "{error}; additionally, settling the publication afterward failed: {settle_error}"
                                ),
                            ));
                        }
                    }
                    return Err(error);
                }
            };
            let source = match &parent.workspace.source {
                Some(source) => Some(source.publication.clone().lock_owned().await),
                None => None,
            };
            let cache = if source_owner == creator {
                match &parent.workspace.build {
                    Some(build) => Some(build.publication.clone().lock_owned().await),
                    None => None,
                }
            } else {
                None
            };
            let capture_layout = layout.clone();
            let host_path = parent.workspace.host_path.clone();
            let parent_helper_draft = creator_helper_draft.clone();
            let preserved = parent.workspace.source_preserved_mounts.clone();
            let capture_span = tracing::info_span!(
                "source_capture",
                creator = ?creator,
                source_owner = ?source_owner,
            );
            let captured = tidepool_runtime::spawn_blocking_in_span(move || {
                let _entered = capture_span.enter();
                // Publication has already begun. Its owner retains this task
                // through caller cancellation until capture and release settle.
                // Never substitute old HEAD for an unavailable live checkpoint.
                let wait_started = std::time::Instant::now();
                let _admission = capture_layout
                    .worktrees
                    .git()
                    .capture_within(SOURCE_CAPTURE_WAIT)
                    .ok_or_else(|| {
                        tracing::warn!(
                            phase = "git_capture_wait",
                            elapsed_ms = wait_started.elapsed().as_millis() as u64,
                            outcome = "timeout",
                            "source capture phase finished"
                        );
                        io::Error::new(
                            io::ErrorKind::WouldBlock,
                            "source Git operation remained busy for 30 seconds; live fork was not checkpointed",
                        )
                    })?;
                tracing::info!(
                    phase = "git_capture_wait",
                    elapsed_ms = wait_started.elapsed().as_millis() as u64,
                    outcome = "acquired",
                    "source capture phase finished"
                );
                let capture_started = std::time::Instant::now();
                let captured = capture_layout.capture(
                    &authorized,
                    &namespace,
                    &host_path,
                    &preserved,
                    source,
                    parent_helper_draft,
                );
                tracing::info!(
                    phase = "git_capture",
                    elapsed_ms = capture_started.elapsed().as_millis() as u64,
                    success = captured.is_ok(),
                    "source capture phase finished"
                );
                // Source bytes and private Git state are now frozen. Build
                // publication consumes another resource and cannot alter that
                // baseline, so it must not exclude unrelated repository work.
                drop(_admission);
                let captured = captured?;
                let build_started = std::time::Instant::now();
                let published = WorkspaceLayout::publish_build(&namespace, cache);
                tracing::info!(
                    phase = "build_snapshot",
                    elapsed_ms = build_started.elapsed().as_millis() as u64,
                    success = published.is_ok(),
                    "source capture phase finished"
                );
                published?;
                Ok::<_, io::Error>(captured)
            })
            .await
            .map_err(io::Error::other);
            parent
                .settle_publication(&mut publication, backend.as_ref())
                .await?;
            let captured = captured??;
            let build = if source_owner == creator
                && policy.native_tools != exomonad_actor::NativeToolClass::InspectionOnly
            {
                parent
                    .workspace
                    .build
                    .as_ref()
                    .and_then(SharedOverlayResource::latest_snapshot)
                    .or(build)
            } else {
                build
            };
            // Keep siblings queued until the worktree created by this
            // publication is finalized. Otherwise the next sibling can race
            // its Git capture against that finalization and fall back cold.
            let admitted = tidepool_runtime::spawn_blocking_in_span(move || {
                layout.prepare_captured(captured, policy, build, Some(donor_view))
            })
            .await
            .map_err(io::Error::other)?;
            drop(publication);
            admitted
        })
        .await
        .map_err(io::Error::other)?
    }
}

impl BoundWorkspace {
    pub(super) async fn settle_publication(
        &self,
        publication: &mut WorkspacePublication,
        backend: &dyn InteractiveAgentBackend,
    ) -> io::Result<()> {
        if !publication.is_pending() {
            return Ok(());
        }
        // A lost begin reply needs replay to learn which admission we own.
        // Once identity is known, descriptor-capture failure must not prevent
        // releasing that admission; retained mount operations settle separately.
        if !publication.has_identity() {
            let reply = publication.begin(backend, &self.thread).await;
            if !publication.is_pending() {
                return Ok(());
            }
            if !publication.has_identity() {
                return match reply {
                    Err(error) => Err(error),
                    _ => Err(io::Error::other("workspace admission remains unsettled")),
                };
            }
        }
        for resource in self
            .workspace
            .source
            .iter()
            .chain(self.workspace.build.iter())
        {
            let mut resource = resource.publication.clone().lock_owned().await;
            tidepool_runtime::spawn_blocking_in_span(move || {
                resource
                    .as_mut()
                    .ok_or_else(|| io::Error::other("workspace retired"))?
                    .settle_pending()
            })
            .await
            .map_err(io::Error::other)??;
        }
        publication.finish(backend, &self.thread).await
    }
}

impl WorkspaceLayout {
    fn capture(
        &self,
        authorized: &AuthorizedForkWorkspace,
        namespace: &exomonad_node::MountNamespace,
        source_path: &Path,
        preserved: &[PathBuf],
        mut parent_source: Option<tokio::sync::OwnedMutexGuard<Option<OverlayResourceLease>>>,
        parent_helper_draft: Option<PathBuf>,
    ) -> io::Result<CapturedSource> {
        let files = namespace.retained_view_path(Path::new(ACTOR_PROJECT_ROOT))?;
        let excluded = self.source_exclusions(source_path, files.as_path())?;
        // Native writers and host Git operations are excluded by the caller.
        // Resolve the child's Git baseline only after this source checkpoint.
        // Children mount their own durable .exomonad, so the working-file
        // snapshot omits it. The Git checkpoint still needs its authored files
        // and workspace gitlink; only runtime state is excluded there.
        let mut checkpoint_excluded = excluded
            .iter()
            .filter(|path| path.as_os_str() != std::ffi::OsStr::new(".exomonad"))
            .cloned()
            .collect::<Vec<_>>();
        checkpoint_excluded.extend(
            exomonad_worktree::git::EXOMONAD_LOCAL_EXCLUDES
                .iter()
                .map(|path| std::ffi::OsString::from(path.trim_matches('/'))),
        );
        checkpoint_excluded.push(".exomonad/helpers".into());
        self.worktrees
            .git()
            .checkpoint_source(source_path, &checkpoint_excluded)
            .map_err(io::Error::other)?;
        let git = authorized
            .prepare_source()
            .map_err(|error| io::Error::other(format!("{error:?}")))?;
        let helper_branch = git.receipt().worktree_id.as_str().to_owned();
        if let Some(parent_draft) = parent_helper_draft.as_deref() {
            self.inherit_helper_branch(parent_draft, &helper_branch)?;
        }
        let source_pathname = self
            .resource_root(git.receipt().worktree_id.as_str())
            .join("source");
        let (source, fallback) = if let Some(parent_source) = &mut parent_source {
            let parent_source = parent_source
                .as_mut()
                .ok_or_else(|| io::Error::other("source workspace retired"))?;
            let snapshot = match parent_source.unchanged_snapshot()? {
                Some(snapshot) => Ok(snapshot),
                None => match parent_source.publish(
                    namespace,
                    Path::new(ACTOR_PROJECT_ROOT),
                    preserved,
                )? {
                    exomonad_node::OverlayRotationOutcome::Rotated => {
                        Ok(parent_source.latest_snapshot().ok_or_else(|| {
                            io::Error::other("source rotation published no generation")
                        })?)
                    }
                    exomonad_node::OverlayRotationOutcome::Unconfirmed(detail) => {
                        return Err(io::Error::other(detail))
                    }
                    exomonad_node::OverlayRotationOutcome::Busy => Err(SourceFallback::Busy),
                    outcome => Err(SourceFallback::Unavailable(format!("{outcome:?}"))),
                },
            };
            match snapshot {
                Ok(snapshot) => (
                    Some(OverlayResourceLease::allocate_path(
                        source_pathname,
                        Some(snapshot),
                    )?),
                    None,
                ),
                Err(fallback) => (None, Some(fallback)),
            }
        } else {
            let selection = self.source_selection(source_path, &excluded)?;
            let inherited = self.reusable_import(source_path, &excluded);
            let source = OverlayResourceLease::allocate_path(source_pathname, inherited.clone())?;
            if inherited.is_some() {
                tracing::info!(path = %source_path.display(), "reused imported source base");
                (Some(source), None)
            } else {
                let import_started = std::time::Instant::now();
                tracing::info!(path = %source_path.display(), exclusions = ?excluded, selected_paths = selection.paths.len(), "selected source import");
                let imported = source
                    .import_source(source_path, &selection, self.source_import)
                    .and_then(|()| {
                        if excluded == self.source_exclusions(source_path, files.as_path())?
                            && selection == self.source_selection(source_path, &excluded)?
                        {
                            self.remember_import(source_path, &excluded, &selection, &source)
                        } else {
                            Err(io::Error::new(
                                io::ErrorKind::WouldBlock,
                                "source exclusions changed during import",
                            ))
                        }
                    });
                match imported {
                    Ok(()) => {
                        tracing::info!(
                            path = %source_path.display(),
                            import_ms = import_started.elapsed().as_millis() as u64,
                            "imported source base"
                        );
                        (Some(source), None)
                    }
                    Err(error) => {
                        source.discard_unsubmitted()?;
                        (None, Some(SourceFallback::ImportFailed(error.to_string())))
                    }
                }
            }
        };
        Ok(CapturedSource {
            git,
            source,
            fallback,
        })
    }

    fn publish_build(
        namespace: &MountNamespace,
        mut parent_build: Option<tokio::sync::OwnedMutexGuard<Option<OverlayResourceLease>>>,
    ) -> io::Result<()> {
        if let Some(build) = &mut parent_build {
            let build = build
                .as_mut()
                .ok_or_else(|| io::Error::other("build workspace retired"))?;
            if build.unchanged_snapshot()?.is_none() {
                let outcome = build.publish(
                    namespace,
                    &PathBuf::from(ACTOR_PROJECT_ROOT).join(ACTOR_BUILD_TARGET),
                    &[],
                )?;
                tracing::info!(?outcome, "workspace build snapshot publication");
                if let exomonad_node::OverlayRotationOutcome::Unconfirmed(detail) = outcome {
                    return Err(io::Error::other(detail));
                }
            }
        }
        Ok(())
    }

    fn prepare_captured(
        &self,
        captured: CapturedSource,
        policy: exomonad_actor::ForkWorkspacePolicy,
        build: Option<OverlaySnapshot>,
        donor: Option<MountNamespace>,
    ) -> io::Result<AdmittedWorkspace> {
        let CapturedSource {
            git,
            source,
            fallback,
        } = captured;
        let id = git.receipt().worktree_id.clone();
        let path = git.receipt().cwd.clone();
        if source.is_none() {
            tracing::info!(?fallback, "using committed source fallback");
            let handle = self
                .worktrees
                .finish_committed_source(git)
                .map_err(io::Error::other)?;
            self.restore_fallback_mtimes(&handle, donor.as_ref());
            let workspace = self.prepare(
                path,
                Some(id.clone()),
                id.as_str(),
                false,
                policy,
                None,
                build,
                None,
            )?;
            return Ok(AdmittedWorkspace {
                handle: handle_to_wire(&handle),
                workspace,
                notice: fallback.map(|reason| reason.notice()),
            });
        }
        let workspace = self.prepare(
            path,
            Some(id.clone()),
            id.as_str(),
            false,
            policy,
            source,
            build,
            None,
        )?;
        let handle = self
            .worktrees
            .finish_inherited_source(git, workspace.view.clone(), Path::new(ACTOR_PROJECT_ROOT))
            .map_err(io::Error::other)?;
        Ok(AdmittedWorkspace {
            handle: handle_to_wire(&handle),
            workspace,
            notice: None,
        })
    }

    fn prepare_committed(
        &self,
        authorized: AuthorizedForkWorkspace,
        policy: exomonad_actor::ForkWorkspacePolicy,
        build: Option<OverlaySnapshot>,
        fallback: Option<SourceFallback>,
        donor: Option<MountNamespace>,
        parent_helper_draft: Option<PathBuf>,
    ) -> io::Result<AdmittedWorkspace> {
        if fallback.is_some() {
            tracing::info!(?fallback, "using committed source fallback");
        }
        let handle = authorized
            .materialize_committed()
            .map_err(|error| io::Error::other(format!("{error:?}")))?;
        let id = WorktreeId::from_raw(&handle.handle_receipt.tree_id.raw);
        if let Some(parent_draft) = parent_helper_draft.as_deref() {
            self.inherit_helper_branch(parent_draft, id.as_str())?;
        }
        if let Some(donor) = donor.as_ref() {
            let domain_handle = self
                .worktrees
                .lookup(&id)
                .map_err(io::Error::other)?
                .ok_or_else(|| io::Error::other("materialized checkout missing from registry"))?;
            self.restore_fallback_mtimes(&domain_handle, Some(donor));
        }
        let workspace = self.prepare(
            PathBuf::from(&handle.handle_receipt.cwd),
            Some(id.clone()),
            id.as_str(),
            false,
            policy,
            None,
            build,
            None,
        )?;
        Ok(AdmittedWorkspace {
            handle,
            workspace,
            notice: fallback.map(|reason| reason.notice()),
        })
    }

    fn restore_fallback_mtimes(
        &self,
        handle: &exomonad_worktree::WorktreeHandle,
        donor: Option<&MountNamespace>,
    ) {
        if let Some(donor) = donor {
            match self.worktrees.restore_matching_mtimes_from_view(
                handle,
                donor,
                Path::new(ACTOR_PROJECT_ROOT),
            ) {
                Ok(restored) => tracing::debug!(restored, "restored matching checkout mtimes"),
                Err(error) => {
                    tracing::warn!(%error, "committed checkout mtime restoration skipped")
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "workspace_tests.rs"]
mod tests;
