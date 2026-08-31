//! The effect-row / answer-contract definitions: the outer loop's own
//! decl row (`outer_decls`/`outer_template`), the
//! answerer's scoped decl row (`typed_request_agent_decls`, widened by
//! `typed_request_agent_decls_with_delegate`), and per-hole
//! [`crate::harness::AnswerContract`] construction.

use super::SelfHarnessDriver;
use crate::engine::{self};
use crate::harness::AnswerContract;

/// The outer Harness-monad's OWN decl list — `Eff '[RunLLMTurn, AskUser]`,
/// distinct from the nested Agent's full stack (`crate::engine`'s private
/// `agent_decls`). `RunLLMTurn` is the loop's model-spawning verb (no BASE
/// effects on the outer row in v1); `AskUser` is
/// added so an AUTHORED `loop` can present a typed operator form directly —
/// `Tidepool.Form`'s `askUser` is auto-imported into every outer compile
/// whenever `AskUser` is in this decl list (see
/// `tidepool_mcp::pragmas_and_imports`), and the driver services the resulting
/// suspension via [`SelfHarnessDriver::service_outer_askuser_hole`]. This does
/// NOT add `AskUser` to `Tidepool.Harness`/`HarnessEff` (whose row stays
/// `'[RunLLMTurn]`, now stale-but-unused): the reference `loop :: State ->
/// Harness State` uses only `runLLMTurn`, and a loop that wants a form imports
/// `Tidepool.Form` and relies on `askUser`'s `Member AskUser` constraint
/// unifying against this wider row.
/// The outer resident session uses `EffectRunPolicy::SuspendAll`, so
/// declaration order has no Rust dispatch meaning. Every request reaches the
/// driver as a suspension.
///
/// `worktree_decl` is a HARD companion of `subagent_decl`: Subagent's
/// type_defs reference `WorktreeSpec`/`WorktreeId`/`WorktreeHandle`, and its
/// `renderSpawnError` helper calls Worktree's `renderWorktreeError` — helper
/// emission is row-membership-gated, so Worktree must be IN the row, not just
/// in vocab.
///
/// This row also carries `Console`/`RepoEvent`/`Exec`: an authored `loop` can `say`,
/// drive managed worktrees AND observe their repository events, and run
/// shell commands — every one of them suspension-serviced by
/// [`SelfHarnessDriver::service_outer_effect`], exactly like `Subagent`.
///
/// The row also carries `Journal`: an authored `loop` can `record` a durable progress step,
/// serviced the same suspension way through
/// [`SelfHarnessDriver::service_outer_effect`]/[`SelfHarnessDriver::set_journal_handler`].
/// `Journal` is deliberately absent from `tidepool-handlers`'
/// `base_effects!`/`handler_for!` row (opt-in, like `Worktree`/`RepoEvent`/
/// `Subagent`) — which journal file a run appends to, and folding it at
/// boot, is a driver/binary wiring concern, not a base-stack default.
///
/// The READ half is wired: [`SelfHarnessDriver::open_run_journal`]
/// loads and folds a run's journal at boot and the first cycle after boot
/// injects it through the harness's `resumeLoop`. `record` is untouched by
/// that and stays WRITE-ONLY on the authored surface — nothing in this row
/// reads a journal.
///
/// `Journal`'s membership here also carries `Tidepool.Resume` onto every outer
/// compile's import list (its `EffectDecl::extra_imports`), which is what puts
/// `Resume.ResumeFold` in scope for the `__selfHarnessResume` splice.
///
/// The row also carries `Green`. Under the
/// registry representation (threads park as new continuations in the
/// session's multi-hole registry), `Green` is NOT
/// serviced through [`SelfHarnessDriver::service_outer_effect`]'s mechanical
/// decode-dispatch-convert shape the way `Console`/`Worktree`/`RepoEvent`/
/// `Exec`/`Journal` are: an `async` suspension needs the spawned thread
/// body's rooted-value custody taken off the spawner's parked frame and a NEW
/// suspension-capable top-level run started under its own realm — driver
/// machinery in the `RunLLMTurn`/`AskUser` class ([`engine::classify_hole`]/
/// [`SuspensionRouting`]), not the `OuterEffectKind`/`dispatch_outer_effect` class.
/// [`SelfHarnessDriver::service_green_hole`] is that servicing: a driver-
/// owned thread table + waiter map + FIFO ready queue, scoped to one
/// `run_loop_fragment_inner` call (structured concurrency — nothing survives
/// past the `loop` fragment that spawned it).
pub(crate) fn outer_decls() -> Vec<tidepool_mcp::EffectDecl> {
    vec![
        tidepool_mcp::runllmturn_decl(),
        tidepool_mcp::askuser_decl(),
        tidepool_mcp::console_decl(),
        tidepool_mcp::worktree_decl(),
        tidepool_mcp::event_decl(),
        tidepool_mcp::exec_decl(),
        tidepool_mcp::subagent_decl(),
        tidepool_mcp::journal_decl(),
        tidepool_mcp::green_decl(),
    ]
}

