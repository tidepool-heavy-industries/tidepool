//! Immutable workspace and branch-local helper source graphs.
//!
//! An ordinary run freezes its authored Haskell roots under
//! `<run_root>/workspace/sources/<capture>/<index>` and verifies that capture
//! when the run is loaded ([`super::workspace::FrozenWorkspace::load`]). A
//! completed prepared workspace retains those roots in its deployment.
//!
//! Boot preparation publishes an `active` link to the initial revision:
//!
//! ```text
//! <run_root>/workspace/revisions/<identity>/{0,1,…,resources}
//! <run_root>/workspace/active -> revisions/<identity>
//! ```
//!
//! The initial revision may refer to a prepared deployment. Reload candidates
//! are complete run-owned revision directories. The generated
//! `Exomonad.Workspace` resource and immutable shared libraries have explicit
//! roots; an older workspace or cache cannot restore a removed module.
//!
//! Once a generation is issued, source reads, compilation, children and
//! checkpoints consume its opaque source capsule. Reload stages another exact
//! graph and installer for the actor's generation publication; it does not
//! move the boot links. Durable generation references restore the same graph
//! without consulting those links. Every capsule retains the configured run
//! owner until its last admitted capture releases.
//!
//! `exomonad check --recipes` compiles against the frozen capture. Native
//! installer preparation validates the complete staged source graph before
//! the actor publishes source, tools, prompts and notebook together.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::{fs::File, os::unix::fs::OpenOptionsExt};

use parking_lot::{Mutex, RwLock};
use tidepool_repr::PrincipalId;

use super::workspace::FrozenWorkspace;

mod generation;
mod prompt_files;
pub(crate) use prompt_files::CapturedPromptFiles;

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
/// compiler dependency selection. `generation` is reserved serialized metadata,
/// always zero for content-issued snapshots. Notebook publication ordinals belong
/// to `NotebookGenerationId`, not the reusable source graph.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SourceRevision {
    pub(crate) identity: String,
    pub(crate) generation: u64,
    /// Every module the revision provides, by module name, with the hex digest
    /// of its source. Ordered by module name.
    pub(crate) modules: Vec<(String, String)>,
}

impl SourceRevision {
    fn from_content(identity: String, modules: Vec<(String, String)>) -> Self {
        Self {
            identity,
            generation: 0,
            modules,
        }
    }

