//! Live source revisions for one Shoal run.
//!
//! A run freezes its Haskell source roots once, into
//! `<run_root>/workspace/sources/<capture>/<index>`, and that capture is
//! verified byte for byte every time the run is reloaded
//! ([`super::workspace::FrozenWorkspace::load`]). Nothing here writes inside
//! it. Instead this module owns a second layer in FRONT of that floor:
//!
//! ```text
//! <run_root>/workspace/revisions/<identity>/{0,1,…,resources}
//! <run_root>/workspace/active -> revisions/<identity>
//! ```
//!
//! Every compile in the run receives `active/<index>` ahead of the frozen
//! `sources/<capture>/<index>`, so a module in the active revision shadows the
//! frozen copy. Publishing a revision is one `rename(2)` of that symlink: a
//! compile that opens `active/0` sees either the whole previous revision or
//! the whole new one, never a mixture. The include VECTOR handed to the
//! compiler never changes for the life of the run — only what one path on it
//! resolves to.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::workspace::FrozenWorkspace;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Separates a source revision's content identity from every other identity
/// derived from the same per-file manifests.
const DOMAIN: &[u8] = b"tidepool-shoal-source-revision-v1";

/// The generated module each revision carries, so an authored module can
/// record the source snapshot it was compiled against.
const REVISION_MODULE: &str = "Shoal/Source/Revision.hs";

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

