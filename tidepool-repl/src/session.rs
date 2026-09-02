//! The resident session: ONE live [`JitEffectMachine`] + the Lane-A decl
//! library + the value-plane [`BindingTable`], driven turn-by-turn via the
//! re-entry APIs. See `tidepool-repl/CLAUDE.md`'s "Internals: session
//! lifecycle" for the split between this module and `manager.rs`.
//!
//! `run_block` drives a `session_run` block by classifying each item and
//! reusing the per-item handlers: `run_def`, `run_eval`, `run_meta`.
//!
//! # Suspension is data, not a blocked thread
//!
//! An in-item `ask` STOWS: the continuation stays on the JIT machine, and the
//! whole `Session` — including the owned per-item tail ([`PendingTail`]) and
//! the block loop's state ([`BlockCursor`], stowed as [`SuspendedBlockCursor`])
//! — goes back to the manager slot as plain data (as ONE [`SuspendedTurn`],
//! never two independently-optional fields). Nothing blocks; a `Session`
//! outlives the turn that was running in it, and [`Session::resume_turn`]
//! re-enters the machine and calls the SAME `finish_*` the non-suspending path
//! would have.
//!
//! Only a SINGLE item can suspend: a decl batch never runs the machine, and a
//! `:command` never does either. That is why the cursor needs one pending-item
//! slot, not a stack.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tidepool_codegen::binding_table::{BindingEntry, BoundValue};
use tidepool_codegen::old_space::RootSlot;
use tidepool_codegen::scope::ScopeId;
use tidepool_codegen::suspension::{ResumeInput, Suspendable, SuspendableOutcome};
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_effect::pause::PauseGate;
use tidepool_eval::value::Value;
use tidepool_mcp::{
    first_sentence, input_binding_source, library_vocab, CapturedOutput, EffectDecl, EffectRoster,
};
use tidepool_repr::{
    BindingName, DataConTable, Generation, SessionId, SessionModule, SessionVarId,
};
use tidepool_runtime::session::{
    assemble_bind_module, classify_block, extract_ask_request, insert_preamble_imports,
    place_turn_stmt, subtract_import_list_names, BoundBinder, CompiledTurn, ExportItem,
    GateDispatcher, InspectionQuery, InspectionRequest, ModuleEnv, PersistentSession,
    SessionCompileView, SessionError, SessionLib, SourceImports, TemplateSelector,
    TurnClassification, TurnFailure, TurnKind, TurnRequest, TurnResult, TurnTemplate, ValueTier,
    WorkSequence,
};
use tidepool_runtime::{
    classify_compile, classify_session, value_to_json, CompileError, FailureClass, Phase,
};

use crate::command::{
    BlockItem, BlockItemResult, BlockValue, BoundComponent, DeclarationMetadata, ExprText,
    ItemKind, MetaCommand, ResponseShape, SessionCommand, TurnOutcome,
};

/// Default session nursery: 64 MiB (matches the eval runtime default).
pub const DEFAULT_NURSERY_SIZE: usize = 1 << 26;

/// The server's effect handler stack, type-erased so neither [`Session`] nor the
/// manager is generic over it (the same shape `tidepool-harness` uses for its
/// resident sessions).
pub type BoxedStack = Box<dyn DispatchEffect<CapturedOutput> + Send>;

/// Mints a FRESH handler stack per turn: one factory per session (so
/// `new_with_session_builder`'s per-session stack still holds), invoked once
/// per turn.
pub type StackFactory = Box<dyn Fn() -> BoxedStack + Send>;

/// A bridged `ask` request the caller must answer: the prompt and the optional
/// `AskWith` metadata (schema + friends) the server turns into a suspension
/// envelope.
pub struct AskRequest {
    pub prompt: String,
    pub meta: Option<serde_json::Value>,
}

/// What driving (or resuming) a turn produced: either the turn's finished
/// [`TurnOutcome`], or an `ask` suspension whose continuation is stowed on the
/// session's machine and whose tail/cursor are stowed on the [`Session`].
pub enum TurnStep {
    Completed(TurnOutcome),
    Suspended(AskRequest),
}

struct ReplCompileFailure {
    error: Box<CompileError>,
    source: String,
    user_lines: Option<(usize, usize)>,
}

/// REPL-local view of a compiled resident turn. Runtime owns the compiled
/// artifact; this frontend additionally needs the names installed by bind
/// turns.
struct ReplCompiledTurn {
    binders: Vec<BoundBinder>,
    compiled: CompiledTurn,
}

/// One ITEM's outcome — the same shape as [`TurnStep`], named separately
/// because a suspended item is resumed back INTO the block loop rather than
/// returned to the caller.
// `PendingTail` is already `#[allow(large_enum_variant)]`'d for carrying the
// turn's whole `DataConTable` in its largest variant; `Suspended` inherits
// that size and the same justification — this is a transient boundary value
// (immediately destructured by its one caller), never accumulated.
#[allow(clippy::large_enum_variant)]
enum ItemStep {
    Done(TurnOutcome),
    /// Carries the tail that must be stowed alongside the block cursor the
    /// caller assembles — see [`Session::drive_block`]/[`Session::resume_block`],
    /// the only two places a [`SuspendedTurn`] is built.
    Suspended(PendingTail, AskRequest),
}

/// What a re-entry feeds the stowed `ask`. Kept JSON-side because the bridge to
/// a Core [`Value`] needs the table the SUSPENDING run used, which only the
/// stowed [`PendingTail`] knows.
enum ResumeAnswer {
    Answer(serde_json::Value),
    Abort(String),
}

impl ResumeAnswer {
    /// Bridge into the machine's resume input against `table` — the table the
    /// suspending run was driven with, so the answer's constructors land in the
    /// same namespace the continuation expects.
    ///
    /// A bridge failure becomes an ABORT carrying the bridge error: the `ask`
    /// fails, the turn unwinds, and the continuation is consumed rather than
    /// left stowed with nobody able to answer it.
    fn into_input(self, table: &DataConTable) -> ResumeInput {
        match self {
            ResumeAnswer::Answer(json) => {
                use tidepool_bridge::ToCore;
                match json.to_value(table) {
                    Ok(v) => ResumeInput::Answer(v),
                    Err(e) => ResumeInput::Abort(format!("ask answer could not be bridged: {e}")),
                }
            }
            ResumeAnswer::Abort(reason) => ResumeInput::Abort(reason),
        }
    }
}

/// Static configuration for a resident session, assembled once at open.
#[derive(Clone)]
pub struct SessionConfig {
    pub id: SessionId,
    /// Root of the session include tree (`Tidepool/Session/Lib/G<g>.hs` live here).
    pub root: PathBuf,
    /// Base GHC include dirs (generated `Tidepool.Effects` dir + prelude/stdlib).
    pub base_include: Vec<PathBuf>,
    /// The single-sourced effect roster (ordered decls + the `Ask` suspend
    /// tag) for this server, built only via `EffectRoster::from_handlers`.
    pub roster: EffectRoster,
    /// The assembled eval preamble (from `tidepool_mcp::build_preamble`).
    pub preamble: String,
    /// The effect-stack type string (e.g. `'[Console, Ask]`).
    pub effect_stack: String,
    /// Import/pragma surface for the generated `Lib.G<g>` decl modules.
    pub module_env: ModuleEnv,
    /// Session nursery size in bytes.
    pub nursery_size: usize,
}

/// The resident session — the value plane + the decl plane + a generation.
/// Lives in the [`crate::manager::SessionManager`] slot between turns and is
/// MOVED into a turn's blocking task for its duration.
pub struct Session {
    cfg: SessionConfig,
    /// Mints this session's per-turn effect handler stack (wrapped in the shared
    /// [`GateDispatcher`] timeout checkpoint before each run).
    make_handlers: StackFactory,
    /// The shared persistent-session core: the resident [`JitEffectMachine`], the
    /// accumulated `DataConTable`, the [`SessionLib`] decl plane, the
    /// `BindingTable` value plane, and the value-binding generation. The
    /// turn-run primitives (bootstrap, add-fragment, run/bind + their resume
    /// siblings, table merge, decl accumulation) live in the core, shared with
    /// the harness's resident session.
    core: PersistentSession,
    /// Per-block `input` payload from the `session_run` request. Injected into
    /// the generated module so `input :: Aeson.Value` is in scope. CLONED (not
    /// taken) by every evaluated item so it is visible to all items in the block
    /// — INCLUDING items that run after an in-block `ask`/resume, which is why
    /// the block cursor carries a copy across a suspension. The server resets it
    /// at the start of each `session_run`.
    eval_input: Option<serde_json::Value>,
    /// The session's SUSPENDED turn, if any: the stowed per-item tail
    /// ([`PendingTail`]) paired with the `session_run` block loop state it
    /// suspended out of. `Some` exactly while the session is suspended at an
    /// `ask`; [`Self::reenter`] consumes it wholesale, so the tail and its
    /// cursor can never drift apart the way a two-field split would allow (a
    /// resume path could take one and forget the other).
    suspended: Option<SuspendedTurn>,
    /// Shared slot the server reads to abort a runaway turn at a JIT safepoint.
    /// `None` until the manager wires it via [`Session::set_cancel_slot`]; the
    /// session publishes the machine's [`CancelHandle`] into it the moment the
    /// machine bootstraps, so even a session's FIRST turn is cancellable.
    cancel_slot: Option<crate::manager::CancelSlot>,
    /// Subtrees elided from the last truncated result, indexed by stub id
    /// (`stub_0` ⇒ index 0) — fetched via `:stub <n>`, REPLACED each time a
    /// new truncating result lands. See [`crate::truncate`].
    last_stubs: Vec<serde_json::Value>,
    /// PURE binds routed into the decl plane (`x <- pure e` / `let x = e` → the
    /// decl `x = e`, which generalizes). They have no heap value — they resolve
    /// via the decl-module import — but they ARE part of the session
    /// environment, so this registry surfaces them in `:bindings`.
    /// alongside effectful (materialized) binds. Latest-wins per name; a
    /// cross-plane rebind removes the name from the other plane.
    pure_binds: std::collections::BTreeMap<String, PureBind>,
}

/// A pure bind that lives in the decl plane (GHCi-environment model). Its
/// source is owned by the declaration log and re-instantiated per use by GHC.
struct PureBind {
    type_display: String,
    gen: Generation,
}

/// One run item's outcome inside `run_block`: its position, classified kind, and
/// the [`TurnOutcome`] it produced. Named fields, not a positional tuple, so
/// index/kind/outcome can't be misread by position.
struct ItemRun {
    index: usize,
    kind: ItemKind,
    outcome: TurnOutcome,
}

// ---------------------------------------------------------------------------
// Run-path tails: owned data (borrowing nothing from `self`) a run path builds
// right before its machine call and hands to `finish_*` once the call
// returns — whether that's the same stack frame or, after a suspension, a
// later `session_resume`.
// ---------------------------------------------------------------------------

/// [`Session::run_bind`]'s tail: the bound name, the value-binding generation
/// it mints, the binder's identity/tier/type from the extract, and the source
/// text recorded as the binding's `defining_expr`.
struct BindTail {
    name: String,
    g: Generation,
    var_id: u64,
    tier: ValueTier,
    type_display: String,
    defining_expr: String,
}

/// [`Session::run_multi_bind`]'s tail: the value-binding generation and the
/// per-component binder metadata the extract returned, plus the source text
/// shared as every component's `defining_expr`.
struct MultiBindTail {
    g: Generation,
    binders: Vec<BoundBinder>,
    defining_expr: String,
}

/// [`Session::run_reference_fragment`]'s tail: the caller-resolved inner type
/// — the only state this path needs after the run to render its result.
struct ReferenceTail {
    inner_type: Option<String>,
}

/// [`Session::run_bare_expr`]'s tail: the value-binding generation and the
/// `it` binder's identity/tier/type from the extract, plus the source text
/// recorded as `it`'s `defining_expr`. `type_display` doubles as the type
/// reported alongside the rendered value (there is only one type here — the
/// bound `it`'s).
struct BareExprTail {
    g: Generation,
    var_id: u64,
    tier: ValueTier,
    type_display: String,
    defining_expr: String,
}

/// The tail of the ONE item currently suspended at an `ask`, stowed on the
/// session for the duration. The variant selects BOTH which machine resume
/// entry re-enters the continuation (the four materialization policies) and
/// which `finish_*` closes the item out — the same `finish_*` the
/// ran-to-completion path calls, so nothing about completion is duplicated
/// across the two arms.
enum PendingTail {
    /// [`Session::run_bind`] — resumes through the `Bind{forced}` policy.
    Bind(BindTail),
    /// [`Session::run_multi_bind`] — resumes through `Project{n_fields}`.
    MultiBind(MultiBindTail),
    /// [`Session::run_reference_fragment`] — resumes against the accumulated
    /// session table.
    Reference(ReferenceTail),
    /// [`Session::run_bare_expr`] — resumes through `Render{field0_forced}`.
    BareExpr(BareExprTail),
}

impl PendingTail {
    /// The [`DataConTable`] this tail's run was driven against. Everything that
    /// crosses the machine boundary for this item — the bridged `ask` request,
    /// the bridged answer, the completed value — is keyed on it, so a resume
    /// must use the same one the run did. Resident-session paths all use the
    /// accumulated session table.
    fn run_table<'a>(&'a self, session_table: &'a DataConTable) -> &'a DataConTable {
        session_table
    }

    /// The label a run failure on this path reports under.
    fn error_label(&self) -> &'static str {
        match self {
            PendingTail::Reference(_) | PendingTail::BareExpr(_) => "runtime error",
            PendingTail::Bind(_) => "bind runtime error",
            PendingTail::MultiBind(_) => "multi-bind runtime error",
        }
    }
}

/// A suspended turn: the stowed per-item tail plus the block loop state it
/// suspended out of. The ONLY thing [`Session::suspended`] ever holds, and the
/// ONLY thing [`Session::reenter`] ever consumes — a resume can't take the
/// tail without also taking the cursor (or vice versa), which is exactly the
/// bug the old separate `pending`/`cursor` fields allowed.
struct SuspendedTurn {
    tail: PendingTail,
    cursor: SuspendedBlockCursor,
}

/// The suspended item's position and classified kind, held so its result lands
/// at the right index with the right `kind` when the resume finishes it.
/// NON-optional: it exists only as part of a [`SuspendedBlockCursor`], which
/// itself exists only while a block is actually suspended on this item.
struct PendingItem {
    index: usize,
    kind: ItemKind,
}

/// A [`BlockCursor`] stowed mid-block, paired with the ONE item it suspended
/// on. Converts back to an ordinary running `BlockCursor` only by being
/// destructured during resume ([`Session::resume_block`]) — there is no path
/// that produces a cursor with a pending item the type doesn't know about.
struct SuspendedBlockCursor {
    cursor: BlockCursor,
    pending_item: PendingItem,
}

/// [`BlockCursor`]'s accumulated last-expression-value state, captured
/// together because they describe the SAME value: `type_display`/`truncated`
/// are metadata about `value`, and `result_pos` is where it lives in
/// `results`. Four independently-optional fields let the four drift apart
/// (e.g. a `truncated` hint surviving after `value` was cleared); one struct
/// makes that unrepresentable.
struct LastBlockValue {
    value: serde_json::Value,
    type_display: Option<String>,
    truncated: Option<String>,
    /// The `results` INDEX of the item that produced this value — recorded at
    /// the moment it is assigned, so the finish step strips fields from
    /// exactly that item, never an unrelated one re-derived by some other
    /// heuristic.
    result_pos: usize,
}

/// `run_block`'s loop state, made re-enterable: everything the item loop had on
/// the native stack when one of its items suspended.
///
/// The cursor is created per `session_run`, driven by
/// [`Session::drive_block`], and stowed on the session (wrapped in a
/// [`SuspendedBlockCursor`]) only while an item is suspended. It owns its
/// work sequence and verdicts (rather than borrowing the request's) precisely
/// because it must outlive the call that built it.
struct BlockCursor {
    /// The shared workbench cursor owns both the request and its committed
    /// prefix, so suspension cannot advance past an item whose result has not
    /// settled.
    sequence: WorkSequence<BlockItem, BlockItemResult>,
    /// This block's ONE batch classify verdict per item (`None` for
    /// `Decl`/`Meta`, or when the batch classify itself failed).
    verdicts: Vec<Option<TurnClassification>>,
    /// The block's `input` payload lane. Carried HERE (not merely left on the
    /// session) so the do-block invariant — `input` in scope for EVERY item,
    /// including items that run after an in-block `ask`/resume — is a property
    /// of the cursor rather than of nothing having disturbed the session
    /// meanwhile.
    eval_input: Option<serde_json::Value>,
    /// `verbose: true` ⇒ the full diagnostic response shape.
    verbose: bool,
    /// The most recent `TurnOutcome::Value`'s value + metadata, if any.
    last: Option<LastBlockValue>,
}

impl BlockCursor {
    fn new(
        items: Vec<BlockItem>,
        verdicts: Vec<Option<TurnClassification>>,
        eval_input: Option<serde_json::Value>,
        verbose: bool,
    ) -> BlockCursor {
        BlockCursor {
            sequence: WorkSequence::new(items),
            verdicts,
            eval_input,
            verbose,
            last: None,
        }
    }

    /// Record one finished item. Returns `true` when the block must STOP here
    /// (the stop-on-first-error contract).
    ///
    /// `ok` also reflects a meta command that reported an `error` in its payload
    /// (e.g. `:i` on a missing name) — a `Meta` outcome, not an `Error` variant,
    /// but still a failure for ok-scripting and the stop-on-first-error
    /// contract. (#319)
    fn absorb(&mut self, run: ItemRun) -> bool {
        let ItemRun {
            index,
            kind,
            outcome,
        } = run;
        let ok = !outcome.is_error()
            && !matches!(&outcome, TurnOutcome::Meta(v) if v.get("error").is_some());

        // Track the last value-producing expression result, and WHICH `results`
        // slot it will land in.
        if let TurnOutcome::Value {
            ref value,
            ref type_display,
            ref truncated,
        } = outcome
        {
            self.last = Some(LastBlockValue {
                value: value.clone(),
                type_display: type_display.clone(),
                truncated: truncated.clone(),
                result_pos: self.sequence.committed().len(),
            });
        }

        let committed_index = self.sequence.commit_next(BlockItemResult {
            index,
            kind,
            ok,
            result: slim_item_result(&outcome),
            result_full: outcome.render(),
        });
        debug_assert_eq!(committed_index, index);
        !ok
    }
}

