use std::path::PathBuf;

use tidepool_agent::backend::OneCycleBackend;
use tidepool_agent::spawn::{CoupledSpawner, SpawnError as DomainSpawnError};
use tidepool_worktree::error::WorktreeError as DomainWorktreeError;
use tidepool_worktree::git::GitCli;
use tidepool_worktree::registry::WorktreeRegistry;
use tidepool_worktree::WorktreeManager;

use crate::effect_glue::JsonArg;
use tidepool_bridge_effects::{AgSpawnOutcome, AgSpawnSpec};

// ============================================================================
// Tag: Subagent (PRD 18 lane 1 — coupled agent+worktree spawn; deliberately
// NOT in the default base_effects! row, same opt-in status as Worktree /
// RepoEvent. A row containing Subagent must also contain Worktree — the
// generated types reference WorktreeSpec/WorktreeHandle/WorktreeError.)
// ============================================================================

// SubagentReq + DescribeEffect + EffectHandler dispatch + the wire SpawnError
// enum are generated from the single-source definition; only the handler
// struct and the per-verb method bodies below are hand-written.
tidepool_mcp::subagent_effect_def!(crate::effect_glue::effect_rust_projection);

/// Serves `SubagentSpawn`: the whole coupled-spawn saga behind ONE verb.
///
/// Owns the worktree substrate handles (via [`CoupledSpawner`] — whose
/// `BindingTable` holds the single-owner lifetime flock for its binding root)
/// and a [`OneCycleBackend`]. Production wires the codex adapter; every
/// committed test wires [`tidepool_agent::backend::mock::MockBackend`] — no
/// live-model turns in tests, ever (standing rule, Inanna 2026-08-09).
///
/// Not `Clone`, deliberately (RepoEventHandler precedent): it owns a boxed
/// backend and a flocked binding table, neither of which has a meaningful
/// second owner.
pub struct SubagentHandler {
    spawner: CoupledSpawner,
    backend: Box<dyn OneCycleBackend + Send>,
}

impl SubagentHandler {
    /// `registry_root`, `worktree_root`, and `binding_root` must live OUTSIDE
    /// `source_repository` — the never-dirty-the-source rule
    /// (`tidepool-worktree/CLAUDE.md`). Fallible: opening the registry and the
    /// binding table both are, and the binding table refuses a root another
    /// live table owns.
    pub fn new(
        registry_root: PathBuf,
        worktree_root: PathBuf,
        binding_root: PathBuf,
        source_repository: PathBuf,
        backend: Box<dyn OneCycleBackend + Send>,
    ) -> Result<Self, DomainWorktreeError> {
        let registry = WorktreeRegistry::open(&registry_root)?;
        let manager =
            WorktreeManager::new(GitCli::new(), registry, worktree_root, source_repository);
        Ok(Self {
            spawner: CoupledSpawner::open(manager, binding_root)?,
            backend,
        })
    }

    /// The spawner, for post-run assertions in tests (binding state, registry
    /// lookups) without reopening flocked state.
    pub fn spawner(&self) -> &CoupledSpawner {
        &self.spawner
    }

    // ------------------------------------------------------------------
    // Verb methods (errors-tagged: typed Result, no cx — the generated
    // dispatch arm wraps with cx.respond, Ok→Right / Err→Left).
    // ------------------------------------------------------------------

    /// One atomic coupled spawn + one cycle. Implemented by the `handler`
    /// dev per `plans/post-restart/agent-lanes/lane1-scaffold-plan.md`:
    /// wire→domain conversion of `spec`, `schema` rides through as the
    /// backend `output_schema`, then `spawner.spawn_one_cycle(backend, req)`,
    /// then domain→wire conversion of the outcome / error.
    fn subagent_spawn(
        &mut self,
        spec: AgSpawnSpec,
        schema: JsonArg,
    ) -> Result<AgSpawnOutcome, SpawnError> {
        let _ = (&mut self.spawner, &mut self.backend, spec, schema);
        let _: fn(DomainSpawnError) -> SpawnError = |_| {
            todo!("handler dev: total domain→wire SpawnError map (worktree.rs error_to_wire precedent)")
        };
        todo!("handler dev: implemented per lane1-scaffold-plan.md")
    }
}