/// A captured revision that exists on disk but is not yet the active one.
pub(crate) struct PendingRevision {
    directory: PathBuf,
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

/// The source layer of one run.
#[derive(Clone, Debug)]
pub(crate) struct SourceLayer {
    directory: PathBuf,
}

impl SourceLayer {
    pub(crate) fn new(run_root: &Path) -> Self {
        Self {
            directory: run_root.join("workspace"),
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

    /// Materialize revision one from the run's own frozen capture, unless a
    /// revision is already active. Idempotent, and the only way `active` comes
    /// into existence: every compile in the run needs it to resolve.
    pub(crate) fn ensure_active(&self, frozen: &FrozenWorkspace) -> Result<SourceRevision> {
        if let Some(active) = self.read_active()? {
            return Ok(active);
        }
        let roots = frozen.captured_source_roots();
        let pending = self.capture_roots(frozen, roots)?;
        self.publish(pending)
    }

    /// The revision currently on the search path, or `None` before the first
    /// one is materialized.
    pub(crate) fn read_active(&self) -> Result<Option<SourceRevision>> {
        if !self.active_link().exists() {
            return Ok(None);
        }
        let record = match std::fs::read(self.active_record()) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let record: ActiveRecord = serde_json::from_slice(&record)?;
        let directory = self.revisions().join(&record.identity);
        Ok(Some(SourceRevision {
            modules: revision_modules(&directory, record.roots),
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
        self.capture_roots(frozen, &roots)
    }

    fn capture_roots(
        &self,
        frozen: &FrozenWorkspace,
        roots: &[PathBuf],
    ) -> Result<PendingRevision> {
        let pending = self
            .revisions()
            .join(format!(".pending-{}", uuid::Uuid::new_v4()));
        if pending.exists() {
            std::fs::remove_dir_all(&pending)?;
        }
        std::fs::create_dir_all(&pending)?;
        for (index, root) in roots.iter().enumerate() {
            let mut captured = BTreeMap::new();
            super::workspace::capture_sources(
                root,
                Path::new(&index.to_string()),
                &pending,
                &mut captured,
            )?;
        }

        // The identity covers the captured source only. The generated module
        // below carries that identity, so hashing it too would be circular —
        // the same ordering `freeze` uses for `Shoal/Workspace.hs`.
        let mut domain = DOMAIN.to_vec();
        domain.extend_from_slice(frozen.identity().as_bytes());
        let captured_roots: Vec<PathBuf> = (0..roots.len())
            .map(|index| pending.join(index.to_string()))
            .collect();
        let identity = tidepool_runtime::cache::source_roots_identity(&domain, &captured_roots);
        let modules = revision_modules(&pending, roots.len());

        std::fs::create_dir_all(pending.join("resources/Shoal/Source"))?;
        tidepool_atomic_write::write_durable(
            &pending.join("resources").join(REVISION_MODULE),
            revision_module(&identity).as_bytes(),
        )?;

        // A revision directory is named by its content, so an existing one
        // holds the same bytes; keep it and discard the fresh copy.
        let directory = self.revisions().join(&identity);
        if directory.exists() {
            std::fs::remove_dir_all(&pending)?;
        } else {
            std::fs::rename(&pending, &directory)?;
        }
        Ok(PendingRevision {
            directory,
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
        let roots = pending
            .directory
            .read_dir()?
            .filter_map(std::result::Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.parse::<usize>().is_ok())
            })
            .count();
        let staged = self
            .directory
            .join(format!(".active-{}", uuid::Uuid::new_v4()));
        std::os::unix::fs::symlink(
            Path::new("revisions").join(&pending.revision.identity),
            &staged,
        )?;
        std::fs::rename(&staged, self.active_link())?;
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

/// The run's answer to the `Source` effect.
///
/// It owns the three things a reload needs and nothing else: the run's frozen
/// workspace, where its authored source lives, and the source layer it
/// publishes into. The compile that decides whether a candidate is acceptable
/// is the run's ordinary driver compile, so a reload is checked by exactly the
/// compiler the run uses.
pub(crate) struct ShoalSourceReload {
    frozen: FrozenWorkspace,
    workspace: PathBuf,
    run_root: PathBuf,
    haskell_root: PathBuf,
    layer: SourceLayer,
    /// One reload at a time: step 5 and step 6 of a publication must not
    /// interleave with another actor's.
    gate: parking_lot::Mutex<()>,
}

impl ShoalSourceReload {
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
            gate: parking_lot::Mutex::new(()),
        }
    }

    fn wire(revision: &SourceRevision) -> tidepool_bridge_effects::SrRevision {
        tidepool_handlers::revision_to_wire(
            &revision.identity,
            revision.generation,
            &revision.modules,
        )
    }
}

fn unreadable(error: Box<dyn std::error::Error>) -> tidepool_handlers::SourceError {
    tidepool_handlers::SourceError::SourceUnreadable(error.to_string())
}

impl tidepool_handlers::SourceReloadService for ShoalSourceReload {
    fn reload(
        &self,
        also_check: &[String],
    ) -> std::result::Result<tidepool_bridge_effects::SrReloadOutcome, tidepool_handlers::SourceError>
    {
        use tidepool_bridge_effects::SrReloadOutcome;
        let _one_at_a_time = self.gate.lock();
        let active = self.layer.ensure_active(&self.frozen).map_err(unreadable)?;
        let pending = self
            .layer
            .capture_from_workspace(&self.frozen, &self.workspace)
            .map_err(unreadable)?;
        if pending.revision().identity == active.identity {
            return Ok(SrReloadOutcome::ReloadUnchanged(Self::wire(&active)));
        }
        let candidate = pending.include_paths(self.frozen.captured_source_roots().len());
        if let Err(error) = crate::actor_host::typecheck_candidate_revision(
            &self.frozen,
            &self.run_root,
            &self.haskell_root,
            &candidate,
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
        let changed = self.layer.changed_modules(&active, pending.revision());
        let published = self.layer.publish(pending).map_err(unreadable)?;
        Ok(SrReloadOutcome::ReloadPublished(
            Self::wire(&active),
            Self::wire(&published),
            changed,
        ))
    }

    fn status(
        &self,
    ) -> std::result::Result<tidepool_bridge_effects::SrStatus, tidepool_handlers::SourceError>
    {
        let _one_at_a_time = self.gate.lock();
        let active = self.layer.ensure_active(&self.frozen).map_err(unreadable)?;
        let disk = self
            .layer
            .capture_from_workspace(&self.frozen, &self.workspace)
            .map_err(unreadable)?;
        let disk = if disk.revision().identity == active.identity {
            active.clone()
        } else {
            disk.revision().clone()
        };
        Ok(tidepool_bridge_effects::SrStatus {
            active: Self::wire(&active),
            disk: Self::wire(&disk),
        })
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

/// Every module a captured revision provides, by module name, first root
/// wins — exactly the shadowing GHC applies across the same include roots in
/// the same order.
fn revision_modules(directory: &Path, roots: usize) -> Vec<(String, String)> {
    let mut modules: BTreeMap<String, String> = BTreeMap::new();
    for index in 0..roots {
        for (relative, digest) in
            tidepool_runtime::cache::source_root_manifest(&directory.join(index.to_string()))
        {
            let Some(module) = module_name(&relative) else {
                continue;
            };
            modules.entry(module).or_insert(digest);
        }
    }
    modules.into_iter().collect()
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
         -- per revision by the Shoal run; import it from an authored module to\n\
         -- record, honestly, which snapshot built that module's code.\n\
         module Shoal.Source.Revision (compiledSourceRevision) where\n\
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
        let authored = project.path().join(".shoal");
        std::fs::create_dir_all(authored.join("Project")).unwrap();
        std::fs::write(
            authored.join("config.toml"),
            "[defaults]\nmodel = 'gpt-5.6-sol'\n[haskell]\nsource_roots = ['.']\nmodules = ['Project.Work']\n",
        )
        .unwrap();
        std::fs::write(authored.join("Project/Work.hs"), source).unwrap();
        (project, run)
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
            project.path().join(".shoal/Project/Work.hs"),
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
            project.path().join(".shoal/Project/Work.hs"),
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
        // …and the run still loads, which is the tamper check passing.
        FrozenWorkspace::load(project.path(), run.path()).unwrap();
    }

    // ------------------------------------------------------------------
    // The reload transaction itself, checked by the run's own compiler.
    // ------------------------------------------------------------------

    /// A two-module workspace: `Project.Work` is the configured module and it
    /// reads `Project.Types`, so `Types` has a reverse dependency to rebuild.
    fn cooperating_pair() -> (tempfile::TempDir, tempfile::TempDir, ShoalSourceReload) {
        let project = tempfile::tempdir().unwrap();
        let run = tempfile::tempdir().unwrap();
        let authored = project.path().join(".shoal");
        std::fs::create_dir_all(authored.join("Project")).unwrap();
        std::fs::write(
            authored.join("config.toml"),
            "[defaults]\nmodel = 'gpt-5.6-sol'\n[haskell]\nsource_roots = ['.']\nmodules = ['Project.Work']\n",
        )
        .unwrap();
        write_types(project.path(), "evidenceValue");
        write_work(project.path(), "evidenceValue");
        let frozen = FrozenWorkspace::load(project.path(), run.path()).unwrap();
        let reload = ShoalSourceReload::new(
            frozen,
            project.path().to_path_buf(),
            run.path().to_path_buf(),
            crate::haskell_sources::ensure_shoal_haskell().unwrap(),
        );
        (project, run, reload)
    }

    fn write_types(project: &Path, accessor: &str) {
        std::fs::write(
            project.join(".shoal/Project/Types.hs"),
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
            project.join(".shoal/Project/Work.hs"),
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

    /// The compiled-artifact key an extract over these include roots would
    /// get. The include VECTOR is identical before and after a reload, so this
    /// is the honest question "would a later compile be served the previous
    /// artifact?".
    fn cache_key(include: &[PathBuf]) -> String {
        let input = PathBuf::from("Turn.hs");
        let argv = vec![std::ffi::OsString::from("Turn.hs")];
        tidepool_runtime::cache::invocation_key(&tidepool_runtime::cache::Invocation {
            source: "module Turn where",
            argv: &argv,
            input_path: &input,
            include,
            endpoint_identity: b"source-reload-test",
            stable_val: None,
        })
        .expect("an include-only invocation is cacheable")
        .to_string()
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
        let outcome = tidepool_handlers::SourceReloadService::reload(&reload, &[]).unwrap();
        let tidepool_bridge_effects::SrReloadOutcome::ReloadPublished(previous, published, changed) =
            outcome
        else {
            panic!("a consistent pair must publish: {outcome:?}");
        };
        assert_eq!(previous.identity, before.identity);
        assert_ne!(published.identity, before.identity);
        assert_eq!(published.generation, 2);
        assert_eq!(changed, vec!["Project.Types", "Project.Work"]);

        // Same include vector, different compiled-artifact key: a later
        // compile cannot be served the previous revision's artifact.
        assert_eq!(include, reload.layer.include_paths(1));
        assert_ne!(cache_key(&include), key_before);

        // And GHC agrees: this only compiles if BOTH new files were read.
        crate::actor_host::validate_workspace_program(&reload.frozen, run.path()).unwrap();
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
        let expected = reload
            .layer
            .capture_from_workspace(&reload.frozen, project.path())
            .unwrap()
            .revision()
            .identity
            .clone();
        let outcome = tidepool_handlers::SourceReloadService::reload(&reload, &[]).unwrap();
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
        assert!(
            std::fs::read_to_string(project.path().join(".shoal/Project/Types.hs"))
                .unwrap()
                .contains("evidenceAmount")
        );
        crate::actor_host::validate_workspace_program(&reload.frozen, run.path()).unwrap();
    }

    /// Reloading a workspace nobody edited republishes nothing — the identity
    /// is content, so there is nothing to publish.
    #[test]
    fn an_unedited_workspace_reloads_to_the_same_revision() {
        let (project, run, reload) = cooperating_pair();
        let active = reload.layer.ensure_active(&reload.frozen).unwrap();
        let outcome = tidepool_handlers::SourceReloadService::reload(&reload, &[]).unwrap();
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
        let status = tidepool_handlers::SourceReloadService::status(&reload).unwrap();
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
        let status = tidepool_handlers::SourceReloadService::status(&reload).unwrap();
        assert_ne!(status.active.identity, status.disk.identity);
        assert_eq!(work(&status.active), before);
        assert_ne!(work(&status.disk), before);
        assert_eq!(status.disk.generation, 0, "an unpublished snapshot");
        drop(run);
    }
}
