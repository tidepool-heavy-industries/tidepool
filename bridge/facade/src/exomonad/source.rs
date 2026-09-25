//! Live source layers: one per checkout, plus the run's own.
//!
//! A run freezes its Haskell source roots once, into
//! `<run_root>/workspace/sources/<capture>/<index>`, and that capture is
//! verified byte for byte every time the run is reloaded
//! ([`super::workspace::FrozenWorkspace::load`]). Nothing here writes inside
//! it. Instead this module owns mutable layers in FRONT of that floor. The
//! run's layer is shared by every actor:
//!
//! ```text
//! <run_root>/workspace/revisions/<identity>/{0,1,…,resources}
//! <run_root>/workspace/active -> revisions/<identity>
//! ```
//!
//! and an actor launched with a managed checkout gets one of its own, captured
//! from that checkout's `.exomonad` source roots:
//!
//! ```text
//! <run_root>/workspace/checkouts/<worktree>/revisions/<identity>/{0,…,resources}
//! <run_root>/workspace/checkouts/<worktree>/active -> revisions/<identity>
//! ```
//!
//! A layer is the same object either way — same capture walk, same content
//! identity, same typecheck before publication, same one-`rename(2)`
//! publication — and differs only in where it lives and what it reads. What
//! differs is reach: the run's layer sits ahead of the frozen capture in the
//! include list every actor shares, and a checkout layer sits ahead of THAT,
//! in one actor's include list alone
//! (`exomonad_actor::ActorCompileView::include_paths`). So an actor editing a
//! module inside its own checkout shadows the run's copy for its own later
//! cells and for nobody else's.
//!
//! Publishing a revision is one `rename(2)` of a symlink: a compile that opens
//! `active/0` sees either the whole previous revision or the whole new one,
//! never a mixture. No include VECTOR changes for the life of an actor — only
//! what one path on it resolves to.
//!
//! Not implemented, and why: an actor's already-installed tool record is not
//! re-derived on reload (it is a one-shot compile at actor startup; the
//! refresh boundary is the actor's next incarnation); `exomonad check --recipes`
//! still compiles against the frozen capture only; deleting a module from a
//! source root stops it being updated but not being importable, since the
//! frozen capture stays on the search path beneath the active layer; and a
//! checkout's declaration validation still runs against the forest-wide run
//! layer rather than the checkout's own, because the session library's
//! validation include and the resident machine are shared by the whole
//! forest.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use exomonad_worktree::{GitCli, WorktreeId, WorktreeManager};
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
/// `identity` is content-based, over the same per-file manifest the compiled
/// artifact cache keys on, so equal source gives equal identity and changed
/// source a different one. `generation` is the publication ordinal and exists
/// only for display: it is 1-based, and 0 means "this snapshot has never been
/// published".
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

/// One mutable source layer: its revisions, and the symlink naming the live
/// one. The run has one; so does every checkout that carries source.
#[derive(Clone, Debug)]
pub(crate) struct SourceLayer {
    directory: PathBuf,
}

impl SourceLayer {
    /// The run's own layer, shared by every actor that has no checkout layer
    /// of its own.
    pub(crate) fn new(run_root: &Path) -> Self {
        Self {
            directory: run_root.join("workspace"),
        }
    }

    /// The layer belonging to one managed checkout. It lives under the run
    /// root, never inside the checkout, so a capture can never read a
    /// previous capture of itself and Git never sees it.
    pub(crate) fn checkout(run_root: &Path, worktree: &str) -> Self {
        Self {
            directory: run_root.join("workspace").join("checkouts").join(worktree),
        }
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
    /// published record rather than a caller-supplied count. A checkout layer
    /// captures whatever roots that checkout has, which need not be as many as
    /// the run froze.
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
        self.ensure_active_from(frozen.identity(), frozen.captured_source_roots())
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
        let captured = self.capture(domain_identity, roots)?;
        captured.directory.close()?;
        Ok(captured.revision)
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
        for (index, root) in roots.iter().enumerate() {
            let mut captured = BTreeMap::new();
            super::workspace::capture_sources(
                root,
                Path::new(&index.to_string()),
                pending.path(),
                &mut captured,
            )?;
        }

        // The identity covers the captured source only. The generated module
        // below carries that identity, so hashing it too would be circular —
        // the same ordering `freeze` uses for `Exomonad/Workspace.hs`.
        let mut domain = DOMAIN.to_vec();
        domain.extend_from_slice(domain_identity.as_bytes());
        let captured_roots: Vec<PathBuf> = (0..roots.len())
            .map(|index| pending.path().join(index.to_string()))
            .collect();
        let identity = tidepool_toolchain::cache::source_roots_identity(&domain, &captured_roots)?;
        let modules = revision_modules(pending.path(), roots.len())?;

        std::fs::create_dir_all(pending.path().join("resources/Exomonad/Source"))?;
        tidepool_atomic_write::write_durable(
            &pending.path().join("resources").join(REVISION_MODULE),
            revision_module(&identity).as_bytes(),
        )?;

        Ok(CapturedRevision {
            directory: pending,
            revision: SourceRevision {
                identity,
                generation: 0,
                modules,
            },
        })
    }

