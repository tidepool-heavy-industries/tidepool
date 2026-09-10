//! Prepare one complete workspace before deferred actor/native startup.

use super::workspace_publication::WorkspacePublication;
use super::*;
use std::io;
use tidepool_bridge_effects::WtWorktreeHandle;
use tidepool_handlers::handlers::worktree::{handle_to_wire, AuthorizedForkWorkspace};
use tidepool_node::MountNamespace;
use tidepool_worktree::{PreparedSourceWorktree, WorktreeSource};
use workspace_publication::Admission;

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

enum SourceCapture {
    Ready(CapturedSource),
    Fallback(SourceFallback),
}

struct CapturedSource {
    git: PreparedSourceWorktree,
    source: Option<OverlayResourceLease>,
    fallback: Option<SourceFallback>,
}

#[derive(Clone)]
pub(super) struct WorkspaceLayout {
    pub(super) run_namespace: String,
    pub(super) source_root: PathBuf,
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
    pub(super) view: tidepool_node::MountNamespace,
    pub(super) source: Option<SharedOverlayResource>,
    pub(super) build: Option<SharedOverlayResource>,
    pub(super) owns_source: bool,
    pub(super) publication: Arc<tokio::sync::Mutex<WorkspacePublication>>,
}

pub(super) struct ActiveWorkspace {
    prepared: Arc<PreparedWorkspace>,
    pub(super) view: tidepool_node::MountNamespace,
}

impl std::ops::Deref for ActiveWorkspace {
    type Target = PreparedWorkspace;
    fn deref(&self) -> &Self::Target {
        &self.prepared
    }
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
        tokio::task::spawn_blocking(move || -> io::Result<()> {
            if let Some(id) = worktree {
                manager
                    .materialize_retired_view(&id, &active, Path::new(ACTOR_PROJECT_ROOT))
                    .map_err(io::Error::other)?;
            }
            active.detach_retired_tree(Path::new(ACTOR_PROJECT_ROOT))?;
            prepared.detach_retired_tree(Path::new(ACTOR_PROJECT_ROOT))
        })
        .await
        .map_err(io::Error::other)??;
        for resource in self.source.iter().chain(self.build.iter()) {
            resource.retire().await?;
        }
        Ok(())
    }

    pub(super) fn activate(
        self: Arc<Self>,
        worktrees: &WorktreeManager,
        view: tidepool_node::MountNamespace,
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
            let expected = tidepool_worktree::git::inspect::git_dir(
                &git.with_mount_namespace(self.view.clone()),
                Path::new(ACTOR_PROJECT_ROOT),
            )
            .map_err(io::Error::other)?;
            let observed = tidepool_worktree::git::inspect::git_dir(
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
    pub(super) fn resource_root(&self, key: &str) -> PathBuf {
        // Actor IDs restart in each run; retained resources belong to that run.
        self.worktrees
            .managed_root()
            .join(".resources")
            .join(&self.run_namespace)
            .join(key)
    }

    pub(super) fn prepare(
        &self,
        host_path: PathBuf,
        worktree: Option<WorktreeId>,
        key: &str,
        root: bool,
        policy: tidepool_actor::ForkWorkspacePolicy,
        mut source: Option<OverlayResourceLease>,
        inherited_build: Option<OverlaySnapshot>,
    ) -> io::Result<Arc<PreparedWorkspace>> {
        let visible = PathBuf::from(ACTOR_PROJECT_ROOT);
        let common =
            tidepool_worktree::git::inspect::git_common_dir(self.worktrees.git(), &host_path)
                .map_err(io::Error::other)?;
        let roots = writable_repository_roots(
            root,
            policy.workspace,
            &self.source_root,
            worktree.as_ref().map(|_| host_path.as_path()),
            &common,
        );
        let resource_root = self.resource_root(key);
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
        if let Some(source) = &source {
            boundary = source.mount(boundary, &visible).map_err(io::Error::other)?;
        }
        let canonical = self.source_root.join(".shoal");
        if canonical.is_dir() {
            boundary = if root {
                boundary.with_writable_overlay(&canonical, visible.join(".shoal"))
            } else {
                boundary.with_read_only_overlay(&canonical, visible.join(".shoal"))
            }
            .map_err(io::Error::other)?;
        }
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
        if !root && policy.workspace != tidepool_actor::WorkspaceAccess::WritableBound {
            boundary = boundary.with_read_only_project();
        }
        // The short bootstrap may acquire mounts even if its receipt is lost.
        for resource in source.iter_mut().chain(build.iter_mut()) {
            resource.process_may_exist();
        }
        let view = boundary.prepare_view(
            BUBBLEWRAP_PROGRAM,
            std::time::Instant::now() + PROCESS_OPERATION_TIMEOUT,
        )?;
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
            view,
            source: source.map(SharedOverlayResource::new),
            build: build.map(SharedOverlayResource::new),
            owns_source: root || policy.workspace == tidepool_actor::WorkspaceAccess::WritableBound,
            publication: Arc::new(tokio::sync::Mutex::new(WorkspacePublication::default())),
        }))
    }
}

