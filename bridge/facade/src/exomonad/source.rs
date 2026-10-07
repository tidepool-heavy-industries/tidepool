//! Live source layers: the run's authored tooling and branch-local notebook helpers.
//!
//! An ordinary run freezes its authored Haskell roots under
//! `<run_root>/workspace/sources/<capture>/<index>`, and verifies that capture
//! when the run is reloaded ([`super::workspace::FrozenWorkspace::load`]). A
//! completed prepared workspace keeps those roots in its separately owned
//! deployment; the run publishes a local active link to that exact prepared
//! source revision as its initial live revision. Neither path is rewritten by
//! this module. Later live reloads publish run-owned revisions in front of
//! that immutable floor. The run's layer is shared by every actor:
//!
//! ```text
//! <run_root>/workspace/revisions/<identity>/{0,1,…,resources}
//! <run_root>/workspace/active -> revisions/<identity>
//! ```
//!
//! For a prepared workspace, the initial `revisions/<identity>` entry points
//! to the deployment-owned source revision; subsequent candidates are regular
//! run-owned revision directories.
//!
//! The generated `Exomonad.Workspace` resource is retained as its own exact
//! root after the run source roots. A managed checkout's historical
//! `.exomonad` package is not silently added to the graph. Every actor uses the
//! run's current tooling, while `SessionHelpers` remain branch-local.
//!
//! Publishing a revision is one `rename(2)` of a symlink: a compile that opens
//! `active/0` sees either the whole previous revision or the whole new one,
//! never a mixture. Workbench and checkpoint admission resolve that publication
//! to immutable revision paths and issue an opaque source capsule. Its clones
//! retain the configured run owner until every admitted capture releases.
//!
//! `exomonad check --recipes` compiles against the frozen capture. A run reload
//! that removes an authored module is rejected before the frozen floor can
//! expose it. AgentSpec reload stages an immutable source capsule and prepares
//! its installer before declared-surface comparison. The existing publication
//! owner replaces source and the actor's installed dispatcher at the same
//! visibility boundary; durability confirmation follows that paired commit.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::{fs::File, os::unix::fs::OpenOptionsExt};

use exomonad_worktree::GitCli;
use parking_lot::{Mutex, RwLock};
use tidepool_repr::PrincipalId;

use super::workspace::FrozenWorkspace;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Separates a source revision's content identity from every other identity
/// derived from the same per-file manifests.
const DOMAIN: &[u8] = b"tidepool-exomonad-source-revision-v1";

/// The generated module each revision carries, so an authored module can
/// record the source snapshot it was compiled against.
const REVISION_MODULE: &str = "Exomonad/Source/Revision.hs";

/// One captured state of the workspace's source roots.
///
/// `identity` describes the ordered source snapshot, independently from
/// compiler dependency selection. `generation` is the publication ordinal:
/// it is 1-based, and 0 means "this snapshot has never been published".
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SourceRevision {
    pub(crate) identity: String,
    pub(crate) generation: u64,
    /// Every module the revision provides, by module name, with the hex digest
    /// of its source. Ordered by module name.
    pub(crate) modules: Vec<(String, String)>,
}

impl SourceRevision {
    /// Module names whose source differs from `previous`, including modules
    /// `previous` did not have.
    fn changed_since(&self, previous: &Self) -> Vec<String> {
        let before: BTreeMap<&str, &str> = previous
            .modules
            .iter()
            .map(|(name, digest)| (name.as_str(), digest.as_str()))
            .collect();
        self.modules
            .iter()
            .filter(|(name, digest)| before.get(name.as_str()) != Some(&digest.as_str()))
            .map(|(name, _)| name.clone())
            .collect()
    }
}

/// Temporary source bytes are removed unless explicitly retained.
struct CapturedRevision {
    directory: tempfile::TempDir,
    revision: SourceRevision,
}

/// A retained revision that exists on disk but is not yet the active one.
pub(crate) struct PendingRevision {
    directory: PathBuf,
    source_roots: Vec<PathBuf>,
    revision: SourceRevision,
}

impl PendingRevision {
    pub(crate) fn revision(&self) -> &SourceRevision {
        &self.revision
    }

    /// The include roots that stand in for `active/*` while this candidate is
    /// being checked. Same order, same count.
    pub(crate) fn include_paths(&self, roots: usize) -> Vec<PathBuf> {
        revision_include_paths(&self.directory, roots)
    }
}

/// One mutable source layer: its revisions and the symlink naming the live one.
/// The run and each helper branch have separate layers.
#[derive(Clone, Debug)]
pub(crate) struct SourceLayer {
    directory: PathBuf,
    retained_revision: Arc<Mutex<Option<RetainedRevision>>>,
}

#[derive(Clone, Debug)]
struct RetainedRevision {
    identity: String,
    paths: Vec<PathBuf>,
    manifests: Arc<[tidepool_toolchain::cache::SourceRootManifest]>,
}

impl SourceLayer {
    /// Resolve one active publication to its immutable revision directory.
    fn checkpoint_revision(&self, domain: &str) -> Result<RetainedRevision> {
        let record = self
            .read_record()?
            .ok_or("checkpoint source layer has no active revision")?;
        let path = self.revisions().join(&record.identity);
        let mut retained = self.retained_revision.lock();
        if let Some(revision) = retained.as_ref() {
            if revision.identity == record.identity {
                return Ok(revision.clone());
            }
        }
        let revision = inspect_retained_revision(domain, &record.identity, &path, record.roots)?;
        *retained = Some(revision.clone());
        Ok(revision)
    }
    /// The run's own layer, shared by every actor.
    pub(crate) fn new(run_root: &Path) -> Self {
        Self {
            directory: run_root.join("workspace"),
            retained_revision: Default::default(),
        }
    }

    /// One branch-local active snapshot of the session-owned helper modules.
    /// The writable draft lives beside these layers but is independently
    /// snapshotted when a child workspace is admitted.
    pub(crate) fn helpers(helper_root: &Path, branch: &str) -> Self {
        Self {
            directory: helper_root.join("layers").join(branch),
            retained_revision: Default::default(),
        }
    }