    /// Point `active` at a checked candidate. One `rename(2)`: the previous
    /// revision stays complete until the instant the new one is complete.
    pub(crate) fn publish(&self, pending: PendingRevision) -> Result<SourceRevision> {
        let previous = self.read_active()?;
        let generation = previous.map_or(1, |previous| previous.generation + 1);
        let roots = revision_root_count(&pending.directory)?;
        let staged = self
            .directory
            .join(format!(".active-{}", uuid::Uuid::new_v4()));
        sync_revision_tree(&pending.directory)?;
        std::os::unix::fs::symlink(
            Path::new("revisions").join(&pending.revision.identity),
            &staged,
        )?;
        std::fs::rename(&staged, self.active_link())?;
        tidepool_atomic_write::sync_parent_directory(&self.active_link())?;
        tidepool_atomic_write::write_durable(
            &self.active_record(),
            &serde_json::to_vec_pretty(&ActiveRecord {
                identity: pending.revision.identity.clone(),
                generation,
                roots,
            })?,
        )?;
        Ok(SourceRevision {
            generation,
            ..pending.revision
        })
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

/// One checkout's source layer, and the checkout it is read from.
#[derive(Clone)]
struct CheckoutSource {
    layer: SourceLayer,
    workspace: PathBuf,
    /// The authored roots this checkout provides, resolved once, when the
    /// actor holding the checkout was constructed. Fixing them there is what
    /// makes the actor's search path and its reload target the same thing.
    roots: Arc<[PathBuf]>,
}

/// Which layer one actor's own source calls act on.
///
/// Selected when the actor is constructed and never afterwards, so an actor
/// cannot reach another actor's source by asking differently. This is the
/// whole authority story for `Source`: there is no role test anywhere below
/// this point, because by then the layer is already decided.
#[derive(Clone)]
enum ActorSourceScope {
    /// The run's own layer. Publishing here changes what every actor without a
    /// layer of its own compiles against, so it belongs to the actor that owns
    /// the run.
    Run,
    /// The actor's own checkout layer.
    Checkout(CheckoutSource),
    /// The run's layer, readable but not publishable. This actor compiles
    /// against it — that is what `sourceStatus` reports — and has no source of
    /// its own to publish.
    RunReadOnly,
}

/// The run's answer to the `Source` effect, for every actor in it.
///
/// It owns the run's frozen workspace, where its authored source lives, the
/// run's own layer, and one layer per checkout that carries source. The
/// compile that decides whether a candidate is acceptable is the run's
/// ordinary driver compile, so every reload — the run's and a checkout's — is
/// checked by exactly the compiler the run uses.
pub(crate) struct ExomonadSourceReload {
    frozen: FrozenWorkspace,
    workspace: PathBuf,
    run_root: PathBuf,
    haskell_root: PathBuf,
    layer: SourceLayer,
    /// Resolves a launch worktree id to the checkout on disk. Absent in the
    /// unit tests below, which exercise the run's own layer only.
    worktrees: Option<WorktreeManager>,
    /// One layer per checkout, by worktree id, materialized on first use.
    /// `None` records a checkout that carries no source of its own, so the
    /// answer is not recomputed for every actor that holds it.
    checkouts: Mutex<HashMap<String, Option<CheckoutSource>>>,
    /// What each actor's own source calls reach.
    scopes: RwLock<HashMap<PrincipalId, ActorSourceScope>>,
    /// One reload at a time: a publication's check and its `rename(2)` must
    /// not interleave with another actor's.
    gate: Mutex<()>,
    /// The last drift read per layer, and the signature of the disk it was
    /// read from. Drift is read on a timer for every actor; while neither the
    /// disk nor the active revision has moved, the answer has not either.
    drift_seen: Mutex<HashMap<String, (String, String, exomonad_actor::SourceLayerDrift)>>,
}

impl ExomonadSourceReload {
    pub(crate) fn new(
        frozen: FrozenWorkspace,
        workspace: PathBuf,
        run_root: PathBuf,
        haskell_root: PathBuf,
    ) -> Self {
        let layer = SourceLayer::new(&run_root);
        Self {
            frozen,
            workspace,
            run_root,
            haskell_root,
            layer,
            worktrees: None,
            checkouts: Mutex::new(HashMap::new()),
            scopes: RwLock::new(HashMap::new()),
            gate: Mutex::new(()),
            drift_seen: Mutex::new(HashMap::new()),
        }
    }

    /// Resolve launch worktrees through this manager, which is what makes
    /// per-checkout layers possible at all.
    #[must_use]
    pub(crate) fn with_worktrees(mut self, worktrees: WorktreeManager) -> Self {
        self.worktrees = Some(worktrees);
        self
    }

    /// Name the actor that owns the run's own layer. Exactly one actor does,
    /// and the host says which while admitting it.
    pub(crate) fn bind_run(&self, actor: PrincipalId) {
        self.scopes.write().insert(actor, ActorSourceScope::Run);
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

    /// The layer belonging to the checkout an actor is launched with, made
    /// live on first use. `None` when the actor holds no checkout, when the
    /// checkout is not a managed worktree, or when it carries no `.exomonad`
    /// source of its own — in every one of those cases the actor simply
    /// compiles against what the run provides.
    fn checkout(&self, worktrees: &[String]) -> Option<CheckoutSource> {
        let manager = self.worktrees.as_ref()?;
        let [id] = worktrees else { return None };
        let mut known = self.checkouts.lock();
        if let Some(checkout) = known.get(id) {
            return checkout.clone();
        }
        // Only an answer is remembered. A lookup that failed may succeed once
        // the worktree has finished being made, and remembering the failure
        // would leave every actor on this checkout read-only for the whole run.
        match self.materialize_checkout(manager, id) {
            Ok(resolved) => {
                known.insert(id.clone(), resolved.clone());
                resolved
            }
            Err(error) => {
                tracing::warn!(worktree = %id, %error, "checkout source layer unavailable");
                None
            }
        }
    }

    fn materialize_checkout(
        &self,
        manager: &WorktreeManager,
        id: &str,
    ) -> Result<Option<CheckoutSource>> {
        let Some(handle) = manager.lookup(&WorktreeId::from_raw(id))? else {
            return Ok(None);
        };
        let config = self.frozen.config()?;
        let roots = super::workspace::checkout_source_roots(handle.cwd(), &config.haskell);
        if roots.is_empty() {
            return Ok(None);
        }
        let layer = SourceLayer::checkout(&self.run_root, id);
        // Revision one before the actor exists, so its very first cell already
        // resolves `active/<index>` and the include list is well-formed.
        layer.ensure_active_from(self.frozen.identity(), &roots)?;
        Ok(Some(CheckoutSource {
            layer,
            workspace: handle.cwd().to_path_buf(),
            roots: roots.into(),
        }))
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
    ) -> std::result::Result<tidepool_bridge_effects::SrReloadOutcome, tidepool_handlers::SourceError>
    {
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
        )
    }

    /// Reload one checkout's own layer. Same transaction, one checkout's
    /// source, and the run's layer stays exactly where it is: the candidate
    /// goes in FRONT of it for the check, never in place of it.
    fn reload_checkout(
        &self,
        checkout: &CheckoutSource,
        also_check: &[String],
        intent: Option<&str>,
    ) -> std::result::Result<tidepool_bridge_effects::SrReloadOutcome, tidepool_handlers::SourceError>
    {
        let active = checkout
            .layer
            .ensure_active_from(self.frozen.identity(), &checkout.roots)
            .map_err(unreadable)?;
        let pending = checkout
            .layer
            .capture_from_roots(self.frozen.identity(), &checkout.roots)
            .map_err(unreadable)?;
        let candidate = |pending: &PendingRevision| pending.include_paths(checkout.roots.len());
        self.settle(
            &checkout.layer,
            active,
            pending,
            &candidate,
            &checkout.workspace,
            false,
            also_check,
            intent,
        )
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
    ) -> std::result::Result<tidepool_bridge_effects::SrReloadOutcome, tidepool_handlers::SourceError>
    {
        use tidepool_bridge_effects::SrReloadOutcome;
        if pending.revision().identity == active.identity {
            return Ok(SrReloadOutcome::ReloadUnchanged(Self::wire(&active)));
        }
        let active_modules: std::collections::BTreeSet<&str> = active
            .modules
            .iter()
            .map(|(module, _)| module.as_str())
            .collect();
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
            return Ok(SrReloadOutcome::ReloadRejected(
                Self::wire(&active),
                Self::wire(pending.revision()),
                format!(
                    "new module(s) {} cannot be imported in this running session; add them to [haskell].modules and restart",
                    new_modules.join(", ")
                ),
            ));
        }
        if let Err(error) = crate::actor_host::typecheck_candidate_revision(
            &self.frozen,
            &self.run_root,
            &self.haskell_root,
            &candidate(&pending),
            replaces_run_layer,
            also_check,
        ) {
            // Nothing moved: the active symlink still points where it did, and
            // the edited files are exactly as the caller wrote them.
            return Ok(SrReloadOutcome::ReloadRejected(
                Self::wire(&active),
                Self::wire(pending.revision()),
                error.to_string(),
            ));
        }
        let changed = layer.changed_modules(&active, pending.revision());
        let capture_directory = pending.directory.clone();
        let source_roots = pending.source_roots.clone();
        let published = layer.publish(pending).map_err(unreadable)?;
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
            ActorSourceScope::Run | ActorSourceScope::RunReadOnly => {
                let config = self.frozen.config().map_err(unreadable)?;
                let roots =
                    super::workspace::resolve_source_roots(&self.workspace, &config.haskell)
                        .map_err(unreadable)?;
                ("run".to_owned(), roots)
            }
            // A checkout's roots are its own directories, so they name it.
            ActorSourceScope::Checkout(checkout) => (
                checkout
                    .roots
                    .iter()
                    .map(|root| root.display().to_string())
                    .collect::<Vec<_>>()
                    .join(":"),
                checkout.roots.to_vec(),
            ),
        };
        let signature = super::workspace::sources_signature(&roots).map_err(unreadable)?;
        let active_now = match &scope {
            ActorSourceScope::Run | ActorSourceScope::RunReadOnly => {
                self.layer.ensure_active(&self.frozen).map_err(unreadable)?
            }
            ActorSourceScope::Checkout(checkout) => checkout
                .layer
                .ensure_active_from(self.frozen.identity(), &checkout.roots)
                .map_err(unreadable)?,
        };
        if let Some((seen_signature, seen_active, drift)) = self.drift_seen.lock().get(&layer_key) {
            if *seen_signature == signature && *seen_active == active_now.identity {
                return Ok(drift.clone());
            }
        }
        let (active, disk) = match scope {
            ActorSourceScope::Run | ActorSourceScope::RunReadOnly => {
                let active = self.layer.ensure_active(&self.frozen).map_err(unreadable)?;
                let disk = self
                    .layer
                    .observe_from_workspace(&self.frozen, &self.workspace)
                    .map_err(unreadable)?;
                (active, disk)
            }
            ActorSourceScope::Checkout(checkout) => {
                let active = checkout
                    .layer
                    .ensure_active_from(self.frozen.identity(), &checkout.roots)
                    .map_err(unreadable)?;
                let disk = checkout
                    .layer
                    .observe_from_roots(self.frozen.identity(), &checkout.roots)
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
        drift = workspace_source_drift(&workspace, &scopes, &source)?;

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
            if source_scopes_contain(&scopes, path) && is_haskell_source(path) {
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

fn is_haskell_source(path: &str) -> bool {
    matches!(
        Path::new(path)
            .extension()
            .and_then(|extension| extension.to_str()),
        Some("hs" | "lhs" | "hs-boot" | "h")
    )
}

fn workspace_source_drift(
    workspace: &Path,
    scopes: &[(usize, PathBuf, PathBuf, PathBuf)],
    captured: &BTreeMap<String, PathBuf>,
) -> Result<Vec<String>> {
    let mut live = BTreeMap::new();
    fn walk(
        root: &Path,
        current: &Path,
        prefix: &Path,
        live: &mut BTreeMap<String, PathBuf>,
    ) -> Result<()> {
        for entry in current.read_dir()? {
            let entry = entry?;
            let path = entry.path();
            let kind = entry.file_type()?;
            if kind.is_dir() {
                if entry.file_name() != ".git" {
                    walk(root, &path, prefix, live)?;
                }
            } else if kind.is_file() {
                let relative = prefix.join(path.strip_prefix(root)?);
                let name = relative
                    .to_str()
                    .ok_or("workspace source path is not valid UTF-8")?
                    .replace(std::path::MAIN_SEPARATOR, "/");
                if is_haskell_source(&name) {
                    live.insert(name, path);
                }
            }
        }
        Ok(())
    }
    for (_, _, _, prefix) in scopes {
        let root = workspace.join(prefix);
        if root.is_dir() {
            walk(&root, &root, prefix, &mut live)?;
        }
    }
    let paths: std::collections::BTreeSet<_> =
        captured.keys().chain(live.keys()).cloned().collect();
    let mut drift = Vec::new();
    for path in paths {
        let same = match (captured.get(&path), live.get(&path)) {
            (Some(snapshot), Some(current)) => std::fs::read(snapshot)? == std::fs::read(current)?,
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
            ActorSourceScope::Run => self.reload_run(also_check, intent),
            ActorSourceScope::Checkout(checkout) => {
                self.reload_checkout(&checkout, also_check, intent)
            }
            ActorSourceScope::RunReadOnly => {
                Err(tidepool_handlers::SourceError::SourceUnavailable(
                    "this actor has no source layer of its own: it compiles against the run's, \
                     which only the actor that owns the run republishes"
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
            ActorSourceScope::Run | ActorSourceScope::RunReadOnly => {
                let active = self.layer.ensure_active(&self.frozen).map_err(unreadable)?;
                let disk = self
                    .layer
                    .observe_from_workspace(&self.frozen, &self.workspace)
                    .map_err(unreadable)?;
                Ok(self.status_of(active, &disk))
            }
            ActorSourceScope::Checkout(checkout) => {
                let active = checkout
                    .layer
                    .ensure_active_from(self.frozen.identity(), &checkout.roots)
                    .map_err(unreadable)?;
                let disk = checkout
                    .layer
                    .observe_from_roots(self.frozen.identity(), &checkout.roots)
                    .map_err(unreadable)?;
                Ok(self.status_of(active, &disk))
            }
        }
    }
}

impl exomonad_actor::ActorSourceLayers for ExomonadSourceReload {
    fn layer_include(&self, worktrees: &[String]) -> Vec<PathBuf> {
        self.checkout(worktrees)
            .map(|checkout| {
                checkout
                    .layer
                    .active_include_paths()
                    .unwrap_or_else(|error| {
                        tracing::warn!(%error, "checkout source layer has no include roots");
                        Vec::new()
                    })
            })
            .unwrap_or_default()
    }

    fn bind(&self, actor: PrincipalId, worktrees: &[String]) {
        let scope = match self.checkout(worktrees) {
            Some(checkout) => ActorSourceScope::Checkout(checkout),
            None => ActorSourceScope::RunReadOnly,
        };
        self.scopes.write().insert(actor, scope);
    }

    /// Publish the caller's own layer, through exactly the path the `Source`
    /// effect takes.
    ///
    /// Same `scope` resolution, same gate, same transaction. An actor asking
    /// for its spec to be reloaded therefore reaches the one layer the host
    /// bound to it and no other, and a coding actor still cannot republish the
    /// swarm's source graph — not because this is checked, but because the
    /// only layer it can name is its own.
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

/// Every module `roots` provide, by module name, first root wins — exactly
/// the shadowing GHC applies across the same include roots in the same
/// order. Shared by a captured revision directory (whose roots are
/// `directory/0`, `directory/1`, …, via [`revision_modules`]) and a live,
/// uncaptured root list alike (e.g. [`ExomonadSourceReload::frozen_drift`]).
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn source_observation_discards_scratch_and_retention_keeps_the_revision() {
        let run = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("A.hs"), "module A where").unwrap();
        let layer = SourceLayer::new(run.path());
        let roots = [root.path().to_path_buf()];
        let observed = layer.observe_from_roots("test", &roots).unwrap();
        assert_eq!(std::fs::read_dir(layer.revisions()).unwrap().count(), 0);
        let retained = layer.capture_from_roots("test", &roots).unwrap();
        assert_eq!(retained.revision(), &observed);
        assert!(retained.directory.join("0/A.hs").is_file());
        let observed_again = layer.observe_from_roots("test", &roots).unwrap();
        assert_eq!(observed_again, observed);
        assert_eq!(std::fs::read_dir(layer.revisions()).unwrap().count(), 1);
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

    /// A checkout's layer is the run's layer's equal in every way but two:
    /// where it lives, and who sees it. Materializing one leaves the run's
    /// exactly where it was, and a checkout with no authored source of its own
    /// has no roots to capture and so gets no layer at all.
    #[test]
    fn a_checkout_layer_is_its_own_and_leaves_the_run_alone() {
        let (project, run) = workspace_with("module Project.Work where\nwork :: Int\nwork = 1\n");
        let frozen = FrozenWorkspace::load(project.path(), run.path()).unwrap();
        let run_layer = SourceLayer::new(run.path());
        let published = run_layer.ensure_active(&frozen).unwrap();

        // A checkout of the same project, carrying different source.
        let checkout = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(checkout.path().join(".exomonad/Project")).unwrap();
        std::fs::write(
            checkout.path().join(".exomonad/Project/Work.hs"),
            "module Project.Work where\nwork :: Int\nwork = 2\n",
        )
        .unwrap();
        let haskell = frozen.config().unwrap().haskell;
        let roots = super::super::workspace::checkout_source_roots(checkout.path(), &haskell);
        let layer = SourceLayer::checkout(run.path(), "tree-7");
        let first = layer.ensure_active_from(frozen.identity(), &roots).unwrap();

        assert_ne!(first.identity, published.identity);
        assert!(run
            .path()
            .join("workspace/checkouts/tree-7/active")
            .exists());
        assert_eq!(run_layer.read_active().unwrap().unwrap(), published);

        // Its include roots are its own, read from what it actually captured
        // rather than from the run's root count.
        let include = layer.active_include_paths().unwrap();
        assert_eq!(include.len(), roots.len() + 1);
        assert!(std::fs::read_to_string(include[0].join("Project/Work.hs"))
            .unwrap()
            .contains("work = 2"));

        // A checkout with no authored package contributes nothing, which is
        // how an ordinary coding worktree ends up with no layer.
        let bare = tempfile::tempdir().unwrap();
        assert!(super::super::workspace::checkout_source_roots(bare.path(), &haskell).is_empty());
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

    fn write_work(project: &Path, accessor: &str) {
        std::fs::write(
            project.join(".exomonad/workspace/Project/Work.hs"),
            format!(
                "module Project.Work (describe) where\n\
                 \n\
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
        git.try_run(path, &["init", "-q"]).unwrap();
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
    fn a_checkout_reload_commits_its_captured_workspace_and_rejects_a_broken_dependent() {
        let (project, run, reload) = cooperating_pair();
        let child_root = tempfile::tempdir().unwrap();
        let child = child_root.path().join("child");
        let git = GitCli::new();
        git.try_run(
            project.path(),
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "child",
                child.to_str().unwrap(),
                "HEAD",
            ],
        )
        .unwrap();
        let child_workspace = child.join(".exomonad/workspace");
        git.try_run(
            &child,
            &[
                "clone",
                "-q",
                project.path().join(".exomonad/workspace").to_str().unwrap(),
                ".exomonad/workspace",
            ],
        )
        .unwrap();
        git.try_run(&child_workspace, &["config", "user.name", "Reload test"])
            .unwrap();
        git.try_run(
            &child_workspace,
            &["config", "user.email", "reload-test@example.invalid"],
        )
        .unwrap();
        let root_head_before = git.try_run(project.path(), &["rev-parse", "HEAD"]).unwrap();
        let root_status_before = git
            .try_run(
                project.path(),
                &["status", "--porcelain=v1", "--untracked-files=all"],
            )
            .unwrap();
        let root_workspace_head_before = git
            .try_run(
                &project.path().join(".exomonad/workspace"),
                &["rev-parse", "HEAD"],
            )
            .unwrap();
        let root_source_before =
            std::fs::read_to_string(project.path().join(".exomonad/workspace/Project/Types.hs"))
                .unwrap();
        let roots = super::super::workspace::checkout_source_roots(
            &child,
            &reload.frozen.config().unwrap().haskell,
        );
        let layer = SourceLayer::checkout(run.path(), "child");
        let before = layer
            .ensure_active_from(reload.frozen.identity(), &roots)
            .unwrap();
        let run_before = reload.layer.ensure_active(&reload.frozen).unwrap();
        let checkout = CheckoutSource {
            layer: layer.clone(),
            workspace: child.clone(),
            roots: roots.into(),
        };

        write_types(&child, "evidenceAmount");
        write_work(&child, "evidenceAmount");
        let outcome = reload.reload_checkout(&checkout, &[], None).unwrap();
        let tidepool_bridge_effects::SrReloadOutcome::ReloadPublished(
            previous,
            published,
            changed,
            workspace,
        ) = outcome
        else {
            panic!("a consistent child pair must publish: {outcome:?}");
        };
        assert_eq!(previous.identity, before.identity);
        assert_ne!(published.identity, before.identity);
        assert_eq!(changed, vec!["Project.Types", "Project.Work"]);
        let tidepool_bridge_effects::SrWorkspaceCommitOutcome::WorkspaceCommitted(commit, drift) =
            workspace
        else {
            panic!("child workspace must be committed: {workspace:?}");
        };
        assert!(drift.is_empty());
        assert_eq!(
            git.try_run(&child_workspace, &["rev-parse", "HEAD"])
                .unwrap()
                .trimmed(),
            commit
        );
        assert!(
            git.try_run(
                &child_workspace,
                &["show", &format!("{commit}:Project/Types.hs")]
            )
            .unwrap()
            .stdout
            .contains("evidenceAmount"),
            "the commit must contain the captured checkout bytes"
        );
        assert!(
            git.try_run(
                &child,
                &["ls-files", "--stage", "--", ".exomonad/workspace"]
            )
            .unwrap()
            .stdout
            .contains(&commit),
            "the child index must stage the new gitlink"
        );
        assert_eq!(reload.layer.read_active().unwrap().unwrap(), run_before);

        write_types(&child, "brokenAccessor");
        let child_head_before = git
            .try_run(&child_workspace, &["rev-parse", "HEAD"])
            .unwrap();
        let child_index_before = git
            .try_run(
                &child,
                &["ls-files", "--stage", "--", ".exomonad/workspace"],
            )
            .unwrap();
        let active_before = layer.read_active().unwrap().unwrap();
        let outcome = reload.reload_checkout(&checkout, &[], None).unwrap();
        let tidepool_bridge_effects::SrReloadOutcome::ReloadRejected(active, _, diagnostics) =
            outcome
        else {
            panic!("a broken child dependent must reject: {outcome:?}");
        };
        assert!(diagnostics.contains("evidenceAmount"), "{diagnostics}");
        assert_eq!(active.identity, active_before.identity);
        assert_eq!(layer.read_active().unwrap().unwrap(), active_before);
        assert_eq!(
            git.try_run(&child_workspace, &["rev-parse", "HEAD"])
                .unwrap(),
            child_head_before
        );
        assert_eq!(
            git.try_run(
                &child,
                &["ls-files", "--stage", "--", ".exomonad/workspace"]
            )
            .unwrap(),
            child_index_before
        );
        assert_eq!(reload.layer.read_active().unwrap().unwrap(), run_before);
        assert_eq!(
            git.try_run(project.path(), &["rev-parse", "HEAD"]).unwrap(),
            root_head_before
        );
        assert_eq!(
            git.try_run(
                project.path(),
                &["status", "--porcelain=v1", "--untracked-files=all"]
            )
            .unwrap(),
            root_status_before
        );
        assert_eq!(
            git.try_run(
                &project.path().join(".exomonad/workspace"),
                &["rev-parse", "HEAD"]
            )
            .unwrap(),
            root_workspace_head_before
        );
        assert_eq!(
            std::fs::read_to_string(project.path().join(".exomonad/workspace/Project/Types.hs"))
                .unwrap(),
            root_source_before
        );
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
