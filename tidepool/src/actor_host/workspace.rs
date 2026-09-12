//! Prepare one complete workspace before deferred actor/native startup.

use super::overlay_resource::{source_inventory, source_manifest, SourceManifest, SourceStamp};
use super::workspace_publication::WorkspacePublication;
use super::*;
use std::ffi::OsString;
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

pub(super) struct RootImport {
    inventory: std::collections::BTreeMap<PathBuf, SourceStamp>,
    exclusions: Vec<OsString>,
    manifest: SourceManifest,
    snapshot: OverlaySnapshot,
}

#[derive(Clone)]
pub(super) struct WorkspaceLayout {
    pub(super) run_namespace: String,
    pub(super) source_root: PathBuf,
    pub(super) source_exclude: Vec<String>,
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
    pub(super) view: tidepool_node::MountNamespace,
    pub(super) source: Option<SharedOverlayResource>,
    pub(super) build: Option<SharedOverlayResource>,
    source_preserved_mounts: Vec<PathBuf>,
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
    fn reusable_import(&self, source: &Path, excluded: &[OsString]) -> Option<OverlaySnapshot> {
        let candidate = self.root_imports.lock().get(source).cloned()?;
        if candidate.exclusions != excluded {
            return None;
        }
        let excluded = excluded.iter().map(OsString::as_os_str).collect::<Vec<_>>();
        let before = source_inventory(source, &excluded).ok()?;
        if before != candidate.inventory {
            return None;
        }
        let manifest = source_manifest(source, &excluded).ok()?;
        let after = source_inventory(source, &excluded).ok()?;
        (before == after && manifest == candidate.manifest).then(|| candidate.snapshot.clone())
    }