    /// Serialize helper draft publication with fork capture, including across
    /// the source service and workspace admission owners.
    pub(crate) fn lock_helpers(&self) -> Result<File> {
        std::fs::create_dir_all(&self.directory)?;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .mode(0o600)
            .open(self.directory.join("helpers.lock"))?;
        rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive)?;
        Ok(lock)
    }

    pub(crate) fn helper_draft(helper_root: &Path, branch: &str) -> PathBuf {
        helper_root.join("drafts").join(branch)
    }

    fn active_link(&self) -> PathBuf {
        self.directory.join("active")
    }

    fn active_record(&self) -> PathBuf {
        self.directory.join("active.json")
    }

    fn revisions(&self) -> PathBuf {
        self.directory.join("revisions")
    }

    /// The include roots a compile receives in place of the workspace's
    /// source roots. Stable for the life of the run.
    pub(crate) fn include_paths(&self, roots: usize) -> Vec<PathBuf> {
        revision_include_paths(&self.active_link(), roots)
    }

    /// The include roots the layer's owner compiles against, read from the
    /// published record rather than a caller-supplied count.
    pub(crate) fn active_include_paths(&self) -> Result<Vec<PathBuf>> {
        let record = self.read_record()?;
        Ok(record
            .map(|record| revision_include_paths(&self.active_link(), record.roots))
            .unwrap_or_default())
    }

    /// Materialize revision one from the run's own frozen capture, unless a
    /// revision is already active. Idempotent, and the only way `active` comes
    /// into existence: every compile in the run needs it to resolve.
    pub(crate) fn ensure_active(&self, frozen: &FrozenWorkspace) -> Result<SourceRevision> {
        if let Some(revision) = frozen.prepared_source_revision()? {
            return self.ensure_active_from_prepared(frozen.identity(), &revision);
        }
        self.ensure_active_from(frozen.identity(), frozen.captured_source_roots())
    }

    /// Publish a validated prepared source tree as this run's initial source
    /// revision. The run owns its active link and publication record; the
    /// revision directory itself remains owned by the prepared deployment.
    fn ensure_active_from_prepared(
        &self,
        domain: &str,
        prepared_revision: &Path,
    ) -> Result<SourceRevision> {
        if let Some(active) = self.read_active()? {
            return Ok(active);
        }
        let prepared_revision = std::fs::canonicalize(prepared_revision)?;
        let roots = revision_root_count(&prepared_revision)?;
        // A prepared directory is named by its preparation revision, whereas
        // the live source identity is derived from the run's frozen domain.
        // Validate its contents under that identity before retaining it.
        let manifests = source_manifests(&prepared_revision, roots)?;
        let revision = source_revision(domain, &manifests[..roots]);
        validate_revision_resource(&prepared_revision, roots, &revision.identity, &manifests)?;
        let target = self.revisions().join(&revision.identity);
        std::fs::create_dir_all(self.revisions())?;
        match std::os::unix::fs::symlink(&prepared_revision, &target) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if std::fs::canonicalize(&target)? != prepared_revision {
                    return Err(
                        "prepared source revision identity is already owned by another path".into(),
                    );
                }
            }
            Err(error) => return Err(error.into()),
        }
        let pending = PendingRevision {
            directory: target,
            source_roots: Vec::new(),
            revision,
        };
        self.publish(pending)
    }

    /// As [`Self::ensure_active`], for a layer with no frozen capture of its
    /// own: revision one is captured from `roots` as they stand.
    pub(crate) fn ensure_active_from(
        &self,
        domain: &str,
        roots: &[PathBuf],
    ) -> Result<SourceRevision> {
        if let Some(active) = self.read_active()? {
            return Ok(active);
        }
        let pending = self.capture_from_roots(domain, roots)?;
        self.publish(pending)
    }

    /// Seed a private layer from another layer's current immutable revision.
    /// The source bytes may be shared by identity, but the new layer gets its
    /// own active link so later publication by either owner cannot upgrade the
    /// other. `roots` are the draft roots this branch will reload from.
    pub(crate) fn inherit_active_from(
        &self,
        parent: &SourceLayer,
        roots: &[PathBuf],
    ) -> Result<SourceRevision> {
        if let Some(active) = self.read_active()? {
            return Ok(active);
        }
        let active = parent
            .read_active()?
            .ok_or("parent source layer has no active revision to inherit")?;
        let parent_revision = parent.revisions().join(&active.identity);
        let child_revision = self.revisions().join(&active.identity);
        std::fs::create_dir_all(self.revisions())?;
        if !child_revision.exists() {
            let staged = self
                .revisions()
                .join(format!(".inherit-{}", uuid::Uuid::new_v4()));
            copy_revision_tree(&parent_revision, &staged)?;
            if let Err(error) = std::fs::rename(&staged, &child_revision) {
                let _ = std::fs::remove_dir_all(&staged);
                return Err(error.into());
            }
            tidepool_atomic_write::sync_parent_directory(&child_revision)?;
        }
        let pending = PendingRevision {
            directory: child_revision,
            source_roots: roots.to_vec(),
            revision: SourceRevision {
                generation: 0,
                ..active
            },
        };
        self.publish(pending)
    }

    fn read_record(&self) -> Result<Option<ActiveRecord>> {
        let target = match std::fs::read_link(self.active_link()) {
            Ok(target) => target,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let mut components = target.components();
        let valid = matches!(components.next(), Some(std::path::Component::Normal(part)) if part == "revisions");
        let identity = match components.next() {
            Some(std::path::Component::Normal(identity)) if components.next().is_none() => {
                identity.to_str().filter(|identity| {
                    identity.len() == 64 && identity.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
            }
            _ => None,
        }
        .ok_or("active source link has an invalid revision target")?
        .to_owned();
        if !valid {
            return Err("active source link escapes the revision store".into());
        }
        let revision = self.revisions().join(&identity);
        if !revision.is_dir() {
            return Err("active source revision directory is missing".into());
        }
        let roots = revision_root_count(&revision)?;
        let recorded = match std::fs::read(self.active_record()) {
            Ok(bytes) => Some(serde_json::from_slice::<ActiveRecord>(&bytes)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        if let Some(record) = recorded.as_ref() {
            if record.identity == identity {
                if record.roots != roots {
                    return Err(
                        "active source record root count does not match its revision".into(),
                    );
                }
                return Ok(recorded);
            }
        }
        // The link is the publication point. A stale or absent side record is
        // the bounded crash window after its rename; reconstruct it from the
        // exact immutable target before admitting another compile.
        let record = ActiveRecord {
            identity,
            generation: recorded.map_or(1, |record| record.generation.saturating_add(1)),
            roots,
        };
        tidepool_atomic_write::write_durable(
            &self.active_record(),
            &serde_json::to_vec_pretty(&record)?,
        )?;
        Ok(Some(record))
    }

    /// The revision currently on the search path, or `None` before the first
    /// one is materialized.
    pub(crate) fn read_active(&self) -> Result<Option<SourceRevision>> {
        let Some(record) = self.read_record()? else {
            return Ok(None);
        };
        let directory = self.revisions().join(&record.identity);
        Ok(Some(SourceRevision {
            modules: revision_modules(&directory, record.roots)?,
            identity: record.identity,
            generation: record.generation,
        }))
    }

    /// Re-read the workspace's declared source roots and capture them as a
    /// candidate revision. Nothing becomes active here.
    ///
    /// The roots are resolved with the run's own frozen configuration, through
    /// the one resolver a freeze uses, so a reload can never widen to
    /// directories the run did not start from. Pinned flake inputs resolve
    /// through the same `nix flake archive` call that pins them, which is what
    /// keeps `[haskell.flake_overrides]` the supported way to develop one.
    pub(crate) fn capture_from_workspace(
        &self,
        frozen: &FrozenWorkspace,
        workspace: &Path,
    ) -> Result<PendingRevision> {
        let config = frozen.config()?;
        let roots = super::workspace::resolve_source_roots(workspace, &config.haskell)?;
        if roots.len() != frozen.captured_source_roots().len() {
            return Err("the workspace's source-root list changed; start a new swarm".into());
        }
        self.capture_from_roots(frozen.identity(), &roots)
    }

    /// Capture `roots` as a candidate revision of this layer. `domain_identity`
    /// frames the content digest so a revision is only ever compared within one
    /// run's configuration, prompts and library build.
    pub(crate) fn capture_from_roots(
        &self,
        domain_identity: &str,
        roots: &[PathBuf],
    ) -> Result<PendingRevision> {
        let CapturedRevision {
            directory: captured,
            revision,
        } = self.capture(domain_identity, roots)?;
        let directory = self.revisions().join(&revision.identity);
        if directory.exists() {
            captured.close()?;
        } else {
            std::fs::rename(captured.path(), &directory)?;
            // The temporary name is gone; the retained revision now owns the tree.
            let _retained = captured.keep();
            tidepool_atomic_write::sync_parent_directory(&directory)?;
        }
        Ok(PendingRevision {
            directory,
            source_roots: roots.to_vec(),
            revision,
        })
    }

    /// What `roots` would be as a revision, without keeping it. A status or
    /// drift read answers a question about the disk; it must not leave a
    /// revision directory behind each time the answer changes.
    pub(crate) fn observe_from_roots(
        &self,
        domain_identity: &str,
        roots: &[PathBuf],
    ) -> Result<SourceRevision> {
        let manifests = roots
            .iter()
            .map(|root| {
                let files = super::workspace::inspect_sources(root)?;
                Ok(tidepool_toolchain::cache::SourceRootManifest::from_file_digests(files)?)
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(source_revision(domain_identity, &manifests))
    }

    /// [`Self::observe_from_roots`] over the workspace's declared roots.
    pub(crate) fn observe_from_workspace(
        &self,
        frozen: &FrozenWorkspace,
        workspace: &Path,
    ) -> Result<SourceRevision> {
        let config = frozen.config()?;
        let roots = super::workspace::resolve_source_roots(workspace, &config.haskell)?;
        if roots.len() != frozen.captured_source_roots().len() {
            return Err("the workspace's source-root list changed; start a new swarm".into());
        }
        self.observe_from_roots(frozen.identity(), &roots)
    }

    fn capture(&self, domain_identity: &str, roots: &[PathBuf]) -> Result<CapturedRevision> {
        std::fs::create_dir_all(self.revisions())?;
        let pending = tempfile::Builder::new()
            .prefix(".pending-")
            .tempdir_in(self.revisions())?;
        let mut manifests = Vec::with_capacity(roots.len());
        for (index, root) in roots.iter().enumerate() {
            let relative = PathBuf::from(index.to_string());
            let mut captured = BTreeMap::new();
            super::workspace::capture_sources(root, &relative, pending.path(), &mut captured)?;
            let files = captured
                .into_iter()
                .map(|(path, digest)| Ok((path.strip_prefix(&relative)?.to_path_buf(), digest)))
                .collect::<Result<Vec<_>>>()?;
            manifests
                .push(tidepool_toolchain::cache::SourceRootManifest::from_file_digests(files)?);
        }

        // The identity covers the captured source only. The generated module
        // below carries that identity, so hashing it too would be circular —
        // the same ordering `freeze` uses for `Exomonad/Workspace.hs`.
        let revision = source_revision(domain_identity, &manifests);

        std::fs::create_dir_all(pending.path().join("resources/Exomonad/Source"))?;
        tidepool_atomic_write::write_durable(
            &pending.path().join("resources").join(REVISION_MODULE),
            revision_module(&revision.identity).as_bytes(),
        )?;

        Ok(CapturedRevision {
            directory: pending,
            revision,
        })
    }

    /// Point `active` at a checked candidate. One `rename(2)`: the previous
    /// revision stays complete until the instant the new one is complete.
    pub(crate) fn publish(&self, pending: PendingRevision) -> Result<SourceRevision> {
        self.publish_checked(pending, None, None)
            .map_err(|failure| failure.to_string().into())
    }

    fn publish_checked(
        &self,
        pending: PendingRevision,
        expected: Option<&SourceRevision>,
        decision: Option<&Arc<tidepool_runtime::session::PublicationDecision>>,
    ) -> std::result::Result<SourceRevision, SourcePublishFailure> {
        self.publish_checked_with_visible(pending, expected, decision, None)
    }

    fn publish_checked_with_visible(
        &self,
        pending: PendingRevision,
        expected: Option<&SourceRevision>,
        decision: Option<&Arc<tidepool_runtime::session::PublicationDecision>>,
        on_visible: Option<Box<dyn FnOnce() + '_>>,
    ) -> std::result::Result<SourceRevision, SourcePublishFailure> {
        let before = |error: Box<dyn std::error::Error>| {
            SourcePublishFailure::BeforeVisibility(error.to_string())
        };
        let previous = self.read_active().map_err(before)?;
        let generation = previous
            .as_ref()
            .map_or(1, |previous| previous.generation + 1);
        let roots = revision_root_count(&pending.directory).map_err(before)?;
        let staged = self
            .directory
            .join(format!(".active-{}", uuid::Uuid::new_v4()));
        sync_revision_tree(&pending.directory).map_err(before)?;
        std::os::unix::fs::symlink(
            Path::new("revisions").join(&pending.revision.identity),
            &staged,
        )
        .map_err(|error| SourcePublishFailure::BeforeVisibility(error.to_string()))?;
        let current = match self.read_active() {
            Ok(current) => current,
            Err(error) => {
                std::fs::remove_file(&staged).ok();
                return Err(before(error));
            }
        };
        if expected.is_some_and(|expected| current.as_ref() != Some(expected)) {
            std::fs::remove_file(&staged).ok();
            return Err(SourcePublishFailure::BeforeVisibility(
                "active source revision changed after candidate checking".into(),
            ));
        }
        let claim = match decision {
            Some(decision) => match decision.claim_commit() {
                Some(claim) => Some(claim),
                None => {
                    std::fs::remove_file(&staged).ok();
                    return Err(SourcePublishFailure::Cancelled);
                }
            },
            None => None,
        };
        if let Err(error) = std::fs::rename(&staged, self.active_link()) {
            std::fs::remove_file(&staged).ok();
            if let Some(claim) = claim {
                claim.before_rename_failure();
            }
            return Err(SourcePublishFailure::BeforeVisibility(error.to_string()));
        }
        let published = SourceRevision {
            generation,
            ..pending.revision
        };
        if let Some(on_visible) = on_visible {
            on_visible();
        }
        let confirm = || -> Result<()> {
            tidepool_atomic_write::sync_parent_directory(&self.active_link())?;
            tidepool_atomic_write::write_durable(
                &self.active_record(),
                &serde_json::to_vec_pretty(&ActiveRecord {
                    identity: published.identity.clone(),
                    generation,
                    roots,
                })?,
            )?;
            Ok(())
        };
        if let Err(error) = confirm() {
            // A visible revision may have survived a crash. Keeping the native
            // claim unconfirmed prevents cancellation from authorizing replay.
            return Err(SourcePublishFailure::VisibleUnconfirmed {
                revision: published.identity,
                diagnostics: error.to_string(),
            });
        }
        if let Some(claim) = claim {
            claim.published();
        }
        Ok(published)
    }

    /// What a reload of `candidate` would change relative to what is active.
    pub(crate) fn changed_modules(
        &self,
        active: &SourceRevision,
        candidate: &SourceRevision,
    ) -> Vec<String> {
        candidate.changed_since(active)
    }
}

#[derive(Debug, thiserror::Error)]
enum ReloadRunFailure {
    #[error("{0:?}")]
    Source(tidepool_handlers::SourceError),
    #[error("{0}")]
    Publication(#[from] SourcePublishFailure),
}

impl From<tidepool_handlers::SourceError> for ReloadRunFailure {
    fn from(error: tidepool_handlers::SourceError) -> Self {
        Self::Source(error)
    }
}

#[derive(Debug, thiserror::Error)]
enum SourcePublishFailure {
    #[error("source publication cancelled")]
    Cancelled,
    #[error("{0}")]
    BeforeVisibility(String),
    #[error("source revision {revision} is visible but durability is unconfirmed: {diagnostics}")]
    VisibleUnconfirmed {
        revision: String,
        diagnostics: String,
    },
}

fn copy_revision_tree(source: &Path, destination: &Path) -> Result<()> {
    std::fs::create_dir(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = destination.join(entry.file_name());
        if kind.is_symlink() {
            return Err(format!(
                "source revision contains a symlink: {}",
                entry.path().display()
            )
            .into());
        }
        if kind.is_dir() {
            copy_revision_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), target)?;
        } else {
            return Err(format!(
                "source revision contains an unsupported entry: {}",
                entry.path().display()
            )
            .into());
        }
    }
    Ok(())
}

fn sync_revision_tree(root: &Path) -> Result<()> {
    fn sync(directory: &Path) -> std::io::Result<()> {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                sync(&entry.path())?;
            } else if kind.is_file() {
                std::fs::File::open(entry.path())?.sync_all()?;
            } else {
                return Err(std::io::Error::other(
                    "source revision contains a non-file entry",
                ));
            }
        }
        std::fs::File::open(directory)?.sync_all()
    }
    sync(root).map_err(Into::into)
}

fn revision_root_count(directory: &Path) -> Result<usize> {
    let mut indices = Vec::new();
    for entry in directory.read_dir()? {
        let entry = entry?;
        let Some(index) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<usize>().ok())
        else {
            continue;
        };
        if !entry.file_type()?.is_dir() {
            return Err("source revision root is not a directory".into());
        }
        indices.push(index);
    }
    indices.sort_unstable();
    if indices.iter().copied().ne(0..indices.len()) {
        return Err("source revision root indexes are not contiguous".into());
    }
    Ok(indices.len())
}

fn source_manifests(
    directory: &Path,
    roots: usize,
) -> Result<Vec<tidepool_toolchain::cache::SourceRootManifest>> {
    revision_include_paths(directory, roots)
        .into_iter()
        .map(|root| {
            let files = tidepool_toolchain::cache::source_root_manifest(&root)?;
            Ok(tidepool_toolchain::cache::SourceRootManifest::from_file_digests(files)?)
        })
        .collect()
}

fn source_root_manifest(root: &Path) -> Result<tidepool_toolchain::cache::SourceRootManifest> {
    let files = tidepool_toolchain::cache::source_root_manifest(root)?;
    Ok(tidepool_toolchain::cache::SourceRootManifest::from_file_digests(files)?)
}

fn same_source_manifest(
    left: &tidepool_toolchain::cache::SourceRootManifest,
    right: &tidepool_toolchain::cache::SourceRootManifest,
) -> bool {
    left.files().eq(right.files())
}

fn validate_revision_resource(
    directory: &Path,
    roots: usize,
    identity: &str,
    manifests: &[tidepool_toolchain::cache::SourceRootManifest],
) -> Result<()> {
    let resources = manifests[roots]
        .files()
        .map(|(path, digest)| (path.to_owned(), *digest))
        .collect::<Vec<_>>();
    if resources
        != vec![(
            PathBuf::from(REVISION_MODULE),
            blake3::hash(revision_module(identity).as_bytes()),
        )]
    {
        return Err(format!(
            "immutable source revision resource differs from its publication at {}",
            directory.display()
        )
        .into());
    }
    Ok(())
}

fn inspect_retained_revision(
    domain: &str,
    identity: &str,
    directory: &Path,
    roots: usize,
) -> Result<RetainedRevision> {
    let paths = revision_include_paths(directory, roots)
        .into_iter()
        .map(std::fs::canonicalize)
        .collect::<std::io::Result<Vec<_>>>()?;
    let manifests = source_manifests(directory, roots)?;
    if source_revision(domain, &manifests[..roots]).identity != identity {
        return Err("immutable source revision differs from its publication".into());
    }
    validate_revision_resource(directory, roots, identity, &manifests)?;
    Ok(RetainedRevision {
        identity: identity.to_owned(),
        paths,
        manifests: manifests.into(),
    })
}

/// Which layer one actor's own source calls act on.
///
/// Selected when the actor is constructed and never afterwards, so an actor
/// cannot reach another actor's source by asking differently. This is the
/// whole authority story for `Source`: there is no role test anywhere below
/// this point, because by then the layer is already decided.
#[derive(Clone)]
enum ActorSourceScope {
    /// The run's own layer. Publishing here changes what every actor compiles
    /// against, so it belongs to the actor that owns the run.
    Run,
    /// The run's layer, readable but not publishable. This actor compiles
    /// against it — that is what `sourceStatus` reports — and has no source of
    /// its own to publish.
    RunReadOnly,
    /// The child compiles against immutable revisions from a delegated
    /// checkpoint. Reload must not switch its source graph behind that view.
    Checkpoint(exomonad_actor::CheckpointSourceLayer),
}

/// Existing owner of the physical run source tree. Persistent revisions have
/// no per-revision reclamation: the run lease excludes maintenance until the
/// last issued capture releases. Disposable sessions retain their TempDir.
pub(crate) enum SourceRootOwner {
    Host(Arc<crate::actor_host::HostIncarnationLease>),
    /// Immutable deployment preparation has no live run or host lease.
    Prepared(Arc<tidepool_atomic_write::DirectoryAnchor>),
    Temporary(Arc<tempfile::TempDir>),
}

impl SourceRootOwner {
    fn validate(&self, run_root: &Path) -> Result<()> {
        let owns = match self {
            Self::Host(owner) => owner.owns_run(run_root)?,
            Self::Prepared(owner) => owner.path() == std::fs::canonicalize(run_root)?.as_path(),
            Self::Temporary(owner) => {
                std::fs::canonicalize(owner.path())? == std::fs::canonicalize(run_root)?
            }
        };
        if !owns {
            return Err("source owner belongs to another run tree".into());
        }
        Ok(())
    }
}

struct RetainedSourceGraph {
    _root_owner: Arc<SourceRootOwner>,
    _prepared_owner: Option<Arc<tidepool_atomic_write::DirectoryAnchor>>,
    identities: Vec<String>,
    include_paths: Vec<PathBuf>,
    manifests: Vec<tidepool_toolchain::cache::SourceRootManifest>,
    entries: exomonad_actor::SourceEntryStorage,
}

impl exomonad_actor::RetainedSourceLayer for RetainedSourceGraph {
    fn identities(&self) -> &[String] {
        &self.identities
    }

    fn include_paths(&self) -> &[PathBuf] {
        &self.include_paths
    }

    fn source_manifests(&self) -> Option<&[tidepool_toolchain::cache::SourceRootManifest]> {
        Some(&self.manifests)
    }

    fn prepared_entries(&self) -> Option<&exomonad_actor::SourceEntryStorage> {
        Some(&self.entries)
    }
}

/// The run's answer to the `Source` effect, for every actor in it.
///
/// It owns the run's frozen workspace and authored source layer. The
/// compile that decides whether a candidate is acceptable is the run's
/// ordinary driver compile, so a run reload is checked by exactly the
/// compiler the run uses.
pub(crate) struct ExomonadSourceReload {
    source_issuer: exomonad_actor::SourceLayerIssuer,
    source_owner: Arc<SourceRootOwner>,
    entry_storage: exomonad_actor::SourceEntryStorage,
    fresh_entry_storage: exomonad_actor::SourceEntryStorage,
    frozen: FrozenWorkspace,
    workspace: PathBuf,
    run_root: PathBuf,
    helper_root: PathBuf,
    haskell_root: PathBuf,
    layer: SourceLayer,
    /// Helper branches are keyed by `run` for the root and by worktree id for
    /// forked actors. Each branch has its own draft and active revision.
    helpers: Mutex<HashMap<String, SourceLayer>>,
    /// What each actor's own source calls reach.
    scopes: RwLock<HashMap<PrincipalId, ActorSourceScope>>,
    helper_scopes: RwLock<HashMap<PrincipalId, String>>,
    /// One reload at a time: a publication's check and its `rename(2)` must
    /// not interleave with another actor's.
    gate: Mutex<()>,
    /// The last drift read per layer, and the signature of the disk it was
    /// read from. Drift is read on a timer for every actor; while neither the
    /// disk nor the active revision has moved, the answer has not either.
    drift_seen: Mutex<HashMap<String, (String, String, exomonad_actor::SourceLayerDrift)>>,
}

impl ExomonadSourceReload {
    pub(crate) fn new_owned(
        frozen: FrozenWorkspace,
        workspace: PathBuf,
        run_root: PathBuf,
        haskell_root: PathBuf,
        owner: SourceRootOwner,
    ) -> Result<Self> {
        owner.validate(&run_root)?;
        let run_root = std::fs::canonicalize(run_root)?;
        let entries = tidepool_atomic_write::DirectoryAnchor::open_existing(&run_root)?
            .child("workspace/entries")?;
        let preparation = match (&owner, &frozen.preparation) {
            (
                SourceRootOwner::Prepared(_),
                Some(super::workspace::WorkspacePreparation::Preparing { original }),
            ) => *original,
            _ => uuid::Uuid::new_v4(),
        };
        let fresh_entry_storage = exomonad_actor::SourceEntryStorage::FreshCompilation {
            directory: entries.path().to_owned(),
            preparation,
        };
        let entry_storage = match &frozen.preparation {
            Some(super::workspace::WorkspacePreparation::Completed { .. }) => {
                let deployment = frozen
                    .prepared_deployment
                    .as_ref()
                    .ok_or("completed workspace has no acquired immutable deployment owner")?;
                exomonad_actor::SourceEntryStorage::CompletedOriginal {
                    directory: deployment.path().join("workspace/entries"),
                    selections: frozen.completed_entry_selections()?,
                }
            }
            _ => fresh_entry_storage.clone(),
        };
        let helper_root = run_root.join("helpers");
        let layer = SourceLayer::new(&run_root);
        Ok(Self {
            source_issuer: exomonad_actor::SourceLayerIssuer::default(),
            source_owner: Arc::new(owner),
            entry_storage,
            fresh_entry_storage,
            frozen,
            workspace,
            run_root,
            helper_root,
            haskell_root,
            layer,
            helpers: Mutex::new(HashMap::new()),
            scopes: RwLock::new(HashMap::new()),
            helper_scopes: RwLock::new(HashMap::new()),
            gate: Mutex::new(()),
            drift_seen: Mutex::new(HashMap::new()),
        })
    }

    #[cfg(test)]
    fn new(
        frozen: FrozenWorkspace,
        workspace: PathBuf,
        run_root: PathBuf,
        haskell_root: PathBuf,
    ) -> Self {
        let owner = Arc::new(
            crate::actor_host::HostIncarnationLease::claim(
                &tidepool_atomic_write::DirectoryAnchor::open_existing(&run_root).unwrap(),
            )
            .unwrap(),
        );
        Self::new_owned(
            frozen,
            workspace,
            run_root,
            haskell_root,
            SourceRootOwner::Host(owner),
        )
        .unwrap()
    }

    pub(crate) fn with_helper_root(mut self, root: PathBuf) -> Self {
        self.helper_root = root;
        self
    }

    /// Name the actor that owns the run's own layer. Exactly one actor does,
    /// and the host says which while admitting it.
    pub(crate) fn bind_run(&self, actor: PrincipalId) {
        self.scopes.write().insert(actor, ActorSourceScope::Run);
        self.helper_scopes.write().insert(actor, "run".to_owned());
    }

    /// What `caller`'s own source calls reach.
    ///
    /// An actor the host never bound gets the run's layer read-only: it can
    /// see what its cells compile against and cannot publish anything. The
    /// bootstrap principal is the run itself, before any actor exists.
    fn scope(&self, caller: PrincipalId) -> ActorSourceScope {
        if caller == PrincipalId::SYSTEM {
            return ActorSourceScope::Run;
        }
        self.scopes
            .read()
            .get(&caller)
            .cloned()
            .unwrap_or(ActorSourceScope::RunReadOnly)
    }

    fn helper_branch(worktrees: &[String]) -> String {
        match worktrees {
            [id] => id.clone(),
            [] => "run".to_owned(),
            _ => "run".to_owned(),
        }
    }

    fn helper_layer(&self, branch: &str) -> SourceLayer {
        let mut layers = self.helpers.lock();
        layers
            .entry(branch.to_owned())
            .or_insert_with(|| SourceLayer::helpers(&self.helper_root, branch))
            .clone()
    }

    /// The generated workspace interface is part of the exact run source
    /// graph. It is separate from the authored source layer and appears once,
    /// after that layer's ordered roots.
    fn append_workspace_resources(
        &self,
        identities: &mut Vec<String>,
        include_paths: &mut Vec<PathBuf>,
        manifests: &mut Vec<tidepool_toolchain::cache::SourceRootManifest>,
    ) -> Result<()> {
        if include_paths.len() != manifests.len() {
            return Err("source graph manifest count differs from its roots".into());
        }
        let path = std::fs::canonicalize(self.frozen.workspace_resources())?;
        let manifest = source_root_manifest(&path)?;
        let mut matches = Vec::new();
        for (index, include) in include_paths.iter().enumerate() {
            if std::fs::canonicalize(include)? == path {
                matches.push(index);
            }
        }
        if matches.len() > 1 {
            return Err(
                "generated workspace resource appears more than once in the source graph".into(),
            );
        }
        if let Some(index) = matches.first().copied() {
            if !same_source_manifest(&manifests[index], &manifest) {
                return Err(
                    "generated workspace resource changed after workspace validation".into(),
                );
            }
            include_paths[index] = path;
        } else {
            include_paths.push(path);
            manifests.push(manifest);
        }
        let identity = format!("workspace:{}", self.frozen.identity());
        let workspace_identities = identities
            .iter()
            .filter(|existing| existing.starts_with("workspace:"))
            .collect::<Vec<_>>();
        if workspace_identities.len() > 1
            || workspace_identities
                .first()
                .is_some_and(|existing| existing.as_str() != identity.as_str())
        {
            return Err("source graph carries a different workspace resource identity".into());
        }
        if workspace_identities.is_empty() {
            identities.push(identity);
        }
        Ok(())
    }

    /// Paths that may represent the completed deployment's original live
    /// source revision in an issued graph. Other deployment paths are never
    /// accepted by toolset projection.
    fn prepared_run_revision(&self) -> Result<Option<RetainedRevision>> {
        let Some(directory) = self.frozen.prepared_source_revision()? else {
            return Ok(None);
        };
        let directory = std::fs::canonicalize(directory)?;
        let roots = revision_root_count(&directory)?;
        let manifests = source_manifests(&directory, roots)?;
        let identity = source_revision(self.frozen.identity(), &manifests[..roots]);
        validate_revision_resource(&directory, roots, &identity.identity, &manifests)?;
        Ok(Some(RetainedRevision {
            identity: identity.identity,
            paths: revision_include_paths(&directory, roots)
                .into_iter()
                .map(std::fs::canonicalize)
                .collect::<std::io::Result<Vec<_>>>()?,
            manifests: manifests.into(),
        }))
    }

    /// The deployment's promised recipes belong to this original immutable
    /// graph, even when a recovered run has accepted a newer source revision.
    pub(crate) fn prepared_toolset_layer(&self) -> Result<exomonad_actor::CheckpointSourceLayer> {
        let revision = self
            .prepared_run_revision()?
            .ok_or("source owner has no completed deployment revision")?;
        let mut identities = vec![format!("run:{}", revision.identity)];
        let mut include_paths = revision.paths;
        let mut manifests = revision.manifests.iter().cloned().collect();
        self.append_workspace_resources(&mut identities, &mut include_paths, &mut manifests)?;
        Ok(self.source_issuer.issue(Arc::new(RetainedSourceGraph {
            _root_owner: Arc::clone(&self.source_owner),
            _prepared_owner: self.frozen.prepared_deployment.clone(),
            identities,
            include_paths,
            manifests,
            entries: self.entry_storage.clone(),
        })))
    }

    fn pinned_checkpoint_graph(
        &self,
        helper_branch: &str,
    ) -> Result<exomonad_actor::CheckpointSourceLayer> {
        self.layer.ensure_active(&self.frozen)?;
        let mut identities = Vec::new();
        let mut include_paths = Vec::new();
        let mut manifests = Vec::new();
        let mut add = |layer: &SourceLayer, kind: &str| -> Result<()> {
            let revision = layer.checkpoint_revision(self.frozen.identity())?;
            identities.push(format!("{kind}:{}", revision.identity));
            include_paths.extend(revision.paths);
            manifests.extend(revision.manifests.iter().cloned());
            Ok(())
        };
        add(&self.helper_layer(helper_branch), "helpers")?;
        add(&self.layer, "run")?;
        self.append_workspace_resources(&mut identities, &mut include_paths, &mut manifests)?;
        Ok(self.source_issuer.issue(Arc::new(RetainedSourceGraph {
            _root_owner: Arc::clone(&self.source_owner),
            _prepared_owner: self.frozen.prepared_deployment.clone(),
            identities,
            include_paths,
            manifests,
            entries: self.entry_storage.clone(),
        })))
    }

    fn helper_draft(&self, branch: &str) -> PathBuf {
        SourceLayer::helper_draft(&self.helper_root, branch)
    }

    pub(crate) fn helper_branch_for(&self, actor: PrincipalId) -> Option<String> {
        self.helper_scopes.read().get(&actor).cloned()
    }

    fn fork_helper_branch(&self, creator: PrincipalId, branch: &str) -> Result<()> {
        let parent_branch = self
            .helper_branch_for(creator)
            .ok_or("creator has no session helper branch")?;
        let parent = self.helper_layer(&parent_branch);
        let _parent_lock = parent.lock_helpers()?;
        self.ensure_helper_active(&parent_branch)?;
        let child_draft = self.helper_draft(branch);
        crate::actor_host::copy_helper_draft(&self.helper_draft(&parent_branch), &child_draft)?;
        self.helper_layer(branch)
            .inherit_active_from(&parent, std::slice::from_ref(&child_draft))?;
        Ok(())
    }

    fn ensure_helper_active(&self, branch: &str) -> Result<SourceRevision> {
        let seed = self.helper_root.join("empty");
        std::fs::create_dir_all(&seed)?;
        let draft = self.helper_draft(branch);
        if branch == "run" {
            crate::actor_host::initialize_helper_draft(&self.workspace, &draft)?;
        }
        std::fs::create_dir_all(&draft)?;
        self.helper_layer(branch)
            .ensure_active_from(self.frozen.identity(), &[seed])
    }

    fn wire(revision: &SourceRevision) -> tidepool_bridge_effects::SrRevision {
        tidepool_handlers::revision_to_wire(
            &revision.identity,
            revision.generation,
            &revision.modules,
        )
    }

    /// Reload the run's own layer: re-read the workspace's declared roots and
    /// publish them in place of the layer every actor shares.
    fn reload_run(
        &self,
        also_check: &[String],
        intent: Option<&str>,
        publication: Option<&Arc<tidepool_runtime::session::PublicationDecision>>,
    ) -> std::result::Result<tidepool_bridge_effects::SrReloadOutcome, ReloadRunFailure> {
        let active = self.layer.ensure_active(&self.frozen).map_err(unreadable)?;
        let pending = self
            .layer
            .capture_from_workspace(&self.frozen, &self.workspace)
            .map_err(unreadable)?;
        let candidate = |pending: &PendingRevision| {
            pending.include_paths(self.frozen.captured_source_roots().len())
        };
        self.settle(
            &self.layer,
            active,
            pending,
            &candidate,
            &self.workspace,
            true,
            also_check,
            intent,
            publication,
        )
    }

    fn reload_helpers_owned(
        &self,
        actor: PrincipalId,
        also_check: &[String],
        publication: Option<&Arc<tidepool_runtime::session::PublicationDecision>>,
    ) -> exomonad_actor::SourceLayerReload {
        if matches!(self.scope(actor), ActorSourceScope::Checkpoint(_)) {
            return exomonad_actor::SourceLayerReload::Unavailable(
                "checkpoint source is frozen; session helpers cannot be republished".into(),
            );
        }
        let _one_at_a_time = self.gate.lock();
        let Some(branch) = self.helper_scopes.read().get(&actor).cloned() else {
            return exomonad_actor::SourceLayerReload::Unavailable(
                "this actor has no session helper branch".into(),
            );
        };
        self.reload_helper_branch(&branch, also_check, publication)
    }

    fn reload_helper_branch(
        &self,
        branch: &str,
        also_check: &[String],
        publication: Option<&Arc<tidepool_runtime::session::PublicationDecision>>,
    ) -> exomonad_actor::SourceLayerReload {
        use exomonad_actor::SourceLayerReload;

        let layer = self.helper_layer(branch);
        let _branch_lock = match layer.lock_helpers() {
            Ok(lock) => lock,
            Err(error) => {
                return SourceLayerReload::Unavailable(format!(
                    "session helper branch is locked out: {error}"
                ));
            }
        };
        let active = match self.ensure_helper_active(branch) {
            Ok(active) => active,
            Err(error) => {
                return SourceLayerReload::Unavailable(format!(
                    "session helper revision is unavailable: {error}"
                ));
            }
        };
        let draft = self.helper_draft(branch);
        let pending =
            match layer.capture_from_roots(self.frozen.identity(), std::slice::from_ref(&draft)) {
                Ok(pending) => pending,
                Err(error) => {
                    return SourceLayerReload::Unavailable(format!(
                        "session helper draft could not be captured: {error}"
                    ));
                }
            };
        if pending.revision().identity == active.identity {
            return SourceLayerReload::Unchanged {
                revision: active.identity,
            };
        }
        let helpers: Vec<String> = pending
            .revision()
            .modules
            .iter()
            .map(|(module, _)| module.clone())
            .collect();
        if let Some(module) = helpers
            .iter()
            .find(|module| *module != "SessionHelpers" && !module.starts_with("SessionHelpers."))
        {
            return SourceLayerReload::Rejected {
                active: active.identity,
                rejected: pending.revision().identity.clone(),
                diagnostics: format!(
                    "helper module `{module}` is outside the reserved SessionHelpers namespace"
                ),
            };
        }
        let mut protected_modules: BTreeSet<String> = helpers.iter().cloned().collect();
        protected_modules.extend(active.modules.iter().map(|(module, _)| module.clone()));
        let mut lower_modules = BTreeSet::new();
        for module in &protected_modules {
            if self.frozen.provides_module(module) {
                lower_modules.insert(module.clone());
            }
        }
        let run = match self.layer.read_active() {
            Ok(run) => run,
            Err(error) => {
                return SourceLayerReload::Unavailable(format!(
                    "run source revision is unavailable: {error}"
                ));
            }
        };
        if let Some(run) = run {
            lower_modules.extend(
                run.modules
                    .iter()
                    .map(|(module, _)| module.clone())
                    .filter(|module| protected_modules.contains(module)),
            );
        }
        if let Some(module) = lower_modules.first() {
            return SourceLayerReload::Rejected {
                active: active.identity,
                rejected: pending.revision().identity.clone(),
                diagnostics: format!(
                    "reserved helper module `{module}` also exists in a lower source layer; removing it would expose that older module"
                ),
            };
        }
        let mut checked = helpers;
        for module in also_check {
            if !checked.contains(module) {
                checked.push(module.clone());
            }
        }
        let candidate = pending.include_paths(1);
        if let Err(error) = crate::actor_host::typecheck_candidate_revision(
            &self.frozen,
            &self.run_root,
            &self.haskell_root,
            &candidate,
            false,
            &checked,
        ) {
            return SourceLayerReload::Rejected {
                active: active.identity,
                rejected: pending.revision().identity.clone(),
                diagnostics: error.to_string(),
            };
        }
        let changed = changed_modules(&active, pending.revision());
        match layer.publish_checked(pending, Some(&active), publication) {
            Ok(published) => SourceLayerReload::Published {
                previous: active.identity,
                revision: published.identity,
                changed,
            },
            Err(SourcePublishFailure::Cancelled) => SourceLayerReload::Cancelled,
            Err(SourcePublishFailure::VisibleUnconfirmed {
                revision,
                diagnostics,
            }) => SourceLayerReload::PublicationUnconfirmed {
                revision,
                diagnostics,
            },
            Err(error) => SourceLayerReload::Unavailable(format!(
                "session helper revision could not be published: {error}"
            )),
        }
    }

    fn check_candidate(
        &self,
        active: &SourceRevision,
        pending: &PendingRevision,
        candidate_paths: &[PathBuf],
        replaces_run_layer: bool,
        also_check: &[String],
    ) -> std::result::Result<Option<String>, ReloadRunFailure> {
        let active_modules: std::collections::BTreeSet<&str> = active
            .modules
            .iter()
            .map(|(module, _)| module.as_str())
            .collect();
        let pending_modules: std::collections::BTreeSet<&str> = pending
            .revision()
            .modules
            .iter()
            .map(|(module, _)| module.as_str())
            .collect();
        if replaces_run_layer {
            let removed: Vec<_> = active_modules
                .difference(&pending_modules)
                .copied()
                .collect();
            if !removed.is_empty() {
                return Ok(Some(format!(
                        "removed module(s) {} would resolve from the frozen run capture; restart with a new source graph",
                        removed.join(", ")
                    )));
            }
        }
        let configured_modules: std::collections::BTreeSet<&str> =
            self.frozen.import_modules().collect();
        let new_modules: Vec<&str> = pending
            .revision()
            .modules
            .iter()
            .map(|(module, _)| module.as_str())
            .filter(|module| {
                !active_modules.contains(module) && !configured_modules.contains(module)
            })
            .collect();
        if !new_modules.is_empty() {
            return Ok(Some(format!(
                    "new module(s) {} cannot be imported in this running session; add them to [haskell].modules and restart",
                    new_modules.join(", ")
                )));
        }
        if let Err(error) = crate::actor_host::typecheck_candidate_revision(
            &self.frozen,
            &self.run_root,
            &self.haskell_root,
            candidate_paths,
            replaces_run_layer,
            also_check,
        ) {
            // Nothing moved: the active symlink still points where it did, and
            // the edited files are exactly as the caller wrote them.
            return Ok(Some(error.to_string()));
        }
        Ok(None)
    }

    /// The publication transaction, which is the same for every layer: an
    /// unchanged candidate publishes nothing, a candidate that does not
    /// typecheck moves nothing, and a candidate that does becomes the revision
    /// the layer's owner compiles against from its next cell.
    #[allow(
        clippy::too_many_arguments,
        reason = "publication transaction takes each layer's independent inputs (revision, candidate, workspace, check scope, intent); no natural grouping"
    )]
    fn settle(
        &self,
        layer: &SourceLayer,
        active: SourceRevision,
        pending: PendingRevision,
        candidate: &dyn Fn(&PendingRevision) -> Vec<PathBuf>,
        workspace: &Path,
        replaces_run_layer: bool,
        also_check: &[String],
        intent: Option<&str>,
        publication: Option<&Arc<tidepool_runtime::session::PublicationDecision>>,
    ) -> std::result::Result<tidepool_bridge_effects::SrReloadOutcome, ReloadRunFailure> {
        use tidepool_bridge_effects::SrReloadOutcome;
        if pending.revision().identity == active.identity {
            return Ok(SrReloadOutcome::ReloadUnchanged(Self::wire(&active)));
        }
        if let Some(diagnostics) = self.check_candidate(
            &active,
            &pending,
            &candidate(&pending),
            replaces_run_layer,
            also_check,
        )? {
            return Ok(SrReloadOutcome::ReloadRejected(
                Self::wire(&active),
                Self::wire(pending.revision()),
                diagnostics,
            ));
        }
        let changed = layer.changed_modules(&active, pending.revision());
        let capture_directory = pending.directory.clone();
        let source_roots = pending.source_roots.clone();
        let published = layer.publish_checked(pending, Some(&active), publication)?;
        let workspace = commit_captured_workspace(
            workspace,
            &source_roots,
            &capture_directory,
            intent,
            &changed,
        );
        Ok(SrReloadOutcome::ReloadPublished(
            Self::wire(&active),
            Self::wire(&published),
            changed,
            workspace,
        ))
    }

    /// This caller's source layer, active vs. the latest observed on-disk
    /// capture, and which modules' digests differ between them — row 1 of
    /// the what-is-live status view (`exomonad_actor::SourceLayerDrift`).
    /// Same read `status` answers; wired directly for the actor-runtime
    /// observation channel, since that channel's type has no place for
    /// `status`'s `SrRevision` wire pair and cannot compute a diff without
    /// duplicating `SourceRevision::changed_since`.
    pub(crate) fn drift(
        &self,
        caller: PrincipalId,
    ) -> std::result::Result<exomonad_actor::SourceLayerDrift, tidepool_handlers::SourceError> {
        let _one_at_a_time = self.gate.lock();
        let scope = self.scope(caller);
        // What the copy below would read, signed without reading it.
        let (layer_key, roots) = match &scope {
            ActorSourceScope::Checkpoint(_) => {
                return Err(tidepool_handlers::SourceError::SourceUnavailable(
                    "checkpoint source is frozen; drift is unavailable".into(),
                ));
            }
            ActorSourceScope::Run | ActorSourceScope::RunReadOnly => {
                let config = self.frozen.config().map_err(unreadable)?;
                let roots =
                    super::workspace::resolve_source_roots(&self.workspace, &config.haskell)
                        .map_err(unreadable)?;
                ("run".to_owned(), roots)
            }
        };
        let signature = super::workspace::sources_signature(&roots).map_err(unreadable)?;
        let active_now = match &scope {
            ActorSourceScope::Checkpoint(_) => unreachable!("handled before reading drift"),
            ActorSourceScope::Run | ActorSourceScope::RunReadOnly => {
                self.layer.ensure_active(&self.frozen).map_err(unreadable)?
            }
        };
        if let Some((seen_signature, seen_active, drift)) = self.drift_seen.lock().get(&layer_key) {
            if *seen_signature == signature && *seen_active == active_now.identity {
                return Ok(drift.clone());
            }
        }
        let (active, disk) = match scope {
            ActorSourceScope::Checkpoint(_) => unreachable!("handled before reading drift"),
            ActorSourceScope::Run | ActorSourceScope::RunReadOnly => {
                let active = self.layer.ensure_active(&self.frozen).map_err(unreadable)?;
                let disk = self
                    .layer
                    .observe_from_workspace(&self.frozen, &self.workspace)
                    .map_err(unreadable)?;
                (active, disk)
            }
        };
        let disk = if disk.identity == active.identity {
            active.clone()
        } else {
            disk
        };
        let changed_modules = disk.changed_since(&active);
        let drift = exomonad_actor::SourceLayerDrift {
            active_identity: active.identity,
            active_generation: active.generation,
            disk_identity: disk.identity,
            disk_generation: disk.generation,
            changed_modules,
        };
        self.drift_seen.lock().insert(
            layer_key,
            (signature, drift.active_identity.clone(), drift.clone()),
        );
        Ok(drift)
    }

    /// Frozen workspace modules whose digest differs from the same module
    /// read live off disk right now — row 3 of the what-is-live status view.
    /// The frozen capture ([`super::workspace::FrozenWorkspace`]) is the
    /// run's immutable floor; this is independent of any layer republished
    /// in front of it ([`Self::drift`]), so a clean answer here does not
    /// imply a clean answer there or the reverse.
    pub(crate) fn frozen_drift(&self) -> Result<exomonad_actor::FrozenSourceDrift> {
        let frozen_modules = manifest_of_roots(self.frozen.captured_source_roots())?;
        let config = self.frozen.config()?;
        let live_roots = super::workspace::resolve_source_roots(&self.workspace, &config.haskell)?;
        let live_modules = manifest_of_roots(&live_roots)?;
        let frozen = SourceRevision {
            identity: String::new(),
            generation: 0,
            modules: frozen_modules,
        };
        let live = SourceRevision {
            identity: String::new(),
            generation: 0,
            modules: live_modules,
        };
        Ok(exomonad_actor::FrozenSourceDrift {
            changed_modules: live.changed_since(&frozen),
        })
    }

    fn status_of(
        &self,
        active: SourceRevision,
        disk: &SourceRevision,
    ) -> tidepool_bridge_effects::SrStatus {
        let disk = if disk.identity == active.identity {
            active.clone()
        } else {
            disk.clone()
        };
        tidepool_bridge_effects::SrStatus {
            active: Self::wire(&active),
            disk: Self::wire(&disk),
        }
    }
}