/// Map a binder's [`ValueTier`] to the [`BoundValue`] wrapping its root slot —
/// the single source of truth for the tier → bound-value expansion.
fn bound_value(tier: ValueTier, slot: RootSlot) -> BoundValue {
    match tier {
        ValueTier::Tier0Data => BoundValue::Tier0Forced(slot),
        ValueTier::Tier1Closure => BoundValue::Tier1Closure(slot),
    }
}

/// The text of an item [`Session::run_block`]'s batch classify needs a verdict
/// for (`Decl`/`Auto`/`Stmt`), or `None` for a meta-command. Explicit
/// declarations still need the structured export items carried by the verdict.
fn block_item_text(item: &BlockItem) -> Option<&str> {
    match item {
        BlockItem::Auto(e) | BlockItem::Stmt(e) => Some(&e.0),
        BlockItem::Decl(d) => Some(&d.0),
        BlockItem::Meta(_) => None,
    }
}

impl Session {
    /// Open a fresh session rooted at `cfg.root`. `make_handlers` mints this
    /// session's per-turn effect handler stack.
    pub fn open(cfg: SessionConfig, make_handlers: StackFactory) -> std::io::Result<Session> {
        let lib = SessionLib::open(cfg.id, cfg.root.clone(), cfg.module_env.clone())
            .map_err(|e| std::io::Error::other(e.to_string()))?
            // Decl validation must resolve the same imports eval does (notably
            // the generated `Tidepool.Effects`), so feed it the base include.
            .with_validation_include(cfg.base_include.clone());
        let core = PersistentSession::new(Some(lib), cfg.nursery_size);
        Ok(Session {
            cfg,
            make_handlers,
            core,
            eval_input: None,
            suspended: None,
            cancel_slot: None,
            last_stubs: Vec::new(),
            pure_binds: std::collections::BTreeMap::new(),
        })
    }

    /// This session's id — the identity its include-tree root is named after
    /// (`session-<id>`), and how a caller distinguishes one session from the one
    /// that replaced it.
    pub fn id(&self) -> SessionId {
        self.cfg.id
    }

    /// Whether the session is currently parked at an in-turn `ask` — the
    /// `self.suspended.is_some()` read the kernel adapter
    /// (`crate::kernel_adapter`) needs from outside this module. Single-hole
    /// by construction: `Some` exactly while one item's tail/cursor pair is
    /// stowed, `None` otherwise.
    pub fn is_suspended(&self) -> bool {
        self.suspended.is_some()
    }

    /// The directory the session's `Val.G<g>.hi` ifaces are written to / read
    /// from. The same include root the Lane-A `Lib` modules live under, so a
    /// reference turn's `import Tidepool.Session.Val.G<g>` resolves from the
    /// injected HPT and the `.hi` path lines up with `writeSessionIface`.
    fn session_root(&self) -> &Path {
        &self.cfg.root
    }

    /// Exact source-side snapshot of the REPL's root lexical scope. The REPL
    /// always opens a declaration plane, so absence here is an internal
    /// construction error rather than an alternate session mode.
    fn compile_view(&self) -> SessionCompileView {
        match self.core.compile_view_in(ScopeId::ROOT) {
            Some(view) => view,
            None => unreachable!("REPL session opened without a declaration plane"),
        }
    }

    /// The GHC include path for a turn: the session's base includes (generated
    /// `Tidepool.Effects` + prelude/stdlib) plus the live `Lib.G<g>` dir. Borrows
    /// `&self`, so block-scope the result before any `&mut self` call (e.g.
    /// another mutable session operation) — same constraint the inlined copies
    /// had.
    fn turn_include(&self) -> Vec<&Path> {
        let mut include: Vec<&Path> = self.cfg.base_include.iter().map(PathBuf::as_path).collect();
        include.push(self.core.lib().include_dir());
        include
    }

    /// Module names of every live value binding — what a turn injects
    /// (`--inject-val`) AND imports so a session reference typechecks. Delegates
    /// to the shared core (the value plane lives there).
    fn live_val_modules(&self) -> Vec<String> {
        self.compile_view().injected_module_names()
    }

    /// The `imports` block a turn prepends: the current `Lib.G<g>` decl module
    /// (if any) plus the current `Val.G<g>` module of each live name (newest
    /// gen only — shadowed gens are injected but not imported).
    fn session_imports(&self) -> String {
        // A name that migrates decl→value is RETRACTED from the decl plane by
        // `bind_materialized` (via `SessionLib::retract`), so `current_module`
        // no longer exports it and there is no decl/value collision to hide here
        // — the value plane's `Val.G<g>` module is the sole provider. (One
        // mechanism: retraction at the source, not per-consumer hiding.)
        self.compile_view().turn_imports(&SourceImports::new())
    }

    /// [`Self::session_imports`] plus the quasi-quoter import when the turn's
    /// text splices one — the same per-request gating the oneshot eval does
    /// (`[fmt|]`/`[j|]`/… cost the quoter-module import only when used).
    fn turn_imports(&self, turn_text: &str) -> String {
        let mut imports = self.session_imports();
        if tidepool_mcp::uses_qq(turn_text) {
            if !imports.is_empty() {
                imports.push('\n');
            }
            imports.push_str("Tidepool.QQ (fmt, j, patch, uri, form)");
        }
        imports
    }

    /// Merge a turn's DataCons into the session-accumulated table (loud on a
    /// genuine `stableVarId` collision — gen-versioned names make that a real
    /// bug, not churn). Delegates to the shared core.
    fn merge_table(&mut self, table: &DataConTable) -> Result<(), String> {
        self.core.merge_table(table)
    }

    /// Bind `entry` on the value (materialized) plane, EVICTING any pure
    /// decl-plane binding of the same name in the same step — a name lives in
    /// AT MOST one plane, enforced HERE and in [`Self::bind_pure`] so a bind
    /// site can't smear a name across both by forgetting the paired removal.
    fn bind_materialized(&mut self, entry: BindingEntry) -> Result<(), SessionError> {
        // Durable retraction is the commit point. Do not touch the
        // frontend-only pure-bind view until the shared core confirms it.
        let receipt = self.core.bind_replacing_decl(entry)?;
        self.pure_binds.remove(&receipt.name);
        Ok(())
    }

    /// Register REPL-only metadata for a pure declaration after
    /// [`Self::define_scoped`] has atomically made that declaration its name's
    /// sole visible plane.
    fn bind_pure(&mut self, name: &str, pb: PureBind) {
        self.pure_binds.insert(name.to_string(), pb);
    }

    /// Run one turn to its first boundary: a finished [`TurnOutcome`], or an
    /// `ask` suspension whose continuation is stowed on the machine and whose
    /// tail/cursor are stowed on `self`. Errors are folded into
    /// [`TurnOutcome::Error`].
    ///
    /// `gate` is the turn's abort latch: [`GateDispatcher`] makes every effect
    /// dispatch a checkpoint, so a server-side `request_abort` unwinds the turn
    /// at the next effect. It does not decide whether an unhandled request
    /// suspends; the resident run policy does.
    pub fn run_turn(
        &mut self,
        cmd: &SessionCommand,
        gate: Arc<PauseGate>,
        captured: &CapturedOutput,
    ) -> TurnStep {
        // Clear any cancellation left from a prior timed-out turn so this turn
        // starts clean (no-op until the machine bootstraps).
        self.reset_cancel();
        self.heal_effects_module();
        let mut handlers = GateDispatcher::new((self.make_handlers)(), gate);
        match cmd {
            SessionCommand::Block { items, verbose } => {
                self.run_block(items, &mut handlers, captured, *verbose)
            }
        }
    }

    /// Re-enter the suspended turn with a (already schema-validated,
    /// canonicalized) JSON answer and drive it to its next boundary. The stowed
    /// tail decides which machine resume entry re-enters the continuation and
    /// which `finish_*` closes the item out; the stowed cursor, when there is
    /// one, continues the rest of the block.
    pub fn resume_turn(
        &mut self,
        answer: serde_json::Value,
        gate: Arc<PauseGate>,
        captured: &CapturedOutput,
    ) -> TurnStep {
        self.reenter(ResumeAnswer::Answer(answer), gate, captured)
    }

    /// Abort the suspended turn WITHOUT answering: the `ask` itself fails, the
    /// turn unwinds, and the session comes back usable with everything it had
    /// already accumulated intact. The reaper's reclaim of an abandoned
    /// suspension — the threadless equivalent of dropping the answer channel a
    /// parked worker was blocked on.
    pub fn abort_turn(
        &mut self,
        reason: String,
        gate: Arc<PauseGate>,
        captured: &CapturedOutput,
    ) -> TurnStep {
        self.reenter(ResumeAnswer::Abort(reason), gate, captured)
    }

    fn reenter(
        &mut self,
        answer: ResumeAnswer,
        gate: Arc<PauseGate>,
        captured: &CapturedOutput,
    ) -> TurnStep {
        self.reset_cancel();
        self.heal_effects_module();
        let mut handlers = GateDispatcher::new((self.make_handlers)(), gate);
        // Consuming `self.suspended` wholesale here — rather than taking the
        // tail and the cursor from two separate fields — is what makes a
        // resume unable to forget one half of the pair: there is no `Some`
        // tail without a cursor to resume it into, and no cursor without a
        // tail to feed it.
        let Some(SuspendedTurn { tail, cursor }) = self.suspended.take() else {
            return TurnStep::Completed(TurnOutcome::Error(
                "internal: no suspended turn to resume".into(),
            ));
        };
        self.resume_block(cursor, tail, answer, &mut handlers, captured)
    }

    /// Self-heal the generated Tidepool.Effects/Orchestrate staging dir before
    /// every turn (two `exists()` stats when healthy): an external
    /// `rm -rf ~/.cache/tidepool` mid-session would otherwise break every
    /// subsequent compile with "Could not find module Tidepool.Effects" until a
    /// server restart. Mirrors the oneshot eval server, which self-heals per
    /// eval the same way.
    fn heal_effects_module(&self) {
        if let Err(e) = tidepool_mcp::ensure_effects_module(self.cfg.roster.decls()) {
            tracing::warn!("effects-module self-heal failed: {e}");
        }
    }