    fn remember_import(
        &self,
        source_path: &Path,
        excluded: &[OsString],
        source: &OverlayResourceLease,
    ) -> io::Result<()> {
        let excluded_refs = excluded.iter().map(OsString::as_os_str).collect::<Vec<_>>();
        let before = source_inventory(source_path, &excluded_refs)?;
        let original = match source_manifest(source_path, &excluded_refs) {
            Ok(manifest) => manifest,
            Err(error) => {
                tracing::debug!(%error, "source manifest unavailable; import will not be reused");
                return Ok(());
            }
        };
        let after = source_inventory(source_path, &excluded_refs)?;
        if before != after {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "source changed while verifying imported base",
            ));
        }
        let (base, snapshot) = source.imported_base()?;
        let copied = match source_manifest(
            base,
            &[std::ffi::OsStr::new(".git"), std::ffi::OsStr::new(".shoal")],
        ) {
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
                manifest: original,
                snapshot,
            }),
        );
        Ok(())
    }

    fn source_exclusions(&self, source: &Path) -> io::Result<Vec<std::ffi::OsString>> {
        let mut excluded = vec![".git".into(), ".shoal".into()];
        let git = self.worktrees.git();
        for name in &self.source_exclude {
            if crate::shoal::source_directory_has_tracked(git, source, name)? {
                return Err(io::Error::other(format!(
                    "configured source exclusion {name:?} contains tracked files"
                )));
            }
            excluded.push(name.into());
        }
        for entry in std::fs::read_dir(source)? {
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
            if !crate::shoal::source_directory_has_tracked(git, source, name_text)? {
                excluded.push(name);
            }
        }
        excluded.sort();
        Ok(excluded)
    }

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
        if let Some(source) = &mut source {
            source.prepare_root_metadata()?;
            boundary = source.mount(boundary, &visible).map_err(io::Error::other)?;
            boundary = boundary
                .with_read_only_overlay(host_path.join(".git"), visible.join(".git"))
                .map_err(io::Error::other)?;
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
            view,
            source: source.map(SharedOverlayResource::new),
            build: build.map(SharedOverlayResource::new),
            source_preserved_mounts,
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
                layout.prepare_committed(authorized, policy, build, reason, None)
            })
            .await
            .map_err(io::Error::other)?;
        };
        // Sibling forks queue on the same source publication. Contention here
        // says nothing about native writers or the source's availability.
        // A caller cancelled while waiting has not begun an operation.
        let wait_started = std::time::Instant::now();
        let mut publication = parent.workspace.publication.clone().lock_owned().await;
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
            return tokio::task::spawn_blocking(move || {
                layout.prepare_committed(
                    authorized,
                    policy,
                    build,
                    Some(SourceFallback::Unavailable(
                        "source owner retired while publication was queued".into(),
                    )),
                    None,
                )
            })
            .await
            .map_err(io::Error::other)?;
        }
        // The operation task retains its gate and resources even when its caller
        // abandons the await. Host death ends the wave instead of replaying it.
        let backend = self.backend.clone();
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
                    return tokio::task::spawn_blocking(move || {
                        layout.prepare_committed(
                            authorized,
                            policy,
                            build,
                            Some(SourceFallback::Busy),
                            Some(donor_view),
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
                            Some(donor_view),
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
            let preserved = parent.workspace.source_preserved_mounts.clone();
            let captured = tokio::task::spawn_blocking(move || {
                let captured = capture_layout
                    .worktrees
                    .git()
                    .try_capture()
                    .map(|_admission| {
                        capture_layout.capture(
                            &authorized,
                            &namespace,
                            &host_path,
                            &preserved,
                            source,
                            cache,
                        )
                    });
                (authorized, captured)
            })
            .await
            .map_err(io::Error::other);
            parent
                .settle_publication(&mut publication, backend.as_ref())
                .await?;
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
            // Keep siblings queued until the worktree created by this
            // publication is finalized. Otherwise the next sibling can race
            // its Git capture against that finalization and fall back cold.
            let admitted = tokio::task::spawn_blocking(move || match captured {
                Some(captured) => match captured? {
                    SourceCapture::Ready(captured) => {
                        layout.prepare_captured(captured, policy, build, Some(donor_view))
                    }
                    SourceCapture::Fallback(reason) => layout.prepare_committed(
                        authorized,
                        policy,
                        build,
                        Some(reason),
                        Some(donor_view),
                    ),
                },
                None => layout.prepare_committed(
                    authorized,
                    policy,
                    build,
                    Some(SourceFallback::Busy),
                    Some(donor_view),
                ),
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
        preserved: &[PathBuf],
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
            let snapshot = match parent_source.unchanged_snapshot()? {
                Some(snapshot) => Ok(snapshot),
                None => match parent_source.publish(
                    namespace,
                    Path::new(ACTOR_PROJECT_ROOT),
                    preserved,
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
            let excluded = self.source_exclusions(source_path)?;
            let inherited = self.reusable_import(source_path, &excluded);
            let source = OverlayResourceLease::allocate_path(source_pathname, inherited.clone())?;
            if inherited.is_some() {
                tracing::info!(path = %source_path.display(), "reused imported source base");
                (Some(source), None)
            } else {
                let import_started = std::time::Instant::now();
                let excluded_refs = excluded
                    .iter()
                    .map(std::ffi::OsString::as_os_str)
                    .collect::<Vec<_>>();
                let imported = source
                    .import_source(source_path, &excluded_refs)
                    .and_then(|()| {
                        if excluded == self.source_exclusions(source_path)? {
                            self.remember_import(source_path, &excluded, &source)
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
                    Err(error) => (None, Some(SourceFallback::ImportFailed(error.to_string()))),
                }
            }
        };
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
        donor: Option<MountNamespace>,
    ) -> io::Result<AdmittedWorkspace> {
        if fallback.is_some() {
            tracing::info!(?fallback, "using committed source fallback");
        }
        let handle = authorized
            .materialize_committed()
            .map_err(|error| io::Error::other(format!("{error:?}")))?;
        let id = WorktreeId::from_raw(&handle.handle_receipt.tree_id.raw);
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
        )?;
        Ok(AdmittedWorkspace {
            handle,
            workspace,
            notice: fallback.map(|reason| reason.notice()),
        })
    }

    fn restore_fallback_mtimes(
        &self,
        handle: &tidepool_worktree::WorktreeHandle,
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