enum WorkspaceCommitResult {
    Unchanged,
    Committed { oid: String, drift: Vec<String> },
}

struct WorkspaceCommitFailure {
    reason: String,
    committed: Option<String>,
    drift: Vec<String>,
}

/// Publish the checked snapshot to Git without reading candidate bytes from
/// the mutable workspace working tree. The alternate index starts at HEAD and gets
/// only files present in this source capture.
fn commit_captured_workspace(
    workspace: &Path,
    source_roots: &[PathBuf],
    capture_directory: &Path,
    intent: Option<&str>,
    changed_modules: &[String],
) -> tidepool_bridge_effects::SrWorkspaceCommitOutcome {
    use tidepool_bridge_effects::SrWorkspaceCommitOutcome as WorkspaceOutcome;

    match commit_captured_workspace_inner(
        workspace,
        source_roots,
        capture_directory,
        intent,
        changed_modules,
    ) {
        Ok(WorkspaceCommitResult::Unchanged) => WorkspaceOutcome::WorkspaceUnchanged,
        Ok(WorkspaceCommitResult::Committed { oid, drift }) => {
            WorkspaceOutcome::WorkspaceCommitted(oid, drift)
        }
        Err(failure) => WorkspaceOutcome::WorkspaceCommitFailed(
            failure.reason,
            failure.committed,
            failure.drift,
        ),
    }
}

fn commit_captured_workspace_inner(
    project: &Path,
    source_roots: &[PathBuf],
    capture_directory: &Path,
    intent: Option<&str>,
    changed_modules: &[String],
) -> std::result::Result<WorkspaceCommitResult, WorkspaceCommitFailure> {
    const WORKSPACE_PATH: &str = ".exomonad/workspace";
    let workspace = project.join(WORKSPACE_PATH);
    if !workspace.is_dir() {
        return Ok(WorkspaceCommitResult::Unchanged);
    }

    let mut committed = None;
    let mut drift = Vec::new();
    let result = (|| -> Result<Option<String>> {
        let git = GitCli::new();
        let workspace_root = git.try_run(&workspace, &["rev-parse", "--show-toplevel"])?;
        let workspace_root = PathBuf::from(workspace_root.stdout.trim()).canonicalize()?;
        let workspace = workspace.canonicalize()?;
        if workspace_root != workspace {
            return Err("workspace mount does not resolve to its own Git repository".into());
        }
        let unmerged = git.try_run(project, &["ls-files", "--unmerged", "--", WORKSPACE_PATH])?;
        if !unmerged.stdout.is_empty() {
            return Err("workspace gitlink has an unresolved index conflict".into());
        }
        let parent_index = git.try_run(project, &["ls-files", "--stage", "--", WORKSPACE_PATH])?;
        let staged = parent_index.stdout.lines().next().and_then(|line| {
            let mut fields = line.split_whitespace();
            Some((fields.next()?, fields.next()?, fields.next()?))
        });
        if !matches!(staged, Some(("160000", _, "0"))) || parent_index.stdout.lines().count() != 1 {
            return Err("workspace mount is not recorded as a Git submodule".into());
        }
        let prior_head = git.try_run(&workspace, &["rev-parse", "HEAD"])?;
        if staged.is_some_and(|(_, oid, _)| oid != prior_head.stdout.trim()) {
            return Err(
                "workspace gitlink has a staged revision different from its checkout".into(),
            );
        }
        let scopes = workspace_source_scopes(&workspace, source_roots)?;
        if scopes.is_empty() {
            return Ok(None);
        }
        let source = captured_workspace_sources(&scopes, capture_directory)?;
        drift = workspace_source_drift(&scopes, &source)?;

        let message = match intent {
            Some(message) => {
                let message = message.trim();
                if message.is_empty() || message.contains('\n') || message.contains('\r') {
                    return Err("workspace commit intent must be a non-empty single line".into());
                }
                message.to_owned()
            }
            None if changed_modules.is_empty() => "Update workspace".to_owned(),
            None => format!("Update workspace: {}", changed_modules.join(", ")),
        };

        let temporary_index = tempfile::tempdir()?;
        let index_path = temporary_index.path().join("index");
        let staged_git = git.with_env("GIT_INDEX_FILE", index_path.to_string_lossy());
        staged_git.try_run(&workspace, &["read-tree", "HEAD"])?;
        let existing = staged_git.try_run(&workspace, &["ls-files", "--stage"])?;
        let mut indexed_modes = BTreeMap::new();
        for line in existing.stdout.lines() {
            let Some((metadata, path)) = line.split_once('\t') else {
                continue;
            };
            let mut fields = metadata.split_whitespace();
            let (Some(mode), Some(_oid), Some("0")) = (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            indexed_modes.insert(path.to_owned(), mode.to_owned());
        }

        let source_paths: BTreeMap<String, PathBuf> = source;
        let mut affected: std::collections::BTreeSet<String> =
            source_paths.keys().cloned().collect();
        for path in indexed_modes.keys() {
            if source_scopes_contain(&scopes, path)
                && super::workspace::is_haskell_source(Path::new(path))
            {
                affected.insert(path.clone());
                if !source_paths.contains_key(path) {
                    staged_git.try_run(&workspace, &["update-index", "--remove", "--", path])?;
                }
            }
        }

        for (path, captured) in &source_paths {
            let blob = staged_git.try_run(
                &workspace,
                &[
                    std::ffi::OsStr::new("hash-object"),
                    std::ffi::OsStr::new("-w"),
                    captured.as_os_str(),
                ],
            )?;
            let mode = indexed_modes
                .get(path)
                .map(String::as_str)
                .filter(|mode| matches!(*mode, "100644" | "100755"))
                .unwrap_or("100644");
            let cacheinfo = format!("{mode},{},{}", blob.stdout.trim(), path);
            staged_git.try_run(
                &workspace,
                &["update-index", "--add", "--cacheinfo", &cacheinfo],
            )?;
        }

        let tree = staged_git.try_run(&workspace, &["write-tree"])?;
        let head_tree = git.try_run(&workspace, &["rev-parse", "HEAD^{tree}"])?;
        if tree.stdout.trim() == head_tree.stdout.trim() {
            return Ok(None);
        }
        let prior_head = git.try_run(&workspace, &["rev-parse", "HEAD"])?;
        if let Err(error) = staged_git.try_run(&workspace, &["commit", "-m", &message]) {
            if let (Ok(head), Ok(committed_tree)) = (
                git.try_run(&workspace, &["rev-parse", "HEAD"]),
                git.try_run(&workspace, &["rev-parse", "HEAD^{tree}"]),
            ) {
                if head.stdout.trim() != prior_head.stdout.trim()
                    && committed_tree.stdout.trim() == tree.stdout.trim()
                {
                    committed = Some(head.stdout.trim().to_owned());
                }
            }
            return Err(Box::new(error));
        }
        let oid = git
            .try_run(&workspace, &["rev-parse", "HEAD"])?
            .stdout
            .trim()
            .to_owned();
        committed = Some(oid.clone());

        // Bring only captured source paths in the real index forward. A file
        // edited after capture remains visible as an unstaged drift.
        let mut args: Vec<String> =
            vec!["reset".into(), "--quiet".into(), "HEAD".into(), "--".into()];
        args.extend(affected.iter().cloned());
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        git.try_run(&workspace, &refs)?;

        git.try_run(
            project,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{oid},{WORKSPACE_PATH}"),
            ],
        )?;
        Ok(Some(oid))
    })();

    match result {
        Ok(Some(oid)) => Ok(WorkspaceCommitResult::Committed { oid, drift }),
        Ok(None) => Ok(WorkspaceCommitResult::Unchanged),
        Err(error) => Err(WorkspaceCommitFailure {
            reason: error.to_string(),
            committed,
            drift,
        }),
    }
}

fn captured_workspace_sources(
    scopes: &[(usize, PathBuf, PathBuf, PathBuf)],
    capture_directory: &Path,
) -> Result<BTreeMap<String, PathBuf>> {
    let mut files = BTreeMap::new();
    for (index, _root, captured_suffix, target_prefix) in scopes {
        let root = capture_directory
            .join(index.to_string())
            .join(captured_suffix);
        if !root.is_dir() {
            continue;
        }
        fn walk(
            root: &Path,
            current: &Path,
            prefix: &Path,
            files: &mut BTreeMap<String, PathBuf>,
        ) -> Result<()> {
            for entry in current.read_dir()? {
                let entry = entry?;
                let path = entry.path();
                let kind = entry.file_type()?;
                if kind.is_dir() {
                    walk(root, &path, prefix, files)?;
                } else if kind.is_file() {
                    let relative = path.strip_prefix(root)?;
                    let relative = prefix.join(relative);
                    let name = relative
                        .to_str()
                        .ok_or("workspace source path is not valid UTF-8")?
                        .replace(std::path::MAIN_SEPARATOR, "/");
                    match files.get(&name) {
                        Some(previous) if std::fs::read(previous)? != std::fs::read(&path)? => {
                            return Err(format!(
                                "overlapping source roots captured different bytes for {name}"
                            )
                            .into());
                        }
                        Some(_) => {}
                        None => {
                            files.insert(name, path);
                        }
                    }
                }
            }
            Ok(())
        }
        walk(&root, &root, target_prefix, &mut files)?;
    }
    Ok(files)
}

/// Each tuple is `(source-root index, original root, captured suffix under
/// that root, project-relative prefix within the workspace)`.
fn workspace_source_scopes(
    workspace: &Path,
    source_roots: &[PathBuf],
) -> Result<Vec<(usize, PathBuf, PathBuf, PathBuf)>> {
    let mut scopes = Vec::new();
    for (index, root) in source_roots.iter().enumerate() {
        if let Ok(prefix) = root.strip_prefix(workspace) {
            scopes.push((index, root.clone(), PathBuf::new(), prefix.to_path_buf()));
        } else if let Ok(suffix) = workspace.strip_prefix(root) {
            scopes.push((index, root.clone(), suffix.to_path_buf(), PathBuf::new()));
        }
    }
    Ok(scopes)
}

fn source_scopes_contain(scopes: &[(usize, PathBuf, PathBuf, PathBuf)], path: &str) -> bool {
    let path = Path::new(path);
    scopes
        .iter()
        .any(|(_, _, _, prefix)| path.starts_with(prefix))
}

fn workspace_source_drift(
    scopes: &[(usize, PathBuf, PathBuf, PathBuf)],
    captured: &BTreeMap<String, PathBuf>,
) -> Result<Vec<String>> {
    let mut live = BTreeMap::new();
    for (_, root, suffix, prefix) in scopes {
        if root.is_dir() {
            for (path, digest) in super::workspace::inspect_sources(root)? {
                let Ok(path) = path.strip_prefix(suffix) else {
                    continue;
                };
                let relative = prefix.join(path);
                let name = relative
                    .to_str()
                    .ok_or("workspace source path is not valid UTF-8")?
                    .replace(std::path::MAIN_SEPARATOR, "/");
                live.insert(name, digest);
            }
        }
    }
    let paths: std::collections::BTreeSet<_> =
        captured.keys().chain(live.keys()).cloned().collect();
    let mut drift = Vec::new();
    for path in paths {
        let same = match (captured.get(&path), live.get(&path)) {
            (Some(snapshot), Some(current)) => {
                blake3::hash(&std::fs::read(snapshot)?).to_hex().as_str() == current.as_str()
            }
            _ => false,
        };
        if !same {
            drift.push(path);
        }
    }
    Ok(drift)
}

fn unreadable(error: Box<dyn std::error::Error>) -> tidepool_handlers::SourceError {
    tidepool_handlers::SourceError::SourceUnreadable(error.to_string())
}