/// The ONE template every outer-session fragment compile
/// ([`SelfHarnessDriver::compile_outer`] — the `render`/`loop` entries) goes
/// through: UNPAGINATED, for the same reason
/// [`engine::template_turn_for_fused`] states for the fused cycle entry.
/// Every outer entry's JSON is DRIVER-CONSUMED, never displayed — the loop
/// entry's output round-trips back in as the next cycle's `State`, and the
/// render entry feeds the framing/operator page whole.
///
/// The paginated template (`engine::template_turn_for`) wraps the result in
/// `paginateResult 4096`, whose oversized branch on a Console-bearing row
/// (which [`outer_decls`] is) calls `putStrLn` — a suspension. For the
/// POST-loop render that suspension hit `render_framing_with`'s purity
/// refusal the first time a real fold pushed the rendered framing past 4096
/// bytes, failing the cycle AFTER its turn had already completed — and,
/// because the checkpoint commits after that render, discarding the finished
/// turn: a deterministic crash loop redoing (and re-billing) the same turn
/// forever. Pinned by `outer_template_is_unpaginated` in this module's tests.
pub(crate) fn outer_template(stack: &str, code: &str, imports: &str, helpers: &str) -> String {
    engine::template_turn_for_fused(&outer_decls(), stack, code, imports, helpers, &[])
}

/// The nested answerer Agent's scoped decl row: `[AskUser, Fork, ReadState, Green, Finalize]`.
/// It declares no base effects (`Console`/`KV`/`Fs`/`Http`/`Exec`/`Git`/
/// `Time`/`Meta`) and no `RunLLMTurn`/`Ask`, so an answerer turn compiles
/// against a `Tidepool.Effects` that never defines those verbs — the answerer
/// structurally cannot run a shell command, read files, hit the network, or
/// suspend an in-context `runLLMTurn`. Its whole surface: `askUser` (present a
/// typed form to a human operator, riding `AskUser`), `fork`/`forkAll`
/// (spawn bounded, RECURSIVE sub-answerers, riding `Fork` — the driver
/// services the resulting suspension via [`Self::drive_fork_child_agent_session`],
/// which compiles a fork child against this SAME row, full pump included:
/// a child can `askUser`, `fork` again, and go multi-round, bounded only by
/// the spawn-time budgets in [`Self::check_fork_budgets`] (depth and
/// per-window/per-subtree fan-out), not by row shape), `Tidepool.Async` over
/// `Green` (green threads — the composed idiom `async (fork @T brief)` parks
/// a fork in a thread of its own, so several forks can be outstanding before
/// the first `wait`; serviced by the answerer-plane green scheduler in
/// [`SelfHarnessDriver::drive_agent_session_to_finalize`]), and `finalize` (the
/// answer path).
///
/// `Green` grants NO new external capability: a green thread's body can only
/// perform effects already in this row.
///
pub fn typed_request_agent_decls() -> Vec<tidepool_mcp::EffectDecl> {
    vec![
        tidepool_mcp::askuser_decl(),
        tidepool_mcp::fork_decl(),
        tidepool_mcp::readstate_decl(),
        tidepool_mcp::green_decl(),
        tidepool_mcp::finalize_decl(),
    ]
}

