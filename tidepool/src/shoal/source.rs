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
        let pending = self.capture_roots(frozen, &roots)?;
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

    fn capture_roots(&self, frozen: &FrozenWorkspace, roots: &[PathBuf]) -> Result<PendingRevision> {
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
        let captured_roots: Vec<PathBuf> =
            (0..roots.len()).map(|index| pending.join(index.to_string())).collect();
        let identity =
            tidepool_runtime::cache::source_roots_identity(&domain, &captured_roots);
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
}