    /// The declaration text of an item that is a top-level DECLARATION (and so
    /// batches into a decl run), or `None` for a bind/expression/meta (a
    /// singleton). A keyword decl is one lexically; an `Auto` item is one iff
    /// its precomputed verdict — GHC's parser, via the block's one batch
    /// [`classify_block`] spawn — says `Decl`. Classification failure stops the
    /// block before this function is reached, so an `Auto` item always has a
    /// verdict here.
    ///
    /// Returns the text (not a bool) so the segment scan can carry the decl
    /// sources as it walks, without re-matching items to recover them.
    fn decl_shaped_text<'a>(
        &self,
        item: &'a BlockItem,
        verdict: Option<&TurnClassification>,
    ) -> Option<&'a str> {
        match item {
            BlockItem::Decl(d) => Some(&d.0),
            BlockItem::Auto(e) if verdict.is_some_and(|v| v.kind == TurnKind::Decl) => Some(&e.0),
            BlockItem::Auto(_) | BlockItem::Stmt(_) | BlockItem::Meta(_) => None,
        }
    }

    /// `session_run`: run a list of classified [`BlockItem`]s in sequence,
    /// reusing `run_def`/`run_eval`/`run_meta` as the per-item handlers.
    ///
    /// Execution stops on the first error; the failing item is included in the
    /// `items` array with `ok = false`. An in-turn `ask` inside a `Stmt` or
    /// `Auto` item suspends the BLOCK: the item's tail and the loop's
    /// [`BlockCursor`] are stowed on the session, and `session_resume`
    /// re-enters at [`Self::resume_block`] to run the remaining items.
    ///
    /// `Auto` items dispatch straight from this batch's GHC classify verdict,
    /// never paying for a doomed `run_def` probe. A missing or malformed
    /// classification artifact stops the block; it cannot authorize a second,
    /// unrelated compile.
    fn run_block<H: DispatchEffect<CapturedOutput>>(
        &mut self,
        items: &[BlockItem],
        handlers: &mut H,
        captured: &CapturedOutput,
        verbose: bool,
    ) -> TurnStep {
        // Batch-classify every Auto/Stmt item in ONE extract spawn regardless
        // of block length; verdicts map back onto original indices. This typed
        // result is the sole authority for declaration-vs-expression routing.
        let verdict_indices: Vec<usize> = items
            .iter()
            .enumerate()
            .filter(|(_, it)| block_item_text(it).is_some())
            .map(|(i, _)| i)
            .collect();
        let mut verdicts: Vec<Option<TurnClassification>> = vec![None; items.len()];
        if !verdict_indices.is_empty() {
            #[allow(clippy::expect_used, reason = "filtered to Auto/Stmt above")]
            let texts: Vec<&str> = verdict_indices
                .iter()
                .map(|&i| block_item_text(&items[i]).expect("filtered to Auto/Stmt above"))
                .collect();
            match classify_block(&texts) {
                Ok(classified) => {
                    for (slot, v) in verdict_indices.into_iter().zip(classified) {
                        verdicts[slot] = Some(v);
                    }
                }
                Err(err) => {
                    return TurnStep::Completed(TurnOutcome::Error(compile_fail(&err, "", None)))
                }
            }
        }

        // The cursor OWNS the items and verdicts: it has to outlive this call
        // whenever an item suspends.
        let cursor = BlockCursor::new(items.to_vec(), verdicts, self.eval_input.clone(), verbose);
        self.drive_block(cursor, handlers, captured)
    }

    /// The block item loop, re-enterable from the shared cursor's current
    /// position.
    ///
    /// Items are processed by batching maximal runs of consecutive decl-shaped
    /// items (Decl/Auto) so a sig+binding pair or a mutual-recursion SCC split
    /// across items typecheck TOGETHER. Optimistic: try `define_scoped` on the
    /// whole run; on failure, fall back to processing each item individually.
    /// Stmt/Meta items are singletons.
    ///
    /// **Only the singleton arm can suspend.** Every item on the decl-batch arm
    /// is parser-confirmed a declaration and routes to `run_def`, which never
    /// RUNS the machine; a `:command` never runs it either. So a suspension
    /// always originates in one singleton item, and the cursor needs one
    /// pending-item slot rather than a stack.
    fn drive_block<H: DispatchEffect<CapturedOutput>>(
        &mut self,
        mut cursor: BlockCursor,
        handlers: &mut H,
        captured: &CapturedOutput,
    ) -> TurnStep {
        while cursor.sequence.position() < cursor.sequence.len() {
            let index = cursor.sequence.position();
            // A DECL-shaped item (a keyword decl, or an `Auto` the GHC parser
            // classifies as a top-level declaration) starts a batch run; a
            // stmt/meta/expression is a singleton. The parse verdict — not the
            // lexical `Auto` tag — is what keeps a trailing call (`sq 7` after
            // `sq :: T` / `sq x = …`) OUT of the decl batch: it classifies as an
            // expression, ends the run, and lands on the stmt path (the tool's
            // "define then call in one block" idiom).
            let decl_start = self.decl_shaped_text(
                &cursor.sequence.items()[index],
                cursor.verdicts[index].as_ref(),
            );
            let Some(first) = decl_start else {
                // Singleton — the ONE arm that can suspend.
                let (kind, step) = self.run_one_item(
                    &cursor.sequence.items()[index],
                    cursor.verdicts[index].as_ref(),
                    handlers,
                    captured,
                );
                match step {
                    ItemStep::Suspended(tail, req) => {
                        self.suspended = Some(SuspendedTurn {
                            tail,
                            cursor: SuspendedBlockCursor {
                                cursor,
                                pending_item: PendingItem { index, kind },
                            },
                        });
                        return TurnStep::Suspended(req);
                    }
                    ItemStep::Done(outcome) => {
                        let stop = cursor.absorb(ItemRun {
                            index,
                            kind,
                            outcome,
                        });
                        if stop {
                            break;
                        }
                    }
                }
                continue;
            };

            // Carry the decl sources as we scan the maximal decl-shaped run, so
            // the batch path never re-matches items to recover their text.
            let start = index;
            let mut texts: Vec<String> = vec![first.to_string()];
            let mut defined_heads: Vec<String> = cursor.verdicts[index]
                .as_ref()
                .into_iter()
                .flat_map(|verdict| &verdict.items)
                .map(|item| item.head_name().to_string())
                .collect();
            let mut end = index + 1;
            while end < cursor.sequence.len() {
                match self
                    .decl_shaped_text(&cursor.sequence.items()[end], cursor.verdicts[end].as_ref())
                {
                    Some(t) => {
                        // A GHC-reported export repeated within this segment is
                        // a real redefinition. Signatures report no export, so
                        // `f :: T` followed by `f x = ...` remains one commit;
                        // operators and multi-name patterns need no special
                        // lexical cases.
                        let item_exports = cursor.verdicts[end]
                            .as_ref()
                            .into_iter()
                            .flat_map(|verdict| &verdict.items);
                        if item_exports
                            .clone()
                            .any(|item| defined_heads.iter().any(|head| head == item.head_name()))
                        {
                            break;
                        }
                        defined_heads.extend(item_exports.map(|item| item.head_name().to_string()));
                        texts.push(t.to_string());
                        end += 1;
                    }
                    None => break,
                }
            }

            // Try to elaborate the whole run as one generation. Every item is
            // parser-confirmed a declaration, so the batch is not poisoned by
            // a stray expression; a failure here is a genuine type/scope error
            // and falls to the per-item path for a precise, per-item message.
            let batched = if texts.len() >= 2 {
                let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
                self.define_scoped(&refs).ok()
            } else {
                None
            };
            let mut stop = false;
            match batched {
                Some(receipt) => {
                    let type_display = self.probe_single_declaration_type(&receipt.items);
                    for (k, _) in texts.iter().enumerate() {
                        let items = cursor.verdicts[start + k]
                            .as_ref()
                            .map_or(&[][..], |verdict| verdict.items.as_slice());
                        let outcome = self.defined_outcome(
                            items,
                            type_display.as_deref(),
                            receipt.generation,
                        );
                        if cursor.absorb(ItemRun {
                            index: start + k,
                            kind: ItemKind::Decl,
                            outcome,
                        }) {
                            stop = true;
                            break;
                        }
                    }
                }
                None => {
                    // Fallback: per-item, stopping the whole block on the first
                    // error. Never suspends — see this fn's doc.
                    for k in 0..(end - start) {
                        let (kind, step) = self.run_one_item(
                            &cursor.sequence.items()[start + k],
                            cursor.verdicts[start + k].as_ref(),
                            handlers,
                            captured,
                        );
                        let outcome = match step {
                            ItemStep::Done(outcome) => outcome,
                            // Unreachable by construction (a decl-shaped item
                            // routes to `run_def`); surfaced as an error rather
                            // than a panic if the classification ever drifts.
                            ItemStep::Suspended(..) => TurnOutcome::Error(
                                "internal: a declaration item suspended at an ask".into(),
                            ),
                        };
                        if cursor.absorb(ItemRun {
                            index: start + k,
                            kind,
                            outcome,
                        }) {
                            stop = true;
                            break;
                        }
                    }
                }
            }
            if stop {
                break;
            }
        }

        TurnStep::Completed(self.finish_block(cursor))
    }

    /// Re-enter a suspended block: finish the pending item with the answer, then
    /// continue the loop at the cursor's next item. `tail` is the stowed item tail (moved
    /// out of `self.suspended` by [`Self::reenter`] along with `suspended_cursor`
    /// — the two always travel together, so there is no "cursor with no pending
    /// item" case to guard here).
    fn resume_block<H: DispatchEffect<CapturedOutput>>(
        &mut self,
        suspended_cursor: SuspendedBlockCursor,
        tail: PendingTail,
        answer: ResumeAnswer,
        handlers: &mut H,
        captured: &CapturedOutput,
    ) -> TurnStep {
        let SuspendedBlockCursor {
            mut cursor,
            pending_item,
        } = suspended_cursor;
        // The payload lane is in scope for EVERY item in the block, including
        // the ones after this resume — restore it from the cursor rather than
        // trusting that nothing disturbed the session while it was suspended.
        self.eval_input = cursor.eval_input.clone();
        match self.resume_item(tail, answer, handlers, captured) {
            // The same item asked again: re-stow and hand the new ask up.
            ItemStep::Suspended(new_tail, req) => {
                self.suspended = Some(SuspendedTurn {
                    tail: new_tail,
                    cursor: SuspendedBlockCursor {
                        cursor,
                        pending_item,
                    },
                });
                TurnStep::Suspended(req)
            }
            ItemStep::Done(outcome) => {
                if cursor.absorb(ItemRun {
                    index: pending_item.index,
                    kind: pending_item.kind,
                    outcome,
                }) {
                    return TurnStep::Completed(self.finish_block(cursor));
                }
                self.drive_block(cursor, handlers, captured)
            }
        }
    }

    /// Assemble the block's response from a finished cursor.
    fn finish_block(&self, mut cursor: BlockCursor) -> TurnOutcome {
        // The top-level `value` reflects the block's FINAL executed item ONLY —
        // a block ending in a bind/decl/meta (or one that errored after an
        // earlier expression ran) leaves it null, matching the documented
        // contract ("a block ending in a bind leaves `value` null") and GHCi
        // intuition. `result_pos` was recorded at assignment time, so this is a
        // direct index comparison, not a re-derived "last ok item" scan.
        let is_final = cursor.last.as_ref().is_some_and(|lv| {
            Some(lv.result_pos) == cursor.sequence.committed().len().checked_sub(1)
        });
        if !is_final {
            cursor.last = None;
        }

        // Suppress `value` (and `truncated`) from the item that produced the
        // last value — that data now lives at the top level only, eliminating
        // duplication between items[].value and the top-level value. Strips
        // exactly `results[result_pos]`, never an unrelated item (e.g. a
        // trailing `:stub` meta result that happens to carry its OWN `value`
        // key) — the bug this replaced re-scanned for "the last ok item of any
        // kind" and could strip the wrong one.
        if let Some(lv) = &cursor.last {
            if let Some(r) = cursor.sequence.committed_mut().get_mut(lv.result_pos) {
                if let serde_json::Value::Object(ref mut obj) = r.result {
                    obj.remove("value");
                    obj.remove("truncated");
                }
            }
        }

        let shape = if cursor.verbose {
            ResponseShape::Verbose {
                generation: self.core.lib().generation().0,
                val_gen: self.core.val_gen().0,
            }
        } else {
            ResponseShape::Slim
        };
        let value = cursor.last.map(|lv| BlockValue {
            value: lv.value,
            type_display: lv.type_display,
            truncated: lv.truncated,
        });
        TurnOutcome::Block {
            items: cursor.sequence.into_committed(),
            value,
            shape,
        }
    }

    // -- the stowed-tail lifecycle ------------------------------------------

    /// Re-enter the ONE stowed item tail: pick the machine resume entry its
    /// materialization policy calls for, feed it the answer (bridged against the
    /// table the suspending run used), and settle the result through the SAME
    /// `finish_*` the non-suspending path uses.
    fn resume_item<H: DispatchEffect<CapturedOutput>>(
        &mut self,
        tail: PendingTail,
        answer: ResumeAnswer,
        handlers: &mut H,
        captured: &CapturedOutput,
    ) -> ItemStep {
        let input = answer.into_input(tail.run_table(self.core.session_table()));
        match tail {
            PendingTail::Reference(t) => {
                let outcome = self.core.resume_session(handlers, captured, input);
                self.settle(PendingTail::Reference(t), outcome, handlers, captured)
            }
            PendingTail::Bind(t) => {
                let forced = matches!(t.tier, ValueTier::Tier0Data);
                let outcome = self.core.resume_bind(handlers, captured, input, forced);
                self.settle(PendingTail::Bind(t), outcome, handlers, captured)
            }
            PendingTail::MultiBind(t) => {
                let n_fields = t.binders.len();
                let outcome = self
                    .core
                    .resume_bind_projected(handlers, captured, input, n_fields);
                self.settle_projected(t, outcome, handlers, captured)
            }
            PendingTail::BareExpr(t) => {
                let forced = matches!(t.tier, ValueTier::Tier0Data);
                let outcome = self
                    .core
                    .resume_bind_render(handlers, captured, input, forced);
                self.settle_render(t, outcome, handlers, captured)
            }
        }
    }

    /// Feed an ABORT into the stowed continuation, discarding whatever comes
    /// back. Used when the machine handed back a suspension nobody can answer:
    /// the machine must not be left holding a continuation no `session_resume`
    /// will ever reach.
    fn abort_pending<H: DispatchEffect<CapturedOutput>>(
        &mut self,
        tail: &PendingTail,
        reason: String,
        handlers: &mut H,
        captured: &CapturedOutput,
    ) {
        let input = ResumeInput::Abort(reason);
        match tail {
            PendingTail::Reference(_) => {
                let _ = self.core.resume_session(handlers, captured, input);
            }
            PendingTail::Bind(t) => {
                let forced = matches!(t.tier, ValueTier::Tier0Data);
                let _ = self.core.resume_bind(handlers, captured, input, forced);
            }
            PendingTail::MultiBind(t) => {
                let n_fields = t.binders.len();
                let _ = self
                    .core
                    .resume_bind_projected(handlers, captured, input, n_fields);
            }
            PendingTail::BareExpr(t) => {
                let forced = matches!(t.tier, ValueTier::Tier0Data);
                let _ = self
                    .core
                    .resume_bind_render(handlers, captured, input, forced);
            }
        }
    }

    /// The suspension arm shared by all three `settle_*`: bridge the ask request
    /// and stow the tail, or — if the request itself is malformed, most
    /// plausibly a prompt expression that crashed during evaluation — abort the
    /// stowed continuation and surface the reason.
    fn stow_ask<H: DispatchEffect<CapturedOutput>>(
        &mut self,
        tail: PendingTail,
        request: &Value,
        handlers: &mut H,
        captured: &CapturedOutput,
    ) -> ItemStep {
        let extracted = extract_ask_request(request, tail.run_table(self.core.session_table()));
        match extracted {
            Ok((prompt, meta)) => ItemStep::Suspended(tail, AskRequest { prompt, meta }),
            Err(msg) => {
                self.abort_pending(&tail, msg.clone(), handlers, captured);
                ItemStep::Done(TurnOutcome::Error(tag_failure(
                    FailureClass::UserHaskell,
                    Phase::Run,
                    msg,
                )))
            }
        }
    }

    /// Close out a machine call whose policy completes with a bridged `Value`
    /// (plain eval, session reference, single bind). Completion runs the tail's
    /// own `finish_*` — the SINGLE copy of that item's completion bookkeeping,
    /// reached identically whether the run completed inline or after N
    /// suspensions.
    fn settle<H: DispatchEffect<CapturedOutput>>(
        &mut self,
        tail: PendingTail,
        outcome: Result<SuspendableOutcome, tidepool_runtime::JitError>,
        handlers: &mut H,
        captured: &CapturedOutput,
    ) -> ItemStep {
        match outcome {
            Err(e) => ItemStep::Done(TurnOutcome::Error(run_fail(tail.error_label(), e))),
            Ok(SuspendableOutcome::Completed(value)) => {
                ItemStep::Done(self.finish_value_tail(tail, value))
            }
            Ok(SuspendableOutcome::Suspended { request, .. }) => {
                self.stow_ask(tail, &request, handlers, captured)
            }
        }
    }

    /// [`Self::settle`] for the multi-bind (`Project`) policy, whose completion
    /// IS the per-field tenured roots.
    fn settle_projected<H: DispatchEffect<CapturedOutput>>(
        &mut self,
        tail: MultiBindTail,
        outcome: Result<Suspendable<Vec<RootSlot>>, tidepool_runtime::JitError>,
        handlers: &mut H,
        captured: &CapturedOutput,
    ) -> ItemStep {
        match outcome {
            Err(e) => ItemStep::Done(TurnOutcome::Error(run_fail("multi-bind runtime error", e))),
            Ok(Suspendable::Completed(slots)) => {
                ItemStep::Done(self.finish_multi_bind(tail, slots))
            }
            Ok(Suspendable::Suspended { request, .. }) => {
                self.stow_ask(PendingTail::MultiBind(tail), &request, handlers, captured)
            }
        }
    }

    /// [`Self::settle`] for the bare-expression (`Render`) policy, whose
    /// completion carries `it`'s tenured root AND the rendered value together.
    fn settle_render<H: DispatchEffect<CapturedOutput>>(
        &mut self,
        tail: BareExprTail,
        outcome: Result<Suspendable<(RootSlot, Value)>, tidepool_runtime::JitError>,
        handlers: &mut H,
        captured: &CapturedOutput,
    ) -> ItemStep {
        match outcome {
            Err(e) => ItemStep::Done(TurnOutcome::Error(run_fail("runtime error", e))),
            Ok(Suspendable::Completed((it_slot, rendered))) => {
                ItemStep::Done(self.finish_bare_expr(tail, it_slot, rendered))
            }
            Ok(Suspendable::Suspended { request, .. }) => {
                self.stow_ask(PendingTail::BareExpr(tail), &request, handlers, captured)
            }
        }
    }

    /// Run the completion bookkeeping for a finished `Value`-completing tail.
    fn finish_value_tail(&mut self, tail: PendingTail, value: Value) -> TurnOutcome {
        match tail {
            PendingTail::Reference(t) => self.finish_reference_fragment(t, value),
            PendingTail::Bind(t) => match self.core.take_bound_root() {
                Some(slot) => self.finish_bind(t, slot),
                None => TurnOutcome::Error(tag_failure(
                    FailureClass::Infra,
                    Phase::Run,
                    "bind completed but no tenured root was recorded".into(),
                )),
            },
            // Their policies complete with their own products, so they are
            // settled by `settle_projected`/`settle_render` and never arrive
            // here.
            PendingTail::MultiBind(_) | PendingTail::BareExpr(_) => {
                unreachable!("the projected and render policies do not complete with a bare Value")
            }
        }
    }

    /// Define decl text(s) scoped against live session values
    /// (`current_val_modules` imported unqualified, `live_val_modules` injected
    /// for validation) so a decl like `f x = … g …` can reference a prior
    /// `x <- e`/`let x = e` session value — GHCi parity: a top-level definition
    /// at the prompt sees earlier bindings. EVERY decl-plane route goes through
    /// here (`run_def`, the whole-block decl batch, `try_pure_bind_as_decl`);
    /// an unscoped `SessionLib::define*` call from the repl is a bug.
    fn define_scoped(
        &mut self,
        decl_texts: &[&str],
    ) -> Result<tidepool_runtime::session::DeclarationPlaneCommit, SessionError> {
        let receipt = self.core.define_replacing_values(decl_texts)?;
        for name in receipt.items.iter().flat_map(ExportItem::all_names) {
            self.pure_binds.remove(name);
        }
        Ok(receipt)
    }

    fn name_is_live(&self, name: &str) -> bool {
        self.core.bindings().resolve(name).is_some()
            || self
                .core
                .lib()
                .current_declarations()
                .iter()
                .any(|(item, _)| item.all_names().any(|owned| owned == name))
    }

    /// Declaration handler: append the declaration to the Lane-A log + regenerate
    /// the gen-versioned `Lib.G<g>` module.
    fn run_def(&mut self, decl_text: &str) -> TurnOutcome {
        match self.define_scoped(&[decl_text]) {
            Ok(receipt) => {
                let type_display = self.probe_single_declaration_type(&receipt.items);
                self.defined_outcome(&receipt.items, type_display.as_deref(), receipt.generation)
            }
            Err(e) => TurnOutcome::Error(session_fail(&e, "declaration failed")),
        }
    }

    /// If `expr_text` is a PURE bind of `name`, route it as the top-level decl
    /// `name = <rhs>` (so GHC generalizes it — GHCi parity) and return a `Bound`
    /// outcome. Returns `None` only when it is not a pure bind. Callers decide
    /// whether the `input` lane or an existing-name monadic shadow requires
    /// materialization before entering this function; a declaration failure is
    /// preserved as a structured error rather than retried through another
    /// semantic route.
    ///
    /// `define_batch` always shadows wildcard-imported names (ledger #36) so a
    /// pure bind can shadow a Prelude/Library/effect-verb name exactly as a
    /// genuine top-level decl does — `let lookup = 42` must shadow
    /// `Prelude.lookup`, not raise an "Ambiguous occurrence".
    fn try_pure_bind_as_decl(&mut self, expr_text: &str, name: &str) -> Option<TurnOutcome> {
        let decl = pure_bind_to_decl(expr_text, name)?;
        match self.define_scoped(&[decl.as_str()]) {
            Ok(receipt) => {
                let type_display = self
                    .probe_single_declaration_type(&receipt.items)
                    .unwrap_or_default();
                // Register in the environment (decl plane) so :bindings
                // see it; `bind_pure` evicts any materialized binding of `name`
                // from the value plane (cross-plane shadow, one-plane invariant).
                self.bind_pure(
                    name,
                    PureBind {
                        type_display: type_display.clone(),
                        gen: receipt.generation,
                    },
                );
                Some(TurnOutcome::Bound {
                    name: name.to_string(),
                    type_display,
                })
            }
            Err(e) => Some(TurnOutcome::Error(session_fail(&e, "bind compile error"))),
        }
    }

    /// Read the (generalized) type of a pure session name by compiling it as a
    /// pure reference and reading the captured type. Best-effort — `None` if it
    /// doesn't typecheck as a bare pure value.
    ///
    /// Probes in an NMR context (mirroring the decl module the bind actually
    /// lives in), NOT the eval preamble's monomorphism restriction. A pure bind
    /// generalizes under NMR, so its true type shows — GHCi parity: `n <- pure
    /// 5` reads `Num a => a`, `xs <- pure []` reads `[a]`. Under MR a
    /// constrained bind (`f h = h.path :: HasField "path" r a => r -> a`, the
    /// core record-dot idiom) can't monomorphize the unresolved constraint and
    /// the probe FAILS — an empty type display; NMR reports it faithfully.
    fn probe_name_type(&mut self, name: &str) -> Option<String> {
        let preamble =
            tidepool_runtime::session::enable_no_monomorphism_restriction(&self.patched_preamble());
        let imports = self.session_imports();
        let eval_input = self.eval_input.clone();
        let template = wrap_pure_ref_source(&preamble, &imports, "{{TURN}}", eval_input.as_ref());
        self.compile_repl_turn(
            name,
            vec![TurnTemplate {
                kind: TemplateSelector::Expr,
                source: template,
            }],
            TurnClassification {
                kind: TurnKind::Expr,
                binders: Vec::new(),
                items: Vec::new(),
            },
            self.core.val_gen(),
        )
        .ok()
        .and_then(|compiled| compiled.compiled.warnings.captured_type)
    }

    /// Probe the inferred type only when a commit introduces exactly one value
    /// export. This is the declaration path's one centralized GHC-backed probe:
    /// multi-head commits do not fan out into one compiler spawn per head.
    fn probe_single_declaration_type(&mut self, items: &[ExportItem]) -> Option<String> {
        let mut values = items.iter().filter_map(|item| match item {
            ExportItem::Value { name } => Some(name.as_str()),
            ExportItem::Type { .. } | ExportItem::Class { .. } => None,
        });
        let name = values.next()?;
        if values.next().is_some() {
            return None;
        }
        self.probe_name_type(name)
    }

    fn defined_outcome(
        &self,
        items: &[ExportItem],
        type_display: Option<&str>,
        gen: Generation,
    ) -> TurnOutcome {
        let declarations = items
            .iter()
            .map(|item| match item {
                ExportItem::Value { name } => DeclarationMetadata::Value {
                    name: name.clone(),
                    type_display: type_display.map(str::to_owned),
                },
                ExportItem::Type { name, cons } => DeclarationMetadata::Type {
                    name: name.clone(),
                    constructors: cons.clone(),
                },
                ExportItem::Class { name, methods } => DeclarationMetadata::Class {
                    name: name.clone(),
                    methods: methods.clone(),
                },
            })
            .collect();
        TurnOutcome::Defined {
            generation: gen.0,
            module: tidepool_repr::SessionModule::lib(gen).module_name(),
            declarations,
        }
    }

    /// Run a single block item (no batching): the per-item dispatch used both
    /// for stmt/meta items and as the fallback when a decl batch fails. `verdict`
    /// is this item's precomputed classify verdict from `run_block`'s batch
    /// spawn (`None` only for explicit `Decl`/`Meta` items);
    /// `run_eval` (on the `Stmt`/`Auto` paths) and `Auto`'s own dispatch below
    /// both consume it.
    fn run_one_item<H: DispatchEffect<CapturedOutput>>(
        &mut self,
        item: &BlockItem,
        verdict: Option<&TurnClassification>,
        handlers: &mut H,
        captured: &CapturedOutput,
    ) -> (ItemKind, ItemStep) {
        match item {
            BlockItem::Decl(decl) => (ItemKind::Decl, ItemStep::Done(self.run_def(&decl.0))),
            BlockItem::Stmt(expr) => (
                ItemKind::Stmt,
                self.run_eval(&expr.0, verdict, handlers, captured),
            ),
            BlockItem::Meta(meta) => (ItemKind::Meta, ItemStep::Done(self.run_meta(meta))),
            // GHC's parser already classified this item (the block's batch
            // `classify_block` spawn): a `Decl` verdict runs as a declaration,
            // and a `Bind`/`Expr` verdict runs `run_eval`. No rendered compiler
            // text participates in this decision.
            BlockItem::Auto(expr) => match verdict {
                Some(v) if v.kind == TurnKind::Decl => {
                    (ItemKind::Decl, ItemStep::Done(self.run_def(&expr.0)))
                }
                Some(_) => (
                    ItemKind::Stmt,
                    self.run_eval(&expr.0, verdict, handlers, captured),
                ),
                None => (
                    ItemKind::Stmt,
                    ItemStep::Done(TurnOutcome::Error(tag_failure(
                        FailureClass::VersionSkew,
                        Phase::Compile,
                        "block item is missing its compiler classification verdict".into(),
                    ))),
                ),
            },
        }
    }

    /// Expression/bind handler. `verdict` is this item's precomputed classify
    /// verdict — GHC's parser classifies bind vs expr, never a Rust scanner —
    /// from `run_block`'s one batch [`classify_block`] spawn for the whole
    /// block; a BIND (`x <- e` / `let x = e`) roots a value on the live heap, a
    /// reference-with-live-bindings injects the session ifaces, and a plain
    /// expression (no bindings) takes the bare-expression path.
    fn run_eval<H: DispatchEffect<CapturedOutput>>(
        &mut self,
        expr_text: &str,
        verdict: Option<&TurnClassification>,
        handlers: &mut H,
        captured: &CapturedOutput,
    ) -> ItemStep {
        // Bind-vs-expr + bound names come from GHC (parse-only, via the
        // block's batch classify). A missing verdict is a contract failure,
        // never permission to guess a route and launch another compile.
        let classification = match verdict {
            Some(c) => c,
            None => {
                return ItemStep::Done(TurnOutcome::Error(tag_failure(
                    FailureClass::VersionSkew,
                    Phase::Compile,
                    "block item is missing its compiler classification verdict".into(),
                )))
            }
        };

        if classification.kind == TurnKind::Bind {
            match classification.binders.as_slice() {
                // A bind whose pattern binds NO name (`_ <- e`, `(_,_) <- e`):
                // compile the whole statement inside a `do` block that discards
                // its result and yields `()` — `_ <- e` is an ordinary
                // do-statement there, no pattern-stripping needed. (#321)
                [] => self.run_bind_discard(expr_text, handlers, captured),
                [name] => {
                    let name = name.clone();
                    // A PURE bind (`let x = e`, `x <- pure e`) is routed into the
                    // decl plane as `x = e` so it GENERALIZES (GHCi parity)
                    // instead of freezing to a monomorphic heap value. A
                    // monadic bind that shadows an existing name materializes:
                    // its RHS is evaluated in the old scope, whereas rewriting
                    // it as `name = rhs` would make it recursive. A request
                    // carrying the `input` lane also materializes conservatively;
                    // the declaration plane has no such provider. Neither
                    // decision scans Haskell source for guessed references.
                    let monadic_shadow =
                        !expr_text.trim_start().starts_with("let ") && self.name_is_live(&name);
                    if !monadic_shadow && self.eval_input.is_none() {
                        // The decl route compiles and validates but never runs
                        // the machine, so it cannot suspend.
                        if let Some(outcome) = self.try_pure_bind_as_decl(expr_text, &name) {
                            return ItemStep::Done(outcome);
                        }
                    }
                    self.run_bind(expr_text, name, handlers, captured)
                }
                // Multi-binder flat-tuple pattern (`(a, b) <- …`, `let (x,y) = …`):
                // run the action, project each tuple field, root each component.
                names => {
                    let names: Vec<String> = names.to_vec();
                    self.run_multi_bind(expr_text, names, handlers, captured)
                }
            }
        } else {
            // A confirmed bare EXPRESSION (not a bind, not a discard-bind RHS)
            // — GHCi-style: bind its value to `it` (rebinding each turn) and
            // render the response from that SAME single evaluation. See
            // `run_bare_expr`.
            let imports = self.turn_imports(expr_text);
            self.run_bare_expr(expr_text, &imports, handlers, captured)
        }
    }

    /// Assemble a [`TurnOutcome::Value`], truncating an oversized rendered
    /// value to the result budget and stashing the elided subtrees for
    /// `:stub <n>` (see [`crate::truncate`]).
    fn value_outcome(
        &mut self,
        rendered: serde_json::Value,
        type_display: Option<String>,
    ) -> TurnOutcome {
        let (value, stubs, truncated) = crate::truncate::truncate_result(rendered);
        if !stubs.is_empty() {
            self.last_stubs = stubs;
        }
        TurnOutcome::Value {
            value,
            type_display,
            truncated,
        }
    }

    /// Like [`Self::value_outcome`], but for a turn that ALSO bound the
    /// result to `it` ([`Self::run_bare_expr`]) — the truncation hint
    /// additionally names the `it` affordance (type + rendered size + a
    /// neutral note), and a result whose rendered size exceeds
    /// [`crate::truncate::HUGE_CEILING`] collapses to a terse header
    /// (`{type, size, note}`) instead of a partial structural preview. See
    /// [`crate::truncate::truncate_for_it`].
    fn value_outcome_bound_it(
        &mut self,
        rendered: serde_json::Value,
        type_display: Option<String>,
    ) -> TurnOutcome {
        let (value, stubs, truncated) =
            crate::truncate::truncate_for_it(rendered, type_display.as_deref());
        if !stubs.is_empty() {
            self.last_stubs = stubs;
        }
        TurnOutcome::Value {
            value,
            type_display,
            truncated,
        }
    }

    /// Compile one raw REPL item through the shared resident turn boundary.
    /// Frontend policy is limited to the ordered wrapper templates; GHC owns
    /// classification and variant selection, and there is exactly one extract
    /// spawn for the item.
    fn compile_repl_turn(
        &self,
        turn_text: &str,
        templates: Vec<TurnTemplate>,
        verdict: TurnClassification,
        gen: Generation,
    ) -> Result<ReplCompiledTurn, ReplCompileFailure> {
        let include = self.turn_include();
        let inject = self.live_val_modules();
        let result = tidepool_runtime::session::run_turn(TurnRequest {
            turn_text,
            templates: &templates,
            include: &include,
            session_root: self.session_root(),
            inject_modules: &inject,
            gen: gen.0,
            verdict: Some(verdict),
            target: None,
        })
        .map_err(|failure| {
            let TurnFailure {
                error,
                attempted_source,
            } = failure;
            let source = attempted_source.unwrap_or_default();
            let user_lines = user_code_line_range(&source, turn_text);
            ReplCompileFailure {
                error: Box::new(error),
                source,
                user_lines,
            }
        })?;
        let (binders, compiled) = match result {
            TurnResult::Bind {
                bound, compiled, ..
            } => (bound, compiled),
            TurnResult::Expr { compiled, .. } => (Vec::new(), compiled),
            TurnResult::Decl(_) => {
                return Err(ReplCompileFailure {
                    error: Box::new(CompileError::ExtractFailed(
                        "REPL compile adapter received a declaration outcome".to_string(),
                    )),
                    source: String::new(),
                    user_lines: None,
                })
            }
        };
        Ok(ReplCompiledTurn { binders, compiled })
    }

    /// BIND path (`x <- action` / `let x = e`): wrap into an `Eff`-typed
    /// `__result = do { <stmt>; pure x }`, compile through the session extract
    /// (earlier bindings injected so `action` may reference them), then
    /// `run_fragment_and_bind` to reduce the effect tree, strict-force (Tier-0)
    /// or store-as-is (Tier-1), tenure + root the value, and record it in the
    /// `BindingTable`. The thin `Val.G<g>` iface was already written by the
    /// extract (the type plane); this wires the value plane to the same id.
    fn run_bind<H: DispatchEffect<CapturedOutput>>(
        &mut self,
        turn_text: &str,
        name: String,
        handlers: &mut H,
        captured: &CapturedOutput,
    ) -> ItemStep {
        let preamble = self.patched_preamble();
        let g = self.core.val_gen().next();
        let imports = self.turn_imports(turn_text);
        let eval_input = self.eval_input.clone();
        let template_source = wrap_bind_source(
            &preamble,
            &self.cfg.effect_stack,
            &imports,
            "{{TURN_STMT}}",
            "{{BINDERS}}",
            eval_input.as_ref(),
        );
        let turn = match self.compile_repl_turn(
            turn_text,
            vec![TurnTemplate {
                kind: TemplateSelector::Bind,
                source: template_source.clone(),
            }],
            TurnClassification {
                kind: TurnKind::Bind,
                binders: vec![name.clone()],
                items: Vec::new(),
            },
            g,
        ) {
            Ok(turn) => turn,
            Err(e) => {
                return ItemStep::Done(TurnOutcome::Error(compile_fail(
                    &e.error,
                    &e.source,
                    e.user_lines,
                )))
            }
        };
        if turn.compiled.warnings.has_io {
            return ItemStep::Done(TurnOutcome::Error(tag_failure(
                FailureClass::UserHaskell,
                Phase::Compile,
                "IO type detected in bound value. IO operations are not supported.".into(),
            )));
        }
        let binder = match turn.binders.into_iter().next() {
            Some(b) => b,
            None => {
                return ItemStep::Done(TurnOutcome::Error(tag_failure(
                    FailureClass::Infra,
                    Phase::Compile,
                    "bind turn produced no binder metadata".into(),
                )))
            }
        };
        if let Err(e) = self.merge_table(&turn.compiled.table) {
            return ItemStep::Done(TurnOutcome::Error(tag_failure(
                FailureClass::Runtime,
                Phase::Run,
                e,
            )));
        }

        // Bootstrap the resident machine on the first turn from THIS turn's table
        // (an Eff module → carries the effect ConTags the machine's dispatch
        // needs), publishing the cancel handle. Later binds re-enter the live
        // machine.
        if !self.core.is_bootstrapped() {
            if let Err(e) = self
                .core
                .bootstrap_if_needed(&turn.compiled.expr, &turn.compiled.table)
            {
                return ItemStep::Done(TurnOutcome::Error(run_fail("JIT compile error", e)));
            }
            self.publish_cancel();
        }

        // Seed the env from EXISTING bindings (so `action` resolves earlier x's);
        // the new binding is added after it is rooted. `add_fragment_session`
        // mints the fragment against the accumulated table; `bind_funcid` runs +
        // deep-forces + tenures it. (`run_fragment_and_bind` takes a `forced`
        // bool; the tier is the source of truth — derive the flag here, expand
        // the same tier back to a `BoundValue` via `bound_value`.)
        let referenced = tidepool_repr::free_vars::free_vars(&turn.compiled.expr);
        let env = self.core.seed_external_env(&referenced);
        let fid = match self
            .core
            .add_fragment_session("repl_bind", &turn.compiled.expr, &env)
        {
            Ok(f) => f,
            Err(e) => {
                return ItemStep::Done(TurnOutcome::Error(run_fail(
                    "JIT bind add_function error",
                    e,
                )))
            }
        };

        let tail = BindTail {
            name,
            g,
            var_id: binder.var_id,
            tier: binder.tier,
            type_display: binder.type_display,
            defining_expr: turn_text.to_string(),
        };

        let outcome = self.core.bind_funcid(
            fid,
            handlers,
            captured,
            matches!(tail.tier, ValueTier::Tier0Data),
        );
        self.settle(PendingTail::Bind(tail), outcome, handlers, captured)
    }

    /// Post-run bookkeeping for [`Self::run_bind`]: advance the value
    /// generation, root the bound value, and record it on the value plane.
    fn finish_bind(&mut self, tail: BindTail, slot: RootSlot) -> TurnOutcome {
        self.core.set_val_gen(tail.g);
        let value = bound_value(tail.tier, slot);
        // `bind_materialized` records the value binding AND evicts any pure decl
        // of the same name (cross-plane shadow, one-plane invariant).
        if let Err(e) = self.bind_materialized(BindingEntry {
            name: BindingName(tail.name.clone()),
            id: SessionVarId::from_extract(tail.var_id),
            module: SessionModule::val(tail.g),
            value,
            type_display: Some(tail.type_display.clone()),
            defining_expr: Some(tail.defining_expr),
            // The repl is a flat session: every bind is a ROOT-frame bind.
            scope: ScopeId::ROOT,
        }) {
            return TurnOutcome::Error(session_fail(&e, "bind failed"));
        }
        TurnOutcome::Bound {
            name: tail.name,
            type_display: tail.type_display,
        }
    }

    /// DISCARD-BIND path (`_ <- e`, `(_, _) <- e`): wraps the whole statement
    /// into an `Eff`-typed `__result = do { <stmt>; pure () }` — the same
    /// `{{TURN_STMT}}` placement `run_bind` uses, but yielding `()` and
    /// splicing no binder — and selects the shared discard-bind template (a
    /// discarding bind mints no session value, so the extract receives an
    /// empty GHC-derived binder list). Runs
    /// through [`Self::run_reference_fragment`] exactly like a session
    /// reference, reporting `()` as the value — the statement's own effects
    /// fire exactly once, and nothing is bound.
    fn run_bind_discard<H: DispatchEffect<CapturedOutput>>(
        &mut self,
        turn_text: &str,
        handlers: &mut H,
        captured: &CapturedOutput,
    ) -> ItemStep {
        let preamble = self.patched_preamble();
        let imports = self.turn_imports(turn_text);
        let eval_input = self.eval_input.clone();
        let template_source = wrap_bind_discard_source(
            &preamble,
            &self.cfg.effect_stack,
            &imports,
            "{{TURN_STMT}}",
            eval_input.as_ref(),
        );
        let turn = match self.compile_repl_turn(
            turn_text,
            vec![TurnTemplate {
                kind: TemplateSelector::BindDiscard,
                source: template_source.clone(),
            }],
            TurnClassification {
                kind: TurnKind::Bind,
                binders: Vec::new(),
                items: Vec::new(),
            },
            self.core.val_gen(),
        ) {
            Ok(turn) => turn,
            Err(e) => {
                return ItemStep::Done(TurnOutcome::Error(compile_fail(
                    &e.error,
                    &e.source,
                    e.user_lines,
                )))
            }
        };
        self.run_reference_fragment(turn, Some("()".to_string()), handlers, captured)
    }

    /// MULTI-BIND path: `(a, b) <- action` / `let (x, y) = e`. Wraps the turn as
    /// `__result = do { <stmt>; pure (a, b, …) }` so the fragment yields ONE tuple
    /// Con, then projects each field individually, tenures each as a separate root,
    /// and records one `BindingEntry` per component. The extract validates that the
    /// result type is an N-tuple (via `splitTupleType`); non-tuple patterns (e.g.
    /// constructor patterns with the wrong return type) get a loud compile error.
    fn run_multi_bind<H: DispatchEffect<CapturedOutput>>(
        &mut self,
        turn_text: &str,
        names: Vec<String>,
        handlers: &mut H,
        captured: &CapturedOutput,
    ) -> ItemStep {
        let preamble = self.patched_preamble();
        let g = self.core.val_gen().next();
        let imports = self.turn_imports(turn_text);
        let eval_input = self.eval_input.clone();
        let template_source = wrap_multi_bind_source(
            &preamble,
            &self.cfg.effect_stack,
            &imports,
            "{{TURN_STMT}}",
            &["{{BINDERS}}".to_string()],
            eval_input.as_ref(),
        );
        let turn = match self.compile_repl_turn(
            turn_text,
            vec![TurnTemplate {
                kind: TemplateSelector::Bind,
                source: template_source.clone(),
            }],
            TurnClassification {
                kind: TurnKind::Bind,
                binders: names.clone(),
                items: Vec::new(),
            },
            g,
        ) {
            Ok(turn) => turn,
            Err(e) => {
                return ItemStep::Done(TurnOutcome::Error(compile_fail(
                    &e.error,
                    &e.source,
                    e.user_lines,
                )))
            }
        };
        if turn.compiled.warnings.has_io {
            return ItemStep::Done(TurnOutcome::Error(tag_failure(
                FailureClass::UserHaskell,
                Phase::Compile,
                "IO type detected in bound value. IO operations are not supported.".into(),
            )));
        }
        if turn.binders.len() != names.len() {
            return ItemStep::Done(TurnOutcome::Error(tag_failure(
                FailureClass::Infra,
                Phase::Compile,
                format!(
                    "multi-bind: extract returned {} binders, expected {}",
                    turn.binders.len(),
                    names.len()
                ),
            )));
        }
        if let Err(e) = self.merge_table(&turn.compiled.table) {
            return ItemStep::Done(TurnOutcome::Error(tag_failure(
                FailureClass::Runtime,
                Phase::Run,
                e,
            )));
        }

        if !self.core.is_bootstrapped() {
            if let Err(e) = self
                .core
                .bootstrap_if_needed(&turn.compiled.expr, &turn.compiled.table)
            {
                return ItemStep::Done(TurnOutcome::Error(run_fail("JIT compile error", e)));
            }
            self.publish_cancel();
        }

        let referenced = tidepool_repr::free_vars::free_vars(&turn.compiled.expr);
        let env = self.core.seed_external_env(&referenced);
        let fid = match self
            .core
            .add_fragment_session("repl_multi_bind", &turn.compiled.expr, &env)
        {
            Ok(f) => f,
            Err(e) => {
                return ItemStep::Done(TurnOutcome::Error(run_fail(
                    "JIT multi-bind add_function error",
                    e,
                )))
            }
        };
        let n_fields = names.len();
        let tail = MultiBindTail {
            g,
            binders: turn.binders,
            defining_expr: turn_text.to_string(),
        };

        // bind_funcid_projected deep-forces the whole tuple first (GC-safe:
        // registers all pending parents as Rust roots), then projects each field
        // from the post-GC NF tuple and tenures each separately. Its completion
        // IS the per-field roots — there is no tuple value on this path.
        let outcome = self
            .core
            .bind_funcid_projected(fid, handlers, captured, n_fields);
        self.settle_projected(tail, outcome, handlers, captured)
    }

    /// Post-run bookkeeping for [`Self::run_multi_bind`]: advance the value
    /// generation, then zip each binder with its projected root and record it
    /// on the value plane. Tier is read from binder metadata (`deep_force`
    /// already handled NF).
    fn finish_multi_bind(&mut self, tail: MultiBindTail, slots: Vec<RootSlot>) -> TurnOutcome {
        self.core.set_val_gen(tail.g);
        let mut components: Vec<BoundComponent> = Vec::new();
        let mut entries = Vec::with_capacity(tail.binders.len());
        for (binder, slot) in tail.binders.iter().zip(slots.into_iter()) {
            let value = bound_value(binder.tier, slot);
            entries.push(BindingEntry {
                name: BindingName(binder.name.clone()),
                id: SessionVarId::from_extract(binder.var_id),
                module: SessionModule::val(tail.g),
                value,
                type_display: Some(binder.type_display.clone()),
                // The whole multi-bind turn defines each component (`(a,b) <- e`).
                defining_expr: Some(tail.defining_expr.clone()),
                scope: ScopeId::ROOT,
            });
            components.push(BoundComponent {
                name: binder.name.clone(),
                type_display: binder.type_display.clone(),
            });
        }
        let receipt = match self.core.bind_replacing_decls(entries) {
            Ok(receipt) => receipt,
            Err(e) => return TurnOutcome::Error(session_fail(&e, "multi-bind failed")),
        };
        for binding in receipt.bindings {
            self.pure_binds.remove(&binding.name);
        }
        TurnOutcome::MultiBound { components }
    }

    /// Run a compiled reference fragment on the resident machine, resolving any
    /// session binders through the seeded `ExternalEnv` (load-through-slot).
    /// `inner_type` is the caller-resolved inner value type (`a` in `M a`).
    /// Always runs effectfully (`run_fragment`) — [`Self::run_bind_discard`] is
    /// the sole caller, and a discard-bind's statement is always an `Eff`
    /// action, never a pure reference.
    fn run_reference_fragment<H: DispatchEffect<CapturedOutput>>(
        &mut self,
        turn: ReplCompiledTurn,
        inner_type: Option<String>,
        handlers: &mut H,
        captured: &CapturedOutput,
    ) -> ItemStep {
        if turn.compiled.warnings.has_io {
            return ItemStep::Done(io_type_fail());
        }
        if let Err(e) = self.merge_table(&turn.compiled.table) {
            return ItemStep::Done(TurnOutcome::Error(tag_failure(
                FailureClass::Runtime,
                Phase::Run,
                e,
            )));
        }
        self.ensure_effect_machine();
        if !self.core.is_bootstrapped() {
            if let Err(e) = self
                .core
                .bootstrap_if_needed(&turn.compiled.expr, &turn.compiled.table)
            {
                return ItemStep::Done(TurnOutcome::Error(run_fail("JIT compile error", e)));
            }
            self.publish_cancel();
        }
        let referenced = tidepool_repr::free_vars::free_vars(&turn.compiled.expr);
        let env = self.core.seed_external_env(&referenced);
        let fid = match self
            .core
            .add_fragment_session("repl_ref", &turn.compiled.expr, &env)
        {
            Ok(f) => f,
            Err(e) => {
                return ItemStep::Done(TurnOutcome::Error(run_fail(
                    "JIT reference add_function error",
                    e,
                )))
            }
        };

        let tail = ReferenceTail { inner_type };
        let outcome = self.core.run_funcid_session(fid, handlers, captured);
        self.settle(PendingTail::Reference(tail), outcome, handlers, captured)
    }

    /// Post-run bookkeeping for [`Self::run_reference_fragment`]: render the
    /// value against the live accumulated session table and the caller-resolved
    /// inner type.
    fn finish_reference_fragment(&mut self, tail: ReferenceTail, value: Value) -> TurnOutcome {
        let rendered = value_to_json(&value, self.core.session_table(), 0);
        self.value_outcome(rendered, tail.inner_type)
    }

    /// GHCi-style `it`: a confirmed bare final EXPRESSION (never a bind or a
    /// discard-bind RHS) is bound to `it`, rebinding on every such turn. The
    /// expression's effects (if any) fire EXACTLY ONCE.
    ///
    /// **Single compile, single run:** wraps `expr_text` as a MATERIALIZING
    /// bind whose `result` yields the TUPLE `(it, toWire it)` — `it <- __user`
    /// tried first (monadic), falling back to `let it = __user` (pure) on a
    /// compile failure. Either way, `run_fragment_and_bind_render` runs the ONE
    /// compiled fragment exactly once: field 0 (`it`) drives `__user`'s effect
    /// and gets tenured; field 1 (`toWire it`, pure) is bridged to an owned
    /// `Value` for the response — see that method's doc for why bridging field
    /// 1 BEFORE tenuring field 0 stays safe even when the two fields alias the
    /// same heap object (`pure input`).
    fn run_bare_expr<H: DispatchEffect<CapturedOutput>>(
        &mut self,
        expr_text: &str,
        imports: &str,
        handlers: &mut H,
        captured: &CapturedOutput,
    ) -> ItemStep {
        let preamble = self.patched_preamble();
        let g = self.core.val_gen().next();
        let eval_input = self.eval_input.clone();
        // TWO names, matching the `(it, toWire it)` tuple `result` now yields:
        // rides the same multi-binder `splitTupleType` path
        // `run_multi_bind`/`wrap_multi_bind_source` use, so turn compilation
        // splits `result`'s `(T, Value)` type into `it :: T` + `__it_render ::
        // Value` instead of wrongly taking the whole tuple as `it`'s type.
        // `__it_render`'s binder metadata is discarded below (only
        // `turn.binders[0]`, i.e. `it`, is used).
        let it_names = vec!["it".to_string(), "__it_render".to_string()];

        let monadic_template = wrap_bare_it_monadic(
            &preamble,
            &self.cfg.effect_stack,
            imports,
            "{{TURN}}",
            eval_input.as_ref(),
        );
        let pure_template = wrap_bare_it_pure(
            &preamble,
            &self.cfg.effect_stack,
            imports,
            "{{TURN}}",
            eval_input.as_ref(),
        );
        let turn = match self.compile_repl_turn(
            expr_text,
            vec![
                TurnTemplate {
                    kind: TemplateSelector::Bind,
                    source: monadic_template,
                },
                TurnTemplate {
                    kind: TemplateSelector::Bind,
                    source: pure_template.clone(),
                },
            ],
            TurnClassification {
                kind: TurnKind::Bind,
                binders: it_names,
                items: Vec::new(),
            },
            g,
        ) {
            Ok(turn) => turn,
            Err(e) => {
                return ItemStep::Done(TurnOutcome::Error(compile_fail(
                    &e.error,
                    &e.source,
                    e.user_lines,
                )))
            }
        };

        if turn.compiled.warnings.has_io {
            return ItemStep::Done(TurnOutcome::Error(tag_failure(
                FailureClass::UserHaskell,
                Phase::Compile,
                "IO type detected in bound value. IO operations are not supported.".into(),
            )));
        }
        let it_binder = match turn.binders.into_iter().next() {
            Some(b) => b,
            None => {
                return ItemStep::Done(TurnOutcome::Error(tag_failure(
                    FailureClass::Infra,
                    Phase::Compile,
                    "bare-expression bind produced no binder metadata".into(),
                )))
            }
        };
        if let Err(e) = self.merge_table(&turn.compiled.table) {
            return ItemStep::Done(TurnOutcome::Error(tag_failure(
                FailureClass::Runtime,
                Phase::Run,
                e,
            )));
        }

        // Bootstrap the resident machine on the first turn from THIS turn's
        // table (an Eff module either way — both wraps end in
        // `pure (it, toWire it)`).
        if !self.core.is_bootstrapped() {
            if let Err(e) = self
                .core
                .bootstrap_if_needed(&turn.compiled.expr, &turn.compiled.table)
            {
                return ItemStep::Done(TurnOutcome::Error(run_fail("JIT compile error", e)));
            }
            self.publish_cancel();
        }

        let referenced = tidepool_repr::free_vars::free_vars(&turn.compiled.expr);
        let env = self.core.seed_external_env(&referenced);
        let fid = match self
            .core
            .add_fragment_session("repl_it", &turn.compiled.expr, &env)
        {
            Ok(f) => f,
            Err(e) => {
                return ItemStep::Done(TurnOutcome::Error(run_fail(
                    "JIT bare-expression add_function error",
                    e,
                )))
            }
        };

        let tail = BareExprTail {
            g,
            var_id: it_binder.var_id,
            tier: it_binder.tier,
            type_display: it_binder.type_display,
            defining_expr: expr_text.to_string(),
        };

        // Run EXACTLY ONCE: the effectful step loop drives `__user` to
        // completion here (field 0's bind), whichever wrap compiled. Field 1
        // (`toWire it`, pure) rides along in the SAME run — no second
        // compile, no second execution of `__user`'s effect.
        let outcome = self.core.bind_funcid_render(
            fid,
            handlers,
            captured,
            matches!(tail.tier, ValueTier::Tier0Data),
        );
        self.settle_render(tail, outcome, handlers, captured)
    }

    /// Post-run bookkeeping for [`Self::run_bare_expr`]: advance the value
    /// generation, root `it`, record it on the value plane, and render the
    /// SAME run's field-1 value. Field 1 is bridged to an owned [`Value`]
    /// BEFORE field 0 is tenured (in [`Self::run_bare_expr`]'s machine call),
    /// which is what makes this safe even when `toWire` is the identity and
    /// the two fields alias the same heap object (`pure input`) — see
    /// `bind_funcid_render`'s doc for the field1-before-field0 GC ordering.
    fn finish_bare_expr(
        &mut self,
        tail: BareExprTail,
        it_slot: RootSlot,
        rendered_value: Value,
    ) -> TurnOutcome {
        self.core.set_val_gen(tail.g);
        let it_value = bound_value(tail.tier, it_slot);
        if let Err(e) = self.bind_materialized(BindingEntry {
            name: BindingName("it".to_string()),
            id: SessionVarId::from_extract(tail.var_id),
            module: SessionModule::val(tail.g),
            value: it_value,
            type_display: Some(tail.type_display.clone()),
            defining_expr: Some(tail.defining_expr),
            scope: ScopeId::ROOT,
        }) {
            return TurnOutcome::Error(session_fail(&e, "expression bind failed"));
        }

        let rendered = value_to_json(&rendered_value, self.core.session_table(), 0);
        self.value_outcome_bound_it(rendered, Some(tail.type_display))
    }

    /// Meta-command handler — `:bindings`, `:reset`, `:t <expr>`, `:i <name>`, `:vocab`.
    fn run_meta(&mut self, meta: &MetaCommand) -> TurnOutcome {
        match meta {
            MetaCommand::Bindings => {
                // The unified environment view: materialized (effectful) value
                // binds AND pure binds (decl-backed, GHCi-environment model).
                let mut bindings: Vec<serde_json::Value> = self.core.bindings().iter_current()
                    .map(|(name, entry)| {
                        serde_json::json!({
                            "name": name.0,
                            "type": entry.type_display.clone().unwrap_or_default(),
                            "module": entry.module.module_name(),
                            "tier": if entry.value.is_forced() { "Tier0Data" } else { "Tier1Closure" },
                        })
                    })
                    .collect();
                bindings.extend(self.pure_binds.iter().map(|(name, pb)| {
                    serde_json::json!({
                        "name": name,
                        "type": pb.type_display,
                        "module": tidepool_repr::SessionModule::lib(pb.gen).module_name(),
                        // A pure bind is a lazy decl, not a materialized value.
                        "tier": "DeclBacked",
                    })
                }));
                bindings.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
                TurnOutcome::Meta(serde_json::json!({
                    "bindings": bindings,
                    "generation": self.core.lib().generation().0,
                    "valGeneration": self.core.val_gen().0,
                }))
            }
            MetaCommand::Reset => {
                // Rebuild-then-swap: open the new lib FIRST, mutate `self` only
                // on success. If `SessionLib::open` fails (IO error), the
                // session is left completely untouched instead of a half-reset
                // state (old decl log kept, but every bind that referenced it
                // already cleared) behind an error implying nothing changed.
                match SessionLib::open(
                    self.cfg.id,
                    self.cfg.root.clone(),
                    self.cfg.module_env.clone(),
                ) {
                    Ok(lib) => {
                        // Rebuild the whole core from a fresh decl plane: this
                        // drops the resident machine (freeing the session heap +
                        // every persistent root) and clears both planes (the
                        // value bindings + accumulated table) and the turn
                        // counter in one move.
                        let lib = lib.with_validation_include(self.cfg.base_include.clone());
                        self.core = PersistentSession::new(Some(lib), self.cfg.nursery_size);
                        // The rebuilt core has no machine, so publish the (now
                        // empty) cancel handle explicitly — otherwise the shared
                        // `CancelSlot` keeps the dropped machine's stale
                        // `CancelHandle` until the next bootstrap, and a
                        // server-side timeout in that window cancels a dead flag
                        // instead of a live one.
                        self.publish_cancel();
                        self.last_stubs = Vec::new();
                        self.pure_binds.clear();
                        TurnOutcome::Meta(serde_json::json!({"reset": true}))
                    }
                    Err(e) => TurnOutcome::Error(format!("reset failed: {e}")),
                }
            }
            MetaCommand::Type(ExprText(expr)) => {
                if expr.is_empty() {
                    return TurnOutcome::Meta(serde_json::json!({
                        "error": ":t requires an expression"
                    }));
                }
                self.run_inspection(InspectionQuery::TypeOf(expr.clone()))
            }
            MetaCommand::Info(name) => self.run_inspection(InspectionQuery::Info(name.clone())),
            MetaCommand::Stub(n, page) => {
                TurnOutcome::Meta(crate::truncate::stub_fetch(&self.last_stubs, *n, *page))
            }
            MetaCommand::Program => {
                // Repaint the session as a replayable notebook: declarations
                // first (in log order — they never depend on binds), then each
                // live bind's defining text (val-gen order). Effectful binds
                // re-RUN their effect on replay; that's honest — the heap is a
                // cache of this document, not the source of truth.
                let decls: Vec<&str> = self.core.lib().decl_sources();
                let mut binds: Vec<(&BindingName, &BindingEntry)> =
                    self.core.bindings().iter_current().collect();
                binds.sort_by_key(|(_, e)| e.module.gen.0);

                let mut program = String::new();
                for d in &decls {
                    program.push_str(d.trim_end());
                    program.push_str("\n\n");
                }
                if !binds.is_empty() {
                    program.push_str("-- binds (re-run to restore heap values):\n");
                    for (name, e) in &binds {
                        match &e.defining_expr {
                            Some(src) => {
                                program.push_str(src.trim_end());
                                program.push('\n');
                            }
                            None => program.push_str(&format!(
                                "-- {} :: {} (defining text unavailable)\n",
                                name.0,
                                e.type_display.clone().unwrap_or_default()
                            )),
                        }
                    }
                }
                TurnOutcome::Meta(serde_json::json!({
                    "program": program,
                    "decls": decls.len(),
                    "binds": binds.len(),
                    "generation": self.core.lib().generation().0,
                }))
            }
            MetaCommand::Vocab(only) => {
                let mut dirs: Vec<std::path::PathBuf> = Vec::new();
                if let Ok(cwd) = std::env::current_dir() {
                    if let Some(root) = tidepool_runtime::paths::find_project_root(&cwd) {
                        let lib = root.join(".tidepool").join("lib");
                        if lib.is_dir() {
                            dirs.push(lib);
                        }
                    }
                }
                dirs.extend(tidepool_runtime::paths::global_lib_dirs());
                let vocab = library_vocab(&dirs, only.as_deref());
                TurnOutcome::Meta(serde_json::json!({
                    "vocab": vocab,
                    // The vocab digest lists .tidepool/lib verbs (each module
                    // tagged bare-in-scope vs needs-import). For the built-in
                    // EFFECT verbs (run/grepGlob/kvSet/…), which live in the
                    // effect decls not the lib dirs, use `:browse`.
                    "hint": ":browse lists built-in effect verbs (:browse <Effect> for one effect's \
                             verbs+constructors). Module tags below: \"bare (Library re-export)\" = \
                             in scope directly; \"needs: import <Mod>\" = add that import first.",
                }))
            }
            MetaCommand::Browse(only) => {
                TurnOutcome::Meta(browse_effects(self.cfg.roster.decls(), only.as_deref()))
            }
        }
    }

    fn run_inspection(&self, query: InspectionQuery) -> TurnOutcome {
        let query_source = match &query {
            InspectionQuery::TypeOf(expression) => expression.as_str(),
            InspectionQuery::Info(name) => name.as_str(),
        };
        let preamble = self.patched_preamble();
        let imports = self.turn_imports(query_source);
        let inject_modules = self.live_val_modules();
        let include = self.turn_include();
        match tidepool_runtime::session::run_inspection(InspectionRequest {
            preamble: &preamble,
            imports: &imports,
            include: &include,
            session_root: self.session_root(),
            inject_modules: &inject_modules,
            query,
        }) {
            Ok(result) => TurnOutcome::Inspection(result.render()),
            Err(error) => TurnOutcome::Error(classify_compile(&error).message),
        }
    }

    /// A read-only JSON snapshot of the live session environment for the
    /// `tidepool://session/bindings` resource: one entry per in-scope binding.
    /// Each is `{name, type, kind (decl|bind), generation}`. `decl` entries are
    /// the decl-plane heads (`f x = …`, `data Foo`, `class C`); `bind` entries
    /// are value-plane binds (`x <- e`) and pure decl-backed binds (`let x = e`,
    /// `x <- pure e`). A pure bind lives in the decl log too, so its name is
    /// EXCLUDED from the decl listing here and surfaced once, as a `bind`.
    pub fn bindings_snapshot(&self) -> serde_json::Value {
        let mut entries: Vec<serde_json::Value> = Vec::new();
        // Decl plane: current in-scope heads (latest-wins), minus pure-bind
        // names (those are surfaced as `bind` below).
        for (item, gen) in self.core.lib().current_declarations() {
            let name = item.head_name();
            if self.pure_binds.contains_key(name) {
                continue;
            }
            let mut entry = serde_json::json!({
                "name": name,
                "type": "",
                "kind": "decl",
                "declarationKind": item.kind().as_str(),
                "generation": gen,
            });
            match item {
                ExportItem::Type { cons, .. } => {
                    entry["constructors"] = serde_json::json!(cons);
                }
                ExportItem::Class { methods, .. } => {
                    entry["methods"] = serde_json::json!(methods);
                }
                ExportItem::Value { .. } => {}
            }
            entries.push(entry);
        }
        // Value plane: materialized (effectful) binds.
        for (name, entry) in self.core.bindings().iter_current() {
            entries.push(serde_json::json!({
                "name": name.0,
                "type": entry.type_display.clone().unwrap_or_default(),
                "kind": "bind",
                "generation": entry.module.gen.0,
            }));
        }
        // Pure binds (decl-backed values) — environment members, kind `bind`.
        for (name, pb) in &self.pure_binds {
            entries.push(serde_json::json!({
                "name": name,
                "type": pb.type_display,
                "kind": "bind",
                "generation": pb.gen.0,
            }));
        }
        entries.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        serde_json::json!({
            "bindings": entries,
            "generation": self.core.lib().generation().0,
            "valGeneration": self.core.val_gen().0,
        })
    }

    /// The eval preamble with every import the session can collide with
    /// extended with a `hiding (…)` clause covering EVERY name the session
    /// owns across both planes (decl value binders, decl types/classes, live
    /// value-plane binds).
    ///
    /// Without this, a session name matching a Prelude re-export, a `Library`
    /// verb, or an effect verb becomes a GHC "Ambiguous occurrence" that
    /// POISONS every later turn too (the colliding import regenerates each
    /// turn) — hit in practice by `let glob = …` (vs the `Fs` `glob` verb) and
    /// `data Hit` (vs `Library`'s `Hit`). Hiding makes the session definition
    /// win, the way GHCi shadowing would.
    fn patched_preamble(&self) -> String {
        let mut names: Vec<String> = Vec::new();
        names.extend(
            self.core
                .lib()
                .decl_value_names()
                .into_iter()
                .map(str::to_string),
        );
        names.extend(
            self.core
                .lib()
                .decl_type_names()
                .into_iter()
                .map(str::to_string),
        );
        names.extend(
            self.core
                .bindings()
                .iter_current()
                .map(|(n, _)| n.0.clone()),
        );
        names.sort();
        names.dedup();
        if names.is_empty() {
            return self.cfg.preamble.clone();
        }
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let p = hide_prelude_names(&self.cfg.preamble, &refs);
        let p = hide_module_names(&p, "Library", &refs);
        let p = hide_module_names(&p, "Tidepool.Effects", &refs);
        // The pagination / orchestration helpers (`memo`, `readGlob`, …) live in
        // the generated Tidepool.Orchestrate module, imported unqualified by the
        // eval expr module. A session bind that collides with one (e.g. `let memo
        // = …`) must shadow the import, not become an ambiguous occurrence.
        let p = hide_module_names(&p, "Tidepool.Orchestrate", &refs);
        // Explicit-list imports (e.g. the preamble's `import Tidepool.Shell
        // (sh)`) can't take a `hiding` clause — a session-owned name is
        // instead SUBTRACTED from the list. Shape-detected per line, so no
        // module enumeration to keep in sync with the preamble.
        p.split('\n')
            .map(|line| subtract_import_list_names(line, &refs).unwrap_or_else(|| line.to_string()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Store the per-block `input` payload so every evaluated item can inject it
    /// into its generated module. Called by the server before each
    /// `session_run`; a resume restores it from the block cursor instead.
    pub fn set_eval_input(&mut self, input: Option<serde_json::Value>) {
        self.eval_input = input;
    }

    /// Wire the shared cancel slot the server reads on timeout. Called once by
    /// the manager at install. If the machine has already bootstrapped, publish
    /// its handle immediately.
    pub fn set_cancel_slot(&mut self, slot: crate::manager::CancelSlot) {
        self.cancel_slot = Some(slot);
        self.publish_cancel();
    }

    /// Ensure the resident machine exists AND its DataConTable carries the freer
    /// `Eff` constructors (`Val`/`Leaf`/`Node`/…), by bootstrapping from a
    /// trivial Eff fragment (`pure ()`) if the machine hasn't started. A PURE
    /// fragment's table omits those cons, so a session whose FIRST machine-using
    /// op is a pure reference — common now that pure binds are decls that don't
    /// bootstrap — would otherwise fail a later Eff fragment with "missing
    /// freer-simple constructor 'Val'". No-op once the machine is up.
    fn ensure_effect_machine(&mut self) {
        if self.core.is_bootstrapped() {
            return;
        }
        let preamble = self.patched_preamble();
        let imports = self.session_imports();
        let eval_input = self.eval_input.clone();
        // `template_haskell_show_default` (the non-session `eval`-tool
        // template, target `result`) is the WRONG shape here — it's compiled
        // through the shared turn boundary. Reuse
        // `wrap_probe_source` instead: it already compiles to a properly
        // `Eff`-typed `__result :: Eff {effect_stack} _` binding (forcing the
        // same freer-simple constructor requirement this bootstrap exists
        // for), just via an extra `__probe`/`__t` monadic peel we don't need
        // the result of — only the compiled table and expression are read below.
        let template = wrap_probe_source(
            &preamble,
            &self.cfg.effect_stack,
            &imports,
            "{{TURN}}",
            eval_input.as_ref(),
        );
        let compiled = self.compile_repl_turn(
            "pure ()",
            vec![TurnTemplate {
                kind: TemplateSelector::Expr,
                source: template,
            }],
            TurnClassification {
                kind: TurnKind::Expr,
                binders: Vec::new(),
                items: Vec::new(),
            },
            self.core.val_gen(),
        );
        if let Ok(compiled) = compiled {
            let turn = compiled;
            let _ = self.merge_table(&turn.compiled.table);
            if self
                .core
                .bootstrap_if_needed(&turn.compiled.expr, &turn.compiled.table)
                .is_ok()
            {
                self.publish_cancel();
            }
        }
    }

    /// Publish the machine's cancel handle into the shared slot (no-op if no
    /// slot is wired or the machine hasn't bootstrapped).
    fn publish_cancel(&mut self) {
        if let Some(slot) = &self.cancel_slot {
            *slot.lock() = self.core.cancel_handle();
        }
    }

    /// Clear a prior cancellation so the next turn starts clean. The cancel flag
    /// is per-machine and shared, so this resets it via the live handle.
    fn reset_cancel(&mut self) {
        if let Some(handle) = self.core.cancel_handle() {
            handle.reset();
        }
    }
}

// ---------------------------------------------------------------------------
// Turn-wrapping helpers
// ---------------------------------------------------------------------------

/// Rewrite `import Tidepool.Prelude hiding (…)` in the preamble to also hide
/// the given names. Applied per-turn so that user-defined functions named after
/// Prelude/lens re-exports (e.g. `over`, `view`, `key`) resolve unambiguously
/// to the session decl rather than the Prelude export.
///
/// Names already present in the hiding list are not duplicated. Names that do
/// not exist in Tidepool.Prelude produce no error (GHC silently ignores
/// redundant hiding entries in most configurations).
fn hide_prelude_names(preamble: &str, extra: &[&str]) -> String {
    const PRELUDE_IMPORT_PREFIX: &str = "import Tidepool.Prelude hiding (";
    let Some(start) = preamble.find(PRELUDE_IMPORT_PREFIX) else {
        return preamble.to_string();
    };
    let rest = &preamble[start..];
    let line_len = rest.find('\n').map_or(rest.len(), |i| i + 1);
    let line = &rest[..line_len];

    // Extract the existing hidden list from "import Tidepool.Prelude hiding (X, Y)\n"
    let paren_open = line.find('(').unwrap_or(line.len()) + 1;
    let paren_close = line.rfind(')').unwrap_or(line.len());
    let mut all: Vec<String> = line[paren_open..paren_close]
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();

    for &n in extra {
        // Parenthesize operator names (`.+` → `(.+)`) — a bare operator in a
        // hiding list is a parse error.
        let e = hiding_entry(n);
        if !all.contains(&e) {
            all.push(e);
        }
    }
    let new_line = format!("import Tidepool.Prelude hiding ({})\n", all.join(", "));
    format!(
        "{}{}{}",
        &preamble[..start],
        new_line,
        &preamble[start + line_len..]
    )
}

/// Parenthesize an operator name for a `hiding` list (`.+` → `(.+)`); plain
/// identifiers pass through. (Operators are invalid bare in an import list.)
fn hiding_entry(name: &str) -> String {
    match name.chars().next() {
        Some(c) if c.is_alphanumeric() || c == '_' || c == '(' => name.to_string(),
        _ => format!("({name})"),
    }
}

/// Like [`hide_prelude_names`] but for a clause-less `import <module>` line
/// (e.g. the project `import Library` or the generated `import Tidepool.Effects`).
/// Rewrites `import <module>` → `import <module> hiding (<session names>)` so a
/// session-defined name shadows a same-named re-export / effect verb instead of
/// becoming an ambiguous occurrence. No-op when `<module>` isn't imported. GHC
/// tolerates hiding a name the module doesn't export (a dodgy-import warning,
/// same as the Prelude path).
fn hide_module_names(preamble: &str, module: &str, extra: &[&str]) -> String {
    if extra.is_empty() {
        return preamble.to_string();
    }
    let needle = format!("import {module}");
    let mut from = 0;
    loop {
        let Some(rel) = preamble[from..].find(&needle) else {
            return preamble.to_string(); // module not imported
        };
        let start = from + rel;
        let at_line_start = start == 0 || preamble.as_bytes()[start - 1] == b'\n';
        let after = &preamble[start + needle.len()..];
        // Exact module: next char ends the word (newline) or opens a clause (space).
        if at_line_start && (after.starts_with('\n') || after.starts_with(' ')) {
            let rest = &preamble[start..];
            let line_len = rest.find('\n').map_or(rest.len(), |i| i + 1);
            let line = &rest[..line_len];
            let mut all: Vec<String> = match (line.find('('), line.rfind(')')) {
                (Some(po), Some(pc)) if po < pc => line[po + 1..pc]
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect(),
                _ => Vec::new(),
            };
            for &n in extra {
                let e = hiding_entry(n);
                if !all.contains(&e) {
                    all.push(e);
                }
            }
            let new_line = format!("import {module} hiding ({})\n", all.join(", "));
            return format!(
                "{}{}{}",
                &preamble[..start],
                new_line,
                &preamble[start + line_len..]
            );
        }
        from = start + needle.len();
    }
}

/// Start a user-code module: the imports-injected preamble, the `-- [user]`
/// marker, and the optional `input :: Aeson.Value` binding. The shared prefix of
/// every `wrap_*` source builder.
/// Locate the user's code inside a wrapped module source by the markers the
/// wrappers emit, returning `(line_offset, col_indent)` for coordinate remap.
/// `__user =` (pure-ref / shared eval template, optionally `do`-wrapped) is
/// checked FIRST — those templates also contain a `__result` binding, but the
/// user text lives under `__user`. Bind wrappers emit no `__user`, so their
/// `__result = do` is the marker.
fn user_code_offset(source: &str) -> Option<(usize, usize)> {
    // Verbatim embeddings (pure-ref / probe / shared eval template): user
    // text starts right after the bracket, at its ORIGINAL columns — line
    // offset only, no column indent.
    for marker in ["__user = let {\n __b =\n", "__probe = let {\n __b =\n"] {
        if let Some(pos) = source.find(marker) {
            return Some((source[..pos + marker.len()].matches('\n').count(), 0));
        }
    }
    // Bind wrappers: verbatim inside `__result = do {`. (A `let` bind's first
    // line gains 2 columns from the decl-brace boundary edit — accepted.)
    const RESULT_DO: &str = "\n__result = do {\n";
    source
        .find(RESULT_DO)
        .map(|pos| (source[..pos + RESULT_DO.len()].matches('\n').count(), 0))
}

/// 1-based inclusive line count of `text` as it lands in an assembled module:
/// every wrap_* shape embeds the caller's text verbatim, forcing at most one
/// trailing `\n` if it's missing (see `push_verbatim_binding`/`place_turn_stmt`),
/// and inserts no other line before the closing scaffold — so this count, paired
/// with `user_code_offset`'s start line, gives the exact end line without
/// re-scanning the assembled source.
fn text_line_count(text: &str) -> usize {
    if text.is_empty() {
        1
    } else if text.ends_with('\n') {
        text.matches('\n').count()
    } else {
        text.matches('\n').count() + 1
    }
}

/// The 1-based inclusive `(start, end)` line range of `text` within `wrapped`
/// (a module assembled by one of the `wrap_*` builders below), for the
/// `--user-code-lines` extract flag. `None` when `wrapped` carries none of the
/// markers `user_code_offset` recognizes.
fn user_code_line_range(wrapped: &str, text: &str) -> Option<(usize, usize)> {
    let (offset, _indent) = user_code_offset(wrapped)?;
    let start = offset + 1;
    Some((start, start + text_line_count(text) - 1))
}

/// Remap `Expr.hs:<L>:<C>` GHC coordinates in a compile error to item-relative
/// ones (`<item>:l:c`), using the wrapped source the error was produced from.
/// Foreign paths pass through; unknown wrappers return the error untouched.
/// Prepend the greppable failure-class/phase tag line onto a repl error message
/// so a caller can branch on class/phase, exactly as the MCP server's envelope
/// does. Both servers share the ONE `tidepool_runtime` classifier; this only
/// formats; routing decisions never inspect this rendered text.
fn tag_failure(class: FailureClass, phase: Phase, body: String) -> String {
    format!(
        "**failure-class:** `{}`  **phase:** `{}`\n{}",
        class.tag(),
        phase.tag(),
        body
    )
}

/// Render a structured [`CompileError`] to plain text: when it carries
/// structured diagnostics, render them item-relative via
/// [`tidepool_runtime::diag::render_diagnostics`] using `source`'s own wrapper
/// markers (`user_code_offset`) and `user_lines` (the caller-computed `(start,
/// end)` range of the user's own submitted text, or `None` for an internal
/// probe with nothing to partition against). Any OTHER `CompileError` variant
/// (version-skew, infra, IO) carries no `Expr.hs:N:M` coordinates, so
/// `classify_compile`'s message is used as-is.
fn render_compile_fail_body(
    err: &CompileError,
    source: &str,
    user_lines: Option<(usize, usize)>,
) -> String {
    match err {
        CompileError::Diagnostics(diags) => {
            let (line_offset, col_indent) = user_code_offset(source).unwrap_or((0, 0));
            let rendered = tidepool_runtime::diag::render_diagnostics(
                diags,
                &tidepool_runtime::diag::RenderOpts {
                    anchor: "Expr.hs",
                    label: "<item>",
                    user_lines: user_lines.as_ref().map(std::slice::from_ref),
                    line_offset,
                    col_indent,
                    drop_foreign_gen_warnings_except: None,
                    source,
                },
            );
            prepend_lib_brick_hint(rendered)
        }
        _ => classify_compile(err).message,
    }
}

/// A compile-phase failure envelope from a structured [`CompileError`] (the
/// eval/bind/reference path): classify it and render the body via
/// [`render_compile_fail_body`].
fn compile_fail(err: &CompileError, source: &str, user_lines: Option<(usize, usize)>) -> String {
    let env = classify_compile(err);
    let body = render_compile_fail_body(err, source, user_lines);
    tag_failure(env.class, env.phase, body)
}

/// A compile-phase failure envelope from the declaration path's [`SessionError`].
/// Compiler errors use the owning classifier's message so structured GHC
/// diagnostics and actionable wire-version details are not replaced by the
/// outer enum's terse `Display` text.
fn session_fail(err: &SessionError, prefix: &str) -> String {
    let env = classify_session(err);
    let detail = match err {
        SessionError::Compile(_) => env.message.clone(),
        _ => err.to_string(),
    };
    tag_failure(env.class, env.phase, format!("{prefix}: {detail}"))
}

/// A run-phase JIT/eval failure envelope (always runtime/run), with `context`
/// naming the site (e.g. "runtime error", "JIT compile error").
fn run_fail(context: &str, detail: impl std::fmt::Display) -> String {
    tag_failure(
        FailureClass::Runtime,
        Phase::Run,
        format!("{context}: {detail}"),
    )
}

/// The result binding typed to IO — a compile-phase user-Haskell constraint.
/// (Shared across the eval/bind/multi-bind/reference paths.)
fn io_type_fail() -> TurnOutcome {
    TurnOutcome::Error(tag_failure(
        FailureClass::UserHaskell,
        Phase::Compile,
        "IO type detected in result binding. IO operations are not supported.".into(),
    ))
}

/// If EVERY error location in a failed compile points at a project-lib module
/// (`.tidepool/lib/…` / global `…/tidepool/lib/…`) rather than the user's
/// `<item>`, the user's item is fine — a library module is broken and the
/// auto-imported `Library` re-exports it, so it fails to compile for every
/// eval (friction #24, the "bricked lib module" class). Say so and name the
/// culprit(s) instead of leaving a baffling error against code the user
/// didn't just write.
fn prepend_lib_brick_hint(err: String) -> String {
    // A `<item>:` coordinate means the user's own code is implicated — never
    // reframe then.
    if err.contains("<item>:") {
        return err;
    }
    let mut culprits: Vec<String> = err
        .lines()
        .filter_map(|l| {
            let hs = l.find(".hs:")?;
            let seg = &l[..hs + 3];
            let start = seg.rfind(char::is_whitespace).map_or(0, |i| i + 1);
            let path = &seg[start..];
            (path.contains(".tidepool/lib/") || path.contains("/tidepool/lib/"))
                .then(|| path.to_string())
        })
        .collect();
    if culprits.is_empty() {
        return err;
    }
    culprits.sort_unstable();
    culprits.dedup();
    format!(
        "your item is fine — a project library module is broken and `Library` \
         auto-imports it, so it won't compile for ANY eval until fixed: {}\n\
         (fix or remove the module; if the break also blocks a `writeFile` repair, \
         edit it host-side — the session can't compile its own fix while `Library` \
         is broken)\n\n{err}",
        culprits.join(", ")
    )
}

fn begin_user_module(preamble: &str, imports: &str, input: Option<&serde_json::Value>) -> String {
    let mut out = insert_preamble_imports(preamble, imports);
    out.push_str("-- [user]\n");
    out.push_str(&input_binding_source(input));
    out
}

/// Append `name = <text>` with the user text embedded VERBATIM — no
/// indentation transform. Explicit `let { }` brackets suspend the layout
/// algorithm (Report rule L, explicit context), so unindented user lines are
/// legal and quasiquote payloads keep byte-exact fidelity: per-line indenting
/// would corrupt a multi-line quasiquote payload's byte offsets. `__b` is
/// local to each RHS.
fn push_verbatim_binding(out: &mut String, name: &str, text: &str) {
    out.push_str(name);
    out.push_str(" = let {\n __b =\n");
    out.push_str(text);
    if !text.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(" } in __b\n");
}

/// Wrap a BIND turn into an `Eff`-typed module whose `result` runs the bind
/// statement and yields the bound value, so `run_fragment_and_bind` reduces the
/// effect tree and roots that value. The monad is pinned to the session effect
/// stack (`Eff <stack> _`, the value type inferred via `PartialTypeSignatures`,
/// which the preamble enables) — a bare `pure (42 :: Int)` action would
/// otherwise leave the monad ambiguous. Delegates to
/// [`assemble_bind_module`], the mechanism shared with `tidepool-harness`'s
/// own `template_session_bind`/`session_bind_template`.
fn wrap_bind_source(
    preamble: &str,
    effect_stack: &str,
    imports: &str,
    turn_text: &str,
    binder: &str,
    input: Option<&serde_json::Value>,
) -> String {
    assemble_bind_module(
        &insert_preamble_imports(preamble, imports),
        &input_binding_source(input),
        "__result",
        effect_stack,
        &place_turn_stmt(turn_text),
        binder,
        false,
    )
}

/// Wrap a DISCARD-BIND turn (`_ <- e`, `(_, _) <- e`) into an `Eff`-typed
/// module whose `__result` runs the whole statement for its effects and
/// yields `()` — shaped identically to [`wrap_bind_source`] but with no
/// binder spliced, since a discarding bind mints no session value.
fn wrap_bind_discard_source(
    preamble: &str,
    effect_stack: &str,
    imports: &str,
    turn_text: &str,
    input: Option<&serde_json::Value>,
) -> String {
    assemble_bind_module(
        &insert_preamble_imports(preamble, imports),
        &input_binding_source(input),
        "__result",
        effect_stack,
        &place_turn_stmt(turn_text),
        "()",
        false,
    )
}

/// Wrap a MULTI-BIND turn into an `Eff`-typed module whose `__result` runs the
/// bind statement and yields a tuple of all bound names. For `(a, b) <- action`
/// with `names = ["a", "b"]` this emits:
/// ```haskell
/// __result :: Eff <stack> _
/// __result = do
///   (a, b) <- action
///   pure (a, b)
/// ```
/// The tuple field order matches the binder order in the JSON sidecar.
fn wrap_multi_bind_source(
    preamble: &str,
    effect_stack: &str,
    imports: &str,
    turn_text: &str,
    names: &[String],
    input: Option<&serde_json::Value>,
) -> String {
    let tuple_expr = format!("({})", names.join(", "));
    assemble_bind_module(
        &insert_preamble_imports(preamble, imports),
        &input_binding_source(input),
        "__result",
        effect_stack,
        &place_turn_stmt(turn_text),
        &tuple_expr,
        false,
    )
}

/// Wrap a bare EXPRESSION as a MATERIALIZING bind of `it` — the monadic
/// attempt (tried first by [`Session::run_bare_expr`]): `it <- __user`, an
/// `Eff`-typed action, exactly like a real `x <- e` bind. `__user` is hoisted
/// to a module-level binding so a trailing `where` still attaches legally.
///
/// `result` yields `(it, toWire it)` — see [`Session::run_bare_expr`]'s doc
/// for why single-compiling that tuple keeps effects-fire-once and aliasing
/// safe.
fn wrap_bare_it_monadic(
    preamble: &str,
    effect_stack: &str,
    imports: &str,
    expr_text: &str,
    input: Option<&serde_json::Value>,
) -> String {
    let mut out = begin_user_module(preamble, imports, input);
    push_verbatim_binding(&mut out, "__user", expr_text);
    out.push('\n');
    out.push_str(&format!("__result :: Eff {effect_stack} _\n"));
    out.push_str("__result = do {\n it <- __user ; pure (it, toWire it)\n }\n");
    out
}

/// Pure-fallback sibling of [`wrap_bare_it_monadic`], tried when the monadic
/// wrap fails to compile (`__user`'s type doesn't unify with the session's
/// `Eff` stack — a bare non-monadic expression like `x + 1` / `v ^? key …`).
/// `let` doesn't require its RHS to unify with the do-block's monad, so this
/// binds `it` materially without invoking any effect. Same single-compile
/// `(it, toWire it)` tuple as the monadic wrap.
fn wrap_bare_it_pure(
    preamble: &str,
    effect_stack: &str,
    imports: &str,
    expr_text: &str,
    input: Option<&serde_json::Value>,
) -> String {
    let mut out = begin_user_module(preamble, imports, input);
    push_verbatim_binding(&mut out, "__user", expr_text);
    out.push('\n');
    out.push_str(&format!("__result :: Eff {effect_stack} _\n"));
    out.push_str("__result = do {\n let { it = __user } ; pure (it, toWire it)\n }\n");
    out
}

/// Wrap a PURE reference turn as `result = <expr>` (no `Eff`), run via
/// `run_fragment_pure`. For bare value references like `x + 1` / `f 10` that
/// are not monadic.
///
/// Emits a SECOND `__user = <expr>` binding whose sole purpose is type
/// capture: the extractor reads the inferred type off `__user`
/// (`capturedUserType`), so the reference turn can report `{type, value}`
/// instead of `type: null`. Unused at runtime and harmless (session compiles
/// are not `-Werror`).
///
fn wrap_pure_ref_source(
    preamble: &str,
    imports: &str,
    expr_text: &str,
    input: Option<&serde_json::Value>,
) -> String {
    let mut out = begin_user_module(preamble, imports, input);
    push_verbatim_binding(&mut out, "__user", expr_text);
    out.push('\n');
    push_verbatim_binding(&mut out, "__result", expr_text);
    out
}

/// Wrap an expression for the INNER-TYPE probe. Hoists the whole expression to a
/// module-level `__probe = <expr>` binding (where a trailing `where` attaches
/// legally — a do-statement `__t <- expr where …` does NOT parse), then binds
/// `__t <- __probe` so GHC's monadic bind peels `Eff es a` to the inner `a`. That
/// is the same type-directed peel the `x <- e` bind path uses — no TyCon
/// name-matching. `__probe` is typecheck scaffolding only: the compile targets
/// `__result`, so it is never serialized into the turn.
fn wrap_probe_source(
    preamble: &str,
    effect_stack: &str,
    imports: &str,
    expr_text: &str,
    input: Option<&serde_json::Value>,
) -> String {
    let mut out = begin_user_module(preamble, imports, input);
    push_verbatim_binding(&mut out, "__probe", expr_text);
    out.push('\n');
    out.push_str(&format!("__result :: Eff {effect_stack} _\n"));
    out.push_str("__result = do\n");
    out.push_str("  __t <- __probe\n");
    out.push_str("  pure __t\n");
    out
}

/// Normalize a PURE bind of `name` to its equivalent top-level declaration
/// `name = <rhs>`. `let name = e` → `name = e`; `name <- pure e` /
/// `name <- return e` → `name = e`. Returns `None` for effectful binds or
/// shapes we don't confidently normalize (which then take the materialize
/// path). `pure`/`return` of a value is semantically pure, so unwrapping one
/// layer is exact.
fn pure_bind_to_decl(expr_text: &str, name: &str) -> Option<String> {
    let t = expr_text.trim();
    // `let name = e` (single binder — classify already gave one binder `name`).
    if let Some(rest) = t.strip_prefix("let ") {
        let rest = rest.trim_start();
        // Guard against `let { … }` explicit-brace / pattern shapes.
        if rest.starts_with(name) {
            return Some(rest.trim().to_string());
        }
        return None;
    }
    // `name <- pure e` / `name <- return e`.
    let after_name = t.strip_prefix(name)?.trim_start();
    let rhs = after_name.strip_prefix("<-")?.trim_start();
    for kw in ["pure ", "return "] {
        if let Some(e) = rhs.strip_prefix(kw) {
            return Some(format!("{name} = {}", e.trim()));
        }
    }
    None
}

/// Compute the slim inline JSON result for one block item (the default shape).
/// Fields are merged directly into the item object in `Block::render()`.
/// The `value` key is present here but stripped for the final expression item
/// (see `run_block` — `obj.remove("value")` after the loop).
fn slim_item_result(outcome: &TurnOutcome) -> serde_json::Value {
    match outcome {
        TurnOutcome::Bound { name, type_display } => serde_json::json!({
            "bound": name,
            "type": type_display,
        }),
        TurnOutcome::MultiBound { components } => serde_json::json!({
            "bound": components.iter().map(|c| &c.name).collect::<Vec<_>>(),
            "types": components.iter().map(|c| &c.type_display).collect::<Vec<_>>(),
        }),
        TurnOutcome::Defined { declarations, .. } => {
            let metadata: Vec<serde_json::Value> = declarations
                .iter()
                .map(DeclarationMetadata::to_json)
                .collect();
            if let [declaration] = metadata.as_slice() {
                let mut obj = declaration.clone();
                obj["decl"] = obj["name"].take();
                obj["declKind"] = obj["kind"].take();
                if let Some(map) = obj.as_object_mut() {
                    map.remove("name");
                    map.remove("kind");
                }
                obj
            } else {
                serde_json::json!({
                    "decl": declarations
                        .iter()
                        .map(DeclarationMetadata::name)
                        .collect::<Vec<_>>(),
                    "declarations": metadata,
                })
            }
        }
        TurnOutcome::Value {
            value,
            type_display,
            truncated,
        } => {
            let mut obj = serde_json::Map::new();
            if let Some(t) = type_display {
                obj.insert("type".into(), serde_json::json!(t));
            }
            obj.insert("value".into(), value.clone());
            if let Some(hint) = truncated {
                obj.insert("truncated".into(), serde_json::json!(hint));
            }
            serde_json::Value::Object(obj)
        }
        TurnOutcome::Meta(v) => v.clone(),
        TurnOutcome::Inspection(output) => serde_json::json!({ "output": output }),
        TurnOutcome::Error(e) => serde_json::json!({ "error": e }),
        TurnOutcome::Block { .. } => serde_json::json!({ "error": "nested block" }),
    }
}

/// Extract the declared head name from a Haskell type declaration string.
/// Returns `Some(name)` when the string starts with `data`/`newtype`/`type`
/// and the next token is the type name; `None` for functions, instances, etc.
#[cfg(test)]
fn type_def_head(src: &str) -> Option<&str> {
    let s = src.trim();
    let rest = s
        .strip_prefix("data ")
        .or_else(|| s.strip_prefix("newtype "))
        .or_else(|| s.strip_prefix("type "))?;
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '(' || c == '=')
        .unwrap_or(rest.len());
    let head = &rest[..end];
    if head.is_empty() {
        None
    } else {
        Some(head)
    }
}

/// Render the `:browse` result. Bare (`None`) lists every effect with its
/// one-line description; `Some(name)` (case-insensitive) lists that effect's
/// helper verbs (`name :: signature`) and constructors, or — for an unknown
/// name — an `error` payload naming the valid effects (the #319 not-ok
/// convention: a `Meta` object carrying an `error` key surfaces as not-ok).
fn browse_effects(decls: &[EffectDecl], only: Option<&str>) -> serde_json::Value {
    match only {
        None => {
            let effects: Vec<serde_json::Value> = decls
                .iter()
                .map(|d| {
                    serde_json::json!({
                        "effect": d.type_name,
                        "description": first_sentence(d.description),
                    })
                })
                .collect();
            serde_json::json!({
                "effects": effects,
                "hint": ":browse <Effect> (case-insensitive) lists that effect's verbs + constructors.",
            })
        }
        Some(name) => match decls
            .iter()
            .find(|d| d.type_name.eq_ignore_ascii_case(name))
        {
            Some(d) => {
                let verbs = tidepool_mcp::authored_helper_signatures(d);
                let constructors = tidepool_mcp::authored_constructors(d);
                let types = tidepool_mcp::authored_type_definitions(d);
                serde_json::json!({
                    "effect": d.type_name,
                    "description": d.description,
                    "verbs": verbs,
                    "constructors": constructors,
                    // Real field names, straight from the type_def — the doc
                    // prose above can drift, this can't (#346).
                    "types": types,
                })
            }
            None => {
                let valid: Vec<&str> = decls.iter().map(|d| d.type_name).collect();
                serde_json::json!({
                    "error": format!(
                        "unknown effect '{name}'; valid effects: {}",
                        valid.join(", ")
                    ),
                    "effects": valid,
                })
            }
        },
    }
}

#[cfg(test)]
mod slim_tests {
    use super::{browse_effects, EffectDecl};
    use super::{pure_bind_to_decl, slim_item_result};

    /// Two-effect fixture mirroring the real decl shape: a comment-prefixed
    /// helper (so the sig line is not the first line) and a multi-sentence
    /// description.
    fn sample_decls() -> Vec<EffectDecl> {
        vec![
            EffectDecl {
                type_name: "Exec",
                description: "Run shell commands. And capture output.",
                constructors: &["Run :: Text -> Exec (Int, Text, Text)"],
                type_defs: &[],
                extra_imports: &[],
                helpers: &[
                    "-- | Run a shell command; returns a `Proc`.\nrun :: Text -> M Proc\nrun cmd = undefined",
                    "readProcess :: Text -> M Text\nreadProcess cmd = undefined",
                ],
                type_params: &[],
                default_row_args: &[],
                prompt_card: None,
                helpers_row_polymorphic: false,
            },
            EffectDecl {
                type_name: "KV",
                description: "Persistent key-value store.",
                constructors: &["KvSet :: Text -> Value -> KV ()"],
                type_defs: &[],
                extra_imports: &[],
                helpers: &["kvSet :: Text -> Value -> M ()\nkvSet k v = undefined"],
                type_params: &[],
                default_row_args: &[],
                prompt_card: None,
                helpers_row_polymorphic: false,
            },
        ]
    }

    #[test]
    fn browse_bare_lists_effects_with_descriptions() {
        let v = browse_effects(&sample_decls(), None);
        let effects = v["effects"].as_array().expect("effects array");
        assert_eq!(effects.len(), 2);
        assert_eq!(effects[0]["effect"], "Exec");
        // One-line description only (first sentence).
        assert_eq!(effects[0]["description"], "Run shell commands.");
        assert_eq!(effects[1]["effect"], "KV");
        // Bare browse is not an error.
        assert!(v.get("error").is_none());
    }

    #[test]
    fn browse_known_effect_lists_verbs_and_constructors() {
        let v = browse_effects(&sample_decls(), Some("Exec"));
        assert_eq!(v["effect"], "Exec");
        let verbs: Vec<&str> = v["verbs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap())
            .collect();
        assert!(verbs.contains(&"run :: Text -> M Proc"), "verbs: {verbs:?}");
        assert!(verbs.contains(&"readProcess :: Text -> M Text"));
        let cons = v["constructors"].as_array().unwrap();
        assert_eq!(cons[0], "Run :: Text -> Exec (Int, Text, Text)");
        assert!(v.get("error").is_none());
    }

    #[test]
    fn browse_is_case_insensitive() {
        let v = browse_effects(&sample_decls(), Some("exec"));
        assert_eq!(v["effect"], "Exec");
        assert!(v.get("error").is_none());
    }

    #[test]
    fn browse_unknown_effect_errors_with_candidates() {
        let v = browse_effects(&sample_decls(), Some("Nope"));
        let err = v["error"].as_str().expect("error string");
        // Names the valid effects so the caller can retry.
        assert!(err.contains("Exec"), "{err}");
        assert!(err.contains("KV"), "{err}");
        assert!(err.contains("Nope"), "{err}");
    }

    #[test]
    fn pure_bind_to_decl_normalizes() {
        // let x = e  ->  x = e
        assert_eq!(
            pure_bind_to_decl("let n = 5", "n").as_deref(),
            Some("n = 5")
        );
        // x <- pure e  ->  x = e
        assert_eq!(
            pure_bind_to_decl("xs <- pure []", "xs").as_deref(),
            Some("xs = []")
        );
        // x <- return e  ->  x = e
        assert_eq!(
            pure_bind_to_decl("y <- return (foo bar)", "y").as_deref(),
            Some("y = (foo bar)")
        );
        // effectful bind: not normalized (falls back to materialize)
        assert_eq!(pure_bind_to_decl("p <- run \"ls\"", "p"), None);
        // let with a type annotation on the RHS survives
        assert_eq!(
            pure_bind_to_decl("let d = 5 :: Double", "d").as_deref(),
            Some("d = 5 :: Double")
        );
    }

    use crate::command::{DeclarationMetadata, TurnOutcome};

    #[test]
    fn lib_brick_hint_reframes_lib_only_errors() {
        let lib_err = "/home/u/proj/.tidepool/lib/Repo.hs:31:12: error: [GHC-88464]\n    Variable not in scope: readGlob\nCompilation failed.\n";
        let out = super::prepend_lib_brick_hint(lib_err.to_string());
        assert!(out.starts_with("your item is fine"), "{out}");
        assert!(out.contains(".tidepool/lib/Repo.hs"), "{out}");
        // A user-item error is never reframed.
        let user_err = "<item>:2:1: error: not in scope: foo\n";
        assert_eq!(
            super::prepend_lib_brick_hint(user_err.to_string()),
            user_err
        );
        // A base/non-lib error is not reframed.
        let base_err = "Expr.hs:5:1: error: whatever\n";
        assert_eq!(
            super::prepend_lib_brick_hint(base_err.to_string()),
            base_err
        );
    }

    #[test]
    fn slim_item_result_shapes() {
        let bound = TurnOutcome::Bound {
            name: "vs".into(),
            type_display: "[Text]".into(),
        };
        let r = slim_item_result(&bound);
        assert_eq!(r["bound"], "vs");
        assert_eq!(r["type"], "[Text]");

        let defined = TurnOutcome::Defined {
            generation: 1,
            module: "Tidepool.Session.Lib.G1".into(),
            declarations: vec![DeclarationMetadata::Value {
                name: "slug".into(),
                type_display: Some("Text -> Text".into()),
            }],
        };
        let r = slim_item_result(&defined);
        assert_eq!(r["decl"], "slug");
        assert_eq!(r["type"], "Text -> Text", "inferred type painted (#317)");
        assert!(r.get("generation").is_none(), "no generation in slim decl");
        assert!(r.get("module").is_none(), "no module in slim decl");

        // A non-value decl (or a probe failure) carries no `type` field.
        let defined_no_type = TurnOutcome::Defined {
            generation: 1,
            module: "Tidepool.Session.Lib.G1".into(),
            declarations: vec![DeclarationMetadata::Type {
                name: "Node".into(),
                constructors: vec!["Node".into()],
            }],
        };
        let r = slim_item_result(&defined_no_type);
        assert_eq!(r["decl"], "Node");
        assert_eq!(r["declKind"], "type");
        assert_eq!(r["constructors"], serde_json::json!(["Node"]));
        assert!(r.get("type").is_none(), "no type key for a non-value decl");
    }
}

#[cfg(test)]
mod info_tests {
    use super::type_def_head;
    use tidepool_mcp::subagent_decl;

    #[test]
    fn type_def_head_recognizes_data_newtype_type() {
        assert_eq!(
            type_def_head("data Node = Node { nodeName :: Text }"),
            Some("Node")
        );
        assert_eq!(
            type_def_head("data Position = Position { posLine :: Int }"),
            Some("Position")
        );
        assert_eq!(type_def_head("data Lang = Rust | Python"), Some("Lang"));
        assert_eq!(type_def_head("newtype Foo = Foo Int"), Some("Foo"));
        assert_eq!(type_def_head("type Name = Text"), Some("Name"));
        // Non-type-decl strings return None.
        assert_eq!(type_def_head("matchVars :: Match -> Map Text Text"), None);
        assert_eq!(type_def_head("instance ToJSON Node where"), None);
        assert_eq!(type_def_head("nodeLine :: Node -> Int"), None);
    }

    #[test]
    fn decl_scan_finds_node_in_subagent_decl() {
        let decl = subagent_decl();
        let found = decl
            .type_defs
            .iter()
            .any(|td| type_def_head(td) == Some("WorkerRun"));
        assert!(
            found,
            "WorkerRun must be discoverable in subagent_decl type_defs"
        );
        let id_found = decl
            .type_defs
            .iter()
            .any(|td| type_def_head(td) == Some("AgentId"));
        assert!(
            id_found,
            "AgentId must be discoverable in subagent_decl type_defs"
        );
    }
}

#[cfg(test)]
mod hiding_tests {
    use super::{hide_module_names, hide_prelude_names};

    #[test]
    fn prelude_hiding_parenthesizes_operators() {
        let pre = "import Tidepool.Prelude hiding (error)\n";
        let out = hide_prelude_names(pre, &[".+", "slug"]);
        // operator parenthesized, plain name bare; no bare `.+` (parse error)
        assert!(out.contains("(.+)"), "op must be parenthesized: {out}");
        assert!(out.contains("slug"), "plain name present: {out}");
        assert!(
            !out.contains(" .+,") && !out.contains(", .+)"),
            "bare operator leaked: {out}"
        );
    }

    #[test]
    fn library_hiding_added_with_operators() {
        let pre = "import Library\nimport qualified Prelude as P\n";
        let out = hide_module_names(pre, "Library", &[".+", "sh"]);
        assert!(
            out.contains("import Library hiding ("),
            "Library gets a hiding clause: {out}"
        );
        assert!(
            out.contains("(.+)") && out.contains("sh"),
            "names hidden: {out}"
        );
    }

    #[test]
    fn library_hiding_noop_without_import() {
        let pre = "import Tidepool.Prelude hiding (error)\n";
        assert_eq!(
            hide_module_names(pre, "Library", &["sh"]),
            pre,
            "no Library import → unchanged"
        );
    }

    #[test]
    fn effects_hiding_shadows_verbs() {
        // A session-owned name (e.g. a `let glob = …` value bind) must shadow the
        // generated `Tidepool.Effects` verb of the same name instead of an
        // ambiguous occurrence — the verb/value-plane collision footgun.
        let pre = "import Tidepool.Effects\nimport qualified Prelude as P\n";
        let out = hide_module_names(pre, "Tidepool.Effects", &["glob"]);
        assert!(
            out.contains("import Tidepool.Effects hiding (glob)"),
            "Effects verb shadowed by a session name: {out}"
        );
    }

    #[test]
    fn orchestrate_hiding_shadows_helpers() {
        // A session-owned name (e.g. a `let memo = …` value bind) must shadow the
        // generated `Tidepool.Orchestrate` helper of the same name instead of an
        // ambiguous occurrence — the namespace-poison bug class this fix targets.
        let pre = "import Tidepool.Orchestrate\nimport qualified Prelude as P\n";
        let out = hide_module_names(pre, "Tidepool.Orchestrate", &["memo"]);
        assert!(
            out.contains("import Tidepool.Orchestrate hiding (memo)"),
            "Orchestrate helper shadowed by a session name: {out}"
        );
    }
}

#[cfg(test)]
mod reset_tests {
    use super::{
        BoxedStack, EffectRoster, Generation, MetaCommand, ModuleEnv, PureBind, Session,
        SessionConfig, SessionId, StackFactory, TurnOutcome, DEFAULT_NURSERY_SIZE,
    };

    /// A handler-stack factory for a test session that never dispatches an
    /// effect: these unit tests exercise decl-plane / cancel-slot bookkeeping,
    /// not the effect path.
    fn no_handlers() -> StackFactory {
        Box::new(|| Box::new(frunk::HNil) as BoxedStack)
    }

    fn minimal_config(root: std::path::PathBuf) -> SessionConfig {
        SessionConfig {
            id: SessionId(1),
            root,
            base_include: Vec::new(),
            roster: EffectRoster::from_handlers(&frunk::HNil),
            preamble: String::new(),
            effect_stack: String::new(),
            module_env: ModuleEnv::standalone_default(),
            nursery_size: DEFAULT_NURSERY_SIZE,
        }
    }

    /// A failed `SessionLib::open` inside `:reset` must leave the session's
    /// state COMPLETELY untouched — not a half-reset where the value plane /
    /// turn counter were already cleared before the reopen was even
    /// attempted. `SessionLib::open` only does `fs::create_dir_all`, so this
    /// forces a real IO failure with no GHC/extract dependency.
    #[test]
    fn reset_leaves_state_untouched_when_reopen_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut session = Session::open(minimal_config(dir.path().to_path_buf()), no_handlers())
            .expect("session opens on a fresh dir");

        // Poke markers into state that a failed reopen must NOT clear.
        session.core.set_val_gen(Generation(3));
        session.pure_binds.insert(
            "marker".to_string(),
            PureBind {
                type_display: "Int".to_string(),
                gen: Generation(1),
            },
        );

        // Sabotage `cfg.root` so the reopen inside `:reset` fails: replace the
        // existing directory with a plain file at the same path.
        std::fs::remove_dir_all(&session.cfg.root).expect("remove session root");
        std::fs::write(&session.cfg.root, b"not a directory").expect("write blocker file");

        let outcome = session.run_meta(&MetaCommand::Reset);
        assert!(
            matches!(outcome, TurnOutcome::Error(_)),
            "a failed reopen must surface as an error: {outcome:?}"
        );

        assert_eq!(
            session.core.val_gen(),
            Generation(3),
            "a failed reopen must not clear val_gen"
        );
        assert!(
            session.pure_binds.contains_key("marker"),
            "a failed reopen must not clear pure_binds — the old decl log must stay usable"
        );
    }

    /// Failure injection for the decl → materialized transition.  Retraction
    /// writes a new Lib generation, so block that write after a real decl has
    /// committed and prove the REPL neither installs the value nor discards its
    /// pure-bind metadata while still reporting the error.
    #[test]
    fn failed_later_retraction_does_not_commit_any_materialized_component() {
        use tidepool_codegen::binding_table::{BindingEntry, BoundValue};
        use tidepool_codegen::old_space::RootSlot;
        use tidepool_codegen::scope::ScopeId;
        use tidepool_repr::{BindingName, SessionModule, SessionVarId};

        tidepool_testing::eval_harness::require_extract();
        let dir = tempfile::tempdir().expect("tempdir");
        let mut session = Session::open(minimal_config(dir.path().to_path_buf()), no_handlers())
            .expect("session opens");
        assert!(matches!(
            session.run_def("y = 1 :: Int"),
            TurnOutcome::Defined { .. }
        ));
        session.pure_binds.insert(
            "x".to_string(),
            PureBind {
                type_display: "Int".to_string(),
                gen: Generation(1),
            },
        );
        session.pure_binds.insert(
            "y".to_string(),
            PureBind {
                type_display: "Int".to_string(),
                gen: Generation(1),
            },
        );

        // `Lib.G1.hs` is already durable. Replacing its parent with a file
        // makes the G2 retraction write fail without changing that committed
        // declaration generation.
        let tidepool_dir = dir.path().join("Tidepool");
        std::fs::remove_dir_all(&tidepool_dir).expect("remove generated module tree");
        std::fs::write(&tidepool_dir, b"retraction write blocker").expect("write blocker");

        let mut first_root: *mut u8 = std::ptr::null_mut();
        let mut later_root: *mut u8 = std::ptr::null_mut();
        // SAFETY: this test only verifies table bookkeeping after the fallible
        // retraction; the slot is never dereferenced or executed.
        let first_slot = unsafe { RootSlot::new(&mut first_root as *mut *mut u8) };
        let later_slot = unsafe { RootSlot::new(&mut later_root as *mut *mut u8) };
        let result = session.core.bind_replacing_decls(vec![
            BindingEntry {
                // This first component has no declaration to retract. The old
                // per-entry loop committed it before failing on `y` below.
                name: BindingName("x".to_string()),
                id: SessionVarId::from_extract((0xFE << 56) | 99),
                module: SessionModule::val(Generation(99)),
                value: BoundValue::Tier0Forced(first_slot),
                type_display: Some("Int".to_string()),
                defining_expr: Some("pure 2".to_string()),
                scope: ScopeId::ROOT,
            },
            BindingEntry {
                name: BindingName("y".to_string()),
                id: SessionVarId::from_extract((0xFE << 56) | 100),
                module: SessionModule::val(Generation(99)),
                value: BoundValue::Tier0Forced(later_slot),
                type_display: Some("Int".to_string()),
                defining_expr: Some("pure 3".to_string()),
                scope: ScopeId::ROOT,
            },
        ]);

        assert!(
            result.is_err(),
            "failed durable retraction must fail the bind"
        );
        assert!(
            session.core.resolve_in(ScopeId::ROOT, "x").is_none()
                && session.core.resolve_in(ScopeId::ROOT, "y").is_none(),
            "failed later retraction must not install any materialized component"
        );
        assert!(
            session.pure_binds.contains_key("x"),
            "failed retraction must retain frontend pure-bind metadata"
        );
        assert!(
            session.pure_binds.contains_key("y"),
            "failed retraction must retain later-component pure-bind metadata"
        );
        assert!(
            session.core.lib().decl_value_names().contains(&"y"),
            "failed retraction must retain the last committed declaration"
        );
    }

    /// An in-block `:reset` drops the resident machine WITHOUT a
    /// bootstrap (which is what pairs machine creation with `publish_cancel`),
    /// so the shared `CancelSlot` must be cleared explicitly — otherwise it keeps the
    /// dropped machine's stale `CancelHandle` until the next bootstrap, and a
    /// server-side timeout in that window cancels a dead flag instead of a
    /// live one. Needs a real compiled machine (GHC extract), so this test
    /// mirrors `tests/common/mod.rs`'s minimal-stack setup at the bare
    /// `Session` level.
    #[test]
    fn reset_clears_stale_cancel_handle_from_slot() {
        tidepool_testing::eval_harness::require_extract();
        let stack = tidepool_handlers::build_minimal_stack();
        let roster = EffectRoster::from_handlers(&stack);
        let effects_dir = tidepool_mcp::ensure_effects_module(roster.decls())
            .expect("write Tidepool.Effects module");
        let prelude_dir = tidepool_testing::eval_harness::prelude_path();
        let module_env = tidepool_mcp::session_decl_module_env(roster.decls(), false);
        let preamble = tidepool_mcp::build_preamble_non_interactive_mode(
            roster.decls(),
            false,
            tidepool_mcp::PaginateMode::Passthrough,
        );
        let effect_stack = tidepool_mcp::build_effect_stack_type(roster.decls());

        let dir = tempfile::tempdir().expect("tempdir");
        let mut base_include = effects_dir.include_paths().to_vec();
        base_include.push(prelude_dir);
        let cfg = SessionConfig {
            id: SessionId(1),
            root: dir.path().to_path_buf(),
            base_include,
            roster,
            preamble,
            effect_stack,
            module_env,
            nursery_size: DEFAULT_NURSERY_SIZE,
        };
        let mut session = Session::open(cfg, no_handlers()).expect("session opens");

        let slot = crate::manager::empty_cancel_slot();
        session.set_cancel_slot(slot.clone());
        session.ensure_effect_machine();
        assert!(
            session.core.is_bootstrapped(),
            "machine must bootstrap from a trivial `pure ()` fragment"
        );
        assert!(
            slot.lock().is_some(),
            "bootstrap must publish a cancel handle into the shared slot"
        );

        session.run_meta(&MetaCommand::Reset);

        assert!(
            slot.lock().is_none(),
            "in-block :reset must clear the stale cancel handle, not leave the dropped \
             machine's handle in the shared slot"
        );
    }
}

/// Part 2 of the one-spawn-turn classification-equivalence corpus: proves a
/// [`tidepool_runtime::session::TurnTemplate`] rendered via
/// `render_template` produces BYTE-IDENTICAL output to this module's own
/// `wrap_*_source` builders for the same turn — the assertion that makes
/// "run_turn makes the same decision" mean something concrete. Lives here
/// (rather than as an integration test) because `wrap_bind_source` /
/// `wrap_multi_bind_source` / `begin_user_module` are private to this module.
#[cfg(test)]
mod turn_template_byte_identity_tests {
    use super::{begin_user_module, wrap_bind_source, wrap_multi_bind_source};
    use tidepool_runtime::session::{render_template, TemplateSelector, TurnTemplate};

    const PREAMBLE: &str = "{-# LANGUAGE NoImplicitPrelude #-}\nmodule Expr where\nimport Tidepool.Prelude\ndefault (Int, Double, Text)\n";
    const EFFECT_STACK: &str = "'[Console]";

    /// Mirrors `wrap_bind_source`'s shape exactly: one `{{TURN_STMT}}`
    /// placement splice, `pure {{BINDERS}}` BARE (no parens — a single
    /// binder is spliced as its bare name, exactly as `wrap_bind_source`
    /// does via `pure {binder}`).
    fn single_bind_template() -> TurnTemplate {
        let mut source = begin_user_module(PREAMBLE, "", None);
        source.push_str(&format!("__result :: Eff {EFFECT_STACK} _\n"));
        source.push_str("__result = do {\n");
        source.push_str("{{TURN_STMT}}");
        source.push_str(" ; pure {{BINDERS}}\n }\n");
        TurnTemplate {
            kind: TemplateSelector::Bind,
            source,
        }
    }

    /// Mirrors `wrap_multi_bind_source`'s shape: the bound names are yielded
    /// as a PARENTHESIZED tuple (`pure (a, b)`), not bare — `wrap_multi_bind_source`
    /// bakes the parens into its own `tuple_expr`, so the template bakes them
    /// around `{{BINDERS}}` too.
    fn multi_bind_template() -> TurnTemplate {
        let mut source = begin_user_module(PREAMBLE, "", None);
        source.push_str(&format!("__result :: Eff {EFFECT_STACK} _\n"));
        source.push_str("__result = do {\n");
        source.push_str("{{TURN_STMT}}");
        source.push_str(" ; pure ({{BINDERS}})\n }\n");
        TurnTemplate {
            kind: TemplateSelector::Bind,
            source,
        }
    }

    /// Four turn-text shapes: a plain single bind, a single bind whose text
    /// begins with `let ` (Part 2's named case — `place_turn_stmt`'s
    /// layout-safe `let { … }` rewrite before splicing into the `do` block,
    /// which a verbatim `{{TURN}}` splice cannot reproduce; `{{TURN_STMT}}`
    /// applies the identical normalization), and the same two shapes for a
    /// multi-bind pattern (`wrap_multi_bind_source` also routes through
    /// `place_turn_stmt`).
    #[test]
    fn turn_text_matches_wrap_source() {
        let cases: Vec<(&str, &str, Vec<String>)> = vec![
            ("x <- pure 1", "single", vec!["x".to_string()]),
            ("let y = 2", "single", vec!["y".to_string()]),
            (
                "(a, b) <- pure (1, 2)",
                "multi",
                vec!["a".to_string(), "b".to_string()],
            ),
            (
                "let (a, b) = (1, 2)",
                "multi",
                vec!["a".to_string(), "b".to_string()],
            ),
        ];

        for (turn_text, kind, names) in cases {
            let (template, expected) = if kind == "single" {
                (
                    single_bind_template(),
                    wrap_bind_source(PREAMBLE, EFFECT_STACK, "", turn_text, &names[0], None),
                )
            } else {
                (
                    multi_bind_template(),
                    wrap_multi_bind_source(PREAMBLE, EFFECT_STACK, "", turn_text, &names, None),
                )
            };
            let actual = render_template(&template.source, turn_text, &names);
            assert_eq!(actual, expected, "turn_text: {turn_text:?}");
        }
    }
}