impl tidepool_handlers::SourceReloadService for ExomonadSourceReload {
    fn reload(
        &self,
        caller: PrincipalId,
        also_check: &[String],
        intent: Option<&str>,
    ) -> std::result::Result<tidepool_bridge_effects::SrReloadOutcome, tidepool_handlers::SourceError>
    {
        let _one_at_a_time = self.gate.lock();
        match self.scope(caller) {
            ActorSourceScope::Checkpoint(_) => {
                Err(tidepool_handlers::SourceError::SourceUnavailable(
                    "checkpoint source is frozen; this actor cannot republish it".into(),
                ))
            }
            ActorSourceScope::Run => self.reload_run(also_check, intent, None).map_err(|failure| tidepool_handlers::SourceError::SourceUnreadable(failure.to_string())),
            ActorSourceScope::RunReadOnly => {
                Err(tidepool_handlers::SourceError::SourceUnavailable(
                    "this actor uses the run's current tooling; only the run owner can reload it. \
                     Checkout tooling edits are unavailable until a coherent checkout-source launch mode is selected"
                        .into(),
                ))
            }
        }
    }

    fn status(
        &self,
        caller: PrincipalId,
    ) -> std::result::Result<tidepool_bridge_effects::SrStatus, tidepool_handlers::SourceError>
    {
        let _one_at_a_time = self.gate.lock();
        match self.scope(caller) {
            ActorSourceScope::Checkpoint(_) => {
                Err(tidepool_handlers::SourceError::SourceUnavailable(
                    "checkpoint source is frozen; status compares no mutable draft".into(),
                ))
            }
            ActorSourceScope::Run | ActorSourceScope::RunReadOnly => {
                let active = self.layer.ensure_active(&self.frozen).map_err(unreadable)?;
                let disk = self
                    .layer
                    .observe_from_workspace(&self.frozen, &self.workspace)
                    .map_err(unreadable)?;
                Ok(self.status_of(active, &disk))
            }
        }
    }
}

impl exomonad_actor::ActorSourceLayers for ExomonadSourceReload {
    fn toolset_layer_from(
        &self,
        source: &exomonad_actor::CheckpointSourceLayer,
    ) -> std::result::Result<exomonad_actor::CheckpointSourceLayer, String> {
        self.validate_source_authority(source)?;
        let run_identities = source
            .identities()
            .iter()
            .filter(|identity| identity.starts_with("run:"))
            .cloned()
            .collect::<Vec<_>>();
        if run_identities.len() != 1 {
            return Err("owned source graph must contain exactly one run revision".into());
        }
        let workspace_identity = format!("workspace:{}", self.frozen.identity());
        let workspace_identities = source
            .identities()
            .iter()
            .filter(|identity| identity.starts_with("workspace:"))
            .collect::<Vec<_>>();
        if workspace_identities.len() != 1 || workspace_identities[0] != &workspace_identity {
            return Err("owned source graph has a different workspace resource identity".into());
        }
        let prepared_run_revision = self
            .prepared_run_revision()
            .map_err(|error| error.to_string())?;
        let source_manifests = source
            .source_manifests()
            .ok_or("owned source graph has no immutable manifests")?;
        if source_manifests.len() != source.include_paths().len() {
            return Err("owned source graph manifest count differs from its roots".into());
        }
        let mut include_paths = Vec::new();
        let mut manifests = Vec::new();
        for (path, manifest) in source.include_paths().iter().zip(source_manifests) {
            let owned_run_revision = path.starts_with(self.run_root.join("workspace/revisions"));
            let owned_prepared_revision = prepared_run_revision.as_ref().is_some_and(|revision| {
                run_identities
                    .iter()
                    .any(|run| run == &format!("run:{}", revision.identity))
                    && revision.paths.contains(path)
            });
            if owned_run_revision || owned_prepared_revision {
                include_paths.push(path.clone());
                manifests.push(manifest.clone());
            }
        }
        if include_paths.is_empty() {
            return Err("owned source graph has no admitted run revision roots".into());
        }
        let mut identities = run_identities;
        self.append_workspace_resources(&mut identities, &mut include_paths, &mut manifests)
            .map_err(|error| error.to_string())?;
        Ok(self.source_issuer.issue(Arc::new(RetainedSourceGraph {
            _root_owner: Arc::clone(&self.source_owner),
            _prepared_owner: self.frozen.prepared_deployment.clone(),
            identities,
            include_paths,
            manifests,
            entries: source
                .prepared_entries()
                .cloned()
                .unwrap_or_else(|| self.entry_storage.clone()),
        })))
    }

    fn fresh_toolset_layer_from(
        &self,
        source: &exomonad_actor::CheckpointSourceLayer,
    ) -> std::result::Result<exomonad_actor::CheckpointSourceLayer, String> {
        let selected = self.toolset_layer_from(source)?;
        Ok(self.source_issuer.issue(Arc::new(RetainedSourceGraph {
            _root_owner: Arc::clone(&self.source_owner),
            _prepared_owner: self.frozen.prepared_deployment.clone(),
            identities: selected.identities().to_vec(),
            include_paths: selected.include_paths().to_vec(),
            manifests: selected
                .source_manifests()
                .ok_or("owned toolset graph has no immutable manifests")?
                .to_vec(),
            entries: self.fresh_entry_storage.clone(),
        })))
    }

    fn freeze_toolset_layer(
        &self,
        actor: PrincipalId,
    ) -> std::result::Result<exomonad_actor::CheckpointSourceLayer, String> {
        let _one_at_a_time = self.gate.lock();
        if let ActorSourceScope::Checkpoint(layer) = self.scope(actor) {
            return self.toolset_layer_from(&layer);
        }
        if self
            .layer
            .read_record()
            .map_err(|error| error.to_string())?
            .is_none()
        {
            self.layer
                .ensure_active(&self.frozen)
                .map_err(|error| error.to_string())?;
        }
        let revision = self
            .layer
            .checkpoint_revision(self.frozen.identity())
            .map_err(|error| error.to_string())?;
        let mut identities = vec![format!("run:{}", revision.identity)];
        let mut include_paths = revision.paths;
        let mut manifests = revision.manifests.iter().cloned().collect();
        self.append_workspace_resources(&mut identities, &mut include_paths, &mut manifests)
            .map_err(|error| error.to_string())?;
        Ok(self.source_issuer.issue(Arc::new(RetainedSourceGraph {
            _root_owner: Arc::clone(&self.source_owner),
            _prepared_owner: self.frozen.prepared_deployment.clone(),
            identities,
            include_paths,
            manifests,
            entries: self.entry_storage.clone(),
        })))
    }

    fn validate_source_authority(
        &self,
        source: &exomonad_actor::CheckpointSourceLayer,
    ) -> std::result::Result<(), String> {
        if self.source_issuer.owns(source) {
            Ok(())
        } else {
            Err("checkpoint source was not issued by this run source owner".into())
        }
    }

    fn freeze_checkpoint_layer(
        &self,
        issuer: PrincipalId,
    ) -> std::result::Result<exomonad_actor::CheckpointSourceLayer, String> {
        let _one_at_a_time = self.gate.lock();
        let helper = self
            .helper_branch_for(issuer)
            .unwrap_or_else(|| "run".to_owned());
        self.ensure_helper_active(&helper)
            .map_err(|error| error.to_string())?;
        match self.scope(issuer) {
            ActorSourceScope::Checkpoint(layer) => return Ok(layer),
            ActorSourceScope::Run | ActorSourceScope::RunReadOnly => {}
        }
        self.pinned_checkpoint_graph(&helper)
            .map_err(|error| error.to_string())
    }

    fn admit_checkpoint_layer(
        &self,
        checkpoint: &exomonad_actor::CheckpointSourceLayer,
        creator: PrincipalId,
        helper_branch: &str,
        _worktrees: &[String],
    ) -> std::result::Result<Vec<PathBuf>, String> {
        let _one_at_a_time = self.gate.lock();
        self.validate_source_authority(checkpoint)?;
        let candidate = match self.scope(creator) {
            ActorSourceScope::Checkpoint(layer) => layer,
            _ => {
                self.ensure_helper_active(helper_branch)
                    .map_err(|error| error.to_string())?;
                self.pinned_checkpoint_graph(helper_branch)
                    .map_err(|error| error.to_string())?
            }
        };
        if !candidate.same_revision(checkpoint) {
            return Err(format!(
                "checkpoint source revisions differ from authored entry source: captured {:?}, entry {:?}",
                checkpoint.identities(),
                candidate.identities(),
            ));
        }
        Ok(checkpoint.include_paths().to_vec())
    }

    fn bind_checkpoint_for(
        &self,
        actor: PrincipalId,
        helper_branch: &str,
        checkpoint: &exomonad_actor::CheckpointSourceLayer,
    ) -> std::result::Result<(), String> {
        self.validate_source_authority(checkpoint)?;
        self.helper_scopes
            .write()
            .insert(actor, helper_branch.to_owned());
        self.scopes
            .write()
            .insert(actor, ActorSourceScope::Checkpoint(checkpoint.clone()));
        Ok(())
    }

    fn prepare_helpers(
        &self,
        creator: PrincipalId,
        worktrees: &[String],
        prepared_fork: bool,
    ) -> std::result::Result<String, String> {
        if prepared_fork {
            let [branch] = worktrees else {
                return Err("prepared fork must have exactly one helper branch".into());
            };
            let draft = self.helper_draft(branch);
            let active = self
                .helper_layer(branch)
                .read_active()
                .map_err(|error| format!("prepared helper revision is unavailable: {error}"))?;
            match (draft.exists(), active.is_some()) {
                (true, true) => {}
                (false, false) => self.fork_helper_branch(creator, branch).map_err(|error| {
                    format!("could not snapshot creator's session helpers: {error}")
                })?,
                _ => return Err("prepared helper draft and active revision disagree".into()),
            }
            return Ok(branch.clone());
        }
        let branch = format!("actor-{}", uuid::Uuid::new_v4());
        self.fork_helper_branch(creator, &branch)
            .map_err(|error| format!("could not snapshot creator's session helpers: {error}"))?;
        Ok(branch)
    }

    fn layer_include(&self, worktrees: &[String]) -> std::result::Result<Vec<PathBuf>, String> {
        self.layer_include_for(&Self::helper_branch(worktrees), worktrees)
    }

    fn layer_include_for(
        &self,
        branch: &str,
        _worktrees: &[String],
    ) -> std::result::Result<Vec<PathBuf>, String> {
        self.ensure_helper_active(branch)
            .map_err(|error| format!("session helper layer could not be initialized: {error}"))?;
        self.helper_layer(branch)
            .active_include_paths()
            .map_err(|error| format!("session helper layer has no include roots: {error}"))
    }

    fn bind(&self, actor: PrincipalId, worktrees: &[String]) {
        self.bind_for(actor, &Self::helper_branch(worktrees), worktrees);
    }

    fn bind_for(&self, actor: PrincipalId, helper_branch: &str, _worktrees: &[String]) {
        self.helper_scopes
            .write()
            .insert(actor, helper_branch.to_owned());
        self.scopes
            .write()
            .insert(actor, ActorSourceScope::RunReadOnly);
    }

    /// Publish the run layer through the same transaction as the `Source`
    /// effect. Only the run owner may publish; workers read the run graph and
    /// checkpoint actors retain their frozen graph.
    fn reload(
        &self,
        actor: PrincipalId,
        also_check: &[String],
    ) -> exomonad_actor::SourceLayerReload {
        use exomonad_actor::SourceLayerReload;
        use tidepool_bridge_effects::SrReloadOutcome;
        match tidepool_handlers::SourceReloadService::reload(self, actor, also_check, None) {
            Ok(SrReloadOutcome::ReloadUnchanged(revision)) => SourceLayerReload::Unchanged {
                revision: revision.identity,
            },
            Ok(SrReloadOutcome::ReloadPublished(previous, published, changed, _workspace)) => {
                SourceLayerReload::Published {
                    previous: previous.identity,
                    revision: published.identity,
                    changed,
                }
            }
            Ok(SrReloadOutcome::ReloadRejected(active, rejected, diagnostics)) => {
                SourceLayerReload::Rejected {
                    active: active.identity,
                    rejected: rejected.identity,
                    diagnostics,
                }
            }
            Err(tidepool_handlers::SourceError::SourceUnavailable(detail)) => {
                SourceLayerReload::Unavailable(detail)
            }
            Err(tidepool_handlers::SourceError::SourceUnreadable(detail)) => {
                SourceLayerReload::Unavailable(format!(
                    "the declared source roots could not be re-read: {detail}"
                ))
            }
        }
    }

    fn reload_helpers(
        &self,
        actor: PrincipalId,
        also_check: &[String],
    ) -> exomonad_actor::SourceLayerReload {
        self.reload_helpers_owned(actor, also_check, None)
    }

    fn reload_helpers_with_publication(
        &self,
        actor: PrincipalId,
        also_check: &[String],
        publication: &Arc<tidepool_runtime::session::PublicationDecision>,
    ) -> exomonad_actor::SourceLayerReload {
        self.reload_helpers_owned(actor, also_check, Some(publication))
    }

    fn stage_spec_reload(
        self: Arc<Self>,
        actor: PrincipalId,
        also_check: &[String],
    ) -> std::result::Result<
        Box<dyn exomonad_actor::StagedActorSourceReload>,
        exomonad_actor::SourceLayerReload,
    > {
        use exomonad_actor::SourceLayerReload;
        let unavailable =
            |error: Box<dyn std::error::Error>| SourceLayerReload::Unavailable(error.to_string());
        let _one_at_a_time = self.gate.lock();
        if !matches!(self.scope(actor), ActorSourceScope::Run) {
            return Err(SourceLayerReload::Unavailable(
                "this actor cannot republish the run source layer".into(),
            ));
        }
        let active = self
            .layer
            .ensure_active(&self.frozen)
            .map_err(unavailable)?;
        let pending = self
            .layer
            .capture_from_workspace(&self.frozen, &self.workspace)
            .map_err(unavailable)?;
        // A Source effect may already have published these bytes while this
        // actor still serves an older dispatcher. Explicit spec reload prepares
        // a fresh original even when the global run revision has not changed.
        let candidate_paths = pending.include_paths(self.frozen.captured_source_roots().len());
        let diagnostics = self
            .check_candidate(&active, &pending, &candidate_paths, true, also_check)
            .map_err(|error| SourceLayerReload::Unavailable(error.to_string()))?;
        if let Some(diagnostics) = diagnostics {
            return Err(SourceLayerReload::Rejected {
                active: active.identity,
                rejected: pending.revision.identity,
                diagnostics,
            });
        }
        let helper_branch = self
            .helper_branch_for(actor)
            .unwrap_or_else(|| "run".into());
        let helper_active = self
            .ensure_helper_active(&helper_branch)
            .map_err(unavailable)?;
        let helper = self
            .helper_layer(&helper_branch)
            .checkpoint_revision(self.frozen.identity())
            .map_err(unavailable)?;
        let run = inspect_retained_revision(
            self.frozen.identity(),
            &pending.revision.identity,
            &pending.directory,
            self.frozen.captured_source_roots().len(),
        )
        .map_err(unavailable)?;
        let mut identities = vec![
            format!("helpers:{}", helper.identity),
            format!("run:{}", run.identity),
        ];
        let mut include_paths = helper.paths;
        include_paths.extend(run.paths);
        let mut manifests = helper.manifests.to_vec();
        manifests.extend(run.manifests.iter().cloned());
        self.append_workspace_resources(&mut identities, &mut include_paths, &mut manifests)
            .map_err(unavailable)?;
        let source = self.source_issuer.issue(Arc::new(RetainedSourceGraph {
            _root_owner: Arc::clone(&self.source_owner),
            _prepared_owner: self.frozen.prepared_deployment.clone(),
            identities,
            include_paths,
            manifests,
            entries: exomonad_actor::SourceEntryStorage::FreshCompilation {
                directory: self.fresh_entry_storage.directory().to_owned(),
                preparation: uuid::Uuid::new_v4(),
            },
        }));
        let changed = self.layer.changed_modules(&active, pending.revision());
        drop(_one_at_a_time);
        Ok(Box::new(StagedSpecReload {
            owner: self,
            active,
            pending,
            helper_branch,
            helper_active,
            source,
            changed,
        }))
    }
}

struct StagedSpecReload {
    owner: Arc<ExomonadSourceReload>,
    active: SourceRevision,
    pending: PendingRevision,
    helper_branch: String,
    helper_active: SourceRevision,
    source: exomonad_actor::CheckpointSourceLayer,
    changed: Vec<String>,
}

impl exomonad_actor::StagedActorSourceReload for StagedSpecReload {
    fn source(&self) -> &exomonad_actor::CheckpointSourceLayer {
        &self.source
    }

    fn commit(
        self: Box<Self>,
        publication: &Arc<tidepool_runtime::session::PublicationDecision>,
        on_visible: Box<dyn FnOnce() + '_>,
    ) -> exomonad_actor::SourceLayerReload {
        use exomonad_actor::SourceLayerReload;
        let _one_at_a_time = self.owner.gate.lock();
        let rejected = |diagnostics| SourceLayerReload::Rejected {
            active: self.active.identity.clone(),
            rejected: self.pending.revision.identity.clone(),
            diagnostics,
        };
        let helpers = match self.owner.helper_layer(&self.helper_branch).read_active() {
            Ok(helpers) => helpers,
            Err(error) => return rejected(error.to_string()),
        };
        if helpers.as_ref() != Some(&self.helper_active) {
            return rejected("active helper source changed after candidate checking".into());
        }
        let rejected_identity = self.pending.revision.identity.clone();
        match self.owner.layer.publish_checked_with_visible(
            self.pending,
            Some(&self.active),
            Some(publication),
            Some(on_visible),
        ) {
            Ok(published) => SourceLayerReload::Published {
                previous: self.active.identity,
                revision: published.identity,
                changed: self.changed,
            },
            Err(SourcePublishFailure::Cancelled) => SourceLayerReload::Cancelled,
            Err(SourcePublishFailure::VisibleUnconfirmed {
                revision,
                diagnostics,
            }) => SourceLayerReload::PublicationUnconfirmed {
                revision,
                diagnostics,
            },
            Err(SourcePublishFailure::BeforeVisibility(diagnostics)) => {
                SourceLayerReload::Rejected {
                    active: self
                        .owner
                        .layer
                        .read_active()
                        .ok()
                        .flatten()
                        .map_or(self.active.identity, |revision| revision.identity),
                    rejected: rejected_identity,
                    diagnostics,
                }
            }
        }
    }
}

fn changed_modules(active: &SourceRevision, candidate: &SourceRevision) -> Vec<String> {
    let before: BTreeMap<&str, &str> = active
        .modules
        .iter()
        .map(|(module, digest)| (module.as_str(), digest.as_str()))
        .collect();
    let after: BTreeMap<&str, &str> = candidate
        .modules
        .iter()
        .map(|(module, digest)| (module.as_str(), digest.as_str()))
        .collect();
    before
        .keys()
        .chain(after.keys())
        .copied()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .filter(|module| before.get(module) != after.get(module))
        .map(str::to_owned)
        .collect()
}

#[derive(serde::Deserialize, serde::Serialize)]
struct ActiveRecord {
    identity: String,
    generation: u64,
    roots: usize,
}

fn revision_include_paths(directory: &Path, roots: usize) -> Vec<PathBuf> {
    (0..roots)
        .map(|index| directory.join(index.to_string()))
        .chain(std::iter::once(directory.join("resources")))
        .collect()
}

/// Derive identity and first-root-wins module inventory from one complete
/// inspection of each ordered source root.
fn source_revision(
    domain_identity: &str,
    roots: &[tidepool_toolchain::cache::SourceRootManifest],
) -> SourceRevision {
    let mut domain = DOMAIN.to_vec();
    domain.extend_from_slice(domain_identity.as_bytes());
    let mut modules = BTreeMap::new();
    for root in roots {
        for (relative, digest) in root.files() {
            if let Some(module) = module_name(relative) {
                modules
                    .entry(module)
                    .or_insert_with(|| digest.to_hex().to_string());
            }
        }
    }
    SourceRevision {
        identity: tidepool_toolchain::cache::source_manifests_identity(&domain, roots),
        generation: 0,
        modules: modules.into_iter().collect(),
    }
}

/// Read every module the roots provide, with the same first-root-wins
/// shadowing used for captured revision evidence.
fn manifest_of_roots(roots: &[PathBuf]) -> Result<Vec<(String, String)>> {
    let mut modules: BTreeMap<String, String> = BTreeMap::new();
    for root in roots {
        for (relative, digest) in tidepool_toolchain::cache::source_root_manifest(root)? {
            let Some(module) = module_name(&relative) else {
                continue;
            };
            modules.entry(module).or_insert(digest);
        }
    }
    Ok(modules.into_iter().collect())
}

/// Every module a captured revision provides, by module name, first root
/// wins — exactly the shadowing GHC applies across the same include roots in
/// the same order.
fn revision_modules(directory: &Path, roots: usize) -> Result<Vec<(String, String)>> {
    let root_paths: Vec<PathBuf> = (0..roots)
        .map(|index| directory.join(index.to_string()))
        .collect();
    manifest_of_roots(&root_paths)
}

/// `Project/Types.hs` names `Project.Types`. Boot files describe an existing
/// module rather than providing one, so they contribute no name.
fn module_name(relative: &Path) -> Option<String> {
    let text = relative.to_str()?;
    let stem = text
        .strip_suffix(".hs")
        .or_else(|| text.strip_suffix(".lhs"))?;
    Some(stem.replace('/', "."))
}

fn revision_module(identity: &str) -> String {
    format!(
        "-- | The source revision this module was compiled against. Generated\n\
         -- per revision by the Exomonad run; import it from an authored module to\n\
         -- record, honestly, which snapshot built that module's code.\n\
         module Exomonad.Source.Revision (compiledSourceRevision) where\n\
         \n\
         import Data.Text (Text)\n\
         \n\
         compiledSourceRevision :: Text\n\
         compiledSourceRevision = \"{}\"\n",
        tidepool_runtime::session::escape_workbench_haskell_string(identity)
    )
}