/// [`typed_request_agent_decls`] with `Subagent` and `Worktree` PREPENDED, in that
/// order — the row a recursive-companion branch-node window
/// compiles against when paired with
/// [`crate::engine::EngineConfig::with_delegate_wrap`]. Both reused
/// verbatim (no new Rust registry row); prepended, not appended, and in
/// THIS order, because `Tidepool.Agent.Delegate.runDelegate`'s own
/// signature (`Eff (Delegate ': effs) a -> Eff (Subagent ': Worktree ':
/// effs) a`, freer-simple `reinterpret2`) re-adds them at the HEAD of
/// whatever row it runs in, in that exact order — for that to line up with
/// `type M`, `Subagent` then `Worktree` must be `type M`'s own first two
/// entries.
///
/// `Worktree` rides in the ROW for real, not merely as vocabulary: `Subagent`'s
/// own auto-import (`extra_imports_for!(Subagent)`,
/// `tidepool-mcp/src/effect_defs.rs`) always pulls in
/// `Tidepool.Agent.Spawn`, which imports `Tidepool.Worktree
/// (renderWorktreeError)` — and `Tidepool.Worktree.hs` is a whole module GHC
/// must typecheck to import anything from it, including its own `M`-typed
/// bindings (`worktreeBranch`, `worktreeHead`), which need `Worktree`
/// genuinely present. `runDelegate`'s `reinterpret2` is what keeps this from
/// widening what the MODEL's own block can reach: freshly re-added effects
/// on a `reinterpret`/`reinterpret2` call's OUTPUT are never members of the
/// row its ARGUMENT (the model's block) is checked against — see
/// `Tidepool.Agent.Delegate`'s module doc.
///
/// Does NOT widen `typed_request_agent_decls()` itself — every other harness (dev-tree,
/// the general Agent stack) keeps compiling exactly as before.
pub fn typed_request_agent_decls_with_delegate() -> Vec<tidepool_mcp::EffectDecl> {
    let mut decls = vec![tidepool_mcp::subagent_decl(), tidepool_mcp::worktree_decl()];
    decls.extend(typed_request_agent_decls());
    decls
}

impl SelfHarnessDriver {
    /// The [`AnswerContract`] for a hole of type `ty`: pin `finalize` to it
    /// and import `modules` — the defining modules `asks.json` reported for
    /// `ty` ([`tidepool_runtime::YieldSites::modules_of`], resolved by
    /// extract at the call site from the real type environment) — so the
    /// type resolves to the SAME defining module the outer loop resolved,
    /// meaning the finalized value's constructor ids match at the crossing.
    /// This replaced a harness-import-scraping guess
    /// (`HarnessSource::answerer_imports`, since removed): the scrape only
    /// ever found types the harness AUTHOR imported into `loop`'s own
    /// module, so a type the MODEL declares in the session decl plane could
    /// never be named here even though it compiles everywhere else — the
    /// extract-side lookup has no such blind spot, because it runs over
    /// whichever module the type actually came from.
    ///
    /// `None` when the hole's type is unknown (no `asks.json` entry): there is
    /// nothing to pin `finalize` to, so the turn keeps the polymorphic verb.
    pub(crate) fn answer_contract(
        &self,
        ty: Option<&str>,
        modules: &[String],
    ) -> Option<AnswerContract> {
        Some(AnswerContract {
            ty: ty?.to_string(),
            imports: modules.to_vec(),
        })
    }
}