impl NativeForkAdmission {
    pub(super) async fn prepare_workspace(
        &self,
        creator: ActorRef,
        authorized: AuthorizedForkWorkspace,
        policy: tidepool_actor::ForkWorkspacePolicy,
    ) -> io::Result<AdmittedWorkspace> {
        let layout = self
            .layout
            .clone()
            .ok_or_else(|| io::Error::other("workspace layout unavailable"))?;
        let build = self.build_snapshot(creator, policy.native_tools).await;
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
            return tokio::task::spawn_blocking(move || {
                layout.prepare_committed(authorized, policy, build, reason)
            })
            .await
            .map_err(io::Error::other)?;
        };
        let mut publication = match parent.workspace.publication.clone().try_lock_owned() {
            Ok(publication) => publication,
            Err(_) => {
                return tokio::task::spawn_blocking(move || {
                    layout.prepare_committed(authorized, policy, build, Some(SourceFallback::Busy))
                })
                .await
                .map_err(io::Error::other)?
            }
        };
        // The operation task retains its gate and resources even when its caller
        // abandons the await. Host death ends the wave instead of replaying it.
        let backend = self.backend.clone();
        tokio::spawn(async move {
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
                    return tokio::task::spawn_blocking(move || {
                        layout.prepare_committed(
                            authorized,
                            policy,
                            build,
                            Some(SourceFallback::Busy),
                        )
                    })
                    .await
                    .map_err(io::Error::other)?
                }
                Ok(Admission::Unavailable(detail)) => {
                    return tokio::task::spawn_blocking(move || {
                        layout.prepare_committed(
                            authorized,
                            policy,
                            build,
                            Some(SourceFallback::Unavailable(detail)),
                        )
                    })
                    .await
                    .map_err(io::Error::other)?
                }
                Err(error) => {
                    // If identity is known this may immediately finish; a lost
                    // begin reply remains owned for the fleet's next retry.
                    let _ = parent
                        .settle_publication(&mut publication, backend.as_ref())
                        .await;
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
            let captured = tokio::task::spawn_blocking(move || {
                let captured = capture_layout
                    .worktrees
                    .git()
                    .try_capture()
                    .map(|_admission| {
                        capture_layout.capture(&authorized, &namespace, &host_path, source, cache)
                    });
                (authorized, captured)
            })
            .await
            .map_err(io::Error::other);
            parent
                .settle_publication(&mut publication, backend.as_ref())
                .await?;
            drop(publication);
            let (authorized, captured) = captured?;
            let build = if source_owner == creator
                && policy.native_tools != tidepool_actor::NativeToolClass::InspectionOnly
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
            tokio::task::spawn_blocking(move || match captured {
                Some(captured) => match captured? {
                    SourceCapture::Ready(captured) => {
                        layout.prepare_captured(captured, policy, build)
                    }
                    SourceCapture::Fallback(reason) => {
                        layout.prepare_committed(authorized, policy, build, Some(reason))
                    }
                },
                None => {
                    layout.prepare_committed(authorized, policy, build, Some(SourceFallback::Busy))
                }
            })
            .await
            .map_err(io::Error::other)?
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
            tokio::task::spawn_blocking(move || {
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
        namespace: &tidepool_node::MountNamespace,
        source_path: &Path,
        mut parent_source: Option<tokio::sync::OwnedMutexGuard<Option<OverlayResourceLease>>>,
        mut parent_build: Option<tokio::sync::OwnedMutexGuard<Option<OverlayResourceLease>>>,
    ) -> io::Result<SourceCapture> {
        let git = match authorized.prepare_source() {
            Ok(git) => git,
            Err(tidepool_handlers::WorktreeError::SourceOperationInProgress(kind)) => {
                return Ok(SourceCapture::Fallback(SourceFallback::Unavailable(
                    format!("Git operation in progress: {kind:?}"),
                )));
            }
            Err(error) => return Err(io::Error::other(format!("{error:?}"))),
        };
        let source_pathname = self
            .resource_root(git.receipt().worktree_id.as_str())
            .join("source");
        let (source, fallback) = if let Some(parent_source) = &mut parent_source {
            let parent_source = parent_source
                .as_mut()
                .ok_or_else(|| io::Error::other("source workspace retired"))?;
            let preserved = [PathBuf::from(ACTOR_PROJECT_ROOT).join(".shoal")];
            let snapshot = match parent_source.unchanged_snapshot()? {
                Some(snapshot) => Ok(snapshot),
                None => match parent_source.publish(
                    namespace,
                    Path::new(ACTOR_PROJECT_ROOT),
                    &preserved,
                )? {
                    tidepool_node::OverlayRotationOutcome::Rotated => {
                        Ok(parent_source.latest_snapshot().ok_or_else(|| {
                            io::Error::other("source rotation published no generation")
                        })?)
                    }
                    tidepool_node::OverlayRotationOutcome::Unconfirmed(detail) => {
                        return Err(io::Error::other(detail))
                    }
                    tidepool_node::OverlayRotationOutcome::Busy => Err(SourceFallback::Busy),
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
            let source = OverlayResourceLease::allocate_path(source_pathname, None)?;
            // Exclude Cargo's tagged cache, not ordinary source named target.
            // The native private target lives under the separate .shoal mount.
            let mut excluded = vec![std::ffi::OsStr::new(".git"), std::ffi::OsStr::new(".shoal")];
            if source_path.join("Cargo.toml").is_file()
                && std::fs::read(source_path.join("target/CACHEDIR.TAG")).is_ok_and(|tag| {
                    tag.starts_with(b"Signature: 8a477f597d28d172789f06886806bc55")
                })
            {
                excluded.push(std::ffi::OsStr::new("target"));
            }
            match source.import_source(source_path, &excluded) {
                Ok(()) => (Some(source), None),
                Err(error) => (None, Some(SourceFallback::ImportFailed(error.to_string()))),
            }
        };
        if let Some(source) = &source {
            source.prepare_git_pointer(&git.git_file())?;
        }
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
                if let tidepool_node::OverlayRotationOutcome::Unconfirmed(detail) = outcome {
                    return Err(io::Error::other(detail));
                }
            }
        }
        Ok(SourceCapture::Ready(CapturedSource {
            git,
            source,
            fallback,
        }))
    }

    fn prepare_captured(
        &self,
        captured: CapturedSource,
        policy: tidepool_actor::ForkWorkspacePolicy,
        build: Option<OverlaySnapshot>,
    ) -> io::Result<AdmittedWorkspace> {
        let CapturedSource {
            git,
            source,
            fallback,
        } = captured;
        let id = git.receipt().worktree_id.clone();
        let path = git.receipt().cwd.clone();
        if source.is_none() {
            let handle = self
                .worktrees
                .finish_committed_source(git)
                .map_err(io::Error::other)?;
            let workspace = self.prepare(
                path,
                Some(id.clone()),
                id.as_str(),
                false,
                policy,
                None,
                build,
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
        policy: tidepool_actor::ForkWorkspacePolicy,
        build: Option<OverlaySnapshot>,
        fallback: Option<SourceFallback>,
    ) -> io::Result<AdmittedWorkspace> {
        let handle = authorized
            .materialize_committed()
            .map_err(|error| io::Error::other(format!("{error:?}")))?;
        let id = WorktreeId::from_raw(&handle.handle_receipt.tree_id.raw);
        let workspace = self.prepare(
            PathBuf::from(&handle.handle_receipt.cwd),
            Some(id.clone()),
            id.as_str(),
            false,
            policy,
            None,
            build,
        )?;
        Ok(AdmittedWorkspace {
            handle,
            workspace,
            notice: fallback.map(|reason| reason.notice()),
        })
    }
}

#[cfg(test)]
#[path = "workspace_tests.rs"]
mod tests;