#[cfg(all(test, target_os = "linux"))]
#[path = "source/publication_fault_tests.rs"]
mod publication_fault_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_revision_reuses_manifests_and_preserves_original_after_publication() {
        let (project, run) = workspace_with("module Project.Work where\nwork = 1\n");
        let frozen = FrozenWorkspace::load(project.path(), run.path()).unwrap();
        let layer = SourceLayer::new(run.path());
        layer.ensure_active(&frozen).unwrap();
        let original = layer.checkpoint_revision(frozen.identity()).unwrap();
        let warm = layer
            .clone()
            .checkpoint_revision(frozen.identity())
            .unwrap();
        assert!(Arc::ptr_eq(&original.manifests, &warm.manifests));
        assert_eq!(
            tidepool_toolchain::cache::source_manifests_identity(
                b"exomonad-agent-spec-ordered-source-closure-v1",
                &original.manifests,
            ),
            tidepool_toolchain::cache::source_roots_identity(
                b"exomonad-agent-spec-ordered-source-closure-v1",
                &original.paths,
            )
            .unwrap(),
        );
        std::fs::write(
            project.path().join(".exomonad/Project/Work.hs"),
            "module Project.Work where\nwork = 2\n",
        )
        .unwrap();
        let pending = layer
            .capture_from_workspace(&frozen, project.path())
            .unwrap();
        layer.publish(pending).unwrap();
        let changed = layer.checkpoint_revision(frozen.identity()).unwrap();
        assert_ne!(original.identity, changed.identity);
        assert!(!Arc::ptr_eq(&original.manifests, &changed.manifests));
        assert!(original.paths.iter().all(|path| path.exists()));