    /// Module names whose source differs from `previous`, including modules
    /// `previous` did not have.
    fn changed_since(&self, previous: &Self) -> Vec<String> {
        let before: BTreeMap<&str, &str> = previous
            .modules
            .iter()
            .map(|(name, digest)| (name.as_str(), digest.as_str()))
            .collect();
        let after = self
            .modules
            .iter()
            .map(|(name, digest)| (name.as_str(), digest.as_str()))
            .collect::<BTreeMap<_, _>>();
        before
            .keys()
            .chain(after.keys())
            .filter(|module| before.get(*module) != after.get(*module))
            .map(|module| (*module).to_owned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}

/// Temporary source bytes are removed unless explicitly retained.
struct CapturedRevision {
    directory: tempfile::TempDir,
    revision: SourceRevision,
}

/// A captured immutable revision available for boot publication or staging.
pub(crate) struct PendingRevision {
    directory: PathBuf,
    source_roots: Vec<PathBuf>,
    revision: SourceRevision,
}

impl PendingRevision {
    pub(crate) fn revision(&self) -> &SourceRevision {
        &self.revision
    }

    /// The exact include roots for this captured candidate, in source order.
    pub(crate) fn include_paths(&self, roots: usize) -> Vec<PathBuf> {
        revision_include_paths(&self.directory, roots)
    }
}

/// One source revision store with an initial boot selection.
/// The run and each helper branch have separate stores. Issued generations
/// retain exact revisions independently of this store's boot link.
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

    /// Materialize the initial revision from the run's own frozen capture, unless a
    /// revision is already active. Idempotent, and the only way `active` comes
    /// into existence: every compile in the run needs it to resolve.
    /// Reopen the source published before durable host launch admission.
    /// Missing publication cannot authorize a new capture during host recovery.
    pub(crate) fn reopen_active(&self) -> Result<SourceRevision> {
        self.read_active()?
            .ok_or_else(|| "admitted run has no accepted source publication".into())
    }

    pub(crate) fn ensure_active(&self, frozen: &FrozenWorkspace) -> Result<SourceRevision> {
        if let Some(revision) = frozen.prepared_source_revision()? {
            if self.read_active()?.is_none() {
                let initial = frozen
                    .prepared_initial_source()
                    .ok_or("prepared run has no initial source selection")?;
                inspect_retained_revision(
                    frozen.identity(),
                    &initial.run_revision,
                    &revision,
                    frozen.captured_source_roots().len(),
                )?;
            }
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
    /// own: the initial revision is captured from `roots` as they stand.
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
        let recovered = SourceRevision::from_content(identity, revision_modules(&revision, roots)?);
        let record = ActiveRecord {
            identity: recovered.identity,
            generation: recovered.generation,
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
        Ok(Some(SourceRevision::from_content(
            record.identity,
            revision_modules(&directory, record.roots)?,
        )))
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

    /// Repair a derived source link from a retained captured revision.
    pub(crate) fn publish(&self, pending: PendingRevision) -> Result<SourceRevision> {
        self.project_revision(&pending)?;
        Ok(pending.revision)
    }

    fn project_revision(&self, pending: &PendingRevision) -> Result<()> {
        sync_revision_tree(&pending.directory)?;
        let staged = self
            .directory
            .join(format!(".active-{}", uuid::Uuid::new_v4()));
        std::os::unix::fs::symlink(
            Path::new("revisions").join(&pending.revision.identity),
            &staged,
        )?;
        if let Err(error) = std::fs::rename(&staged, self.active_link()) {
            std::fs::remove_file(staged).ok();
            return Err(error.into());
        }
        tidepool_atomic_write::sync_parent_directory(&self.active_link())?;
        tidepool_atomic_write::write_durable(
            &self.active_record(),
            &serde_json::to_vec(&ActiveRecord {
                identity: pending.revision.identity.clone(),
                generation: pending.revision.generation,
                roots: revision_root_count(&pending.directory)?,
            })?,
        )?;
        Ok(())
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

#[cfg(test)]
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
    if revision_root_count(directory)? != roots {
        return Err("immutable source revision differs from its selected root count".into());
    }
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

/// Validate both owner-selected immutable originals without initializing a run.
pub(crate) fn validate_prepared_initial_source(frozen: &FrozenWorkspace) -> Result<()> {
    let initial = frozen
        .prepared_initial_source()
        .ok_or("completed preparation has no initial source selection")?;
    let run = frozen
        .prepared_source_revision()?
        .ok_or("completed preparation has no original run resource")?;
    let helper = frozen
        .prepared_helper_revision()?
        .ok_or("completed preparation has no original helper resource")?;
    inspect_retained_revision(
        frozen.identity(),
        &initial.run_revision,
        &run,
        frozen.captured_source_roots().len(),
    )?;
    inspect_retained_revision(frozen.identity(), &initial.helper_revision, &helper, 1)?;
    Ok(())
}

/// Which layer one actor's own source calls act on.
///
/// Selected when the actor is constructed and never afterwards, so an actor
/// cannot reach another actor's source by asking differently. This is the
/// whole authority story for `Source`: there is no role test anywhere below
/// this point, because by then the layer is already decided.
#[derive(Clone)]
enum ActorSourceScope {
    /// The run's editable source. Only its owning actor may stage reloads.
    Run,
    /// The run's source without authority to stage reloads.
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

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimePreparationIdentity {
    original: uuid::Uuid,
}

/// A run keeps its installer issuer across host incarnations. The recipe still
/// selects source and effect inputs; this identity selects the exact completed
/// original, including contextual certificates that recompilation can change.
fn runtime_preparation_identity(
    workspace: &tidepool_atomic_write::DirectoryAnchor,
    entries: &tidepool_atomic_write::DirectoryAnchor,
) -> Result<uuid::Uuid> {
    let path = workspace.resolve("runtime-preparation.json")?;
    let read = || -> std::io::Result<File> {
        let file = File::from(rustix::fs::open(
            &path,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK,
            rustix::fs::Mode::empty(),
        )?);
        if !file.metadata()?.is_file() {
            return Err(std::io::Error::other(
                "runtime preparation identity is not a regular file",
            ));
        }
        Ok(file)
    };
    let file = match read() {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if std::fs::read_dir(entries.path())?
                .next()
                .transpose()?
                .is_some()
            {
                return Err("runtime entries have no retained preparation identity; start a new run rather than selecting an arbitrary original".into());
            }
            let identity = RuntimePreparationIdentity {
                original: uuid::Uuid::new_v4(),
            };
            tidepool_atomic_write::write_durable_new(&path, &serde_json::to_vec(&identity)?)?;
            read()?
        }
        Err(error) => return Err(error.into()),
    };
    let identity: RuntimePreparationIdentity = serde_json::from_reader(file)?;
    // A previous exclusive publication may have failed after making the file visible.
    tidepool_atomic_write::sync_parent_directory(&path)?;
    Ok(identity.original)
}

struct RetainedSourceGraph {
    _root_owner: Arc<SourceRootOwner>,
    _prepared_owner: Option<Arc<tidepool_atomic_write::DirectoryAnchor>>,
    identities: Vec<String>,
    include_paths: Vec<PathBuf>,
    manifests: Vec<tidepool_toolchain::cache::SourceRootManifest>,
    entries: exomonad_actor::SourceEntryStorage,
    catalog: tidepool_toolchain::toolchain::CatalogSelection,
    evidence: exomonad_actor::SourceGenerationEvidence,
}

impl exomonad_actor::RetainedSourceLayer for RetainedSourceGraph {
    fn identities(&self) -> &[String] {
        &self.identities
    }

    fn include_paths(&self) -> &[PathBuf] {
        &self.include_paths
    }

    fn catalog_selection(&self) -> tidepool_toolchain::toolchain::CatalogSelection {
        self.catalog.clone()
    }

    fn source_manifests(&self) -> Option<&[tidepool_toolchain::cache::SourceRootManifest]> {
        Some(&self.manifests)
    }

    fn prepared_entries(&self) -> Option<&exomonad_actor::SourceEntryStorage> {
        Some(&self.entries)
    }

    fn generation_evidence(&self) -> Option<&exomonad_actor::SourceGenerationEvidence> {
        Some(&self.evidence)
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
    /// forked actors. Each branch has its own draft and boot revision.
    helpers: Mutex<HashMap<String, SourceLayer>>,
    /// What each actor's own source calls reach.
    scopes: RwLock<HashMap<PrincipalId, ActorSourceScope>>,
    helper_scopes: RwLock<HashMap<PrincipalId, String>>,
    /// Serialize boot selection and capture of the run's source stores.
    /// The actor's generation owner fences publication separately.
    gate: Mutex<()>,
    prompt_composers: RwLock<HashMap<PrincipalId, prompt_files::PromptComposer>>,
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
        if matches!(&owner, SourceRootOwner::Host(_)) {
            SourceLayer::new(&run_root).reopen_active()?;
        }
        let run_root = std::fs::canonicalize(run_root)?;
        let workspace_directory =
            tidepool_atomic_write::DirectoryAnchor::open_existing(&run_root)?.child("workspace")?;
        let entries = workspace_directory.child("entries")?;
        let preparation = match (&owner, &frozen.preparation) {
            (
                SourceRootOwner::Prepared(_),
                Some(
                    super::workspace::WorkspacePreparation::Preparing { original }
                    | super::workspace::WorkspacePreparation::Completed { original, .. },
                ),
            ) => *original,
            _ => runtime_preparation_identity(&workspace_directory, &entries)?,
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
        let helper_root = match &owner {
            SourceRootOwner::Prepared(_) => run_root.join("workspace/helpers"),
            _ => run_root.join("helpers"),
        };
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
            prompt_composers: RwLock::new(HashMap::new()),
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
        let initial_source = self
            .frozen
            .prepared_initial_source()
            .ok_or("prepared run has no initial source selection")?;
        inspect_retained_revision(
            self.frozen.identity(),
            &initial_source.run_revision,
            &directory,
            self.frozen.captured_source_roots().len(),
        )
        .map(Some)
    }

    /// The deployment's promised recipes belong to this original immutable
    /// graph, even when a recovered run has accepted a newer source revision.
    pub(crate) fn prepared_toolset_layer(&self) -> Result<exomonad_actor::CheckpointSourceLayer> {
        let revision = self
            .prepared_run_revision()?
            .ok_or("source owner has no completed deployment revision")?;
        let helper = self
            .prepared_helper_revision()?
            .ok_or("source owner has no completed deployment helper revision")?;
        let (_, prompts) = self.capture_prompts(true)?;
        self.compose_graph("run", helper, revision, self.entry_storage.clone(), prompts)
    }

    fn prepared_helper_revision(&self) -> Result<Option<RetainedRevision>> {
        let Some(directory) = self.frozen.prepared_helper_revision()? else {
            return Ok(None);
        };
        let initial_source = self
            .frozen
            .prepared_initial_source()
            .ok_or("prepared helper has no initial source selection")?;
        inspect_retained_revision(
            self.frozen.identity(),
            &initial_source.helper_revision,
            &directory,
            1,
        )
        .map(Some)
    }

    pub(crate) fn prepared_initial_source(
        &self,
    ) -> Result<super::workspace::PreparedInitialSource> {
        Ok(super::workspace::PreparedInitialSource {
            run_revision: self
                .layer
                .checkpoint_revision(self.frozen.identity())?
                .identity,
            helper_revision: self
                .helper_layer("run")
                .checkpoint_revision(self.frozen.identity())?
                .identity,
        })
    }

    fn compose_graph(
        &self,
        helper_branch: &str,
        helper: RetainedRevision,
        run: RetainedRevision,
        entries: exomonad_actor::SourceEntryStorage,
        prompts: generation::PromptReference,
    ) -> Result<exomonad_actor::CheckpointSourceLayer> {
        let mut identities = vec![
            format!("helpers:{}", helper.identity),
            format!("run:{}", run.identity),
        ];
        let mut include_paths = helper.paths;
        include_paths.extend(run.paths);
        let mut manifests = helper.manifests.iter().cloned().collect::<Vec<_>>();
        manifests.extend(run.manifests.iter().cloned());
        self.append_workspace_resources(&mut identities, &mut include_paths, &mut manifests)?;
        self.append_shared_roots(&mut include_paths, &mut manifests)?;
        self.issue_graph(
            identities,
            include_paths,
            manifests,
            entries,
            prompts,
            helper_branch,
        )
    }

    fn pinned_checkpoint_graph(
        &self,
        helper_branch: &str,
    ) -> Result<exomonad_actor::CheckpointSourceLayer> {
        match self.source_owner.as_ref() {
            SourceRootOwner::Host(_) => {
                self.layer.reopen_active()?;
            }
            _ => {
                self.layer.ensure_active(&self.frozen)?;
            }
        }
        let helper = self
            .helper_layer(helper_branch)
            .checkpoint_revision(self.frozen.identity())?;
        let run = self.layer.checkpoint_revision(self.frozen.identity())?;
        let (_, prompts) = self.capture_prompts(true)?;
        self.compose_graph(
            helper_branch,
            helper,
            run,
            self.entry_storage.clone(),
            prompts,
        )
    }

    fn helper_draft(&self, branch: &str) -> PathBuf {
        SourceLayer::helper_draft(&self.helper_root, branch)
    }

    pub(crate) fn helper_branch_for(&self, actor: PrincipalId) -> Option<String> {
        self.helper_scopes.read().get(&actor).cloned()
    }

    fn ensure_helper_active(&self, branch: &str) -> Result<SourceRevision> {
        let seed = self.helper_root.join("empty");
        std::fs::create_dir_all(&seed)?;
        let draft = self.helper_draft(branch);
        if !matches!(self.source_owner.as_ref(), SourceRootOwner::Prepared(_)) {
            if branch == "run" {
                crate::actor_host::initialize_helper_draft(&self.workspace, &draft)?;
            }
            std::fs::create_dir_all(&draft)?;
        }
        let layer = self.helper_layer(branch);
        if let Some(active) = layer.read_active()? {
            return Ok(active);
        }
        if let Some(prepared) = self.frozen.prepared_helper_revision()? {
            self.prepared_helper_revision()?;
            return layer.ensure_active_from_prepared(self.frozen.identity(), &prepared);
        }
        self.helper_layer(branch)
            .ensure_active_from(self.frozen.identity(), &[seed])
    }

    fn wire(revision: &SourceRevision) -> tidepool_bridge_effects::SrRevision {
        tidepool_bridge_effects::SrRevision {
            identity: revision.identity.clone(),
            generation: i64::try_from(revision.generation).unwrap_or(i64::MAX),
            modules: revision
                .modules
                .iter()
                .map(|(name, digest)| tidepool_bridge_effects::SrModule {
                    name: name.clone(),
                    digest: digest.clone(),
                })
                .collect(),
        }
    }

    /// Frozen workspace modules whose digest differs from the same module
    /// read live off disk right now — row 3 of the what-is-live status view.
    /// The frozen capture ([`super::workspace::FrozenWorkspace`]) is the
    /// run's immutable floor; this is independent of any layer republished
    /// in front of it (through the actor generation), so a clean answer here does not
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

impl exomonad_actor::SourceInspection for ExomonadSourceReload {
    fn read_from(
        &self,
        source: &exomonad_actor::CheckpointSourceLayer,
        module: &str,
    ) -> std::result::Result<(String, String), exomonad_actor::SourceInspectionError> {
        self.read_issued(source, module)
    }
    fn locate_module(
        &self,
        source: &exomonad_actor::CheckpointSourceLayer,
        module: &str,
    ) -> std::result::Result<Option<String>, exomonad_actor::SourceInspectionError> {
        self.locate_issued(source, module)
    }
    fn revision(
        &self,
        source: &exomonad_actor::CheckpointSourceLayer,
    ) -> std::result::Result<
        tidepool_bridge_effects::SrRevision,
        exomonad_actor::SourceInspectionError,
    > {
        self.revision_issued(source)
    }
    fn observe(
        &self,
        source: &exomonad_actor::CheckpointSourceLayer,
    ) -> std::result::Result<tidepool_bridge_effects::SrStatus, exomonad_actor::SourceInspectionError>
    {
        self.observe_issued(source)
    }
}

impl exomonad_actor::ActorSourceLayers for ExomonadSourceReload {
    fn inspection_services(self: std::sync::Arc<Self>) -> exomonad_actor::SourceInspectionServices {
        exomonad_actor::SourceInspectionServices::new(self)
    }
    fn toolset_layer_from(
        &self,
        source: &exomonad_actor::CheckpointSourceLayer,
    ) -> std::result::Result<exomonad_actor::CheckpointSourceLayer, String> {
        self.validate_source_authority(source)?;
        Ok(source.clone())
    }

    fn fresh_toolset_layer_from(
        &self,
        source: &exomonad_actor::CheckpointSourceLayer,
    ) -> std::result::Result<exomonad_actor::CheckpointSourceLayer, String> {
        self.validate_source_authority(source)?;
        let reference = source
            .generation_evidence()
            .ok_or("source graph has no generation evidence")?
            .reference
            .clone();
        self.restore_graph_with_entries(&reference, self.fresh_entry_storage.clone())
            .map_err(|error| error.to_string())
    }

    fn freeze_toolset_layer(
        &self,
        actor: PrincipalId,
    ) -> std::result::Result<exomonad_actor::CheckpointSourceLayer, String> {
        let _one_at_a_time = self.gate.lock();
        if let ActorSourceScope::Checkpoint(layer) = self.scope(actor) {
            return self.toolset_layer_from(&layer);
        }
        let helper = self
            .helper_branch_for(actor)
            .unwrap_or_else(|| "run".to_owned());
        self.ensure_helper_active(&helper)
            .map_err(|error| error.to_string())?;
        self.pinned_checkpoint_graph(&helper)
            .map_err(|error| error.to_string())
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
    ) -> std::result::Result<Vec<PathBuf>, String> {
        let _one_at_a_time = self.gate.lock();
        self.validate_source_authority(checkpoint)?;
        let _ = helper_branch;
        if let ActorSourceScope::Checkpoint(selected) = self.scope(creator) {
            if !selected.same_revision(checkpoint) {
                return Err(
                    "checkpoint source differs from the creator's issued generation".into(),
                );
            }
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

    fn retain_helpers(&self, creator: PrincipalId) -> std::result::Result<String, String> {
        self.helper_branch_for(creator)
            .ok_or_else(|| "creator has no selected session helper source".into())
    }

    fn layer_include_for(&self, branch: &str) -> std::result::Result<Vec<PathBuf>, String> {
        self.ensure_helper_active(branch)
            .map_err(|error| format!("session helper layer could not be initialized: {error}"))?;
        self.helper_layer(branch)
            .active_include_paths()
            .map_err(|error| format!("session helper layer has no include roots: {error}"))
    }

    fn bind_for(&self, actor: PrincipalId, helper_branch: &str) {
        self.helper_scopes
            .write()
            .insert(actor, helper_branch.to_owned());
        self.scopes
            .write()
            .insert(actor, ActorSourceScope::RunReadOnly);
    }

    fn stage_reload(
        self: Arc<Self>,
        actor: PrincipalId,
        expected: &exomonad_actor::CheckpointSourceLayer,
        also_check: &[String],
    ) -> std::result::Result<
        Box<dyn exomonad_actor::StagedActorSourceReload>,
        exomonad_actor::SourceLayerReload,
    > {
        self.stage_generation(actor, expected, also_check)
    }

    fn restore_source(
        &self,
        reference: &tidepool_runtime::session::GenerationArtifactReference,
    ) -> std::result::Result<exomonad_actor::CheckpointSourceLayer, String> {
        self.restore_graph(reference)
            .map_err(|error| error.to_string())
    }

    fn restore_source_with_acquisition(
        &self,
        reference: &tidepool_runtime::session::GenerationArtifactReference,
        acquisition: &exomonad_actor::ToolsetAcquisition,
    ) -> std::result::Result<
        exomonad_actor::CheckpointSourceLayer,
        exomonad_actor::SourceInspectionError,
    > {
        self.restore_graph_with_acquisition(reference, acquisition)
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
    SourceRevision::from_content(
        tidepool_toolchain::cache::source_manifests_identity(&domain, roots),
        modules.into_iter().collect(),
    )
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
         import qualified Data.Text as Text\n\
         \n\
         compiledSourceRevision :: Text\n\
         compiledSourceRevision = Text.pack \"{}\"\n",
        tidepool_runtime::session::escape_workbench_haskell_string(identity)
    )
}

#[cfg(test)]
mod tests {
    use exomonad_actor::{HasSourceInspection, SourceInspection};

    use super::*;

    fn runtime_reload_for_run(
        project: &Path,
        run: Arc<tempfile::TempDir>,
    ) -> Result<ExomonadSourceReload> {
        let frozen = FrozenWorkspace::load(project, run.path())?;
        ExomonadSourceReload::new_owned(
            frozen,
            project.to_path_buf(),
            run.path().to_path_buf(),
            crate::haskell_sources::ensure_exomonad_haskell()?,
            SourceRootOwner::Temporary(run),
        )
    }

    fn fresh_preparation(reload: &ExomonadSourceReload) -> uuid::Uuid {
        match &reload.fresh_entry_storage {
            exomonad_actor::SourceEntryStorage::FreshCompilation { preparation, .. } => {
                *preparation
            }
            _ => panic!("fresh runtime storage must retain its issuing preparation"),
        }
    }

    #[test]
    fn runtime_preparation_survives_host_replacement_after_entry_allocation() {
        let (project, run) = workspace_with("module Project.Work where\nwork = 1\n");
        let construct = || {
            let frozen = FrozenWorkspace::load(project.path(), run.path()).unwrap();
            let root = tidepool_atomic_write::DirectoryAnchor::open_existing(run.path()).unwrap();
            let lease = Arc::new(crate::actor_host::HostIncarnationLease::claim(&root).unwrap());
            ExomonadSourceReload::new_owned(
                frozen,
                project.path().to_path_buf(),
                run.path().to_path_buf(),
                crate::haskell_sources::ensure_exomonad_haskell().unwrap(),
                SourceRootOwner::Host(lease),
            )
            .unwrap()
        };
        let first = construct();
        let issued = fresh_preparation(&first);
        let original = first
            .fresh_entry_storage
            .directory()
            .join("a".repeat(64))
            .join(issued.to_string());
        std::fs::create_dir_all(&original).unwrap();
        std::fs::write(original.join("Expr.hs"), "module Expr where\nvalue = ()\n").unwrap();
        let identity = run.path().join("workspace/runtime-preparation.json");
        let first_bytes = std::fs::read(&identity).ok();
        drop(first);

        let recovered = construct();
        assert_eq!(fresh_preparation(&recovered), issued);
        assert_eq!(Some(std::fs::read(&identity).unwrap()), first_bytes);
        assert!(original.join("Expr.hs").is_file());
    }

    #[test]
    fn runtime_preparation_is_stable_within_a_run_and_distinct_between_runs() {
        let (project, first_run) = workspace_with("module Project.Work where\nwork = 1\n");
        let first_run = Arc::new(first_run);
        let other_run = Arc::new(tempfile::tempdir().unwrap());
        let first = runtime_reload_for_run(project.path(), Arc::clone(&first_run)).unwrap();
        let same = runtime_reload_for_run(project.path(), first_run).unwrap();
        let other = runtime_reload_for_run(project.path(), other_run).unwrap();
        assert_eq!(fresh_preparation(&first), fresh_preparation(&same));
        assert_ne!(fresh_preparation(&first), fresh_preparation(&other));
    }

    #[test]
    fn runtime_preparation_refuses_malformed_and_unknown_identity_fields() {
        for bytes in [
            b"{".to_vec(),
            br#"{"original":"not-a-uuid"}"#.to_vec(),
            serde_json::to_vec(&serde_json::json!({
                "original": uuid::Uuid::new_v4(),
                "unissued": true,
            }))
            .unwrap(),
        ] {
            let (project, run) = workspace_with("module Project.Work where\nwork = 1\n");
            let run = Arc::new(run);
            FrozenWorkspace::load(project.path(), run.path()).unwrap();
            let identity = run.path().join("workspace/runtime-preparation.json");
            std::fs::write(&identity, &bytes).unwrap();
            assert!(runtime_reload_for_run(project.path(), Arc::clone(&run)).is_err());
            assert_eq!(std::fs::read(&identity).unwrap(), bytes);
        }
    }

    #[cfg(unix)]
    #[test]
    fn runtime_preparation_refuses_alias_and_directory_identity() {
        for alias in [true, false] {
            let (project, run) = workspace_with("module Project.Work where\nwork = 1\n");
            let run = Arc::new(run);
            FrozenWorkspace::load(project.path(), run.path()).unwrap();
            let identity = run.path().join("workspace/runtime-preparation.json");
            let target = run.path().join("foreign-preparation.json");
            let bytes = serde_json::to_vec(&serde_json::json!({
                "original": uuid::Uuid::new_v4(),
            }))
            .unwrap();
            std::fs::write(&target, &bytes).unwrap();
            if alias {
                std::os::unix::fs::symlink(&target, &identity).unwrap();
            } else {
                std::fs::create_dir(&identity).unwrap();
            }
            assert!(runtime_reload_for_run(project.path(), Arc::clone(&run)).is_err());
            assert_eq!(std::fs::read(&target).unwrap(), bytes);
            let metadata = std::fs::symlink_metadata(&identity).unwrap();
            assert_eq!(metadata.file_type().is_symlink(), alias);
            assert_eq!(metadata.is_dir(), !alias);
        }
    }

    #[test]
    fn runtime_preparation_refuses_legacy_entries_without_issuing_identity() {
        let (project, run) = workspace_with("module Project.Work where\nwork = 1\n");
        let run = Arc::new(run);
        FrozenWorkspace::load(project.path(), run.path()).unwrap();
        let retained = run.path().join("workspace/entries/legacy-original");
        std::fs::create_dir_all(&retained).unwrap();
        std::fs::write(retained.join("retained"), "original evidence").unwrap();
        for _ in 0..2 {
            assert!(runtime_reload_for_run(project.path(), Arc::clone(&run)).is_err());
            assert!(!run
                .path()
                .join("workspace/runtime-preparation.json")
                .exists());
            assert_eq!(
                std::fs::read_to_string(retained.join("retained")).unwrap(),
                "original evidence"
            );
        }
    }

    #[test]
    fn prepared_source_owner_preserves_declared_preparation_without_runtime_identity() {
        use super::super::workspace::WorkspacePreparation;
        for completed in [false, true] {
            let (project, run) = workspace_with("module Project.Work where\nwork = 1\n");
            let mut frozen = FrozenWorkspace::load(project.path(), run.path()).unwrap();
            let original = uuid::Uuid::new_v4();
            let anchor = Arc::new(
                tidepool_atomic_write::DirectoryAnchor::open_existing(run.path()).unwrap(),
            );
            frozen.preparation = Some(if completed {
                frozen.prepared_deployment = Some(Arc::clone(&anchor));
                WorkspacePreparation::Completed {
                    original,
                    initial_source: super::super::workspace::PreparedInitialSource {
                        run_revision: "a".repeat(64),
                        helper_revision: "b".repeat(64),
                    },
                    // This is source selection metadata, not a compiled entry
                    // or a claim that the deployment is ready to execute.
                    coverage: vec![super::super::workspace::PreparedToolsetCoverage {
                        requested_effects: exomonad_actor::ActorCapabilities::default()
                            .effect_keys()
                            .to_vec(),
                        effective_effects: Vec::new(),
                        program: exomonad_actor::ToolsetProgramSelection::WorkspaceOriginal {
                            recipe: "a".repeat(64),
                            original,
                        },
                    }],
                }
            } else {
                WorkspacePreparation::Preparing { original }
            });
            let reload = ExomonadSourceReload::new_owned(
                frozen,
                project.path().to_path_buf(),
                run.path().to_path_buf(),
                crate::haskell_sources::ensure_exomonad_haskell().unwrap(),
                SourceRootOwner::Prepared(anchor),
            )
            .unwrap();
            assert_eq!(fresh_preparation(&reload), original);
            assert!(!run
                .path()
                .join("workspace/runtime-preparation.json")
                .exists());
            assert_eq!(
                matches!(
                    reload.entry_storage,
                    exomonad_actor::SourceEntryStorage::CompletedOriginal { .. }
                ),
                completed,
            );
        }
    }

    #[test]
    fn reopening_accepted_source_never_recaptures_changed_draft_or_missing_publication() {
        let (project, run) = workspace_with("module Project.Work where\nwork = 1\n");
        let frozen = FrozenWorkspace::load(project.path(), run.path()).unwrap();
        let layer = SourceLayer::new(run.path());
        assert!(layer.reopen_active().is_err());
        let accepted = layer.ensure_active(&frozen).unwrap();
        std::fs::write(
            project.path().join(".exomonad/Project/Work.hs"),
            "invalid changed draft",
        )
        .unwrap();
        assert_eq!(layer.reopen_active().unwrap().identity, accepted.identity);
        std::fs::remove_file(layer.active_link()).unwrap();
        assert!(layer.reopen_active().is_err());
        assert!(!layer.active_link().exists());
    }

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
    fn checkpoint_paths_pin_revision_and_admission_preserves_issued_source() {
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
        assert!(captured.include_paths().contains(&workspace_resource));
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
                || reload.shared_roots().unwrap().contains(path)
        }));
        let manifests = captured.source_manifests().unwrap();
        assert_eq!(manifests.len(), captured.include_paths().len());
        assert!(same_source_manifest(
            &manifests[captured
                .include_paths()
                .iter()
                .position(|path| path == &workspace_resource)
                .unwrap()],
            &source_root_manifest(&workspace_resource).unwrap()
        ));
        assert_eq!(
            reload
                .admit_checkpoint_layer(&captured, PrincipalId::SYSTEM, "run")
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
        assert_eq!(
            reload
                .admit_checkpoint_layer(&captured, PrincipalId::SYSTEM, "run")
                .unwrap(),
            captured.include_paths()
        );
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
            .admit_checkpoint_layer(&foreign, PrincipalId::SYSTEM, "run")
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
        let original_selection = uuid::Uuid::new_v4();
        let helper_seed = tempfile::tempdir().unwrap();
        let helper_layer =
            SourceLayer::helpers(&deployment.path().join("workspace/helpers"), "run");
        let original_helper = helper_layer
            .ensure_active_from(frozen.identity(), &[helper_seed.path().to_owned()])
            .unwrap();
        let retained_helper = helper_layer.checkpoint_revision(frozen.identity()).unwrap();
        let mut deployed = frozen.clone();
        deployed.preparation = Some(super::super::workspace::WorkspacePreparation::Completed {
            original: original_selection,
            initial_source: super::super::workspace::PreparedInitialSource {
                run_revision: original.identity.clone(),
                helper_revision: original_helper.identity.clone(),
            },
            coverage: vec![super::super::workspace::PreparedToolsetCoverage {
                requested_effects: exomonad_actor::ActorCapabilities::default()
                    .effect_keys()
                    .to_vec(),
                effective_effects: Vec::new(),
                program: exomonad_actor::ToolsetProgramSelection::WorkspaceOriginal {
                    recipe: "a".repeat(64),
                    original: original_selection,
                },
            }],
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
        let exomonad_actor::ToolsetProgramRecipe::WorkspaceOriginal(original_key) =
            &original_recipe.program
        else {
            panic!("unmatched row uses workspace original")
        };
        let exomonad_actor::ToolsetProgramSelection::WorkspaceOriginal { recipe, .. } =
            &mut coverage[0].program
        else {
            panic!("workspace original fixture")
        };
        *recipe = original_key.clone();
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
            preflight.include_paths()
                [retained_helper.paths.len()..retained_helper.paths.len() + retained.paths.len()],
            retained.paths
        );
        assert_eq!(
            current.include_paths()
                [retained_helper.paths.len()..retained_helper.paths.len() + live.paths.len()],
            live.paths
        );
        for (expected, actual) in retained
            .manifests
            .iter()
            .zip(&preflight.source_manifests().unwrap()[retained_helper.paths.len()..])
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
        assert_ne!(current_recipe.program, original_recipe.program);
        let exomonad_actor::ToolsetProgramRecipe::WorkspaceOriginal(current_key) =
            &current_recipe.program
        else {
            panic!("unmatched row uses workspace original")
        };
        assert!(reload
            .frozen
            .completed_entry_selections()
            .unwrap()
            .contains_key(original_key));
        assert!(!reload
            .frozen
            .completed_entry_selections()
            .unwrap()
            .contains_key(current_key));
        let before = tidepool_extract_cmd::extract_spawn_count();
        let mut compiler_owner = exomonad_actor::CompilerPreparationOwner::new();
        let lifetime = compiler_owner
            .scope_with_client(|client| async {
                let workbench = workbench.bind_preparation(client);
                let mut waiter = exomonad_actor::CompilerPreparationOwner::new();
                waiter
                    .scope(workbench.prepare_source_toolset(
                        tidepool_toolchain::artifacts::CompileWorkload::Foreground,
                        current.clone(),
                        &requested,
                        &[],
                        Arc::new(tidepool_runtime::session::ImageRegistry::new()),
                    ))
                    .await
            })
            .await;
        assert!(lifetime.cleanup.observation().is_confirmed());
        let refusal = lifetime.action;
        assert!(matches!(
            refusal.action,
            Err(exomonad_actor::ResidentActorWorkbenchError::PreparedEntryAbsent { .. })
        ));
        let cleanup = refusal.cleanup.observation();
        assert!(cleanup.is_confirmed());
        assert!(matches!(
            cleanup.work.as_slice(),
            [exomonad_actor::CompilerWorkClose::Settled(
                tidepool_runtime::CompilerTransactionClose::NotStarted
            )]
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

        let cold = ExomonadSourceReload::new_owned(
            reload.frozen.clone(),
            project.path().to_path_buf(),
            run.path().to_path_buf(),
            crate::haskell_sources::ensure_exomonad_haskell().unwrap(),
            SourceRootOwner::Temporary(Arc::clone(&run)),
        )
        .unwrap();
        let source_reference = &current.generation_evidence().unwrap().reference;
        assert_ne!(
            cold.entry_storage.directory(),
            cold.fresh_entry_storage.directory()
        );
        assert_eq!(
            current.prepared_entries().unwrap().directory(),
            cold.entry_storage.directory()
        );
        let run_original = fresh_preparation(&cold);
        let acquisitions = [
            (
                exomonad_actor::ToolsetAcquisition::FreshRunOriginal {
                    recipe: current_key.clone(),
                    original: run_original,
                },
                cold.fresh_entry_storage.directory(),
            ),
            (
                exomonad_actor::ToolsetAcquisition::ExistingRunOriginal {
                    recipe: current_key.clone(),
                    original: run_original,
                },
                cold.fresh_entry_storage.directory(),
            ),
            (
                exomonad_actor::ToolsetAcquisition::DeploymentOriginal {
                    recipe: original_key.clone(),
                    original: original_selection,
                },
                cold.entry_storage.directory(),
            ),
        ];
        for (acquisition, expected_directory) in acquisitions {
            let restored = cold
                .restore_graph_with_acquisition(source_reference, &acquisition)
                .unwrap();
            assert_eq!(restored.identities(), current.identities());
            assert_eq!(restored.include_paths(), current.include_paths());
            assert_eq!(
                &restored.generation_evidence().unwrap().reference,
                source_reference
            );
            let Some(exomonad_actor::SourceEntryStorage::CompletedOriginal {
                directory,
                selections,
            }) = restored.prepared_entries()
            else {
                panic!("retained acquisition must restore completed storage");
            };
            assert_eq!(directory, expected_directory);
            let (recipe, original) = acquisition.completed_entry_selection().unwrap();
            assert_eq!(selections, &BTreeMap::from([(recipe.to_owned(), original)]));
        }
        assert!(matches!(
            cold.restore_graph_with_acquisition(
                source_reference,
                &exomonad_actor::ToolsetAcquisition::UnretainedCompilation {
                    recipe: current_key.clone()
                }
            ),
            Err(exomonad_actor::SourceInspectionError::Unreadable(_))
        ));
        assert!(matches!(
            cold.restore_graph_with_acquisition(
                source_reference,
                &exomonad_actor::ToolsetAcquisition::FreshRunOriginal {
                    recipe: "invalid".into(),
                    original: run_original
                }
            ),
            Err(exomonad_actor::SourceInspectionError::Unreadable(_))
        ));
    }

    #[test]
    fn source_graph_keeps_workspace_resource_once_through_live_updates() {
        use exomonad_actor::ActorSourceLayers;
        let (project, run) = workspace_with("module Project.Work where\nwork = 1\n");
        let reload = Arc::new(runtime_reload_for_run(project.path(), Arc::new(run)).unwrap());
        let services = reload.clone().inspection_services();
        let inspection = services.source_inspection();
        let selected = reload.freeze_checkpoint_layer(PrincipalId::SYSTEM).unwrap();
        let projected = reload.toolset_layer_from(&selected).unwrap();
        assert_eq!(projected.include_paths(), selected.include_paths());
        let resources = reload.frozen.workspace_resources().canonicalize().unwrap();
        assert_eq!(
            selected
                .include_paths()
                .iter()
                .filter(|path| **path == resources)
                .count(),
            1
        );
        for root in reload.shared_roots().unwrap() {
            assert!(selected.include_paths().contains(&root));
        }
        std::fs::write(
            project.path().join(".exomonad/Project/Work.hs"),
            "module Project.Work where\nwork = 2\n",
        )
        .unwrap();
        let pending = reload
            .layer
            .capture_from_workspace(&reload.frozen, project.path())
            .unwrap();
        reload.layer.publish(pending).unwrap();
        let updated = reload.freeze_checkpoint_layer(PrincipalId::SYSTEM).unwrap();
        assert_ne!(selected.identities(), updated.identities());
        assert_eq!(
            updated
                .include_paths()
                .iter()
                .filter(|path| **path == resources)
                .count(),
            1
        );
        for root in reload.shared_roots().unwrap() {
            assert!(updated.include_paths().contains(&root));
        }
        assert!(inspection
            .read_from(&selected, "Project.Work")
            .unwrap()
            .1
            .contains("work = 1"));
        assert_eq!(
            inspection.locate_module(&selected, "Project.Work").unwrap(),
            Some(inspection.revision(&selected).unwrap().identity)
        );
        assert_eq!(
            inspection
                .locate_module(&selected, "Missing.Module")
                .unwrap(),
            None
        );
        assert!(inspection
            .locate_module(&selected, "../Project.Work")
            .is_err());
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
    fn runtime_state_writes_do_not_change_source_capture() {
        let project = tempfile::tempdir().unwrap();
        let run = tempfile::tempdir().unwrap();
        let root = project.path().join(".exomonad");
        std::fs::create_dir_all(root.join("Project/state")).unwrap();
        let authored = root.join("Project/Work.hs");
        std::fs::write(&authored, "module Project.Work where\nwork = 1\n").unwrap();
        std::fs::write(root.join("Project/state/extra.h"), "#define EXTRA 1\n").unwrap();
        let roots = [root.clone()];
        let layer = SourceLayer::new(run.path());
        let before = layer.observe_from_roots("state-test", &roots).unwrap();

        let memory = root.join("state/Scratch.hs");
        std::fs::create_dir_all(memory.parent().unwrap()).unwrap();
        std::fs::write(&memory, "module Scratch where\nscratch = 2\n").unwrap();
        assert_eq!(
            layer.observe_from_roots("state-test", &roots).unwrap(),
            before
        );
        let captured = layer.capture_from_roots("state-test", &roots).unwrap();
        assert_eq!(captured.revision(), &before);
        assert!(!captured.directory.join("0/state").exists());
        assert!(captured.directory.join("0/Project/Work.hs").is_file());
        assert!(captured.directory.join("0/Project/state/extra.h").is_file());
        assert_eq!(
            std::fs::read_to_string(&memory).unwrap(),
            "module Scratch where\nscratch = 2\n"
        );

        std::fs::write(&authored, "module Project.Work where\nwork = 3\n").unwrap();
        assert_ne!(
            layer
                .observe_from_roots("state-test", &roots)
                .unwrap()
                .identity,
            before.identity
        );
        let changed = layer.capture_from_roots("state-test", &roots).unwrap();
        assert_ne!(changed.revision().identity, before.identity);
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
        assert_ne!(published.identity, first.identity);
        assert_eq!(
            published,
            layer
                .observe_from_workspace(&frozen, project.path())
                .unwrap()
        );
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
    fn retained_helper_selection_creates_no_draft_or_source_generation() {
        use exomonad_actor::ActorSourceLayers;

        let (project, run) = workspace_with("module Project.Work where\nwork = 1\n");
        let frozen = FrozenWorkspace::load(project.path(), run.path()).unwrap();
        let helper_root = run.path().join("retained-helpers");
        let reload = ExomonadSourceReload::new(
            frozen,
            project.path().to_path_buf(),
            run.path().to_path_buf(),
            crate::haskell_sources::ensure_exomonad_haskell().unwrap(),
        )
        .with_helper_root(helper_root.clone());
        let creator = PrincipalId::new(1, 1);
        reload
            .helper_scopes
            .write()
            .insert(creator, "selected-source".into());
        assert!(!helper_root.exists());
        for _ in 0..3 {
            assert_eq!(
                ActorSourceLayers::retain_helpers(&reload, creator).unwrap(),
                "selected-source"
            );
        }
        assert!(
            !helper_root.exists(),
            "retaining a selection must not copy drafts or publish source"
        );
        assert!(ActorSourceLayers::retain_helpers(&reload, PrincipalId::new(1, 2)).is_err());
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

    #[test]
    fn source_publication_histories_preserve_revisions_through_record_recovery() {
        use proptest::prelude::*;

        #[derive(Clone, Debug)]
        enum RecordState {
            Current,
            Missing,
            Stale,
        }

        let state = prop_oneof![
            Just(RecordState::Current),
            Just(RecordState::Missing),
            Just(RecordState::Stale),
        ];
        let histories = proptest::collection::vec((-2i16..3, state), 1..12);
        let mut config = proptest::test_runner::Config::default();
        config.cases = 32;
        let mut config = proptest::test_runner::contextualize_config(config);
        config.source_file = Some(file!());
        config.test_name = Some(concat!(
            module_path!(),
            "::source_publication_histories_preserve_revisions_through_record_recovery"
        ));
        let mut runner = proptest::test_runner::TestRunner::new(config);
        let coverage = std::cell::RefCell::new([0usize; 4]);
        runner
            .run(&histories, |history| {
                let (project, run) =
                    workspace_with("module Project.Work where\nwork :: Int\nwork = 1\n");
                let frozen = FrozenWorkspace::load(project.path(), run.path()).unwrap();
                let layer = SourceLayer::new(run.path());
                let mut active = layer.ensure_active(&frozen).unwrap();
                // Guarantee recovery and repeated identical publication alongside
                // arbitrary publication/recovery ordering in every history.
                let support = [
                    (1, RecordState::Current),
                    (2, RecordState::Missing),
                    (3, RecordState::Stale),
                    (3, RecordState::Missing),
                ];
                for (work, state) in support.iter().chain(&history) {
                    let previous_record = std::fs::read(layer.active_record()).unwrap();
                    let body = format!("module Project.Work where\nwork :: Int\nwork = {work}\n");
                    std::fs::write(project.path().join(".exomonad/Project/Work.hs"), &body)
                        .unwrap();
                    let expected = layer
                        .publish(
                            layer
                                .capture_from_workspace(&frozen, project.path())
                                .unwrap(),
                        )
                        .unwrap();
                    if expected.identity == active.identity {
                        coverage.borrow_mut()[3] += 1;
                    }
                    match state {
                        RecordState::Current => coverage.borrow_mut()[0] += 1,
                        RecordState::Missing => {
                            coverage.borrow_mut()[1] += 1;
                            std::fs::remove_file(layer.active_record()).unwrap();
                        }
                        RecordState::Stale => {
                            coverage.borrow_mut()[2] += 1;
                            std::fs::write(layer.active_record(), &previous_record).unwrap();
                        }
                    }
                    let recovered = layer.read_active().unwrap();
                    prop_assert_eq!(&recovered, &Some(expected.clone()));
                    prop_assert_eq!(layer.read_active().unwrap(), recovered);
                    prop_assert_eq!(
                        std::fs::read_to_string(layer.include_paths(1)[0].join("Project/Work.hs"))
                            .unwrap(),
                        body
                    );
                    active = expected;
                }
                Ok(())
            })
            .unwrap();
        eprintln!(
            "source publication current/missing/stale/repeated coverage: {:?}",
            coverage.borrow()
        );
        assert!(coverage.borrow().iter().all(|count| *count > 0));
    }

    // ------------------------------------------------------------------
    // The reload transaction itself, checked by the run's own compiler.
    // ------------------------------------------------------------------

    /// A two-module workspace: `Project.Work` is the configured module and it
    /// reads `Project.Types`, so `Types` has a reverse dependency to rebuild.
    fn init_git_repo(path: &Path) {
        let git = exomonad_worktree::GitCli::new();
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

    /// Source-revision identity; artifact reuse additionally validates the
    /// compiler's consumed dependency and import-resolution evidence.

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
    struct PreparedSourceFixture {
        project: exomonad_worktree::testing::TestRepo,
        files: Arc<tempfile::TempDir>,
        deployment: Arc<tidepool_atomic_write::DirectoryAnchor>,
        selected: super::super::workspace::PreparedWorkspaceSelection,
    }

    impl PreparedSourceFixture {
        async fn prepare() -> Self {
            tidepool_testing::eval_harness::require_extract();
            let files = Arc::new(tempfile::tempdir().unwrap());
            let project = exomonad_worktree::testing::TestRepo::init().unwrap();
            let settings =
                crate::actor_host::test_campaign::hosted_test_settings(files.as_ref(), 1);
            let authored = project.path().join(".exomonad");
            crate::exomonad::write_fixture_project_config(&authored, "test-model", |config| {
                config.launch.embedded = Some(settings);
                config.haskell.source_roots = vec![PathBuf::from(".")];
            });
            std::fs::create_dir_all(authored.join("Project")).unwrap();
            std::fs::write(
                authored.join("Project/Work.hs"),
                "module Project.Work where\nwork :: Int\nwork = 1\n",
            )
            .unwrap();
            let git = exomonad_worktree::GitCli::new();
            git.try_run(project.path(), &["add", "--all"]).unwrap();
            git.try_run(
                project.path(),
                &[
                    "-c",
                    "commit.gpgsign=false",
                    "commit",
                    "--quiet",
                    "-m",
                    "prepared source fixture",
                ],
            )
            .unwrap();
            let deployment = Arc::new(
                tidepool_atomic_write::DirectoryAnchor::open_existing(files.path())
                    .unwrap()
                    .child("prepared")
                    .unwrap(),
            );
            let selected =
                super::super::workspace::prepare_workspace(project.path(), Arc::clone(&deployment))
                    .await
                    .unwrap();
            Self {
                project,
                files,
                deployment,
                selected,
            }
        }

        fn new_run(&self) -> (Arc<tempfile::TempDir>, Arc<ExomonadSourceReload>) {
            // Each generated history begins at the same authored baseline and
            // receives independent mutable publication and draft owners.
            std::fs::write(
                self.project.path().join(".exomonad/Project/Work.hs"),
                "module Project.Work where\nwork :: Int\nwork = 1\n",
            )
            .unwrap();
            let run = Arc::new(tempfile::tempdir().unwrap());
            let frozen = self
                .selected
                .select_for_run(self.project.path(), run.path())
                .unwrap();
            let reload = Arc::new(
                ExomonadSourceReload::new_owned(
                    frozen.clone(),
                    self.project.path().to_owned(),
                    run.path().to_owned(),
                    frozen.runtime_actors(),
                    SourceRootOwner::Temporary(Arc::clone(&run)),
                )
                .unwrap(),
            );
            (run, reload)
        }

        fn make_writable(&self) {
            fn restore(path: &Path) {
                use std::os::unix::fs::PermissionsExt;
                let metadata = std::fs::symlink_metadata(path).unwrap();
                if metadata.file_type().is_symlink() {
                    return;
                }
                std::fs::set_permissions(
                    path,
                    std::fs::Permissions::from_mode(
                        metadata.permissions().mode()
                            | if metadata.is_dir() { 0o700 } else { 0o600 },
                    ),
                )
                .unwrap();
                if metadata.is_dir() {
                    for entry in std::fs::read_dir(path).unwrap() {
                        restore(&entry.unwrap().path());
                    }
                }
            }
            restore(self.files.path());
        }
    }

    impl Drop for PreparedSourceFixture {
        fn drop(&mut self) {
            self.make_writable();
        }
    }

    #[tokio::test]
    async fn prepared_source_histories_keep_original_pair_and_restore_published_helpers() {
        use exomonad_actor::ActorSourceLayers;
        use proptest::prelude::*;
        let fixture = PreparedSourceFixture::prepare().await;
        let requests = tidepool_extract_cmd::extract_spawn_count();
        let mut config = proptest::test_runner::Config::default();
        config.cases = 16;
        let mut config = proptest::test_runner::contextualize_config(config);
        config.source_file = Some(file!());
        config.test_name = Some(concat!(
            module_path!(),
            "::prepared_source_histories_keep_original_pair_and_restore_published_helpers"
        ));
        let mut runner = proptest::test_runner::TestRunner::new(config);
        let publications = std::cell::RefCell::new([0usize; 2]);
        let restores = std::cell::Cell::new(0usize);
        runner
            .run(&proptest::collection::vec(0u8..4, 0..8), |random| {
                let (run, reload) = fixture.new_run();
                let project = &fixture.project;
                let original = reload.prepared_toolset_layer().unwrap();
                let initial = reload.freeze_checkpoint_layer(PrincipalId::SYSTEM).unwrap();
                prop_assert_eq!(initial.semantic_digest(), original.semantic_digest());
                let mut helper = reload.prepared_helper_revision().unwrap().unwrap();
                let mut authored = reload.prepared_run_revision().unwrap().unwrap();
                let mut captures = vec![initial];
                for (step, action) in [0u8, 1, 2, 3].into_iter().chain(random).enumerate() {
                    match action {
                        0 => {
                            std::fs::write(
                                project.path().join(".exomonad/Project/Work.hs"),
                                format!(
                                    "module Project.Work where\nwork :: Int\nwork = {}\n",
                                    step + 2
                                ),
                            )
                            .unwrap();
                            let pending = reload
                                .layer
                                .capture_from_workspace(&reload.frozen, project.path())
                                .unwrap();
                            reload.layer.publish(pending).unwrap();
                            authored = reload
                                .layer
                                .checkpoint_revision(reload.frozen.identity())
                                .unwrap();
                            publications.borrow_mut()[0] += 1;
                        }
                        1 => {
                            let draft = reload.helper_draft("run");
                            std::fs::write(
                                draft.join("SessionHelpers.hs"),
                                format!(
                                    "module SessionHelpers where\nhelper :: Int\nhelper = {}\n",
                                    step + 2
                                ),
                            )
                            .unwrap();
                            let layer = reload.helper_layer("run");
                            let pending = layer
                                .capture_from_roots(reload.frozen.identity(), &[draft])
                                .unwrap();
                            layer.publish(pending).unwrap();
                            helper = layer.checkpoint_revision(reload.frozen.identity()).unwrap();
                            publications.borrow_mut()[1] += 1;
                        }
                        2 => {
                            let before =
                                reload.freeze_checkpoint_layer(PrincipalId::SYSTEM).unwrap();
                            let pending = reload
                                .layer
                                .capture_from_workspace(&reload.frozen, project.path())
                                .unwrap();
                            reload.layer.publish(pending).unwrap();
                            let layer = reload.helper_layer("run");
                            let pending = layer
                                .capture_from_roots(
                                    reload.frozen.identity(),
                                    &[reload.helper_draft("run")],
                                )
                                .unwrap();
                            layer.publish(pending).unwrap();
                            let after =
                                reload.freeze_checkpoint_layer(PrincipalId::SYSTEM).unwrap();
                            prop_assert_eq!(before.semantic_digest(), after.semantic_digest());
                            captures.push(after);
                        }
                        3 => {
                            let cold = ExomonadSourceReload::new_owned(
                                reload.frozen.clone(),
                                project.path().to_owned(),
                                run.path().to_owned(),
                                reload.frozen.runtime_actors(),
                                SourceRootOwner::Temporary(Arc::clone(&run)),
                            )
                            .unwrap();
                            for capture in &captures {
                                let restored = cold
                                    .restore_graph(
                                        &capture.generation_evidence().unwrap().reference,
                                    )
                                    .unwrap();
                                prop_assert_eq!(restored.include_paths(), capture.include_paths());
                                prop_assert_eq!(
                                    restored.generation_evidence().unwrap().configuration_digest,
                                    capture.generation_evidence().unwrap().configuration_digest
                                );
                                restores.set(restores.get() + 1);
                            }
                        }
                        _ => unreachable!(),
                    }
                    let current = reload.freeze_checkpoint_layer(PrincipalId::SYSTEM).unwrap();
                    // Oracle: retain each lower-level publication separately, then
                    // concatenate the expected precedence independently of composer.
                    let mut expected = helper.paths.clone();
                    expected.extend(authored.paths.iter().cloned());
                    expected.push(reload.frozen.workspace_resources().canonicalize().unwrap());
                    for shared in reload.shared_roots().unwrap() {
                        if !expected.contains(&shared) {
                            expected.push(shared);
                        }
                    }
                    prop_assert_eq!(current.include_paths(), expected.as_slice());
                    for (path, actual) in expected.iter().zip(current.source_manifests().unwrap()) {
                        prop_assert!(same_source_manifest(
                            actual,
                            &source_root_manifest(path).unwrap()
                        ));
                    }
                    prop_assert_eq!(
                        reload.prepared_toolset_layer().unwrap().semantic_digest(),
                        original.semantic_digest()
                    );
                    prop_assert_ne!(
                        current.generation_evidence().unwrap().configuration_digest,
                        original.generation_evidence().unwrap().configuration_digest
                    );
                }
                Ok(())
            })
            .unwrap();
        eprintln!(
            "prepared histories cases=16 run_publications={} helper_publications={} restores={}",
            publications.borrow()[0],
            publications.borrow()[1],
            restores.get()
        );
        assert!(publications.borrow().iter().all(|count| *count >= 16));
        assert!(restores.get() >= 32);
        assert_eq!(tidepool_extract_cmd::extract_spawn_count(), requests);
    }

    #[tokio::test]
    async fn prepared_initial_pair_refuses_missing_corrupt_and_wrong_helper_or_run() {
        let fixture = PreparedSourceFixture::prepare().await;
        let (_run, reload) = fixture.new_run();
        fixture.make_writable();
        assert!(reload
            .frozen
            .prepared_helper_revision()
            .unwrap()
            .unwrap()
            .starts_with(fixture.deployment.path().join("workspace/helpers")));
        let original = reload.prepared_toolset_layer().unwrap();
        let reference = &original.generation_evidence().unwrap().reference;
        assert!(reload.restore_graph(reference).is_ok());
        for legacy in [false, true] {
            let mut graph: serde_json::Value = serde_json::from_slice(reference.bytes()).unwrap();
            if legacy {
                graph["version"] = serde_json::json!(1);
            } else {
                graph["helper_branch"] = serde_json::Value::Null;
            }
            let altered = tidepool_runtime::session::GenerationArtifactReference::capture(
                serde_json::to_vec(&graph).unwrap(),
            );
            assert!(
                reload.restore_graph(&altered).is_err(),
                "helper-free or legacy graphs refuse"
            );
        }
        let assert_refused = || {
            assert!(reload.prepared_toolset_layer().is_err());
            assert!(reload.restore_graph(reference).is_err());
            let new_run = tempfile::tempdir().unwrap();
            assert!(fixture
                .selected
                .select_for_run(fixture.project.path(), new_run.path())
                .is_err());
            assert!(!new_run.path().join("workspace-prepared.json").exists());
        };
        for (selected, layer, wrong_file, wrong_source) in [
            (
                reload.frozen.prepared_helper_revision().unwrap().unwrap(),
                reload.helper_layer("run"),
                "SessionHelpers.hs",
                "module SessionHelpers where\nwrong = 3\n",
            ),
            (
                reload.frozen.prepared_source_revision().unwrap().unwrap(),
                reload.layer.clone(),
                "Project/Work.hs",
                "module Project.Work where\nwrong = 4\n",
            ),
        ] {
            let retained = selected.with_extension("retained");
            std::fs::rename(&selected, &retained).unwrap();
            assert_refused();
            let seed = tempfile::tempdir().unwrap();
            let wrong_path = seed.path().join(wrong_file);
            std::fs::create_dir_all(wrong_path.parent().unwrap()).unwrap();
            std::fs::write(wrong_path, wrong_source).unwrap();
            let wrong = layer
                .capture_from_roots(reload.frozen.identity(), &[seed.path().to_owned()])
                .unwrap();
            copy_revision_tree(&wrong.directory, &selected).unwrap();
            assert_refused();
            std::fs::remove_dir_all(&selected).unwrap();
            copy_revision_tree(&retained, &selected).unwrap();
            let resource = selected.join("resources/Exomonad/Source/Revision.hs");
            std::fs::write(&resource, "corrupt revision resource").unwrap();
            assert_refused();
            std::fs::remove_dir_all(&selected).unwrap();
            std::fs::rename(retained, selected).unwrap();
            assert_eq!(
                reload.prepared_toolset_layer().unwrap().semantic_digest(),
                original.semantic_digest()
            );
        }
    }

    fn generation_owner() -> (tempfile::TempDir, Arc<ExomonadSourceReload>) {
        let (project, run) = workspace_with("module Project.Work where\nwork :: Int\nwork = 1\n");
        let config = project.path().join(".exomonad/config.toml");
        let mut text = std::fs::read_to_string(&config).unwrap();
        text.push_str("[prompts]\nagent = 'agent.md'\n");
        std::fs::write(config, text).unwrap();
        std::fs::write(project.path().join(".exomonad/agent.md"), "Instructions A").unwrap();
        let reload = Arc::new(runtime_reload_for_run(project.path(), Arc::new(run)).unwrap());
        reload
            .set_prompt_composer(
                PrincipalId::SYSTEM,
                Arc::new(|inputs| {
                    Ok(tidepool_runtime::session::ComposedPromptSnapshot::capture(
                        inputs.agent().unwrap_or("default").into(),
                    ))
                }),
            )
            .unwrap();
        (project, reload)
    }

    #[test]
    fn inspection_service_retains_exact_owner_and_refuses_foreign_source() {
        use exomonad_actor::{ActorSourceLayers, HasSourceInspection, SourceInspection};
        fn read<C: HasSourceInspection>(
            services: &C,
            source: &exomonad_actor::CheckpointSourceLayer,
        ) -> std::result::Result<(String, String), exomonad_actor::SourceInspectionError> {
            services
                .source_inspection()
                .read_from(source, "Project.Work")
        }
        let (project, owner) = generation_owner();
        let (_foreign_project, foreign) = generation_owner();
        let issued = owner.freeze_checkpoint_layer(PrincipalId::SYSTEM).unwrap();
        let foreign_issued = foreign
            .freeze_checkpoint_layer(PrincipalId::SYSTEM)
            .unwrap();
        let services = exomonad_actor::SourceInspectionServices::from_source_layers(owner.clone());
        let expected = read(&services, &issued).unwrap();
        std::fs::write(
            project.path().join(".exomonad/Project/Work.hs"),
            "module Project.Work where\nwork :: Int\nwork = 2\n",
        )
        .unwrap();
        // Retained inspection is tied to its capsule, including after its
        // composition root drops its lifecycle service handle.
        drop(owner);
        let retained = services.clone();
        drop(services);
        assert_eq!(read(&retained, &issued).unwrap(), expected);
        assert!(matches!(
            read(&retained, &foreign_issued),
            Err(exomonad_actor::SourceInspectionError::Unavailable(_))
        ));
    }

    #[test]
    fn coherent_source_generation_retains_reads_prompts_and_recovers_without_links() {
        use exomonad_actor::ActorSourceLayers;
        let (project, reload) = generation_owner();
        let services = reload.clone().inspection_services();
        let inspection = services.source_inspection();
        let a = reload.freeze_checkpoint_layer(PrincipalId::SYSTEM).unwrap();
        let read_a = inspection.read_from(&a, "Project.Work").unwrap();
        let status = inspection.observe(&a).unwrap();
        assert_eq!(status.active.identity, status.disk.identity);
        std::fs::write(
            project.path().join(".exomonad/Project/Work.hs"),
            "module Project.Work where\nwork :: Int\nwork = 41\n",
        )
        .unwrap();
        std::fs::write(
            project.path().join(".exomonad/Project/New.hs"),
            "module Project.New where\nanswer :: Int\nanswer = 42\n",
        )
        .unwrap();
        std::fs::write(project.path().join(".exomonad/agent.md"), "Instructions B").unwrap();
        let staged = reload
            .clone()
            .stage_reload(PrincipalId::SYSTEM, &a, &[])
            .unwrap();
        assert!(staged
            .validation_imports()
            .template_text()
            .contains("Project.New ()"));
        assert_eq!(staged.previous_revision().identity, read_a.0);
        assert_eq!(staged.prompt_snapshot().instructions(), "Instructions B");
        let b = staged.source().clone();
        let reference_b = staged.evidence().reference.clone();
        assert_eq!(inspection.read_from(&a, "Project.Work").unwrap(), read_a);
        assert!(inspection
            .read_from(&b, "Project.Work")
            .unwrap()
            .1
            .contains("work = 41"));
        let observation = inspection.observe(&b).unwrap();
        assert_eq!(observation.active.identity, observation.disk.identity);
        // A staged generation leaves the boot selection at A. Its issued
        // reads and durable reference select B even with these stale links.
        assert!(reload
            .layer
            .read_active()
            .unwrap()
            .unwrap()
            .modules
            .iter()
            .all(|(module, _)| module != "Project.New"));
        assert_eq!(
            reload.restore_source(&reference_b).unwrap().identities(),
            b.identities()
        );
        for layer in [&reload.layer, &reload.helper_layer("run")] {
            std::fs::remove_file(layer.active_link()).unwrap();
            std::fs::remove_file(layer.active_record()).unwrap();
        }
        let restored = reload.restore_source(&reference_b).unwrap();
        assert_eq!(restored.identities(), b.identities());
        assert_eq!(restored.include_paths(), b.include_paths());
        assert_eq!(
            inspection.read_from(&restored, "Project.New").unwrap(),
            inspection.read_from(&b, "Project.New").unwrap()
        );
        std::fs::remove_file(project.path().join(".exomonad/Project/New.hs")).unwrap();
        let c = reload
            .clone()
            .stage_reload(PrincipalId::SYSTEM, &b, &[])
            .unwrap();
        assert!(c.changed_modules().contains(&"Project.New".to_owned()));
        assert!(inspection.read_from(c.source(), "Project.New").is_err());
        assert!(inspection.read_from(&b, "Project.New").is_ok());
        assert!(!c
            .validation_imports()
            .template_text()
            .contains("Project.New ()"));
        let status = inspection.observe(&b).unwrap();
        assert_ne!(status.active.identity, status.disk.identity);
        assert!(status
            .active
            .modules
            .iter()
            .any(|module| module.name == "Project.New"));
        assert!(!status
            .disk
            .modules
            .iter()
            .any(|module| module.name == "Project.New"));
        let same = reload
            .clone()
            .stage_reload(PrincipalId::SYSTEM, c.source(), &[])
            .unwrap();
        assert_eq!(same.configuration_digest(), c.configuration_digest());
        let memory = project.path().join(".exomonad/state");
        std::fs::create_dir_all(&memory).unwrap();
        std::fs::write(
            memory.join("Memory.hs"),
            "durable memory is not Haskell source",
        )
        .unwrap();
        let after_memory = reload
            .clone()
            .stage_reload(PrincipalId::SYSTEM, c.source(), &[])
            .unwrap();
        assert_eq!(
            after_memory.configuration_digest(),
            c.configuration_digest()
        );
        std::fs::write(project.path().join(".exomonad/agent.md"), "Instructions C").unwrap();
        let prompts = reload
            .clone()
            .stage_reload(PrincipalId::SYSTEM, c.source(), &[])
            .unwrap();
        assert_ne!(prompts.configuration_digest(), c.configuration_digest());
        assert_eq!(prompts.candidate_revision(), c.candidate_revision());
        assert_eq!(c.prompt_snapshot().instructions(), "Instructions B");
        assert_eq!(prompts.prompt_snapshot().instructions(), "Instructions C");
    }

    #[test]
    fn source_reference_refuses_incomplete_graph_aliases_and_raw_file_drift() {
        use exomonad_actor::ActorSourceLayers;
        use tidepool_runtime::session::GenerationArtifactReference;
        let (project, reload) = generation_owner();
        let services = reload.clone().inspection_services();
        let inspection = services.source_inspection();
        let a = reload.freeze_checkpoint_layer(PrincipalId::SYSTEM).unwrap();
        let reference = &a.generation_evidence().unwrap().reference;
        let original: serde_json::Value = serde_json::from_slice(reference.bytes()).unwrap();
        for mode in 0..3 {
            let mut modified = original.clone();
            let roots = modified["roots"].as_array_mut().unwrap();
            match mode {
                0 => {
                    roots.pop();
                }
                1 => {
                    roots.swap(0, 1);
                }
                _ => {
                    roots[0]["path"] = serde_json::json!(project.path());
                }
            }
            let foreign =
                GenerationArtifactReference::capture(serde_json::to_vec(&modified).unwrap());
            assert!(
                reload.restore_source(&foreign).is_err(),
                "refuse unowned source graph variant {mode}"
            );
        }
        let a_text = inspection.read_from(&a, "Project.Work").unwrap();
        std::fs::write(
            project.path().join(".exomonad/Project/Work.hs"),
            "module Project.Work where\nwork :: Int\nwork = 41\n",
        )
        .unwrap();
        let b = reload
            .clone()
            .stage_reload(PrincipalId::SYSTEM, &a, &[])
            .unwrap();
        let root = b
            .source()
            .include_paths()
            .iter()
            .find(|root| root.join("Project/Work.hs").is_file())
            .unwrap();
        std::fs::write(root.join("Project/Work.hs"), "raw mutation").unwrap();
        assert!(reload.restore_source(&b.evidence().reference).is_err());
        assert!(inspection.read_from(b.source(), "Project.Work").is_err());
        assert_eq!(inspection.read_from(&a, "Project.Work").unwrap(), a_text);
    }

    #[test]
    fn source_recovery_validates_recorded_ownership_and_disk_before_entry_selection() {
        use exomonad_actor::{ActorSourceLayers, SourceEntryStorage, ToolsetAcquisition};
        use tidepool_runtime::session::GenerationArtifactReference;
        let (project, reload) = generation_owner();
        let selected = reload.freeze_checkpoint_layer(PrincipalId::SYSTEM).unwrap();
        let reference = &selected.generation_evidence().unwrap().reference;
        let acquisition = ToolsetAcquisition::FreshRunOriginal {
            recipe: "a".repeat(64),
            original: uuid::Uuid::new_v4(),
        };
        let builtin = ToolsetAcquisition::BuiltinDeploymentProgram {
            program: exomonad_actor::BuiltinDeploymentProgram {
                policy: exomonad_actor::BuiltinToolsetPolicy::Workbench,
                manifest_blake3: "b".repeat(64),
            },
        };
        let assert_refused = |reference: &GenerationArtifactReference| {
            assert!(reload.restore_source(reference).is_err());
            assert!(reload
                .restore_graph_with_entries(reference, reload.fresh_entry_storage.clone())
                .is_err());
            for acquisition in [&acquisition, &builtin] {
                assert!(reload
                    .restore_source_with_acquisition(reference, acquisition)
                    .is_err());
            }
        };
        let foreign = tempfile::tempdir().unwrap();
        let mut graph: serde_json::Value = serde_json::from_slice(reference.bytes()).unwrap();
        graph["entries"]["directory"] = serde_json::json!(foreign.path());
        let foreign_record =
            GenerationArtifactReference::capture(serde_json::to_vec(&graph).unwrap());
        assert_refused(&foreign_record);
        assert!(reload
            .restore_graph_with_entries(
                reference,
                SourceEntryStorage::FreshCompilation {
                    directory: foreign.path().to_owned(),
                    preparation: uuid::Uuid::new_v4(),
                },
            )
            .is_err());
        for acquisition in [
            ToolsetAcquisition::UnretainedCompilation {
                recipe: "a".repeat(64),
            },
            ToolsetAcquisition::FreshRunOriginal {
                recipe: "invalid".into(),
                original: uuid::Uuid::new_v4(),
            },
        ] {
            assert!(matches!(
                reload.restore_source_with_acquisition(reference, &acquisition),
                Err(exomonad_actor::SourceInspectionError::Unreadable(_))
            ));
        }

        let graph: serde_json::Value = serde_json::from_slice(reference.bytes()).unwrap();
        let prompts: PathBuf =
            serde_json::from_value(graph["prompts"]["directory"].clone()).unwrap();
        let retained_prompts = prompts.with_extension("retained");
        std::fs::rename(&prompts, &retained_prompts).unwrap();
        for acquisition in [&acquisition, &builtin] {
            assert!(matches!(
                reload.restore_source_with_acquisition(reference, acquisition),
                Err(exomonad_actor::SourceInspectionError::Unavailable(_))
            ));
        }
        std::fs::rename(retained_prompts, prompts).unwrap();

        // The selected old graph survives a newer boot publication, including
        // a different prompt capture. Recovery follows its reference directly.
        std::fs::write(
            project.path().join(".exomonad/Project/Work.hs"),
            "module Project.Work where\nwork :: Int\nwork = 99\n",
        )
        .unwrap();
        std::fs::write(project.path().join(".exomonad/agent.md"), "Instructions B").unwrap();
        let newer = reload
            .layer
            .capture_from_workspace(&reload.frozen, project.path())
            .unwrap();
        reload.layer.publish(newer).unwrap();
        let staged = reload
            .clone()
            .stage_reload(PrincipalId::SYSTEM, &selected, &[])
            .unwrap();
        assert_ne!(
            staged.configuration_digest(),
            selected.generation_evidence().unwrap().configuration_digest
        );
        let assert_restored = || {
            let restored = reload
                .restore_source_with_acquisition(reference, &acquisition)
                .unwrap();
            assert_eq!(restored.identities(), selected.identities());
            assert_eq!(restored.include_paths(), selected.include_paths());
            assert_eq!(
                &restored.generation_evidence().unwrap().reference,
                reference
            );
            assert!(reload
                .read_issued(&restored, "Project.Work")
                .unwrap()
                .1
                .contains("work = 1\n"));
        };
        assert_restored();

        let run_root = selected
            .include_paths()
            .iter()
            .find(|root| root.join("Project/Work.hs").is_file())
            .unwrap();
        // Both publication identities and the generated revision resource
        // remain authenticated before a recorded or overridden entry is issued.
        for (root, relative) in [
            (&selected.include_paths()[1], REVISION_MODULE),
            (run_root, "Project/Work.hs"),
        ] {
            let path = root.join(relative);
            let original = std::fs::read(&path).unwrap();
            std::fs::write(&path, "corrupt captured source").unwrap();
            assert_refused(reference);
            std::fs::write(path, original).unwrap();
            assert_restored();

            let retained = root.with_extension("retained");
            std::fs::rename(root, &retained).unwrap();
            assert_refused(reference);
            std::os::unix::fs::symlink(&retained, root).unwrap();
            assert_refused(reference);
            std::fs::remove_file(root).unwrap();
            std::fs::rename(&retained, root).unwrap();
            assert_restored();
        }
    }

    /// Capsule staging does not publish an actor generation. The model records
    /// authored facts and explicit retained selections, never the active links.
    #[derive(Clone, Debug)]
    enum SourceHistoryOp {
        Work(i16),
        Prompt(i16),
        Memory(i16),
        Extra(Option<i16>),
        Stage(u8),
        Abandon,
        Retain,
        Read(u8, bool),
        Restore(u8, u8),
        Observe(u8),
        CorruptReference(u8, u8),
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct SourceHistoryModel {
        work: i16,
        extra: Option<i16>,
        prompt: String,
    }

    impl SourceHistoryModel {
        fn modules(&self) -> BTreeMap<String, String> {
            let mut modules = BTreeMap::from([(
                "Project.Work".into(),
                format!(
                    "module Project.Work where\nwork :: Int\nwork = {}\n",
                    self.work
                ),
            )]);
            if let Some(extra) = self.extra {
                modules.insert(
                    "Project.Extra".into(),
                    format!("module Project.Extra where\nextra = {extra}\n"),
                );
            }
            modules
        }
    }

    #[derive(Default, Debug)]
    struct SourceHistoryCoverage {
        stages: usize,
        unchanged_stages: usize,
        prompt_only_stages: usize,
        memory_only_stages: usize,
        abandoned: usize,
        retained: usize,
        delayed_reads: usize,
        missing_reads: usize,
        restores: usize,
        restore_modes: [usize; 4],
        refusals: usize,
        deletion_stages: usize,
        readd_stages: usize,
        empty_candidate_ops: usize,
    }

    struct HistoryCapsule {
        source: exomonad_actor::CheckpointSourceLayer,
        model: SourceHistoryModel,
    }

    fn source_history_ops() -> impl proptest::strategy::Strategy<Value = Vec<SourceHistoryOp>> {
        use proptest::prelude::*;
        use SourceHistoryOp::*;
        let op = prop_oneof![
            3 => (-3i16..4).prop_map(Work),
            2 => (-3i16..4).prop_map(Prompt),
            1 => (-3i16..4).prop_map(Memory),
            2 => proptest::option::of(-3i16..4).prop_map(Extra),
            4 => (0u8..4).prop_map(Stage),
            1 => Just(Abandon),
            3 => Just(Retain),
            3 => (0u8..4, any::<bool>()).prop_map(|(slot, extra)| Read(slot, extra)),
            2 => (0u8..4, 0u8..4).prop_map(|(slot, mode)| Restore(slot, mode)),
            1 => (0u8..4).prop_map(Observe),
            2 => (0u8..4, 0u8..4).prop_map(|(slot, mode)| CorruptReference(slot, mode)),
        ];
        // Embed one interaction among arbitrary operations, preserving both
        // arbitrary ordering and support for the important delayed observations.
        let arbitrary = proptest::collection::vec(op.clone(), 1..32);
        let interactions = (
            proptest::collection::vec(op.clone(), 0..12),
            0u8..4,
            proptest::collection::vec(op, 0..12),
        )
            .prop_map(|(mut prefix, kind, suffix)| {
                prefix.extend(source_history_scenario(kind));
                prefix.extend(suffix);
                prefix
            });
        prop_oneof![4 => arbitrary, 1 => interactions]
    }

    fn source_history_scenario(kind: u8) -> Vec<SourceHistoryOp> {
        use SourceHistoryOp::*;
        match kind {
            0 => vec![
                Work(2),
                Stage(0),
                Retain,
                Abandon,
                Work(3),
                Read(0, false),
                Restore(0, 0),
                Observe(0),
            ],
            1 => vec![
                Stage(0),
                Retain,
                Prompt(2),
                Stage(0),
                Retain,
                Memory(2),
                Stage(0),
                Stage(0),
                Abandon,
                Restore(1, 1),
            ],
            2 => vec![
                Extra(Some(2)),
                Stage(0),
                Retain,
                Extra(None),
                Stage(0),
                Retain,
                Read(0, true),
                Extra(Some(3)),
                Stage(0),
                Retain,
                Read(2, true),
                Read(0, true),
                Restore(1, 2),
            ],
            _ => vec![
                Work(2),
                Stage(0),
                Retain,
                Abandon,
                Work(3),
                Stage(0),
                CorruptReference(0, 0),
                CorruptReference(0, 1),
                CorruptReference(0, 2),
                CorruptReference(0, 3),
                Read(0, false),
                Restore(0, 3),
            ],
        }
    }

    fn check_history_read(
        services: &impl HasSourceInspection,
        capsule: &HistoryCapsule,
        extra: bool,
    ) -> proptest::test_runner::TestCaseResult {
        let module = if extra {
            "Project.Extra"
        } else {
            "Project.Work"
        };
        let expected = capsule.model.modules();
        let read = services
            .source_inspection()
            .read_from(&capsule.source, module);
        if let Some(text) = expected.get(module) {
            proptest::prop_assert_eq!(read.unwrap().1, text.clone());
        } else {
            proptest::prop_assert!(read.is_err(), "absent module was exposed: {module}");
        }
        Ok(())
    }

    fn replay_source_history(
        actions: &[SourceHistoryOp],
        coverage: &mut SourceHistoryCoverage,
    ) -> proptest::test_runner::TestCaseResult {
        use exomonad_actor::ActorSourceLayers;
        use SourceHistoryOp::*;
        let (project, reload) = generation_owner();
        let services = reload.clone().inspection_services();
        let inspection = services.source_inspection();
        let authored = project.path().join(".exomonad");
        let mut draft = SourceHistoryModel {
            work: 1,
            extra: None,
            prompt: "Instructions A".into(),
        };
        let initial = reload.freeze_checkpoint_layer(PrincipalId::SYSTEM).unwrap();
        let mut captures = vec![(
            draft.clone(),
            initial.generation_evidence().unwrap().configuration_digest,
            inspection.revision(&initial).unwrap().identity,
        )];
        let mut retained = vec![HistoryCapsule {
            source: initial,
            model: draft.clone(),
        }];
        let mut candidate: Option<(
            Box<dyn exomonad_actor::StagedActorSourceReload>,
            SourceHistoryModel,
        )> = None;
        let mut memory_since_stage = false;
        let mut extra_was_deleted = false;
        for action in actions {
            // Logical slots are counted backwards from the newest retained
            // capsule; the initial capsule makes reads and restores always valid.
            let slot = |pick: u8| retained.len() - 1 - usize::from(pick) % retained.len();
            match *action {
                Work(value) => {
                    draft.work = value;
                    std::fs::write(
                        authored.join("Project/Work.hs"),
                        &draft.modules()["Project.Work"],
                    )
                    .unwrap();
                }
                Prompt(value) => {
                    draft.prompt = format!("Instructions {value}");
                    std::fs::write(authored.join("agent.md"), &draft.prompt).unwrap();
                }
                Memory(value) => {
                    std::fs::create_dir_all(authored.join("state")).unwrap();
                    std::fs::write(
                        authored.join("state/Memory.hs"),
                        format!("durable memory {value}"),
                    )
                    .unwrap();
                    memory_since_stage = true;
                }
                Extra(value) => {
                    draft.extra = value;
                    if let Some(text) = draft.modules().get("Project.Extra") {
                        std::fs::write(authored.join("Project/Extra.hs"), text).unwrap();
                    } else if authored.join("Project/Extra.hs").exists() {
                        std::fs::remove_file(authored.join("Project/Extra.hs")).unwrap();
                    }
                }
                Stage(pick) => {
                    let previous = &retained[slot(pick)];
                    let staged = reload
                        .clone()
                        .stage_reload(PrincipalId::SYSTEM, &previous.source, &[])
                        .unwrap();
                    let previous_modules = previous.model.modules();
                    let draft_modules = draft.modules();
                    let expected_changes = previous_modules
                        .keys()
                        .chain(draft_modules.keys())
                        .filter(|name| previous_modules.get(*name) != draft_modules.get(*name))
                        .cloned()
                        .collect::<BTreeSet<_>>();
                    let observed_changes = staged
                        .changed_modules()
                        .iter()
                        .filter(|name| name.starts_with("Project."))
                        .cloned()
                        .collect::<BTreeSet<_>>();
                    proptest::prop_assert_eq!(observed_changes, expected_changes);
                    proptest::prop_assert_eq!(
                        staged.prompt_snapshot().instructions(),
                        draft.prompt.as_str()
                    );
                    proptest::prop_assert_eq!(
                        &staged.previous_revision().identity,
                        &inspection.revision(&previous.source).unwrap().identity
                    );
                    let digest = staged.configuration_digest();
                    let identity = staged.candidate_revision().identity.clone();
                    for (model, old_digest, old_identity) in &captures {
                        let same_modules = model.modules() == draft.modules();
                        proptest::prop_assert_eq!(*old_identity == identity, same_modules);
                        proptest::prop_assert_eq!(
                            *old_digest == digest,
                            same_modules && model.prompt == draft.prompt
                        );
                    }
                    if let Some((last, _, _)) = captures.last() {
                        if last == &draft {
                            coverage.unchanged_stages += 1;
                            if memory_since_stage {
                                coverage.memory_only_stages += 1;
                            }
                        } else if last.modules() == draft.modules() {
                            coverage.prompt_only_stages += 1;
                        }
                        if last.extra.is_some() && draft.extra.is_none() {
                            coverage.deletion_stages += 1;
                            extra_was_deleted = true;
                        }
                    }
                    if extra_was_deleted && draft.extra.is_some() {
                        coverage.readd_stages += 1;
                        extra_was_deleted = false;
                    }
                    captures.push((draft.clone(), digest, identity));
                    memory_since_stage = false;
                    coverage.stages += 1;
                    candidate = Some((staged, draft.clone()));
                }
                Abandon => {
                    if candidate.take().is_some() {
                        coverage.abandoned += 1;
                    } else {
                        coverage.empty_candidate_ops += 1;
                    }
                }
                Retain => {
                    if let Some((staged, model)) = &candidate {
                        retained.push(HistoryCapsule {
                            source: staged.source().clone(),
                            model: model.clone(),
                        });
                        coverage.retained += 1;
                    } else {
                        coverage.empty_candidate_ops += 1;
                    }
                }
                Read(pick, extra) => {
                    let capsule = &retained[slot(pick)];
                    check_history_read(&services, capsule, extra)?;
                    if capsule.model.modules() != draft.modules() {
                        coverage.delayed_reads += 1;
                    }
                    if extra && capsule.model.extra.is_none() {
                        coverage.missing_reads += 1;
                    }
                }
                Restore(pick, mode) => {
                    let capsule = &retained[slot(pick)];
                    let reference = &capsule.source.generation_evidence().unwrap().reference;
                    use exomonad_actor::{SourceEntryStorage, ToolsetAcquisition};
                    // Acquisition metadata selects storage only. This history
                    // exercises source admission, not native installer custody.
                    let original = uuid::Uuid::new_v4();
                    let recipe = "a".repeat(64);
                    let restored = match mode {
                        0 => reload.restore_source(reference).unwrap(),
                        1 => reload.fresh_toolset_layer_from(&capsule.source).unwrap(),
                        2 => reload
                            .restore_source_with_acquisition(
                                reference,
                                &ToolsetAcquisition::FreshRunOriginal {
                                    recipe: recipe.clone(),
                                    original,
                                },
                            )
                            .unwrap(),
                        3 => reload
                            .restore_source_with_acquisition(
                                reference,
                                &ToolsetAcquisition::BuiltinDeploymentProgram {
                                    program: exomonad_actor::BuiltinDeploymentProgram {
                                        policy: exomonad_actor::BuiltinToolsetPolicy::Workbench,
                                        manifest_blake3: "b".repeat(64),
                                    },
                                },
                            )
                            .unwrap(),
                        _ => unreachable!(),
                    };
                    match mode {
                        0 | 3 => proptest::prop_assert_eq!(
                            restored.semantic_digest(),
                            capsule.source.semantic_digest()
                        ),
                        1 => proptest::prop_assert!(matches!(
                            restored.prepared_entries(),
                            Some(SourceEntryStorage::FreshCompilation {
                                directory, preparation,
                            }) if directory == reload.fresh_entry_storage.directory()
                                && *preparation == fresh_preparation(&reload)
                        )),
                        2 => {
                            let Some(SourceEntryStorage::CompletedOriginal {
                                directory,
                                selections,
                            }) = restored.prepared_entries()
                            else {
                                return Err(proptest::test_runner::TestCaseError::fail(
                                    "completed acquisition lost its exact selection",
                                ));
                            };
                            proptest::prop_assert_eq!(
                                directory,
                                reload.fresh_entry_storage.directory()
                            );
                            proptest::prop_assert_eq!(
                                selections,
                                &BTreeMap::from([(recipe, original)])
                            );
                        }
                        _ => unreachable!(),
                    }
                    proptest::prop_assert_eq!(restored.identities(), capsule.source.identities());
                    proptest::prop_assert_eq!(
                        restored.include_paths(),
                        capsule.source.include_paths()
                    );
                    proptest::prop_assert_eq!(
                        restored.generation_evidence().unwrap().reference.bytes(),
                        reference.bytes()
                    );
                    proptest::prop_assert_eq!(
                        restored.generation_evidence().unwrap().configuration_digest,
                        capsule
                            .source
                            .generation_evidence()
                            .unwrap()
                            .configuration_digest
                    );
                    let restored = HistoryCapsule {
                        source: restored,
                        model: capsule.model.clone(),
                    };
                    check_history_read(&services, &restored, false)?;
                    check_history_read(&services, &restored, true)?;
                    coverage.restores += 1;
                    coverage.restore_modes[usize::from(mode)] += 1;
                }
                Observe(pick) => {
                    let capsule = &retained[slot(pick)];
                    let status = inspection.observe(&capsule.source).unwrap();
                    proptest::prop_assert_eq!(
                        status.active.identity == status.disk.identity,
                        capsule.model.modules() == draft.modules()
                    );
                    for (observed, expected) in [
                        (&status.active.modules, capsule.model.modules()),
                        (&status.disk.modules, draft.modules()),
                    ] {
                        let observed = observed
                            .iter()
                            .filter(|m| m.name.starts_with("Project."))
                            .map(|m| m.name.clone())
                            .collect::<BTreeSet<_>>();
                        proptest::prop_assert_eq!(
                            observed,
                            expected.into_keys().collect::<BTreeSet<_>>()
                        );
                    }
                }
                CorruptReference(pick, mode) => {
                    let capsule = &retained[slot(pick)];
                    let reference = &capsule.source.generation_evidence().unwrap().reference;
                    // The valid reference must restore before changing exactly
                    // one nearby fact; refusal cannot pass on malformed setup.
                    reload.restore_source(reference).unwrap();
                    let mut graph: serde_json::Value =
                        serde_json::from_slice(reference.bytes()).unwrap();
                    match mode % 4 {
                        0 => {
                            graph["roots"].as_array_mut().unwrap().pop();
                        }
                        1 => {
                            graph["roots"].as_array_mut().unwrap().swap(0, 1);
                        }
                        2 => {
                            graph["roots"][0]["path"] = serde_json::json!(authored);
                        }
                        _ => {
                            graph["prompts"]["files"][0][2] = serde_json::json!("0".repeat(64));
                        }
                    }
                    let corrupted = tidepool_runtime::session::GenerationArtifactReference::capture(
                        serde_json::to_vec(&graph).unwrap(),
                    );
                    proptest::prop_assert!(reload.restore_source(&corrupted).is_err());
                    // Explicit refusal observation, not an invariant read after
                    // every edit: all issued snapshots survive the bad reference.
                    for old in &retained {
                        check_history_read(&services, old, false)?;
                        check_history_read(&services, old, true)?;
                    }
                    coverage.refusals += 1;
                }
            }
        }
        Ok(())
    }

    #[test]
    fn source_generation_history_support_reaches_delayed_reads_and_refusals() {
        let mut coverage = SourceHistoryCoverage::default();
        for kind in 0..4 {
            replay_source_history(&source_history_scenario(kind), &mut coverage).unwrap();
        }
        eprintln!("source history deterministic support: {coverage:?}");
        assert!(
            coverage.unchanged_stages > 0
                && coverage.prompt_only_stages > 0
                && coverage.memory_only_stages > 0
        );
        assert!(
            coverage.abandoned > 0
                && coverage.retained > 1
                && coverage.delayed_reads > 0
                && coverage.missing_reads > 0
        );
        assert!(
            coverage.restores > 0
                && coverage.refusals == 4
                && coverage.deletion_stages > 0
                && coverage.readd_stages > 0
        );
        assert!(coverage.restore_modes.iter().all(|count| *count > 0));
    }

    #[test]
    fn source_generation_histories_preserve_issued_reads_and_exact_recovery() {
        let mut config = proptest::test_runner::Config::default();
        config.cases = 16;
        let mut config = proptest::test_runner::contextualize_config(config);
        config.source_file = Some(file!());
        config.test_name = Some(concat!(
            module_path!(),
            "::source_generation_histories_preserve_issued_reads_and_exact_recovery"
        ));
        let mut runner = proptest::test_runner::TestRunner::new(config);
        let coverage = std::cell::RefCell::new(SourceHistoryCoverage::default());
        let callbacks = std::cell::Cell::new(0usize);
        let result = runner.run(&source_history_ops(), |actions| {
            callbacks.set(callbacks.get() + 1);
            replay_source_history(&actions, &mut coverage.borrow_mut())
        });
        eprintln!(
            "source history callbacks={} coverage={:?}",
            callbacks.get(),
            coverage.borrow()
        );
        result.unwrap();
    }

    #[tokio::test]
    async fn coherent_candidate_single_compile_checks_added_deleted_and_warmed_modules() {
        let mut owner = exomonad_actor::CompilerPreparationOwner::new();
        let outcome = owner.scope_with_client(coherent_candidate_history).await;
        assert!(outcome.cleanup.observation().is_confirmed());
    }

    async fn coherent_candidate_history(client: exomonad_actor::CompilerPreparationClient) {
        use exomonad_actor::ActorSourceLayers;
        let (project, reload) = generation_owner();
        let services = reload.clone().inspection_services();
        let inspection = services.source_inspection();
        let a = reload.freeze_checkpoint_layer(PrincipalId::SYSTEM).unwrap();
        let workbench = exomonad_actor::ActorWorkbenchSource::new(String::new(), Vec::new())
            .bind_preparation(client);
        let registry = Arc::new(tidepool_runtime::session::ImageRegistry::new());
        let mut owner = exomonad_actor::CompilerPreparationOwner::new();
        std::fs::write(
            project.path().join(".exomonad/Project/New.hs"),
            "module Project.New where\nimport Project.Work\nanswer :: Int\nanswer = work + 1\n",
        )
        .unwrap();
        let b = reload
            .clone()
            .stage_reload(PrincipalId::SYSTEM, &a, &[])
            .unwrap();
        let compile = owner
            .scope(workbench.prepare_source_toolset_with_validation(
                tidepool_toolchain::artifacts::CompileWorkload::Foreground,
                b.source().clone(),
                &[],
                &[],
                registry.clone(),
                b.validation_imports(),
            ))
            .await;
        assert!(compile.cleanup.observation().is_confirmed());
        let ready = compile
            .action
            .expect("one native installer compile validates a newly authored module");
        assert!(ready.completed_entry_selection().is_some());
        let reference_b = b.evidence().reference.clone();
        let prompt_b = b.prompt_snapshot().clone();
        std::fs::write(
            project.path().join(".exomonad/Project/Work.hs"),
            "module Project.Work where\nwork :: Int\nwork = 99\n",
        )
        .unwrap();
        std::fs::write(
            project.path().join(".exomonad/agent.md"),
            "Draft instructions C",
        )
        .unwrap();
        let SourceRootOwner::Temporary(run) = reload.source_owner.as_ref() else {
            panic!("generation fixture must own temporary run storage")
        };
        let cold = Arc::new(runtime_reload_for_run(project.path(), Arc::clone(run)).unwrap());
        let restored = cold
            .restore_source_with_acquisition(&reference_b, ready.acquisition())
            .unwrap();
        assert_eq!(
            restored.generation_evidence().unwrap().reference,
            reference_b
        );
        assert_eq!(
            cold.clone()
                .inspection_services()
                .source_inspection()
                .read_from(&restored, "Project.Work")
                .unwrap(),
            inspection.read_from(b.source(), "Project.Work").unwrap()
        );
        assert_eq!(prompt_b.instructions(), "Instructions A");
        let (recipe, original) = ready.completed_entry_selection().unwrap();
        assert!(
            matches!(restored.prepared_entries(), Some(exomonad_actor::SourceEntryStorage::CompletedOriginal { selections, .. })
            if selections.get(recipe) == Some(&original))
        );
        assert_ne!(
            cold.clone()
                .inspection_services()
                .source_inspection()
                .observe(&restored)
                .unwrap()
                .active
                .identity,
            cold.clone()
                .inspection_services()
                .source_inspection()
                .observe(&restored)
                .unwrap()
                .disk
                .identity
        );
        let mut owner = exomonad_actor::CompilerPreparationOwner::new();
        let warm = owner
            .scope(workbench.prepare_source_toolset_with_validation(
                tidepool_toolchain::artifacts::CompileWorkload::Foreground,
                b.source().clone(),
                &[],
                &[],
                registry.clone(),
                b.validation_imports(),
            ))
            .await;
        assert!(warm.cleanup.observation().is_confirmed());
        assert_eq!(warm.action.unwrap().selection(), ready.selection());
        std::fs::remove_file(project.path().join(".exomonad/Project/New.hs")).unwrap();
        let c = reload
            .clone()
            .stage_reload(PrincipalId::SYSTEM, b.source(), &["Project.New".to_owned()])
            .unwrap();
        let mut owner = exomonad_actor::CompilerPreparationOwner::new();
        let missing = owner
            .scope(workbench.prepare_source_toolset_with_validation(
                tidepool_toolchain::artifacts::CompileWorkload::Foreground,
                c.source().clone(),
                &[],
                &[],
                registry.clone(),
                c.validation_imports(),
            ))
            .await;
        assert!(missing.cleanup.observation().is_confirmed());
        assert!(
            missing.action.is_err(),
            "deleted module cannot resurrect from warmed A/B catalogs"
        );
        let valid = reload
            .clone()
            .stage_reload(PrincipalId::SYSTEM, b.source(), &[])
            .unwrap();
        // Each waiter owns its receipt; the shared preparation lifetime spans
        // the complete history, including a failed proposal and its retry.
        let mut owner = exomonad_actor::CompilerPreparationOwner::new();
        let deleted = owner
            .scope(workbench.prepare_source_toolset_with_validation(
                tidepool_toolchain::artifacts::CompileWorkload::Foreground,
                valid.source().clone(),
                &[],
                &[],
                registry,
                valid.validation_imports(),
            ))
            .await;
        assert!(deleted.cleanup.observation().is_confirmed());
        assert!(
            deleted.action.is_ok(),
            "deleting an unused module is a valid complete source revision: {:?}",
            deleted.action
        );
        assert!(inspection.read_from(b.source(), "Project.New").is_ok());
    }
}