        let reacquired = SourceLayer::new(run.path());
        std::fs::write(changed.paths[0].join("Project/Work.hs"), "changed").unwrap();
        assert!(reacquired.checkpoint_revision(frozen.identity()).is_err());
    }

    #[test]
    fn checkpoint_paths_pin_revision_and_admission_refuses_source_drift() {
        use exomonad_actor::ActorSourceLayers;
        let (project, run) = workspace_with("module Project.Work where\nwork = 1\n");
        let frozen = FrozenWorkspace::load(project.path(), run.path()).unwrap();
        let reload = ExomonadSourceReload::new(
            frozen,
            project.path().to_path_buf(),
            run.path().to_path_buf(),
            crate::haskell_sources::ensure_exomonad_haskell().unwrap(),
        );
        let captured = reload.freeze_checkpoint_layer(PrincipalId::SYSTEM).unwrap();
        assert_eq!(captured.identities().len(), 3);
        assert!(captured.identities()[0].starts_with("helpers:"));
        assert!(captured.identities()[1].starts_with("run:"));
        let workspace_identity = format!("workspace:{}", reload.frozen.identity());
        assert_eq!(captured.identities()[2], workspace_identity);
        assert!(!captured.include_paths().is_empty());
        assert!(captured
            .include_paths()
            .iter()
            .all(|path| !path.to_string_lossy().contains("/active/")));
        let workspace_resource =
            std::fs::canonicalize(reload.frozen.workspace_resources()).unwrap();
        assert_eq!(captured.include_paths().last(), Some(&workspace_resource));
        assert_eq!(
            captured
                .include_paths()
                .iter()
                .filter(|path| path.as_path() == workspace_resource.as_path())
                .count(),
            1
        );
        assert!(captured.include_paths().iter().all(|path| {
            path.starts_with(reload.run_root.join("helpers"))
                || path.starts_with(reload.run_root.join("workspace/revisions"))
                || path == &workspace_resource
        }));
        let manifests = captured.source_manifests().unwrap();
        assert_eq!(manifests.len(), captured.include_paths().len());
        assert!(same_source_manifest(
            manifests.last().unwrap(),
            &source_root_manifest(&workspace_resource).unwrap()
        ));
        assert_eq!(
            reload
                .admit_checkpoint_layer(&captured, PrincipalId::SYSTEM, "run", &[])
                .unwrap(),
            captured.include_paths()
        );

        let alternate = tempfile::tempdir().unwrap();
        std::fs::write(
            alternate.path().join("Work.hs"),
            "module Work where\nwork = 2\n",
        )
        .unwrap();
        let pending = reload
            .layer
            .capture_from_roots(reload.frozen.identity(), &[alternate.path().to_path_buf()])
            .unwrap();
        reload.layer.publish(pending).unwrap();
        let refusal = reload
            .admit_checkpoint_layer(&captured, PrincipalId::SYSTEM, "run", &[])
            .unwrap_err();
        assert!(refusal.contains("source revisions differ"));
        assert!(captured.include_paths()[0].exists());
        assert_eq!(
            reload.admit_retained_layer(&captured).unwrap(),
            captured.include_paths(),
            "an already admitted original capsule survives later run publication"
        );
        let checkpoint_actor = PrincipalId::new(1, 1);
        reload
            .bind_checkpoint_for(checkpoint_actor, "run", &captured)
            .unwrap();
        assert_eq!(
            reload.freeze_checkpoint_layer(checkpoint_actor).unwrap(),
            captured,
            "a checkpoint descendant must retain the frozen helper and run graph"
        );
    }

    #[test]
    fn uncovered_toolset_requires_the_same_source_owners_fresh_admission() {
        use exomonad_actor::{ActorSourceLayers, SourceEntryStorage};
        let (project, run) = workspace_with("module Project.Work where\nwork = 1\n");
        let run = Arc::new(run);
        let frozen = FrozenWorkspace::load(project.path(), run.path()).unwrap();
        let mut reload = ExomonadSourceReload::new_owned(
            frozen.clone(),
            project.path().to_path_buf(),
            run.path().to_path_buf(),
            crate::haskell_sources::ensure_exomonad_haskell().unwrap(),
            SourceRootOwner::Temporary(Arc::clone(&run)),
        )
        .unwrap();
        reload.entry_storage = SourceEntryStorage::CompletedOriginal {
            directory: reload.entry_storage.directory().to_owned(),
            selections: Default::default(),
        };
        let selected = reload.freeze_toolset_layer(PrincipalId::SYSTEM).unwrap();
        assert!(matches!(
            selected.prepared_entries(),
            Some(SourceEntryStorage::CompletedOriginal { .. })
        ));
        let fresh = reload.fresh_toolset_layer_from(&selected).unwrap();
        let repeated = reload.fresh_toolset_layer_from(&selected).unwrap();
        assert_eq!(fresh.identities(), selected.identities());
        assert_eq!(fresh.include_paths(), selected.include_paths());
        assert_eq!(fresh.semantic_digest(), repeated.semantic_digest());
        assert_ne!(fresh.semantic_digest(), selected.semantic_digest());
        assert!(matches!(
            fresh.prepared_entries(),
            Some(SourceEntryStorage::FreshCompilation { .. })
        ));
        let other = ExomonadSourceReload::new_owned(
            frozen,
            project.path().to_path_buf(),
            run.path().to_path_buf(),
            crate::haskell_sources::ensure_exomonad_haskell().unwrap(),
            SourceRootOwner::Temporary(run),
        )
        .unwrap();
        assert!(other.fresh_toolset_layer_from(&selected).is_err());
    }

    #[test]
    fn checkpoint_source_capsule_refuses_foreign_issuer_and_edited_observations() {
        use exomonad_actor::ActorSourceLayers;
        let (project, run) = workspace_with("module Project.Work where\nwork = 1\n");
        let run = Arc::new(run);
        let frozen = FrozenWorkspace::load(project.path(), run.path()).unwrap();
        let reload = ExomonadSourceReload::new_owned(
            frozen.clone(),
            project.path().to_path_buf(),
            run.path().to_path_buf(),
            crate::haskell_sources::ensure_exomonad_haskell().unwrap(),
            SourceRootOwner::Temporary(Arc::clone(&run)),
        )
        .unwrap();
        let captured = reload.freeze_checkpoint_layer(PrincipalId::SYSTEM).unwrap();
        let digest = captured.semantic_digest();
        let mut observed_paths = captured.include_paths().to_vec();
        let mut observed_revisions = captured.identities().to_vec();
        observed_paths[0] = project.path().to_path_buf();
        observed_revisions.clear();
        assert_eq!(captured.semantic_digest(), digest);
        assert_ne!(captured.include_paths(), observed_paths);
        assert_ne!(captured.identities(), observed_revisions);

        let foreign_owner = ExomonadSourceReload::new_owned(
            frozen,
            project.path().to_path_buf(),
            run.path().to_path_buf(),
            crate::haskell_sources::ensure_exomonad_haskell().unwrap(),
            SourceRootOwner::Temporary(Arc::clone(&run)),
        )
        .unwrap();
        let foreign = foreign_owner
            .freeze_checkpoint_layer(PrincipalId::SYSTEM)
            .unwrap();
        assert_eq!(foreign.identities(), captured.identities());
        assert_eq!(foreign.include_paths(), captured.include_paths());
        assert_ne!(foreign.semantic_digest(), digest);
        assert!(!foreign.same_revision(&captured));
        assert!(reload.validate_source_authority(&foreign).is_err());
        assert!(reload.admit_retained_layer(&foreign).is_err());
        assert!(reload
            .admit_checkpoint_layer(&foreign, PrincipalId::SYSTEM, "run", &[])
            .is_err());
        assert!(reload
            .bind_checkpoint_for(PrincipalId::new(1, 1), "run", &foreign)
            .is_err());
        assert!(reload
            .validate_source_authority(&exomonad_actor::CheckpointSourceLayer::default())
            .is_err());
        reload.validate_source_authority(&captured).unwrap();
    }

    #[test]
    fn checkpoint_source_capsule_retains_temporary_tree_after_owner_drop() {
        use exomonad_actor::ActorSourceLayers;
        let (project, run) = workspace_with("module Project.Work where\nwork = 1\n");
        let run = Arc::new(run);
        let run_path = run.path().to_path_buf();
        let frozen = FrozenWorkspace::load(project.path(), &run_path).unwrap();
        let reload = ExomonadSourceReload::new_owned(
            frozen,
            project.path().to_path_buf(),
            run_path.clone(),
            crate::haskell_sources::ensure_exomonad_haskell().unwrap(),
            SourceRootOwner::Temporary(Arc::clone(&run)),
        )
        .unwrap();
        let captured = reload.freeze_checkpoint_layer(PrincipalId::SYSTEM).unwrap();
        let surviving_child = captured.clone();
        drop(reload);
        drop(run);
        drop(captured);
        assert!(surviving_child
            .include_paths()
            .iter()
            .all(|path| path.exists()));
        assert!(run_path.exists());
        drop(surviving_child);
        assert!(
            !run_path.exists(),
            "the last capture releases the physical owner"
        );
    }

    #[test]
    fn checkpoint_source_capsule_retains_run_exclusion_after_owner_drop() {
        use exomonad_actor::ActorSourceLayers;
        let (project, run) = workspace_with("module Project.Work where\nwork = 1\n");
        let frozen = FrozenWorkspace::load(project.path(), run.path()).unwrap();
        let reload = ExomonadSourceReload::new(
            frozen,
            project.path().to_path_buf(),
            run.path().to_path_buf(),
            crate::haskell_sources::ensure_exomonad_haskell().unwrap(),
        );
        let captured = reload.freeze_checkpoint_layer(PrincipalId::SYSTEM).unwrap();
        drop(reload);
        assert!(crate::actor_host::HostIncarnationLease::claim(
            &tidepool_atomic_write::DirectoryAnchor::open_existing(run.path()).unwrap()
        )
        .is_err());
        assert!(captured.include_paths().iter().all(|path| path.exists()));
        drop(captured);
        crate::actor_host::HostIncarnationLease::claim(
            &tidepool_atomic_write::DirectoryAnchor::open_existing(run.path()).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn source_service_refuses_owner_for_another_run_tree() {
        let (project, run) = workspace_with("module Project.Work where\nwork = 1\n");
        let foreign_run = Arc::new(tempfile::tempdir().unwrap());
        let frozen = FrozenWorkspace::load(project.path(), run.path()).unwrap();
        let result = ExomonadSourceReload::new_owned(
            frozen,
            project.path().to_path_buf(),
            run.path().to_path_buf(),
            crate::haskell_sources::ensure_exomonad_haskell().unwrap(),
            SourceRootOwner::Temporary(foreign_run),
        );
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn prepared_source_reference_transitions_to_a_run_owned_live_revision() {
        use exomonad_actor::ActorSourceLayers;
        let (project, run) = workspace_with("module Project.Work where\nwork = 1\n");
        let original_run = tempfile::tempdir().unwrap();
        let frozen = FrozenWorkspace::load(project.path(), original_run.path()).unwrap();
        let original_layer = SourceLayer::new(original_run.path());
        let original = original_layer.ensure_active(&frozen).unwrap();

        let deployment = tempfile::tempdir().unwrap();
        let prepared_revision = deployment
            .path()
            .join("workspace/revisions")
            .join(&original.identity);
        std::fs::create_dir_all(prepared_revision.parent().unwrap()).unwrap();
        copy_revision_tree(
            &original_layer.revisions().join(&original.identity),
            &prepared_revision,
        )
        .unwrap();
        let layer = SourceLayer::new(run.path());
        let seeded = layer
            .ensure_active_from_prepared(frozen.identity(), &prepared_revision)
            .unwrap();
        assert_eq!(seeded.identity, original.identity);
        let run_revision = run
            .path()
            .join("workspace/revisions")
            .join(&original.identity);
        assert!(std::fs::symlink_metadata(&run_revision)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            std::fs::canonicalize(&run_revision).unwrap(),
            prepared_revision
        );
        let retained = layer.checkpoint_revision(frozen.identity()).unwrap();
        assert_eq!(
            retained.paths,
            revision_include_paths(
                &prepared_revision,
                revision_root_count(&prepared_revision).unwrap()
            )
            .iter()
            .map(|path| std::fs::canonicalize(path).unwrap())
            .collect::<Vec<_>>()
        );
        std::fs::write(
            project.path().join(".exomonad/Project/Work.hs"),
            "module Project.Work where\nwork = 2\n",
        )
        .unwrap();
        let pending = layer
            .capture_from_workspace(&frozen, project.path())
            .unwrap();
        let updated = layer.publish(pending).unwrap();
        assert_ne!(updated.identity, seeded.identity);
        let live = layer.checkpoint_revision(frozen.identity()).unwrap();
        assert!(live.paths[0].starts_with(run.path().join("workspace/revisions")));
        let live_revision = run
            .path()
            .join("workspace/revisions")
            .join(&updated.identity);
        assert!(!std::fs::symlink_metadata(&live_revision)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            std::fs::read_to_string(prepared_revision.join("0/Project/Work.hs")).unwrap(),
            "module Project.Work where\nwork = 1\n"
        );
        assert_eq!(
            std::fs::read_to_string(live.paths[0].join("Project/Work.hs")).unwrap(),
            "module Project.Work where\nwork = 2\n"
        );

        // These selections exercise source/recipe observation only; no
        // compiled entry or compiler authority is manufactured by this case.
        let config = frozen.config().unwrap();
        let profiles = config
            .preparation
            .selected_profiles(config.research)
            .unwrap();
        let original_selection = uuid::Uuid::new_v4();
        let mut deployed = frozen.clone();
        deployed.preparation = Some(super::super::workspace::WorkspacePreparation::Completed {
            original: original_selection,
            revision: original.identity.clone(),
            coverage: profiles
                .into_iter()
                .map(|profile| super::super::workspace::PreparedToolsetCoverage {
                    profile: profile.profile,
                    requested_effects: profile.requested_effects,
                    effective_effects: Vec::new(),
                    recipe: "a".repeat(64),
                    original: original_selection,
                })
                .collect(),
        });
        deployed.prepared_deployment = Some(Arc::new(
            tidepool_atomic_write::DirectoryAnchor::open_existing(deployment.path()).unwrap(),
        ));
        let run = Arc::new(run);
        let mut reload = ExomonadSourceReload::new_owned(
            deployed,
            project.path().to_path_buf(),
            run.path().to_path_buf(),
            crate::haskell_sources::ensure_exomonad_haskell().unwrap(),
            SourceRootOwner::Temporary(Arc::clone(&run)),
        )
        .unwrap();
        let workbench = exomonad_actor::ActorWorkbenchSource::new(String::new(), Vec::new());
        let requested = reload.frozen.prepared_toolset_coverage().unwrap()[0]
            .requested_effects
            .clone();
        let preflight = reload.prepared_toolset_layer().unwrap();
        let original_recipe = workbench
            .source_toolset_recipe(&preflight, &requested, &[])
            .unwrap();
        let Some(super::super::workspace::WorkspacePreparation::Completed { coverage, .. }) =
            &mut reload.frozen.preparation
        else {
            panic!("the source observation retains its original coverage");
        };
        coverage[0].recipe = original_recipe.recipe.clone();
        reload.entry_storage = exomonad_actor::SourceEntryStorage::CompletedOriginal {
            directory: reload.entry_storage.directory().to_path_buf(),
            selections: reload.frozen.completed_entry_selections().unwrap(),
        };
        let preflight = reload.prepared_toolset_layer().unwrap();
        let current = reload.freeze_toolset_layer(PrincipalId::SYSTEM).unwrap();
        assert!(preflight
            .identities()
            .contains(&format!("run:{}", original.identity)));
        assert!(current
            .identities()
            .contains(&format!("run:{}", updated.identity)));
        assert_eq!(
            preflight.include_paths()[..retained.paths.len()],
            retained.paths
        );
        assert_eq!(current.include_paths()[..live.paths.len()], live.paths);
        for (expected, actual) in retained
            .manifests
            .iter()
            .zip(preflight.source_manifests().unwrap())
        {
            assert!(same_source_manifest(expected, actual));
        }
        reload
            .frozen
            .validate_prepared_toolset_recipes(&workbench, &preflight, &[])
            .unwrap();
        assert!(reload
            .frozen
            .validate_prepared_toolset_recipes(&workbench, &current, &[])
            .is_err());
        let current_recipe = workbench
            .source_toolset_recipe(&current, &requested, &[])
            .unwrap();
        assert_ne!(current_recipe.recipe, original_recipe.recipe);
        assert!(reload
            .frozen
            .completed_entry_selections()
            .unwrap()
            .contains_key(&original_recipe.recipe));
        assert!(!reload
            .frozen
            .completed_entry_selections()
            .unwrap()
            .contains_key(&current_recipe.recipe));
        let before = tidepool_extract_cmd::extract_spawn_count();
        assert!(matches!(
            workbench
                .prepare_source_toolset(
                    tidepool_toolchain::artifacts::CompileWorkload::Foreground,
                    current.clone(),
                    &requested,
                    &[],
                    Arc::new(tidepool_runtime::session::ImageRegistry::new()),
                )
                .await,
            Err(exomonad_actor::ResidentActorWorkbenchError::PreparedEntryAbsent { .. })
        ));
        assert_eq!(tidepool_extract_cmd::extract_spawn_count(), before);
        let fresh = reload.fresh_toolset_layer_from(&current).unwrap();
        assert_eq!(fresh.include_paths(), current.include_paths());
        assert!(fresh.same_revision(&current));
        assert!(matches!(
            fresh.prepared_entries(),
            Some(exomonad_actor::SourceEntryStorage::FreshCompilation { .. })
        ));
        assert_eq!(
            reload.prepared_toolset_layer().unwrap().semantic_digest(),
            preflight.semantic_digest()
        );
    }

    #[test]
    fn source_graph_keeps_workspace_resource_once_through_live_updates() {
        use exomonad_actor::ActorSourceLayers;

        let (project, run) = workspace_with("module Project.Work where\nwork = 1\n");
        let frozen = FrozenWorkspace::load(project.path(), run.path()).unwrap();
        let run = Arc::new(run);
        let reload = ExomonadSourceReload::new_owned(
            frozen,
            project.path().to_path_buf(),
            run.path().to_path_buf(),
            crate::haskell_sources::ensure_exomonad_haskell().unwrap(),
            SourceRootOwner::Temporary(Arc::clone(&run)),
        )
        .unwrap();
        let workspace_resource =
            std::fs::canonicalize(reload.frozen.workspace_resources()).unwrap();
        let workspace_identity = format!("workspace:{}", reload.frozen.identity());
        let selected = reload.freeze_toolset_layer(PrincipalId::SYSTEM).unwrap();
        assert_eq!(selected.identities().last(), Some(&workspace_identity));
        assert_eq!(selected.include_paths().last(), Some(&workspace_resource));
        assert_eq!(
            selected.source_manifests().unwrap().len(),
            selected.include_paths().len()
        );
        assert_eq!(
            selected
                .include_paths()
                .iter()
                .filter(|path| std::fs::canonicalize(path).ok()
                    == std::fs::canonicalize(&workspace_resource).ok())
                .count(),
            1
        );
        let checkpoint = reload.freeze_checkpoint_layer(PrincipalId::SYSTEM).unwrap();
        assert_eq!(checkpoint.include_paths().last(), Some(&workspace_resource));
        assert_eq!(
            checkpoint.source_manifests().unwrap().len(),
            checkpoint.include_paths().len()
        );
        assert_eq!(
            checkpoint
                .include_paths()
                .iter()
                .filter(|path| std::fs::canonicalize(path).ok()
                    == std::fs::canonicalize(&workspace_resource).ok())
                .count(),
            1
        );
        let projected = reload.toolset_layer_from(&checkpoint).unwrap();
        assert_eq!(projected.include_paths(), selected.include_paths());
        assert!(checkpoint
            .include_paths()
            .iter()
            .any(|path| path.starts_with(run.path().join("helpers"))));
        assert!(projected
            .include_paths()
            .iter()
            .all(|path| !path.starts_with(run.path().join("helpers"))));
        assert!(projected.include_paths().iter().all(|path| {
            path.starts_with(run.path().join("workspace/revisions")) || path == &workspace_resource
        }));

        std::fs::write(
            project.path().join(".exomonad/Project/Work.hs"),
            "module Project.Work where\nwork = 2\n",
        )
        .unwrap();
        let active = reload.layer.ensure_active(&reload.frozen).unwrap();
        let pending = reload
            .layer
            .capture_from_workspace(&reload.frozen, &reload.workspace)
            .unwrap();
        let updated = reload.layer.publish(pending).unwrap();
        assert_ne!(updated.identity, active.identity);
        let owned = reload
            .layer
            .checkpoint_revision(reload.frozen.identity())
            .unwrap();
        assert!(owned.paths[0].starts_with(run.path().join("workspace/revisions")));
        assert!(!std::fs::symlink_metadata(
            run.path()
                .join("workspace/revisions")
                .join(&updated.identity)
        )
        .unwrap()
        .file_type()
        .is_symlink());
        let after_reload = reload.freeze_toolset_layer(PrincipalId::SYSTEM).unwrap();
        let mut expected = owned.paths.clone();
        expected.push(workspace_resource.clone());
        assert_eq!(after_reload.include_paths(), expected);
        assert_eq!(
            after_reload.include_paths().last(),
            Some(&workspace_resource)
        );
        assert_eq!(
            after_reload
                .include_paths()
                .iter()
                .filter(|path| path.as_path() == workspace_resource.as_path())
                .count(),
            1
        );
        assert_eq!(
            after_reload.source_manifests().unwrap().len(),
            after_reload.include_paths().len()
        );
        assert!(same_source_manifest(
            after_reload.source_manifests().unwrap().last().unwrap(),
            &source_root_manifest(&workspace_resource).unwrap()
        ));
        assert!(!after_reload
            .include_paths()
            .iter()
            .any(|path| path != &workspace_resource && selected.include_paths().contains(path)));
    }

    fn workspace_with(source: &str) -> (tempfile::TempDir, tempfile::TempDir) {
        let project = tempfile::tempdir().unwrap();
        let run = tempfile::tempdir().unwrap();
        let authored = project.path().join(".exomonad");
        std::fs::create_dir_all(authored.join("Project")).unwrap();
        std::fs::write(
            authored.join("config.toml"),
            "[defaults]\nmodel = 'gpt-6-sol'\n[haskell]\nsource_roots = ['.']\nmodules = ['Project.Work']\n",
        )
        .unwrap();
        std::fs::write(authored.join("Project/Work.hs"), source).unwrap();
        (project, run)
    }

    #[test]
    fn first_root_helper_layer_seeds_its_draft_before_active_creation() {
        let (project, run) = workspace_with("module Project.Work where\nwork = 1\n");
        let seed = project.path().join(".exomonad/workspace/seeds/helpers");
        std::fs::create_dir_all(&seed).unwrap();
        let source = "module SessionHelpers where\nanswer :: Int\nanswer = 42\n";
        std::fs::write(seed.join("SessionHelpers.hs"), source).unwrap();
        let frozen = FrozenWorkspace::load(project.path(), run.path()).unwrap();
        let reload = ExomonadSourceReload::new(
            frozen,
            project.path().to_path_buf(),
            run.path().to_path_buf(),
            crate::haskell_sources::ensure_exomonad_haskell().unwrap(),
        );

        reload.ensure_helper_active("run").unwrap();

        assert_eq!(
            std::fs::read_to_string(reload.helper_draft("run").join("SessionHelpers.hs")).unwrap(),
            source
        );
        assert!(reload
            .helper_layer("run")
            .read_active()
            .unwrap()
            .unwrap()
            .modules
            .is_empty());
    }

    #[test]
    fn authored_helpers_initialize_the_run_draft_without_resurrecting_seed_modules() {
        let (project, run) = workspace_with("module Project.Work where\nwork = 1\n");
        let seed = project.path().join(".exomonad/workspace/seeds/helpers");
        std::fs::create_dir_all(seed.join("SessionHelpers")).unwrap();
        std::fs::write(
            seed.join("SessionHelpers/BrowserChecks.hs"),
            "module SessionHelpers.BrowserChecks where\n",
        )
        .unwrap();
        let authored = project.path().join(".exomonad/helpers");
        std::fs::create_dir_all(&authored).unwrap();
        std::fs::write(
            authored.join("SessionHelpers.hs"),
            "module SessionHelpers where\nanswer :: Int\nanswer = 41\n",
        )
        .unwrap();
        let frozen = FrozenWorkspace::load(project.path(), run.path()).unwrap();
        let reload = ExomonadSourceReload::new(
            frozen,
            project.path().to_path_buf(),
            run.path().to_path_buf(),
            crate::haskell_sources::ensure_exomonad_haskell().unwrap(),
        );

        let active = reload.ensure_helper_active("run").unwrap();
        let draft = reload.helper_draft("run");
        assert!(
            active.modules.is_empty(),
            "draft edits require explicit publication"
        );
        assert_eq!(
            std::fs::read_to_string(draft.join("SessionHelpers.hs")).unwrap(),
            "module SessionHelpers where\nanswer :: Int\nanswer = 41\n"
        );
        assert!(!draft.join("SessionHelpers/BrowserChecks.hs").exists());
        assert_eq!(
            reload.helper_layer("run").read_active().unwrap().unwrap(),
            active
        );
    }

    #[test]
    fn first_reload_rejects_an_invalid_authored_draft_without_publishing_it() {
        let (project, run) = workspace_with("module Project.Work where\nwork = 1\n");
        let authored = project.path().join(".exomonad/helpers");
        std::fs::create_dir_all(&authored).unwrap();
        let invalid = "module SessionHelpers where\nanswer = (\n";
        std::fs::write(authored.join("SessionHelpers.hs"), invalid).unwrap();
        let frozen = FrozenWorkspace::load(project.path(), run.path()).unwrap();
        let reload = ExomonadSourceReload::new(
            frozen,
            project.path().to_path_buf(),
            run.path().to_path_buf(),
            crate::haskell_sources::ensure_exomonad_haskell().unwrap(),
        );

        let outcome = reload.reload_helper_branch("run", &[], None);
        assert!(matches!(
            outcome,
            exomonad_actor::SourceLayerReload::Rejected { .. }
        ));
        assert_eq!(
            std::fs::read_to_string(reload.helper_draft("run").join("SessionHelpers.hs")).unwrap(),
            invalid
        );
        assert!(reload
            .helper_layer("run")
            .read_active()
            .unwrap()
            .unwrap()
            .modules
            .is_empty());
    }

    #[test]
    fn helper_reload_reports_corrupt_lower_run_record_without_publication() {
        let (project, run) = workspace_with("module Project.Work where\nwork = 1\n");
        let authored = project.path().join(".exomonad/helpers");
        std::fs::create_dir_all(&authored).unwrap();
        std::fs::write(
            authored.join("SessionHelpers.hs"),
            "module SessionHelpers where\nanswer = 42\n",
        )
        .unwrap();
        let frozen = FrozenWorkspace::load(project.path(), run.path()).unwrap();
        let reload = ExomonadSourceReload::new(
            frozen,
            project.path().to_path_buf(),
            run.path().to_path_buf(),
            crate::haskell_sources::ensure_exomonad_haskell().unwrap(),
        );
        let lower = reload.layer.ensure_active(&reload.frozen).unwrap();
        let helper = reload.ensure_helper_active("run").unwrap();
        std::fs::write(reload.layer.active_record(), b"corrupt source record").unwrap();

        let outcome = reload.reload_helper_branch("run", &[], None);

        assert!(matches!(
            outcome,
            exomonad_actor::SourceLayerReload::Unavailable(detail)
                if detail.contains("run source revision is unavailable")
        ));
        assert_eq!(
            reload.helper_layer("run").read_active().unwrap(),
            Some(helper)
        );
        assert!(reload.layer.revisions().join(lower.identity).is_dir());
    }

    #[test]
    fn prepared_helper_branch_refuses_a_partial_snapshot() {
        use exomonad_actor::ActorSourceLayers;

        let (project, run) = workspace_with("module Project.Work where\nwork = 1\n");
        let frozen = FrozenWorkspace::load(project.path(), run.path()).unwrap();
        let reload = ExomonadSourceReload::new(
            frozen,
            project.path().to_path_buf(),
            run.path().to_path_buf(),
            crate::haskell_sources::ensure_exomonad_haskell().unwrap(),
        );
        let draft = reload.helper_draft("prepared");
        std::fs::create_dir_all(&draft).unwrap();
        let result = ActorSourceLayers::prepare_helpers(
            &reload,
            PrincipalId::SYSTEM,
            &["prepared".to_owned()],
            true,
        );
        assert!(result.unwrap_err().contains("disagree"));
        assert!(reload
            .helper_layer("prepared")
            .read_active()
            .unwrap()
            .is_none());
    }

    #[test]
    fn failed_source_capture_removes_its_temporary_tree() {
        let run = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("A.hs"), "module A where").unwrap();
        let layer = SourceLayer::new(run.path());
        assert!(layer
            .capture_from_roots(
                "test",
                &[root.path().to_path_buf(), root.path().join("missing"),]
            )
            .is_err());
        assert_eq!(std::fs::read_dir(layer.revisions()).unwrap().count(), 0);
        assert_eq!(
            std::fs::read_to_string(root.path().join("A.hs")).unwrap(),
            "module A where"
        );
    }

    #[test]
    fn source_observation_is_read_only_fresh_and_matches_retained_revision() {
        let run = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("A.hs"), "module A where").unwrap();
        let layer = SourceLayer::new(run.path());
        let roots = [root.path().to_path_buf()];
        let observed = layer.observe_from_roots("test", &roots).unwrap();
        assert!(!layer.revisions().exists());
        let retained = layer.capture_from_roots("test", &roots).unwrap();
        assert_eq!(retained.revision(), &observed);
        assert!(retained.directory.join("0/A.hs").is_file());
        let observed_again = layer.observe_from_roots("test", &roots).unwrap();
        assert_eq!(observed_again, observed);
        assert_eq!(std::fs::read_dir(layer.revisions()).unwrap().count(), 1);
        std::fs::write(root.path().join("A.hs"), "module A where\na = 2").unwrap();
        let changed = layer.observe_from_roots("test", &roots).unwrap();
        assert_ne!(changed.identity, observed.identity);
        assert_ne!(changed.modules, observed.modules);
        assert_eq!(std::fs::read_dir(layer.revisions()).unwrap().count(), 1);
        assert_eq!(
            std::fs::read_to_string(retained.directory.join("0/A.hs")).unwrap(),
            "module A where"
        );
    }

    #[test]
    fn captured_manifest_preserves_identity_root_order_boots_and_header_policy() {
        let run = tempfile::tempdir().unwrap();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        for (root, value) in [(first.path(), "1"), (second.path(), "2")] {
            std::fs::write(root.join("A.hs"), format!("module A where\na = {value}")).unwrap();
            std::fs::write(root.join("A.hs-boot"), "module A where\na :: Int").unwrap();
            std::fs::write(root.join("L.lhs"), "> module L where").unwrap();
            std::fs::write(root.join("L.lhs-boot"), "> module L where").unwrap();
            std::fs::write(root.join("foreign.h"), "#define VALUE 1").unwrap();
            std::fs::create_dir(root.join("target")).unwrap();
            std::fs::write(root.join("target/Ignored.hs"), "module Ignored where").unwrap();
        }
        let layer = SourceLayer::new(run.path());
        let roots = [first.path().to_path_buf(), second.path().to_path_buf()];
        let retained = layer.capture_from_roots("test", &roots).unwrap();
        let captured_roots: Vec<_> = (0..roots.len())
            .map(|index| retained.directory.join(index.to_string()))
            .collect();
        let mut domain = DOMAIN.to_vec();
        domain.extend_from_slice(b"test");
        assert_eq!(
            retained.revision().identity,
            tidepool_toolchain::cache::source_roots_identity(&domain, &captured_roots).unwrap()
        );
        assert_eq!(
            retained.revision().modules,
            manifest_of_roots(&captured_roots).unwrap()
        );
        assert_eq!(retained.revision().modules.len(), 2);
        assert!(retained.directory.join("0/foreign.h").is_file());
        assert!(!retained.directory.join("0/target").exists());
        let observed = layer.observe_from_roots("test", &roots).unwrap();
        assert_eq!(observed, *retained.revision());
        std::fs::write(first.path().join("foreign.h"), "#define VALUE 2").unwrap();
        assert_eq!(layer.observe_from_roots("test", &roots).unwrap(), observed);
        let reversed = layer
            .observe_from_roots("test", &[roots[1].clone(), roots[0].clone()])
            .unwrap();
        assert_ne!(reversed.identity, observed.identity);
        assert_ne!(reversed.modules, observed.modules);
        std::fs::write(first.path().join("A.hs-boot"), "module A where\na :: Bool").unwrap();
        let changed_boot = layer.observe_from_roots("test", &roots).unwrap();
        assert_ne!(changed_boot.identity, observed.identity);
        assert_eq!(changed_boot.modules, observed.modules);
    }

    #[test]
    fn source_visibility_callback_pairs_state_before_durability_and_fences_cancellation() {
        use tidepool_runtime::session::{
            PublicationCancellation, PublicationDecision, PublicationPhase,
        };
        let run = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let roots = [root.path().to_owned()];
        std::fs::write(root.path().join("A.hs"), "module A where\na = 1").unwrap();
        let layer = SourceLayer::new(run.path());
        let active = layer
            .publish(layer.capture_from_roots("test", &roots).unwrap())
            .unwrap();
        std::fs::write(root.path().join("A.hs"), "module A where\na = 2").unwrap();
        let pending = layer.capture_from_roots("test", &roots).unwrap();
        let revision = pending.revision.identity.clone();
        let old_pair = Arc::new((active.identity.clone(), 1));
        let mut pair = old_pair.clone();
        let decision = PublicationDecision::new();
        let published = layer
            .publish_checked_with_visible(
                pending,
                Some(&active),
                Some(&decision),
                Some(Box::new(|| {
                    assert_eq!(
                        std::fs::read_link(layer.active_link()).unwrap(),
                        Path::new("revisions").join(&revision)
                    );
                    let previous: ActiveRecord =
                        serde_json::from_slice(&std::fs::read(layer.active_record()).unwrap())
                            .unwrap();
                    assert_eq!(
                        previous.identity, active.identity,
                        "callback precedes durable side-record confirmation"
                    );
                    assert_eq!(
                        decision.request_cancellation(),
                        PublicationCancellation::PendingCommitOutcome
                    );
                    pair = Arc::new((revision.clone(), 2));
                })),
            )
            .unwrap();
        assert_eq!(pair.as_ref(), &(published.identity.clone(), 2));
        assert_eq!(
            old_pair.as_ref(),
            &(active.identity, 1),
            "accepted callers retain the old pair"
        );
        assert_eq!(decision.phase(), PublicationPhase::Published);
        assert_eq!(layer.read_active().unwrap(), Some(published));
    }

    #[test]
    fn source_visibility_callback_does_not_run_for_cancelled_or_stale_candidates() {
        use tidepool_runtime::session::{PublicationDecision, PublicationPhase};
        let run = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let roots = [root.path().to_owned()];
        std::fs::write(root.path().join("A.hs"), "module A where\na = 1").unwrap();
        let layer = SourceLayer::new(run.path());
        let active = layer
            .publish(layer.capture_from_roots("test", &roots).unwrap())
            .unwrap();
        std::fs::write(root.path().join("A.hs"), "module A where\na = 2").unwrap();
        let cancelled = layer.capture_from_roots("test", &roots).unwrap();
        let stale = layer.capture_from_roots("test", &roots).unwrap();
        let decision = PublicationDecision::new();
        decision.request_cancellation();
        assert!(matches!(
            layer.publish_checked_with_visible(
                cancelled,
                Some(&active),
                Some(&decision),
                Some(Box::new(|| panic!("cancelled source cannot swap tools")))
            ),
            Err(SourcePublishFailure::Cancelled)
        ));
        assert_eq!(layer.read_active().unwrap(), Some(active.clone()));
        std::fs::write(root.path().join("A.hs"), "module A where\na = 3").unwrap();
        let latest = layer
            .publish(layer.capture_from_roots("test", &roots).unwrap())
            .unwrap();
        let decision = PublicationDecision::new();
        assert!(matches!(
            layer.publish_checked_with_visible(
                stale,
                Some(&active),
                Some(&decision),
                Some(Box::new(|| panic!("stale source cannot swap tools")))
            ),
            Err(SourcePublishFailure::BeforeVisibility(_))
        ));
        assert_eq!(decision.phase(), PublicationPhase::Running);
        assert_eq!(layer.read_active().unwrap(), Some(latest));
    }

    #[test]
    fn source_visible_durability_failure_keeps_new_pair_and_unconfirmed_decision() {
        use tidepool_runtime::session::{
            PublicationCancellation, PublicationDecision, PublicationPhase,
        };
        let run = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let roots = [root.path().to_owned()];
        std::fs::write(root.path().join("A.hs"), "module A where\na = 1").unwrap();
        let layer = SourceLayer::new(run.path());
        let active = layer
            .publish(layer.capture_from_roots("test", &roots).unwrap())
            .unwrap();
        std::fs::write(root.path().join("A.hs"), "module A where\na = 2").unwrap();
        let pending = layer.capture_from_roots("test", &roots).unwrap();
        let revision = pending.revision.identity.clone();
        let mut pair = (active.identity.clone(), 1);
        let decision = PublicationDecision::new();
        let outcome = layer.publish_checked_with_visible(
            pending,
            Some(&active),
            Some(&decision),
            Some(Box::new(|| {
                pair = (revision.clone(), 2);
                // Actual filesystem refusal at the existing durability owner.
                std::fs::remove_file(layer.active_record()).unwrap();
                std::fs::create_dir(layer.active_record()).unwrap();
            })),
        );
        assert!(
            matches!(outcome, Err(SourcePublishFailure::VisibleUnconfirmed { revision: visible, .. }) if visible == revision)
        );
        assert_eq!(pair, (revision.clone(), 2));
        assert_eq!(
            std::fs::read_link(layer.active_link()).unwrap(),
            Path::new("revisions").join(&revision)
        );
        assert_eq!(
            decision.phase(),
            PublicationPhase::CommitClaimed {
                cancellation_pending: false
            }
        );
        assert_eq!(
            decision.request_cancellation(),
            PublicationCancellation::PendingCommitOutcome
        );
        assert!(
            decision.claim_commit().is_none(),
            "visible installer cannot be replayed"
        );
    }

    #[test]
    fn staged_spec_candidates_are_immutable_fresh_and_only_one_can_publish() {
        use exomonad_actor::{ActorSourceLayers, SourceEntryStorage, SourceLayerReload};
        use tidepool_runtime::session::{PublicationDecision, PublicationPhase};
        let (project, run) = workspace_with("module Project.Work where\nwork = 1\n");
        let frozen = FrozenWorkspace::load(project.path(), run.path()).unwrap();
        let reload = Arc::new(ExomonadSourceReload::new(
            frozen,
            project.path().to_owned(),
            run.path().to_owned(),
            crate::haskell_sources::ensure_exomonad_haskell().unwrap(),
        ));
        let previous = reload.freeze_checkpoint_layer(PrincipalId::SYSTEM).unwrap();
        let active = reload.layer.read_active().unwrap().unwrap();
        std::fs::write(
            project.path().join(".exomonad/Project/Work.hs"),
            "module Project.Work where\nwork = 2\n",
        )
        .unwrap();
        let first = reload
            .clone()
            .stage_spec_reload(PrincipalId::SYSTEM, &[])
            .unwrap_or_else(|outcome| panic!("stage refused: {outcome:?}"));
        let second = reload
            .clone()
            .stage_spec_reload(PrincipalId::SYSTEM, &[])
            .unwrap_or_else(|outcome| panic!("stage refused: {outcome:?}"));
        assert_eq!(
            reload.layer.read_active().unwrap(),
            Some(active.clone()),
            "staging cannot publish source"
        );
        assert_eq!(first.source().identities(), second.source().identities());
        match (
            first.source().prepared_entries(),
            second.source().prepared_entries(),
        ) {
            (
                Some(SourceEntryStorage::FreshCompilation {
                    preparation: first, ..
                }),
                Some(SourceEntryStorage::FreshCompilation {
                    preparation: second,
                    ..
                }),
            ) => assert_ne!(first, second),
            _ => panic!(
                "edited candidates must compile fresh rather than reuse an original selection"
            ),
        }
        let retained = first.source().clone();
        reload.validate_source_authority(&retained).unwrap();
        std::fs::write(
            project.path().join(".exomonad/Project/Work.hs"),
            "module Project.Work where\nwork = 3\n",
        )
        .unwrap();
        let selected = retained
            .include_paths()
            .iter()
            .find(|path| path.join("Project/Work.hs").is_file())
            .unwrap();
        assert!(std::fs::read_to_string(selected.join("Project/Work.hs"))
            .unwrap()
            .contains("work = 2"));
        let mut pair = previous;
        let first_outcome = first.commit(
            &PublicationDecision::new(),
            Box::new(|| pair = retained.clone()),
        );
        assert!(matches!(first_outcome, SourceLayerReload::Published { .. }));
        assert_eq!(pair, retained);
        let decision = PublicationDecision::new();
        assert!(matches!(
            second.commit(&decision, Box::new(|| panic!("second candidate is stale"))),
            SourceLayerReload::Rejected { .. }
        ));
        assert_eq!(decision.phase(), PublicationPhase::Running);
        assert_eq!(pair, retained);
    }

    #[test]
    fn checked_source_publication_cancellation_keeps_active_revision() {
        use tidepool_runtime::session::{PublicationDecision, PublicationPhase};
        let run = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("A.hs"), "module A where\na = 1").unwrap();
        let layer = SourceLayer::new(run.path());
        let roots = [root.path().to_path_buf()];
        let active = layer
            .publish(layer.capture_from_roots("test", &roots).unwrap())
            .unwrap();
        std::fs::write(root.path().join("A.hs"), "module A where\na = 2").unwrap();
        let pending = layer.capture_from_roots("test", &roots).unwrap();
        let decision = PublicationDecision::new();
        decision.request_cancellation();
        assert!(matches!(
            layer.publish_checked(pending, Some(&active), Some(&decision)),
            Err(SourcePublishFailure::Cancelled)
        ));
        assert_eq!(decision.phase(), PublicationPhase::CancellationRequested);
        assert_eq!(layer.read_active().unwrap(), Some(active));
        assert!(!run.path().read_dir().unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".active-")
        }));
    }

    #[test]
    fn checked_source_publication_revalidates_exact_active_generation() {
        use tidepool_runtime::session::{PublicationDecision, PublicationPhase};
        let run = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("A.hs"), "module A where\na = 1").unwrap();
        let layer = SourceLayer::new(run.path());
        let roots = [root.path().to_path_buf()];
        let active = layer
            .publish(layer.capture_from_roots("test", &roots).unwrap())
            .unwrap();
        std::fs::write(root.path().join("A.hs"), "module A where\na = 2").unwrap();
        let stale = layer.capture_from_roots("test", &roots).unwrap();
        std::fs::write(root.path().join("A.hs"), "module A where\na = 3").unwrap();
        let latest = layer
            .publish(layer.capture_from_roots("test", &roots).unwrap())
            .unwrap();
        let decision = PublicationDecision::new();
        assert!(matches!(
            layer.publish_checked(stale, Some(&active), Some(&decision)),
            Err(SourcePublishFailure::BeforeVisibility(_))
        ));
        assert_eq!(decision.phase(), PublicationPhase::Running);
        assert_eq!(layer.read_active().unwrap(), Some(latest));
    }

    #[test]
    fn checked_source_publication_settles_the_native_commit_claim() {
        use tidepool_runtime::session::{
            PublicationCancellation, PublicationDecision, PublicationPhase,
        };
        let run = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("A.hs"), "module A where\na = 1").unwrap();
        let layer = SourceLayer::new(run.path());
        let roots = [root.path().to_path_buf()];
        let active = layer
            .publish(layer.capture_from_roots("test", &roots).unwrap())
            .unwrap();
        std::fs::write(root.path().join("A.hs"), "module A where\na = 2").unwrap();
        let pending = layer.capture_from_roots("test", &roots).unwrap();
        let decision = PublicationDecision::new();
        let published = layer
            .publish_checked(pending, Some(&active), Some(&decision))
            .unwrap();
        assert_eq!(decision.phase(), PublicationPhase::Published);
        assert_eq!(
            decision.request_cancellation(),
            PublicationCancellation::AlreadyPublished
        );
        assert_eq!(layer.read_active().unwrap(), Some(published));
    }

    #[test]
    fn source_observation_rejects_partial_and_symlinked_inputs_without_writes() {
        let run = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("A.hs"), "module A where").unwrap();
        let layer = SourceLayer::new(run.path());
        assert!(layer
            .observe_from_roots(
                "test",
                &[root.path().to_path_buf(), root.path().join("missing")]
            )
            .is_err());
        assert!(!layer.revisions().exists());
        std::os::unix::fs::symlink(root.path().join("A.hs"), root.path().join("Alias.hs")).unwrap();
        let error = layer
            .observe_from_roots("test", &[root.path().to_path_buf()])
            .unwrap_err();
        assert!(error.to_string().contains("symlink"));
        assert!(!layer.revisions().exists());
    }

    /// Identity is content, not time, path or capture: the same bytes captured
    /// twice name the same revision, and one changed byte names a different
    /// one.
    #[test]
    fn revision_identity_is_content_based() {
        let (project, run) = workspace_with("module Project.Work where\nwork :: Int\nwork = 1\n");
        let frozen = FrozenWorkspace::load(project.path(), run.path()).unwrap();
        let layer = SourceLayer::new(run.path());
        let first = layer.ensure_active(&frozen).unwrap();

        let again = layer
            .capture_from_workspace(&frozen, project.path())
            .unwrap();
        assert_eq!(again.revision().identity, first.identity);

        std::fs::write(
            project.path().join(".exomonad/Project/Work.hs"),
            "module Project.Work where\nwork :: Int\nwork = 2\n",
        )
        .unwrap();
        let changed = layer
            .capture_from_workspace(&frozen, project.path())
            .unwrap();
        assert_ne!(changed.revision().identity, first.identity);
        assert_eq!(
            layer.changed_modules(&first, changed.revision()),
            vec!["Project.Work".to_string()]
        );
    }

    /// Publishing moves one symlink and leaves the run's verified capture
    /// exactly as it was, so the run still loads.
    #[test]
    fn publishing_a_revision_leaves_the_frozen_capture_verifiable() {
        let (project, run) = workspace_with("module Project.Work where\nwork :: Int\nwork = 1\n");
        let frozen = FrozenWorkspace::load(project.path(), run.path()).unwrap();
        let layer = SourceLayer::new(run.path());
        let first = layer.ensure_active(&frozen).unwrap();
        assert_eq!(first.generation, 1);

        std::fs::write(
            project.path().join(".exomonad/Project/Work.hs"),
            "module Project.Work where\nwork :: Int\nwork = 2\n",
        )
        .unwrap();
        let pending = layer
            .capture_from_workspace(&frozen, project.path())
            .unwrap();
        let include = layer.include_paths(1);
        let published = layer.publish(pending).unwrap();
        assert_eq!(published.generation, 2);
        assert_eq!(layer.read_active().unwrap().unwrap(), published);

        // The include vector is unchanged; only what it resolves to moved.
        assert_eq!(include, layer.include_paths(1));
        assert!(std::fs::read_to_string(include[0].join("Project/Work.hs"))
            .unwrap()
            .contains("work = 2"));

        // Compile-time provenance travels with the revision: the generated
        // module on the search path names the snapshot that built whatever
        // imports it.
        let generated = std::fs::read_to_string(
            include
                .last()
                .expect("the revision's resources are on the search path")
                .join(REVISION_MODULE),
        )
        .unwrap();
        assert!(
            generated.contains(&published.identity),
            "{generated}\n{}",
            published.identity
        );

        // …and the run still loads, which is the tamper check passing.
        FrozenWorkspace::load(project.path(), run.path()).unwrap();
    }

    #[test]
    fn inherited_active_revision_is_a_private_branch_snapshot() {
        let (project, run) = workspace_with("module Project.Work where\nwork :: Int\nwork = 1\n");
        let frozen = FrozenWorkspace::load(project.path(), run.path()).unwrap();
        let roots = frozen.captured_source_roots().to_vec();
        let parent = SourceLayer::new(run.path());
        let first = parent.ensure_active(&frozen).unwrap();
        std::fs::write(
            project.path().join(".exomonad/Project/Work.hs"),
            "module Project.Work where\nwork :: Int\nwork = 2\n",
        )
        .unwrap();
        let parent_at_fork = parent
            .publish(
                parent
                    .capture_from_workspace(&frozen, project.path())
                    .unwrap(),
            )
            .unwrap();
        assert_ne!(first.identity, parent_at_fork.identity);

        let child = SourceLayer {
            directory: run.path().join("workspace/checkouts/child"),
            retained_revision: Default::default(),
        };
        let inherited = child.inherit_active_from(&parent, &roots).unwrap();
        assert_eq!(inherited.identity, parent_at_fork.identity);
        assert_eq!(child.read_active().unwrap(), Some(inherited.clone()));
        assert_ne!(child.active_link(), parent.active_link());

        std::fs::write(
            project.path().join(".exomonad/Project/Work.hs"),
            "module Project.Work where\nwork :: Int\nwork = 3\n",
        )
        .unwrap();
        let later_parent = parent
            .publish(
                parent
                    .capture_from_workspace(&frozen, project.path())
                    .unwrap(),
            )
            .unwrap();
        assert_ne!(later_parent.identity, inherited.identity);
        assert_eq!(child.read_active().unwrap(), Some(inherited));
        assert!(
            std::fs::read_to_string(child.include_paths(1)[0].join("Project/Work.hs"))
                .unwrap()
                .contains("work = 2")
        );
    }

    #[test]
    fn helper_revision_tracks_added_modules_and_deletions() {
        let root = tempfile::tempdir().unwrap();
        let draft = root.path().join("drafts/run");
        std::fs::create_dir_all(draft.join("SessionHelpers")).unwrap();
        std::fs::write(
            draft.join("SessionHelpers.hs"),
            "module SessionHelpers where\nvalue = 1\n",
        )
        .unwrap();
        let layer = SourceLayer::helpers(root.path(), "run");
        let first = layer
            .ensure_active_from("helpers-test", std::slice::from_ref(&draft))
            .unwrap();
        std::fs::write(
            draft.join("SessionHelpers/Extra.hs"),
            "module SessionHelpers.Extra where\nextra = 2\n",
        )
        .unwrap();
        let added = layer
            .publish(
                layer
                    .capture_from_roots("helpers-test", std::slice::from_ref(&draft))
                    .unwrap(),
            )
            .unwrap();
        assert!(added
            .modules
            .iter()
            .any(|(module, _)| module == "SessionHelpers.Extra"));
        assert_eq!(
            changed_modules(&first, &added),
            vec!["SessionHelpers.Extra".to_owned()]
        );

        std::fs::remove_file(draft.join("SessionHelpers/Extra.hs")).unwrap();
        let deleted = layer
            .capture_from_roots("helpers-test", std::slice::from_ref(&draft))
            .unwrap();
        assert!(changed_modules(&added, deleted.revision())
            .contains(&"SessionHelpers.Extra".to_owned()));
        let published = layer.publish(deleted).unwrap();
        assert!(!published
            .modules
            .iter()
            .any(|(module, _)| module == "SessionHelpers.Extra"));
    }

    #[test]
    fn helper_reload_typechecks_before_publish_and_never_commits_workspace() {
        let (project, run) = workspace_with("module Project.Work where\nwork = 1\n");
        init_git_repo(project.path());
        let git = GitCli::new();
        let head_before = git.try_run(project.path(), &["rev-parse", "HEAD"]).unwrap();
        let frozen = FrozenWorkspace::load(project.path(), run.path()).unwrap();
        let helper_root = run.path().join("helpers");
        let reload = ExomonadSourceReload::new(
            frozen,
            project.path().to_path_buf(),
            run.path().to_path_buf(),
            crate::haskell_sources::ensure_exomonad_haskell().unwrap(),
        )
        .with_helper_root(helper_root.clone());
        let draft = SourceLayer::helper_draft(&helper_root, "run");
        std::fs::create_dir_all(&draft).unwrap();
        std::fs::write(
            draft.join("SessionHelpers.hs"),
            "module SessionHelpers where\nvalue = (\n",
        )
        .unwrap();
        let helper_actor = PrincipalId::new(1, 1);
        exomonad_actor::ActorSourceLayers::bind(&reload, helper_actor, &[]);
        exomonad_actor::ActorSourceLayers::layer_include(&reload, &[]).unwrap();
        assert!(reload
            .helper_layer("run")
            .read_active()
            .unwrap()
            .unwrap()
            .modules
            .is_empty());
        // A helper checks against the published run layer, including source
        // introduced after the frozen workspace was captured.
        std::fs::write(
            project.path().join(".exomonad/Project/Work.hs"),
            "module Project.Work where\nfreshWork :: Int\nfreshWork = 2\n",
        )
        .unwrap();
        let updated =
            tidepool_handlers::SourceReloadService::reload(&reload, PrincipalId::SYSTEM, &[], None)
                .unwrap();
        assert!(
            matches!(
                updated,
                tidepool_bridge_effects::SrReloadOutcome::ReloadPublished(..)
            ),
            "{updated:?}"
        );
        std::fs::write(
            draft.join("SessionHelpers.hs"),
            "module SessionHelpers where\nimport Project.Work\nvalue :: Int\nvalue = freshWork\n",
        )
        .unwrap();

        let published =
            exomonad_actor::ActorSourceLayers::reload_helpers(&reload, helper_actor, &[]);
        let exomonad_actor::SourceLayerReload::Published { revision, .. } = published else {
            panic!("valid helper source should publish: {published:?}");
        };
        let layer = reload.helper_layer("run");
        assert_eq!(layer.read_active().unwrap().unwrap().identity, revision);

        std::fs::write(
            draft.join("SessionHelpers.hs"),
            "module SessionHelpers where\nvalue = (\n",
        )
        .unwrap();
        let rejected =
            exomonad_actor::ActorSourceLayers::reload_helpers(&reload, helper_actor, &[]);
        assert!(matches!(
            rejected,
            exomonad_actor::SourceLayerReload::Rejected { .. }
        ));
        assert_eq!(layer.read_active().unwrap().unwrap().identity, revision);
        assert!(
            !run.path().join("reload-checks").exists(),
            "checking must not build a scratch driver session"
        );
        assert_eq!(
            git.try_run(project.path(), &["rev-parse", "HEAD"]).unwrap(),
            head_before
        );
    }

    #[test]
    fn reload_check_preserves_spec_and_also_check_graph_before_publication() {
        let (project, run) = workspace_with("module Project.Work where\nwork :: Int\nwork = 1\n");
        let authored = project.path().join(".exomonad");
        let spec = authored.join("AgentSpec.hs");
        std::fs::write(&spec, "module AgentSpec where\nanswer :: Int\nanswer = 1\n").unwrap();
        let extra = authored.join("Project/Extra.hs");
        let valid_extra = "module Project.Extra where\nanswer :: Int\nanswer = 1\n";
        std::fs::write(&extra, valid_extra).unwrap();
        let frozen = FrozenWorkspace::load(project.path(), run.path()).unwrap();
        let reload = ExomonadSourceReload::new(
            frozen,
            project.path().to_path_buf(),
            run.path().to_path_buf(),
            crate::haskell_sources::ensure_exomonad_haskell().unwrap(),
        );
        let before = reload.layer.ensure_active(&reload.frozen).unwrap();
        for (path, source, also_check, diagnostic) in [
            (
                &extra,
                "module Project.Extra where\nanswer :: Int\nanswer = True\n",
                vec!["Project.Extra".to_owned()],
                "Extra.hs",
            ),
            (
                &spec,
                "module AgentSpec where\nanswer :: Int\nanswer = True\n",
                vec![],
                "AgentSpec.hs",
            ),
        ] {
            std::fs::write(&extra, valid_extra).unwrap();
            std::fs::write(path, source).unwrap();
            let outcome = tidepool_handlers::SourceReloadService::reload(
                &reload,
                PrincipalId::SYSTEM,
                &also_check,
                None,
            )
            .unwrap();
            let tidepool_bridge_effects::SrReloadOutcome::ReloadRejected(_, _, diagnostics) =
                outcome
            else {
                panic!("invalid checked module must reject the whole candidate: {outcome:?}");
            };
            assert!(diagnostics.contains(diagnostic), "{diagnostics}");
            assert_eq!(reload.layer.read_active().unwrap().unwrap(), before);
        }
        assert!(!run.path().join("reload-checks").exists());
    }

    #[test]
    fn actors_on_one_worktree_publish_helpers_independently() {
        use exomonad_actor::ActorSourceLayers;

        let (project, run) = workspace_with("module Project.Work where\nwork = 1\n");
        let frozen = FrozenWorkspace::load(project.path(), run.path()).unwrap();
        let helper_root = run.path().join("helpers");
        let reload = ExomonadSourceReload::new(
            frozen,
            project.path().to_path_buf(),
            run.path().to_path_buf(),
            crate::haskell_sources::ensure_exomonad_haskell().unwrap(),
        )
        .with_helper_root(helper_root.clone());
        let worktrees = vec!["shared-worktree".to_owned()];
        let parent = PrincipalId::new(1, 1);
        let child = PrincipalId::new(2, 1);
        let parent_branch = "parent";
        ActorSourceLayers::bind_for(&reload, parent, parent_branch, &worktrees);
        let parent_draft = reload.helper_draft(parent_branch);
        std::fs::write(
            parent_draft.join("SessionHelpers.hs"),
            "module SessionHelpers where\nvalue :: Int\nvalue = 1\n",
        )
        .unwrap();
        let parent_revision = match ActorSourceLayers::reload_helpers(&reload, parent, &[]) {
            exomonad_actor::SourceLayerReload::Published { revision, .. } => revision,
            result => panic!("parent helper publish failed: {result:?}"),
        };

        let child_branch =
            ActorSourceLayers::prepare_helpers(&reload, parent, &worktrees, false).unwrap();
        assert_ne!(child_branch, parent_branch);
        ActorSourceLayers::bind_for(&reload, child, &child_branch, &worktrees);
        assert_eq!(
            reload
                .helper_layer(&child_branch)
                .read_active()
                .unwrap()
                .unwrap()
                .identity,
            parent_revision
        );
        let child_draft = reload.helper_draft(&child_branch);
        assert!(
            std::fs::read_to_string(child_draft.join("SessionHelpers.hs"))
                .unwrap()
                .contains("value = 1")
        );
        std::fs::write(
            child_draft.join("SessionHelpers.hs"),
            "module SessionHelpers where\nvalue :: Int\nvalue = 2\n",
        )
        .unwrap();
        let child_revision = match ActorSourceLayers::reload_helpers(&reload, child, &[]) {
            exomonad_actor::SourceLayerReload::Published { revision, .. } => revision,
            result => panic!("child helper publish failed: {result:?}"),
        };
        assert_ne!(child_revision, parent_revision);
        assert_eq!(
            reload
                .helper_layer(parent_branch)
                .read_active()
                .unwrap()
                .unwrap()
                .identity,
            parent_revision
        );
        assert!(
            std::fs::read_to_string(parent_draft.join("SessionHelpers.hs"))
                .unwrap()
                .contains("value = 1")
        );
        assert_ne!(
            ActorSourceLayers::layer_include_for(&reload, parent_branch, &worktrees).unwrap()[0],
            ActorSourceLayers::layer_include_for(&reload, &child_branch, &worktrees).unwrap()[0]
        );
        std::fs::write(
            parent_draft.join("SessionHelpers.hs"),
            "module SessionHelpers where\nvalue :: Int\nvalue = 3\n",
        )
        .unwrap();
        let later_parent = match ActorSourceLayers::reload_helpers(&reload, parent, &[]) {
            exomonad_actor::SourceLayerReload::Published { revision, .. } => revision,
            result => panic!("later parent helper publish failed: {result:?}"),
        };
        assert_ne!(later_parent, parent_revision);
        assert_eq!(
            reload
                .helper_layer(&child_branch)
                .read_active()
                .unwrap()
                .unwrap()
                .identity,
            child_revision,
            "a later parent publication cannot change an existing child"
        );
    }

    #[test]
    fn missing_active_record_is_reconciled_from_the_published_link() {
        let (project, run) = workspace_with("module Project.Work where\nwork :: Int\nwork = 1\n");
        let frozen = FrozenWorkspace::load(project.path(), run.path()).unwrap();
        let layer = SourceLayer::new(run.path());
        let published = layer.ensure_active(&frozen).unwrap();
        std::fs::remove_file(layer.active_record()).unwrap();

        assert_eq!(layer.read_active().unwrap(), Some(published));
        assert!(layer.active_record().is_file());
    }

    #[test]
    fn corrupt_published_root_indexes_remain_visibly_unavailable() {
        let (project, run) = workspace_with("module Project.Work where\nwork :: Int\nwork = 1\n");
        let frozen = FrozenWorkspace::load(project.path(), run.path()).unwrap();
        let layer = SourceLayer::new(run.path());
        let published = layer.ensure_active(&frozen).unwrap();
        let revision = layer.revisions().join(published.identity);
        std::fs::rename(revision.join("0"), revision.join("1")).unwrap();

        assert!(layer
            .read_active()
            .unwrap_err()
            .to_string()
            .contains("contiguous"));
    }

    #[test]
    fn stale_active_record_is_reconciled_without_republishing_source() {
        let (project, run) = workspace_with("module Project.Work where\nwork :: Int\nwork = 1\n");
        let frozen = FrozenWorkspace::load(project.path(), run.path()).unwrap();
        let layer = SourceLayer::new(run.path());
        let first = layer.ensure_active(&frozen).unwrap();
        let old_record = std::fs::read(layer.active_record()).unwrap();
        std::fs::write(
            project.path().join(".exomonad/Project/Work.hs"),
            "module Project.Work where\nwork :: Int\nwork = 2\n",
        )
        .unwrap();
        let second = layer
            .publish(
                layer
                    .capture_from_workspace(&frozen, project.path())
                    .unwrap(),
            )
            .unwrap();
        assert_ne!(first.identity, second.identity);

        // Simulate a crash after the atomic link switch but before its side
        // record became durable.
        std::fs::write(layer.active_record(), old_record).unwrap();
        assert_eq!(layer.read_active().unwrap(), Some(second));
    }

    // ------------------------------------------------------------------
    // The reload transaction itself, checked by the run's own compiler.
    // ------------------------------------------------------------------

    /// A two-module workspace: `Project.Work` is the configured module and it
    /// reads `Project.Types`, so `Types` has a reverse dependency to rebuild.
    fn cooperating_pair() -> (tempfile::TempDir, tempfile::TempDir, ExomonadSourceReload) {
        let project = tempfile::tempdir().unwrap();
        let run = tempfile::tempdir().unwrap();
        let authored = project.path().join(".exomonad");
        std::fs::create_dir_all(authored.join("Project")).unwrap();
        std::fs::write(
            authored.join("config.toml"),
            "[defaults]\nmodel = 'gpt-6-sol'\n[haskell]\nsource_roots = ['workspace']\nmodules = ['Project.Work']\n",
        )
        .unwrap();
        std::fs::write(
            project.path().join(".gitmodules"),
            "[submodule \"workspace\"]\n\tpath = .exomonad/workspace\n\turl = ../exomonad-default-workspace.git\n",
        )
        .unwrap();
        std::fs::create_dir_all(authored.join("workspace/Project")).unwrap();
        write_types(project.path(), "evidenceValue");
        write_work(project.path(), "evidenceValue");
        init_git_repo(authored.join("workspace").as_path());
        init_git_repo(project.path());
        let frozen = FrozenWorkspace::load(project.path(), run.path()).unwrap();
        let reload = ExomonadSourceReload::new(
            frozen,
            project.path().to_path_buf(),
            run.path().to_path_buf(),
            crate::haskell_sources::ensure_exomonad_haskell().unwrap(),
        );
        (project, run, reload)
    }

    fn write_types(project: &Path, accessor: &str) {
        std::fs::write(
            project.join(".exomonad/workspace/Project/Types.hs"),
            format!(
                "module Project.Types (Evidence(..), {accessor}) where\n\
                 \n\
                 newtype Evidence = Evidence Int\n\
                 \n\
                 {accessor} :: Evidence -> Int\n\
                 {accessor} (Evidence n) = n\n"
            ),
        )
        .unwrap();
    }

    /// `Project.Work` also imports a library module, as the harness project's
    /// `Jev.Operators` imports `Jev.Host`: a published revision must keep
    /// resolving the library beneath it, not just the workspace's own modules.
    fn write_work(project: &Path, accessor: &str) {
        std::fs::write(
            project.join(".exomonad/workspace/Project/Work.hs"),
            format!(
                "module Project.Work (describe) where\n\
                 \n\
                 import Jev.Host ()\n\
                 import Project.Types\n\
                 \n\
                 describe :: Evidence -> Int\n\
                 describe e = {accessor} e + 1\n"
            ),
        )
        .unwrap();
    }

    fn init_git_repo(path: &Path) {
        let git = GitCli::new();
        git.init_repository(path, &["-q"]).unwrap();
        git.try_run(path, &["config", "user.name", "Reload test"])
            .unwrap();
        git.try_run(
            path,
            &["config", "user.email", "reload-test@example.invalid"],
        )
        .unwrap();
        git.try_run(path, &["add", "--all"]).unwrap();
        git.try_run(path, &["commit", "-q", "-m", "initial"])
            .unwrap();
    }

    #[test]
    fn workspace_gitlink_commit_uses_captured_bytes_and_reports_later_drift() {
        let project = tempfile::tempdir().unwrap();
        let workspace = project.path().join(".exomonad/workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(
            project.path().join(".gitmodules"),
            "[submodule \"workspace\"]\n\tpath = .exomonad/workspace\n\turl = ../exomonad-default-workspace.git\n",
        )
        .unwrap();
        std::fs::write(workspace.join("FieldNotes.hs"), "module FieldNotes where\n").unwrap();
        std::fs::create_dir(workspace.join("target")).unwrap();
        std::fs::write(
            workspace.join("target/Ignored.hs"),
            "module Ignored where\n",
        )
        .unwrap();
        init_git_repo(&workspace);
        init_git_repo(project.path());

        let capture = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(capture.path().join("0")).unwrap();
        let captured = "module FieldNotes where\n-- checked snapshot\n";
        std::fs::write(capture.path().join("0/FieldNotes.hs"), captured).unwrap();
        std::fs::write(
            workspace.join("FieldNotes.hs"),
            "module FieldNotes where\n-- changed after capture\n",
        )
        .unwrap();

        let outcome = commit_captured_workspace(
            project.path(),
            &[workspace.canonicalize().unwrap()],
            capture.path(),
            None,
            &["FieldNotes".to_owned()],
        );
        let tidepool_bridge_effects::SrWorkspaceCommitOutcome::WorkspaceCommitted(oid, drift) =
            outcome
        else {
            panic!("expected captured workspace commit, got {outcome:?}");
        };
        assert_eq!(drift, vec!["FieldNotes.hs"]);

        let git = GitCli::new();
        assert_eq!(
            git.try_run(&workspace, &["show", &format!("{oid}:FieldNotes.hs")])
                .unwrap()
                .stdout,
            captured
        );
        assert_eq!(
            git.try_run(&workspace, &["log", "-1", "--format=%s"])
                .unwrap()
                .stdout
                .trim(),
            "Update workspace: FieldNotes"
        );
        let parent_index = git
            .try_run(
                project.path(),
                &["ls-files", "--stage", "--", ".exomonad/workspace"],
            )
            .unwrap();
        assert!(parent_index.stdout.contains(&oid));
        assert!(parent_index.stdout.starts_with("160000 "));
    }

    #[test]
    fn workspace_gitlink_commit_removes_a_tracked_lhs_boot_source() {
        let project = tempfile::tempdir().unwrap();
        let workspace = project.path().join(".exomonad/workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(
            project.path().join(".gitmodules"),
            "[submodule \"workspace\"]\n\tpath = .exomonad/workspace\n\turl = ../exomonad-default-workspace.git\n",
        )
        .unwrap();
        std::fs::write(workspace.join("L.lhs-boot"), "> module L where\n").unwrap();
        init_git_repo(&workspace);
        init_git_repo(project.path());

        let capture = tempfile::tempdir().unwrap();
        let captured_root = capture.path().join("0");
        std::fs::create_dir_all(&captured_root).unwrap();
        std::fs::remove_file(workspace.join("L.lhs-boot")).unwrap();

        let outcome = commit_captured_workspace(
            project.path(),
            &[workspace.canonicalize().unwrap()],
            capture.path(),
            None,
            &[],
        );
        let tidepool_bridge_effects::SrWorkspaceCommitOutcome::WorkspaceCommitted(oid, drift) =
            outcome
        else {
            panic!("expected captured workspace commit, got {outcome:?}");
        };
        assert!(drift.is_empty());
        assert!(GitCli::new()
            .try_run(
                &workspace,
                &["cat-file", "-e", &format!("{oid}:L.lhs-boot")]
            )
            .is_err());
    }

    #[test]
    fn workspace_commit_failure_is_typed_and_retains_a_commit_if_index_reconcile_fails() {
        let project = tempfile::tempdir().unwrap();
        let workspace = project.path().join(".exomonad/workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(
            project.path().join(".gitmodules"),
            "[submodule \"workspace\"]\n\tpath = .exomonad/workspace\n\turl = ../exomonad-default-workspace.git\n",
        )
        .unwrap();
        std::fs::write(workspace.join("FieldNotes.hs"), "module FieldNotes where\n").unwrap();
        init_git_repo(&workspace);
        init_git_repo(project.path());
        let capture = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(capture.path().join("0")).unwrap();
        std::fs::write(
            capture.path().join("0/FieldNotes.hs"),
            "module FieldNotes where\n-- captured\n",
        )
        .unwrap();
        std::fs::write(
            workspace.join("FieldNotes.hs"),
            "module FieldNotes where\n-- captured\n",
        )
        .unwrap();
        std::fs::write(workspace.join(".git/index.lock"), "occupied").unwrap();

        let outcome = commit_captured_workspace(
            project.path(),
            &[workspace.canonicalize().unwrap()],
            capture.path(),
            None,
            &["FieldNotes".to_owned()],
        );
        let tidepool_bridge_effects::SrWorkspaceCommitOutcome::WorkspaceCommitFailed(
            reason,
            Some(oid),
            _,
        ) = outcome
        else {
            panic!("expected post-commit index failure with its commit id: {outcome:?}");
        };
        assert!(reason.contains("index.lock"), "{reason}");
        assert_eq!(
            GitCli::new()
                .try_run(&workspace, &["rev-parse", "HEAD"])
                .unwrap()
                .stdout
                .trim(),
            oid
        );
        std::fs::remove_file(workspace.join(".git/index.lock")).unwrap();
    }

    #[test]
    fn staged_parent_gitlink_mismatch_is_not_overwritten() {
        let project = tempfile::tempdir().unwrap();
        let workspace = project.path().join(".exomonad/workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(
            project.path().join(".gitmodules"),
            "[submodule \"workspace\"]\n\tpath = .exomonad/workspace\n\turl = ../exomonad-default-workspace.git\n",
        )
        .unwrap();
        std::fs::write(workspace.join("FieldNotes.hs"), "module FieldNotes where\n").unwrap();
        init_git_repo(&workspace);
        init_git_repo(project.path());
        let git = GitCli::new();
        git.try_run(
            &workspace,
            &["commit", "--allow-empty", "-m", "checkout moved"],
        )
        .unwrap();
        let prior_head = git.try_run(&workspace, &["rev-parse", "HEAD"]).unwrap();
        let parent_index = git
            .try_run(
                project.path(),
                &["ls-files", "--stage", "--", ".exomonad/workspace"],
            )
            .unwrap();
        let capture = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(capture.path().join("0")).unwrap();
        std::fs::write(
            capture.path().join("0/FieldNotes.hs"),
            "module FieldNotes where\n-- captured\n",
        )
        .unwrap();

        let outcome = commit_captured_workspace(
            project.path(),
            &[workspace.canonicalize().unwrap()],
            capture.path(),
            None,
            &["FieldNotes".to_owned()],
        );
        let tidepool_bridge_effects::SrWorkspaceCommitOutcome::WorkspaceCommitFailed(_, None, _) =
            outcome
        else {
            panic!("staged gitlink mismatch should fail before a workspace commit: {outcome:?}");
        };
        assert_eq!(
            git.try_run(&workspace, &["rev-parse", "HEAD"])
                .unwrap()
                .stdout,
            prior_head.stdout
        );
        assert_eq!(
            git.try_run(
                project.path(),
                &["ls-files", "--stage", "--", ".exomonad/workspace"]
            )
            .unwrap()
            .stdout,
            parent_index.stdout
        );
    }

    /// Source-revision identity; artifact reuse additionally validates the
    /// compiler's consumed dependency and import-resolution evidence.
    fn cache_key(include: &[PathBuf]) -> String {
        tidepool_toolchain::cache::source_roots_identity(b"source-revision-test", include).unwrap()
    }

    /// Two cooperating files edited together are one transaction, and what a
    /// LATER compile reads is the pair that was published. The proof is
    /// GHC's: the new `Project.Work` calls a name that exists only in the new
    /// `Project.Types`, and the run's frozen capture — still on the search
    /// path underneath — provides neither.
    #[test]
    fn a_reloaded_pair_is_what_a_later_compile_reads() {
        let (project, run, reload) = cooperating_pair();
        crate::actor_host::validate_workspace_program(&reload.frozen, run.path()).unwrap();
        let before = reload.layer.read_active().unwrap().unwrap();
        let include = reload.layer.include_paths(1);
        let key_before = cache_key(&include);

        write_types(project.path(), "evidenceAmount");
        write_work(project.path(), "evidenceAmount");
        let outcome =
            tidepool_handlers::SourceReloadService::reload(&reload, PrincipalId::SYSTEM, &[], None)
                .unwrap();
        let tidepool_bridge_effects::SrReloadOutcome::ReloadPublished(
            previous,
            published,
            changed,
            workspace,
        ) = outcome
        else {
            panic!("a consistent pair must publish: {outcome:?}");
        };
        assert_eq!(previous.identity, before.identity);
        assert_ne!(published.identity, before.identity);
        assert_eq!(published.generation, 2);
        assert_eq!(changed, vec!["Project.Types", "Project.Work"]);
        assert!(matches!(
            workspace,
            tidepool_bridge_effects::SrWorkspaceCommitOutcome::WorkspaceCommitted(_, ref drift)
                if drift.is_empty()
        ));
        let git = GitCli::new();
        let workspace = project.path().join(".exomonad/workspace");
        let commit = git
            .try_run(&workspace, &["log", "-1", "--format=%s"])
            .unwrap();
        assert_eq!(
            commit.stdout.trim(),
            "Update workspace: Project.Types, Project.Work"
        );
        let workspace_head = git.try_run(&workspace, &["rev-parse", "HEAD"]).unwrap();
        let staged_gitlink = git
            .try_run(
                project.path(),
                &["ls-files", "--stage", "--", ".exomonad/workspace"],
            )
            .unwrap();
        assert!(
            staged_gitlink.stdout.contains(workspace_head.stdout.trim()),
            "the project index must stage the newly committed workspace gitlink: {}",
            staged_gitlink.stdout
        );

        // Same include vector, different compiled-artifact key: a later
        // compile cannot be served the previous revision's artifact.
        assert_eq!(include, reload.layer.include_paths(1));
        assert_ne!(cache_key(&include), key_before);

        // And GHC agrees: this only compiles if BOTH new files were read.
        crate::actor_host::validate_workspace_program(&reload.frozen, run.path()).unwrap();
    }

    #[test]
    fn a_reload_can_supply_a_one_line_workspace_commit_intent() {
        let (project, _run, reload) = cooperating_pair();
        write_types(project.path(), "evidenceAmount");
        write_work(project.path(), "evidenceAmount");

        let outcome = tidepool_handlers::SourceReloadService::reload(
            &reload,
            PrincipalId::SYSTEM,
            &[],
            Some("Add evidence amount accessor"),
        )
        .unwrap();
        assert!(
            matches!(
                &outcome,
                tidepool_bridge_effects::SrReloadOutcome::ReloadPublished(..)
            ),
            "reload with a one-line workspace intent should publish: {outcome:?}"
        );
        let message = GitCli::new()
            .try_run(
                &project.path().join(".exomonad/workspace"),
                &["log", "-1", "--format=%s"],
            )
            .unwrap();
        assert_eq!(message.stdout.trim(), "Add evidence amount accessor");
    }

    #[test]
    fn a_multiline_workspace_commit_intent_is_reported_after_source_publication() {
        let (project, _run, reload) = cooperating_pair();
        write_types(project.path(), "evidenceAmount");
        write_work(project.path(), "evidenceAmount");
        let git = GitCli::new();
        let workspace = project.path().join(".exomonad/workspace");
        let head = git.try_run(&workspace, &["rev-parse", "HEAD"]).unwrap();
        let workspace_status = git
            .try_run(
                &workspace,
                &["status", "--porcelain=v1", "--untracked-files=all"],
            )
            .unwrap();
        let parent_index = git
            .try_run(
                project.path(),
                &["diff", "--cached", "--", ".exomonad/workspace"],
            )
            .unwrap();

        let Ok(tidepool_bridge_effects::SrReloadOutcome::ReloadPublished(
            _,
            _,
            _,
            tidepool_bridge_effects::SrWorkspaceCommitOutcome::WorkspaceCommitFailed(
                error,
                None,
                _,
            ),
        )) = tidepool_handlers::SourceReloadService::reload(
            &reload,
            PrincipalId::SYSTEM,
            &[],
            Some("first line\nsecond line"),
        )
        else {
            panic!("bad intent must be a typed workspace failure after publication");
        };
        assert!(error.contains("single line"));
        assert_eq!(
            git.try_run(&workspace, &["rev-parse", "HEAD"])
                .unwrap()
                .stdout,
            head.stdout
        );
        assert_eq!(
            git.try_run(
                &workspace,
                &["status", "--porcelain=v1", "--untracked-files=all"]
            )
            .unwrap()
            .stdout,
            workspace_status.stdout
        );
        assert_eq!(
            git.try_run(
                project.path(),
                &["diff", "--cached", "--", ".exomonad/workspace"]
            )
            .unwrap()
            .stdout,
            parent_index.stdout
        );
    }

    /// Changing one module rebuilds everything that imports it, and a break
    /// anywhere in that closure rejects the WHOLE reload: the previous graph
    /// stays active, the edited file stays on disk exactly as written, and the
    /// receipt names the snapshot that failed.
    #[test]
    fn a_reload_that_breaks_a_dependent_changes_nothing() {
        let (project, run, reload) = cooperating_pair();
        crate::actor_host::validate_workspace_program(&reload.frozen, run.path()).unwrap();
        let before = reload.layer.read_active().unwrap().unwrap();
        let key_before = cache_key(&reload.layer.include_paths(1));

        // Only Project.Types is edited. Project.Work still calls the old name.
        write_types(project.path(), "evidenceAmount");
        let git = GitCli::new();
        let workspace = project.path().join(".exomonad/workspace");
        let head_before = git.try_run(&workspace, &["rev-parse", "HEAD"]).unwrap();
        let workspace_status_before = git
            .try_run(
                &workspace,
                &["status", "--porcelain=v1", "--untracked-files=all"],
            )
            .unwrap();
        let project_status_before = git
            .try_run(project.path(), &["status", "--porcelain"])
            .unwrap();
        let expected = reload
            .layer
            .capture_from_workspace(&reload.frozen, project.path())
            .unwrap()
            .revision()
            .identity
            .clone();
        let outcome =
            tidepool_handlers::SourceReloadService::reload(&reload, PrincipalId::SYSTEM, &[], None)
                .unwrap();
        let tidepool_bridge_effects::SrReloadOutcome::ReloadRejected(active, rejected, diagnostics) =
            outcome
        else {
            panic!("a dependent that no longer compiles must reject: {outcome:?}");
        };

        // The dependent was rebuilt against the new module — that is the only
        // way this diagnostic exists, since Project.Work itself is unedited.
        assert!(diagnostics.contains("evidenceValue"), "{diagnostics}");
        assert_eq!(active.identity, before.identity);
        assert_eq!(rejected.identity, expected);
        assert_eq!(reload.layer.read_active().unwrap().unwrap(), before);
        assert_eq!(cache_key(&reload.layer.include_paths(1)), key_before);

        // The edited source is untouched, and the previous graph still
        // compiles, which is what "still active" means.
        assert!(std::fs::read_to_string(
            project.path().join(".exomonad/workspace/Project/Types.hs")
        )
        .unwrap()
        .contains("evidenceAmount"));
        assert_eq!(
            git.try_run(&workspace, &["rev-parse", "HEAD"])
                .unwrap()
                .stdout,
            head_before.stdout,
            "a refused reload must not create a workspace commit"
        );
        assert_eq!(
            git.try_run(
                &workspace,
                &["status", "--porcelain=v1", "--untracked-files=all"]
            )
            .unwrap()
            .stdout,
            workspace_status_before.stdout,
            "a refused reload must not stage workspace edits"
        );
        assert_eq!(
            git.try_run(project.path(), &["status", "--porcelain"])
                .unwrap()
                .stdout,
            project_status_before.stdout,
            "a refused reload must not stage the project gitlink"
        );
        crate::actor_host::validate_workspace_program(&reload.frozen, run.path()).unwrap();
    }

    /// Reloading a workspace nobody edited republishes nothing — the identity
    /// is content, so there is nothing to publish.
    #[test]
    fn an_unedited_workspace_reloads_to_the_same_revision() {
        let (project, run, reload) = cooperating_pair();
        let active = reload.layer.ensure_active(&reload.frozen).unwrap();
        let outcome =
            tidepool_handlers::SourceReloadService::reload(&reload, PrincipalId::SYSTEM, &[], None)
                .unwrap();
        let tidepool_bridge_effects::SrReloadOutcome::ReloadUnchanged(revision) = outcome else {
            panic!("an unedited workspace must not republish: {outcome:?}");
        };
        assert_eq!(revision.identity, active.identity);
        assert_eq!(revision.generation, 1);
        drop((project, run));
    }

    /// Provenance is data, not prose: status names what later cells compile
    /// against and what the roots hold right now, and a module is looked up in
    /// the revision rather than parsed out of a message.
    #[test]
    fn status_reports_the_active_and_the_on_disk_revision() {
        let (project, run, reload) = cooperating_pair();
        let status =
            tidepool_handlers::SourceReloadService::status(&reload, PrincipalId::SYSTEM).unwrap();
        assert_eq!(status.active.identity, status.disk.identity);
        let work = |revision: &tidepool_bridge_effects::SrRevision| {
            revision
                .modules
                .iter()
                .find(|module| module.name == "Project.Work")
                .expect("the configured module is in the revision")
                .digest
                .clone()
        };
        let before = work(&status.active);

        write_work(project.path(), "evidenceValue + 0 `seq` evidenceValue");
        let status =
            tidepool_handlers::SourceReloadService::status(&reload, PrincipalId::SYSTEM).unwrap();
        assert_ne!(status.active.identity, status.disk.identity);
        assert_eq!(work(&status.active), before);
        assert_ne!(work(&status.disk), before);
        assert_eq!(status.disk.generation, 0, "an unpublished snapshot");
        drop(run);
    }

    #[test]
    fn deleting_an_authored_module_refuses_stale_frozen_fallback() {
        let (project, _run, reload) = cooperating_pair();
        let active = reload.layer.ensure_active(&reload.frozen).unwrap();
        std::fs::remove_file(project.path().join(".exomonad/workspace/Project/Types.hs")).unwrap();
        let outcome =
            tidepool_handlers::SourceReloadService::reload(&reload, PrincipalId::SYSTEM, &[], None)
                .unwrap();
        let tidepool_bridge_effects::SrReloadOutcome::ReloadRejected(previous, _, diagnostics) =
            outcome
        else {
            panic!("module deletion must be rejected: {outcome:?}");
        };
        assert_eq!(previous.identity, active.identity);
        assert!(diagnostics.contains("Project.Types"), "{diagnostics}");
        assert_eq!(reload.layer.read_active().unwrap(), Some(active));
    }

    /// `drift` answers the same question `status` does, plus the module a
    /// caller would otherwise have to diff out by hand: unedited, it reports
    /// checked-and-identical (equal identities, no changed modules); edited,
    /// it names exactly the module that changed.
    #[test]
    fn drift_names_a_changed_module_and_reports_identical_when_unedited() {
        let (project, run, reload) = cooperating_pair();
        let unedited = reload.drift(PrincipalId::SYSTEM).unwrap();
        assert_eq!(unedited.active_identity, unedited.disk_identity);
        assert!(unedited.changed_modules.is_empty());

        write_work(project.path(), "evidenceValue + 0 `seq` evidenceValue");
        let edited = reload.drift(PrincipalId::SYSTEM).unwrap();
        assert_ne!(edited.active_identity, edited.disk_identity);
        assert_eq!(edited.changed_modules, vec!["Project.Work".to_string()]);
        drop(run);
    }

    /// `frozen_drift` compares the run's immutable floor to what the same
    /// roots hold on disk right now, independent of the layer `drift`
    /// reports on: editing a file changes this even though nothing has been
    /// reloaded or republished.
    #[test]
    fn frozen_drift_names_a_module_edited_on_disk_after_the_freeze() {
        let (project, run, reload) = cooperating_pair();
        let unedited = reload.frozen_drift().unwrap();
        assert!(unedited.changed_modules.is_empty());

        write_work(project.path(), "evidenceValue + 0 `seq` evidenceValue");
        let edited = reload.frozen_drift().unwrap();
        assert_eq!(edited.changed_modules, vec!["Project.Work".to_string()]);
        drop(run);
    }
}
