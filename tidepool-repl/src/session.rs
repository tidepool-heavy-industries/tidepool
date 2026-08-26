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
use tidepool_codegen::emit::ExternalEnv;
use tidepool_codegen::jit_machine::{ResumeInput, Suspendable, SuspendableOutcome};
use tidepool_codegen::old_space::RootSlot;
use tidepool_codegen::scope::ScopeId;
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_effect::pause::PauseGate;
use tidepool_eval::value::Value;
use tidepool_mcp::{
    first_sentence, helper_sig, input_binding_source, library_vocab, template_haskell_show_default,
    CapturedOutput, EffectDecl, EffectRoster,
};
use tidepool_repr::{
    BindingName, DataConTable, Generation, SessionId, SessionModule, SessionVarId,
};
use tidepool_runtime::session::{
    assemble_bind_module, classify_block, compile_session_turn, extract_ask_request,
    insert_preamble_imports, place_turn_stmt, subtract_import_list_names, BoundBinder,
    GateDispatcher, ModuleEnv, PersistentSession, SessionBind, SessionError, SessionLib,
    TurnClassification, TurnKind, ValueTier,
};
use tidepool_runtime::{
    classify_compile, classify_session, compile_haskell_salted, value_to_json, CompileError,
    CompileResult, FailureClass, Phase,
};

use crate::command::{
    BlockItem, BlockItemResult, BlockValue, BoundComponent, ExprText, ItemKind, MetaCommand,
    ResponseShape, SessionCommand, TurnOutcome,
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
    /// environment, so this registry surfaces them in `:bindings`/stale/etc.
    /// alongside effectful (materialized) binds. Latest-wins per name; a
    /// cross-plane rebind removes the name from the other plane.
    pure_binds: std::collections::BTreeMap<String, PureBind>,
}

/// A pure bind that lives in the decl plane (GHCi-environment model): its value
/// is its source (`defining_expr`), re-instantiated per use by GHC. No root.
struct PureBind {
    type_display: String,
    defining_expr: String,
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

/// [`Session::run_plain_eval`]'s tail: the turn's own [`DataConTable`] (needed
/// to render the run result via `value_to_json`) and the probed inner type
/// (`a` in `M a`).
struct PlainEvalTail {
    table: DataConTable,
    inner_type: Option<String>,
}

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
// The plain-eval variant carries the turn's whole `DataConTable` and is much
// the largest; there is at most ONE pending tail per session, so the size
// asymmetry costs one enum-sized slot, not a per-item allocation.
#[allow(clippy::large_enum_variant)]
enum PendingTail {
    /// [`Session::run_plain_eval`] — resumes against the turn's OWN table.
    PlainEval(PlainEvalTail),
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
    /// must use the same one the run did. Only the plain-eval path carries its
    /// own table (a standalone turn's metadata); the rest run against the
    /// accumulated session table.
    fn run_table<'a>(&'a self, session_table: &'a DataConTable) -> &'a DataConTable {
        match self {
            PendingTail::PlainEval(t) => &t.table,
            PendingTail::Bind(_)
            | PendingTail::MultiBind(_)
            | PendingTail::Reference(_)
            | PendingTail::BareExpr(_) => session_table,
        }
    }

    /// The label a run failure on this path reports under.
    fn error_label(&self) -> &'static str {
        match self {
            PendingTail::PlainEval(_) | PendingTail::Reference(_) | PendingTail::BareExpr(_) => {
                "runtime error"
            }
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
/// `items`/`verdicts` (rather than borrowing the request's) precisely because
/// it must outlive the call that built it.
struct BlockCursor {
    /// The block's items, owned so the loop survives the suspension.
    items: Vec<BlockItem>,
    /// This block's ONE batch classify verdict per item (`None` for
    /// `Decl`/`Meta`, or when the batch classify itself failed).
    verdicts: Vec<Option<TurnClassification>>,
    /// Per-item results accumulated so far.
    results: Vec<BlockItemResult>,
    /// The next item index to process.
    next: usize,
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
            results: Vec::with_capacity(items.len()),
            items,
            verdicts,
            next: 0,
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
                result_pos: self.results.len(),
            });
        }

        self.results.push(BlockItemResult {
            index,
            kind,
            ok,
            result: slim_item_result(&outcome),
            result_full: outcome.render(),
        });
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
/// for (`Auto`/`Stmt`), or `None` for a `Decl`/`Meta` item (unambiguous
/// already, no GHC verdict needed).
fn block_item_text(item: &BlockItem) -> Option<&str> {
    match item {
        BlockItem::Auto(e) | BlockItem::Stmt(e) => Some(&e.0),
        BlockItem::Decl(_) | BlockItem::Meta(_) => None,
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
        let effect_names = cfg
            .roster
            .decls()
            .iter()
            .map(|d| d.type_name.to_string())
            .collect();
        let core = PersistentSession::new(
            Some(lib),
            cfg.roster.suspend_tag(),
            effect_names,
            cfg.nursery_size,
        );
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

    /// The GHC include path for a turn: the session's base includes (generated
    /// `Tidepool.Effects` + prelude/stdlib) plus the live `Lib.G<g>` dir. Borrows
    /// `&self`, so block-scope the result before any `&mut self` call (e.g.
    /// `query_inner_type`) — same constraint the inlined copies had.
    fn turn_include(&self) -> Vec<&Path> {
        let mut include: Vec<&Path> = self.cfg.base_include.iter().map(PathBuf::as_path).collect();
        include.push(self.core.lib().include_dir());
        include
    }

    /// Module names of every live value binding — what a turn injects
    /// (`--inject-val`) AND imports so a session reference typechecks. Delegates
    /// to the shared core (the value plane lives there).
    fn live_val_modules(&self) -> Vec<String> {
        self.core.live_val_modules()
    }

    /// The CURRENT (newest) `Val.G<g>` module per still-live name — what a turn
    /// IMPORTS (unqualified). Excludes shadowed older gens (still injected, not
    /// imported). Delegates to the shared core.
    fn current_val_modules(&self) -> Vec<String> {
        self.core.current_val_modules()
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
        let mut lines: Vec<String> = Vec::new();
        if let Some(m) = self.core.current_lib_module() {
            lines.push(m.module_name());
        }
        lines.extend(self.current_val_modules());
        lines.join("\n")
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
    fn bind_materialized(&mut self, entry: BindingEntry) {
        // Retract from the decl plane too, so `SessionLib` stops exporting a
        // now-stale decl. No-op when the name was never a decl head. Best-effort:
        // a rare module-write failure leaves the binding materialized correctly.
        let _ = self.core.retract(&entry.name.0);
        self.pure_binds.remove(&entry.name.0);
        self.core.bind(entry);
    }

    /// Register `pb` on the decl (pure) plane under `name`, EVICTING any
    /// materialized value-plane binding of the same name from the current view.
    /// The dual of [`Self::bind_materialized`] — the single site that upholds
    /// the one-name-one-plane invariant for pure binds.
    fn bind_pure(&mut self, name: &str, pb: PureBind) {
        self.core.bindings_mut().remove_current(name);
        self.pure_binds.insert(name.to_string(), pb);
    }

    /// Run one turn to its first boundary: a finished [`TurnOutcome`], or an
    /// `ask` suspension whose continuation is stowed on the machine and whose
    /// tail/cursor are stowed on `self`. Errors are folded into
    /// [`TurnOutcome::Error`].
    ///
    /// `gate` is the turn's abort latch: [`GateDispatcher`] makes every effect
    /// dispatch a checkpoint, so a server-side `request_abort` unwinds the turn
    /// at the next effect. It does NOT intercept the ask tag — the JIT's own
    /// suspend driver catches that first.
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
    /// [`classify_block`] spawn — says `Decl`. A missing verdict means the
    /// `Auto` item is NOT decl-shaped, so it takes the resilient per-item path.
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
    /// `Auto` items dispatch straight from this batch's classify verdict when
    /// one is present, never paying for a doomed `run_def` probe GHC's own
    /// parser already ruled out. With no verdict (batch classify failed) they
    /// fall back to the try-cascade: `run_def` first, then `run_eval` on a GHC
    /// parse error (a type/scope error means the item IS a declaration, just a
    /// broken one, and surfaces as-is).
    fn run_block<H: DispatchEffect<CapturedOutput>>(
        &mut self,
        items: &[BlockItem],
        handlers: &mut H,
        captured: &CapturedOutput,
        verbose: bool,
    ) -> TurnStep {
        // Batch-classify every Auto/Stmt item in ONE extract spawn regardless
        // of block length; verdicts map back onto original indices. A batch
        // failure degrades exactly as a per-item classify failure would: every
        // verdict stays `None` and `run_eval` falls back to `run_plain_eval`.
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
                // A stale extract is NOT something to degrade around. Without
                // verdicts every bind would fall to the plain-eval path and
                // fail with `parse error on input '<-'` — an error about the
                // user's Haskell, for a deployment problem they cannot see.
                // `MalformedDiagnostics` is the boundary's version-skew
                // reading, so it stops the block with the real reason.
                Err(CompileError::MalformedDiagnostics(msg)) => {
                    return TurnStep::Completed(TurnOutcome::Error(msg))
                }
                // Any other failure (the extractor genuinely unavailable) keeps
                // the resilient path: verdicts stay `None`, decl-shaped items
                // take the per-item route, and GHC re-reports any real error
                // from the compile itself.
                Err(_) => {}
            }
        }

        // The cursor OWNS the items and verdicts: it has to outlive this call
        // whenever an item suspends.
        let cursor = BlockCursor::new(items.to_vec(), verdicts, self.eval_input.clone(), verbose);
        self.drive_block(cursor, handlers, captured)
    }

    /// The block item loop, re-enterable from any `cursor.next`.
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
        while cursor.next < cursor.items.len() {
            let index = cursor.next;
            // A DECL-shaped item (a keyword decl, or an `Auto` the GHC parser
            // classifies as a top-level declaration) starts a batch run; a
            // stmt/meta/expression is a singleton. The parse verdict — not the
            // lexical `Auto` tag — is what keeps a trailing call (`sq 7` after
            // `sq :: T` / `sq x = …`) OUT of the decl batch: it classifies as an
            // expression, ends the run, and lands on the stmt path (the tool's
            // "define then call in one block" idiom).
            let decl_start =
                self.decl_shaped_text(&cursor.items[index], cursor.verdicts[index].as_ref());
            let Some(first) = decl_start else {
                // Singleton — the ONE arm that can suspend.
                let (kind, step) = self.run_one_item(
                    &cursor.items[index],
                    cursor.verdicts[index].as_ref(),
                    handlers,
                    captured,
                );
                cursor.next = index + 1;
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
            let mut end = index + 1;
            while end < cursor.items.len() {
                match self.decl_shaped_text(&cursor.items[end], cursor.verdicts[end].as_ref()) {
                    Some(t) => {
                        // Within-block REDEFINITION ends the segment: if this
                        // item defines a head an earlier item in the segment
                        // also DEFINES (not a sig+binding pair — those must
                        // batch), batching would hand GHC two equation groups
                        // it merges as multi-clause (first wins). Splitting
                        // starts a new generation, so replace-latest applies,
                        // matching the cross-turn GHCi-parity rule. (#320)
                        let h = decl_head(t);
                        if !h.is_empty()
                            && defines_head(t, h)
                            && texts
                                .iter()
                                .any(|prev| decl_head(prev) == h && defines_head(prev, h))
                        {
                            break;
                        }
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
            cursor.next = end;

            let mut stop = false;
            match batched {
                Some(gen) => {
                    for (k, text) in texts.iter().enumerate() {
                        let outcome = self.defined_outcome(text, decl_head(text).to_string(), gen);
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
                            &cursor.items[start + k],
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
    /// continue the loop at `cursor.next`. `tail` is the stowed item tail (moved
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
        let is_final = cursor
            .last
            .as_ref()
            .is_some_and(|lv| Some(lv.result_pos) == cursor.results.len().checked_sub(1));
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
            if let Some(r) = cursor.results.get_mut(lv.result_pos) {
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
            items: cursor.results,
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
            PendingTail::PlainEval(t) => {
                let outcome = self
                    .core
                    .resume_with_table(&t.table, handlers, captured, input);
                self.settle(PendingTail::PlainEval(t), outcome, handlers, captured)
            }
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
            PendingTail::PlainEval(t) => {
                let _ = self
                    .core
                    .resume_with_table(&t.table, handlers, captured, input);
            }
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
            PendingTail::PlainEval(t) => self.finish_plain_eval(t, value),
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
    fn define_scoped(&mut self, decl_texts: &[&str]) -> Result<Generation, SessionError> {
        self.core.define_scoped(decl_texts)
    }

    /// Declaration handler: append the declaration to the Lane-A log + regenerate
    /// the gen-versioned `Lib.G<g>` module.
    fn run_def(&mut self, decl_text: &str) -> TurnOutcome {
        let head = decl_head(decl_text).to_string();
        match self.define_scoped(&[decl_text]) {
            Ok(gen) => self.defined_outcome(decl_text, head, gen),
            Err(e) => TurnOutcome::Error(session_fail(&e, "declaration failed")),
        }
    }

    /// If `expr_text` is a PURE bind of `name`, route it as the top-level decl
    /// `name = <rhs>` (so GHC generalizes it — GHCi parity) and return a `Bound`
    /// outcome. Returns `None` when it is not a pure bind, or when the decl
    /// route fails because the RHS needs the value plane (a reference the val
    /// imports don't cover, e.g. the `input` payload lane, or a self-reference
    /// `let x = … x …`) — the caller then falls back to the materialize path.
    ///
    /// A decl failure for any OTHER reason (a plain type error) is returned as
    /// `Some(Error)`, not `None`: materializing that case would paper over a
    /// real error with a broken binding (a polymorphic value forced into a
    /// monomorphic `Tier1Closure` fails cryptically later, on reference).
    ///
    /// `define_batch` always shadows wildcard-imported names (ledger #36) so a
    /// pure bind can shadow a Prelude/Library/effect-verb name exactly as a
    /// genuine top-level decl does — `let lookup = 42` must shadow
    /// `Prelude.lookup`, not raise an "Ambiguous occurrence".
    fn try_pure_bind_as_decl(&mut self, expr_text: &str, name: &str) -> Option<TurnOutcome> {
        let decl = pure_bind_to_decl(expr_text, name)?;
        match self.define_scoped(&[decl.as_str()]) {
            Ok(gen) => {
                let type_display = self.probe_pure_type(name).unwrap_or_default();
                // Register in the environment (decl plane) so :bindings/stale/etc.
                // see it; `bind_pure` evicts any materialized binding of `name`
                // from the value plane (cross-plane shadow, one-plane invariant).
                self.bind_pure(
                    name,
                    PureBind {
                        type_display: type_display.clone(),
                        defining_expr: expr_text.to_string(),
                        gen,
                    },
                );
                Some(TurnOutcome::Bound {
                    name: name.to_string(),
                    type_display,
                })
            }
            Err(e) => {
                // Materialize is the right fallback when the RHS references (a)
                // a materialized session value, or (b) the `input` payload lane
                // (value/stmt-plane only). Detect (b) by the decl error itself —
                // "not in scope: input" — not by scanning the text: a user's own
                // locally-bound `input` compiles fine on the decl plane and
                // never trips this. Any other decl failure is a real error —
                // surface it rather than materialize a broken binding.
                let err_str = e.to_string();
                let refs_materialized_value = self
                    .core
                    .bindings()
                    .iter_current()
                    .any(|(n, _)| mentions_word(expr_text, &n.0));
                // Whole-word `input` (GHC: "Variable not in scope: input :: Value"),
                // never `inputText`/`input'` — check the char after the match is not
                // an identifier continuation.
                let refs_input_lane = {
                    let needle = "not in scope: input";
                    err_str.match_indices(needle).any(|(i, _)| {
                        let after = &err_str[i + needle.len()..];
                        !after
                            .chars()
                            .next()
                            .is_some_and(|c| c.is_alphanumeric() || c == '\'' || c == '_')
                    })
                };
                if refs_materialized_value || refs_input_lane {
                    None
                } else {
                    Some(TurnOutcome::Error(session_fail(&e, "bind compile error")))
                }
            }
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
    fn probe_pure_type(&mut self, name: &str) -> Option<String> {
        let preamble = to_nmr_pragmas(&self.patched_preamble());
        let imports = self.session_imports();
        let inject = self.live_val_modules();
        let eval_input = self.eval_input.clone();
        let src = wrap_pure_ref_source(&preamble, &imports, name, eval_input.as_ref());
        let include = self.turn_include();
        compile_session_turn(&src, &include, self.session_root(), &inject, None)
            .ok()
            .and_then(|turn| turn.warnings.captured_type)
    }

    /// Build the `Defined` outcome for one decl head at generation `gen`,
    /// computing the `stale` set (live binds whose defining expression
    /// references this (re)defined name — notebook display truthfulness) and
    /// the inferred `type` the server had at compile time (#317).
    /// Shared by `run_def` and the whole-block decl-batch path. `text` is the
    /// decl item's source, used to gate the type probe to VALUE bindings.
    fn defined_outcome(&mut self, text: &str, head: String, gen: Generation) -> TurnOutcome {
        let mut stale: Vec<String> = self
            .core
            .bindings()
            .iter_current()
            .filter(|(_, e)| {
                e.defining_expr
                    .as_deref()
                    .is_some_and(|src| mentions_word(src, &head))
            })
            .map(|(n, _)| n.0.clone())
            .collect();
        // Pure binds (decl-backed) that reference the redefined head are also
        // stale (they hold their old generalized value until re-run).
        stale.extend(
            self.pure_binds
                .iter()
                .filter(|(_, pb)| mentions_word(&pb.defining_expr, &head))
                .map(|(n, _)| n.clone()),
        );
        // Paint the inferred type — render every mutation fully, once, at
        // mutation time. Best-effort: only for items with an actual DEFINING
        // equation for `head` (a value/function binding — `defines_head` is
        // false for type/class/data/instance/import/fixity decls and for a
        // signature-only item in a split sig+bind pair, so the primary bind
        // item is the one painted). The probe is one extra extract compile;
        // its failure never fails the decl — the field is simply omitted.
        let type_display = if defines_head(text, &head) {
            self.probe_pure_type(&head)
        } else {
            None
        };
        TurnOutcome::Defined {
            generation: gen.0,
            module: tidepool_repr::SessionModule::lib(gen).module_name(),
            head,
            type_display,
            stale,
        }
    }

    /// Run a single block item (no batching): the per-item dispatch used both
    /// for stmt/meta items and as the fallback when a decl batch fails. `verdict`
    /// is this item's precomputed classify verdict from `run_block`'s batch
    /// spawn (`None` for `Decl`/`Meta`, or when the batch classify failed);
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
            // `classify_block` spawn) — when that verdict is present, dispatch
            // straight from it instead of paying for a doomed `run_def` probe
            // first: a `Decl` verdict runs as a declaration directly, a
            // `Bind`/`Expr` verdict runs `run_eval` directly. The try-cascade
            // (attempt `run_def`, fall back to `run_eval` on a GHC parse
            // error) is a degradation path for when NO verdict is available
            // (the batch classify itself failed) — there, GHC's parser is the
            // only way left to tell decl from stmt.
            BlockItem::Auto(expr) => match verdict {
                Some(v) if v.kind == TurnKind::Decl => {
                    (ItemKind::Decl, ItemStep::Done(self.run_def(&expr.0)))
                }
                Some(_) => (
                    ItemKind::Stmt,
                    self.run_eval(&expr.0, verdict, handlers, captured),
                ),
                None => {
                    let def_result = self.run_def(&expr.0);
                    match def_result {
                        TurnOutcome::Error(ref msg) if is_parse_error(msg) => (
                            ItemKind::Stmt,
                            self.run_eval(&expr.0, verdict, handlers, captured),
                        ),
                        other => (ItemKind::Decl, ItemStep::Done(other)),
                    }
                }
            },
        }
    }

    /// Expression/bind handler. `verdict` is this item's precomputed classify
    /// verdict — GHC's parser classifies bind vs expr, never a Rust scanner —
    /// from `run_block`'s one batch [`classify_block`] spawn for the whole
    /// block; a BIND (`x <- e` / `let x = e`) roots a value on the live heap, a
    /// reference-with-live-bindings injects the session ifaces, and a plain
    /// expression (no bindings) stays on the plain-eval path
    /// ([`Self::run_plain_eval`]).
    fn run_eval<H: DispatchEffect<CapturedOutput>>(
        &mut self,
        expr_text: &str,
        verdict: Option<&TurnClassification>,
        handlers: &mut H,
        captured: &CapturedOutput,
    ) -> ItemStep {
        // Bind-vs-expr + bound names come from GHC (parse-only, via the
        // block's batch classify). A missing verdict (batch classify failed —
        // e.g. extractor unavailable) falls back to the plain path, where GHC
        // re-reports any real error.
        let classification = match verdict {
            Some(c) => c,
            None => return self.run_plain_eval(expr_text, handlers, captured),
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
                    // instead of freezing to a monomorphic heap value; falls back
                    // to materialize when the RHS is out of decl scope.
                    //
                    // EXCEPT a self-referential monadic pure bind (`n <- pure
                    // (n+1)`): the decl route would emit the RECURSIVE top-level
                    // `n = n + 1` (self-forcing blackhole). GHCi's `>>=` reads the
                    // PRIOR `n` and shadows — exactly what materialize does — so
                    // divert straight to it.
                    if !self_referential_monadic_pure_bind(expr_text, &name) {
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
            let inject = self.live_val_modules();
            self.run_bare_expr(expr_text, &imports, &inject, handlers, captured)
        }
    }

    /// The plain expression path: compile an `M a` expression against the
    /// session include and run it on the resident machine. Used when the turn
    /// neither binds nor references a session binding.
    fn run_plain_eval<H: DispatchEffect<CapturedOutput>>(
        &mut self,
        expr_text: &str,
        handlers: &mut H,
        captured: &CapturedOutput,
    ) -> ItemStep {
        let preamble = self.patched_preamble();
        let mut imports = self
            .core
            .lib()
            .current_module()
            .map(|m| format!("{}\n", m.module_name()))
            .unwrap_or_default();
        // Same per-turn quasi-quoter gating as `turn_imports` (this path
        // assembles its imports independently of session_imports).
        if tidepool_mcp::uses_qq(expr_text) {
            imports.push_str("Tidepool.QQ (fmt, j, patch, uri, form)\n");
        }
        // Clone (not take): `input` stays in scope for EVERY item in the block
        // — including items that run after an in-block `ask`/resume — and for the
        // type-probe recompiles below. The worker resets `eval_input` per job.
        let eval_input = self.eval_input.clone();
        let source = template_haskell_show_default(
            &preamble,
            &self.cfg.effect_stack,
            expr_text,
            &imports,
            "",
            eval_input.as_ref(),
            None,
        );

        let salt = self.core.lib().cache_salt();
        // Block-scope `include` so the borrow on `self.cfg.base_include` is
        // released before we call `query_inner_type` (which needs `&mut self`).
        let compile_result = {
            let include = self.turn_include();
            compile_haskell_salted(&source, "result", &include, Some(&salt))
        };
        let CompileResult {
            expr,
            mut table,
            warnings,
        } = match compile_result {
            Ok(r) => r,
            // No `user_lines` computed here (this is the plain-eval path, not a
            // session-turn compile) — `None` is the correct default for this site.
            Err(e) => return ItemStep::Done(TurnOutcome::Error(compile_fail(&e, &source, None))),
        };
        if warnings.has_io {
            return ItemStep::Done(io_type_fail());
        }
        table.populate_siblings_from_expr(&expr);

        // Query the inner value type (`a` in `M a`) via the bind mechanism:
        // `__t <- <expr>` gives `__t :: a`, not the Eff-wrapped action type.
        let inner_type = self.query_inner_type(expr_text);

        let tail = PlainEvalTail { table, inner_type };

        let run_result = if self.core.is_bootstrapped() {
            // Later turn: add this expression as a fragment against ITS OWN table
            // (a standalone plain-eval turn carries its own metadata) with an
            // empty env, and run it on the resident machine.
            match self.core.add_fragment_with_table(
                "repl_turn",
                &expr,
                &tail.table,
                &ExternalEnv::new(),
            ) {
                Ok(fid) => self
                    .core
                    .run_funcid_with_table(fid, &tail.table, handlers, captured),
                Err(e) => {
                    return ItemStep::Done(TurnOutcome::Error(run_fail("JIT re-entry error", e)))
                }
            }
        } else {
            // First turn: bootstrap the machine from `expr` (the seed IS the
            // program) and publish the cancel handle BEFORE running, so a runaway
            // on this bare-expression path is cancellable from the start.
            if let Err(e) = self.core.bootstrap_if_needed(&expr, &tail.table) {
                return ItemStep::Done(TurnOutcome::Error(run_fail("JIT compile error", e)));
            }
            self.publish_cancel();
            self.core.run_entry(&tail.table, handlers, captured)
        };

        self.settle(PendingTail::PlainEval(tail), run_result, handlers, captured)
    }

    /// Post-run bookkeeping for [`Self::run_plain_eval`]: render the value
    /// against the turn's own table and the probed inner type.
    fn finish_plain_eval(&mut self, tail: PlainEvalTail, value: Value) -> TurnOutcome {
        self.value_outcome(value_to_json(&value, &tail.table, 0), tail.inner_type)
    }

    /// Assemble a [`TurnOutcome::Value`], truncating an oversized rendered
    /// value to the result budget and stashing the elided subtrees for
    /// `:stub <n>` (see [`crate::truncate`]). Shared by the plain-eval and
    /// reference paths — the two that render a value.
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
        let inject = self.live_val_modules();
        let imports = self.turn_imports(turn_text);
        let eval_input = self.eval_input.clone();
        let wrapped = wrap_bind_source(
            &preamble,
            &self.cfg.effect_stack,
            &imports,
            turn_text,
            &name,
            eval_input.as_ref(),
        );

        let include = self.turn_include();

        let single = vec![name.clone()];
        let user_lines = user_code_line_range(&wrapped, turn_text);
        let turn = match compile_session_turn(
            &wrapped,
            &include,
            self.session_root(),
            &inject,
            Some(SessionBind {
                names: &single,
                gen: g.0,
                probe_only: false,
            }),
        ) {
            Ok(t) => t,
            Err(e) => {
                return ItemStep::Done(TurnOutcome::Error(compile_fail(&e, &wrapped, user_lines)))
            }
        };
        if turn.warnings.has_io {
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
        if let Err(e) = self.merge_table(&turn.table) {
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
            if let Err(e) = self.core.bootstrap_if_needed(&turn.expr, &turn.table) {
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
        let referenced = tidepool_repr::free_vars::free_vars(&turn.expr);
        let env = self.core.seed_external_env(&referenced);
        let fid = match self
            .core
            .add_fragment_session("repl_bind", &turn.expr, &env)
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
        self.bind_materialized(BindingEntry {
            name: BindingName(tail.name.clone()),
            id: SessionVarId::from_extract(tail.var_id),
            module: SessionModule::val(tail.g),
            value,
            type_display: Some(tail.type_display.clone()),
            defining_expr: Some(tail.defining_expr),
            // The repl is a flat session: every bind is a ROOT-frame bind.
            scope: ScopeId::ROOT,
        });
        TurnOutcome::Bound {
            name: tail.name,
            type_display: tail.type_display,
        }
    }

    /// DISCARD-BIND path (`_ <- e`, `(_, _) <- e`): wraps the whole statement
    /// into an `Eff`-typed `__result = do { <stmt>; pure () }` — the same
    /// `{{TURN_STMT}}` placement `run_bind` uses, but yielding `()` and
    /// splicing no binder — and compiles it with no [`SessionBind`] (a
    /// discarding bind mints no session value, so it must not flow through
    /// `SessionBind`: the extract rejects an empty `--bind-name` list). Runs
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
        let inject = self.live_val_modules();
        let imports = self.turn_imports(turn_text);
        let eval_input = self.eval_input.clone();
        let wrapped = wrap_bind_discard_source(
            &preamble,
            &self.cfg.effect_stack,
            &imports,
            turn_text,
            eval_input.as_ref(),
        );

        let include = self.turn_include();
        let user_lines = user_code_line_range(&wrapped, turn_text);
        let turn =
            match compile_session_turn(&wrapped, &include, self.session_root(), &inject, None) {
                Ok(t) => t,
                Err(e) => {
                    return ItemStep::Done(TurnOutcome::Error(compile_fail(
                        &e, &wrapped, user_lines,
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
        let inject = self.live_val_modules();
        let imports = self.turn_imports(turn_text);
        let eval_input = self.eval_input.clone();
        let wrapped = wrap_multi_bind_source(
            &preamble,
            &self.cfg.effect_stack,
            &imports,
            turn_text,
            &names,
            eval_input.as_ref(),
        );

        let include = self.turn_include();

        let user_lines = user_code_line_range(&wrapped, turn_text);
        let turn = match compile_session_turn(
            &wrapped,
            &include,
            self.session_root(),
            &inject,
            Some(SessionBind {
                names: &names,
                gen: g.0,
                probe_only: false,
            }),
        ) {
            Ok(t) => t,
            Err(e) => {
                return ItemStep::Done(TurnOutcome::Error(compile_fail(&e, &wrapped, user_lines)))
            }
        };
        if turn.warnings.has_io {
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
        if let Err(e) = self.merge_table(&turn.table) {
            return ItemStep::Done(TurnOutcome::Error(tag_failure(
                FailureClass::Runtime,
                Phase::Run,
                e,
            )));
        }

        if !self.core.is_bootstrapped() {
            if let Err(e) = self.core.bootstrap_if_needed(&turn.expr, &turn.table) {
                return ItemStep::Done(TurnOutcome::Error(run_fail("JIT compile error", e)));
            }
            self.publish_cancel();
        }

        let referenced = tidepool_repr::free_vars::free_vars(&turn.expr);
        let env = self.core.seed_external_env(&referenced);
        let fid = match self
            .core
            .add_fragment_session("repl_multi_bind", &turn.expr, &env)
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
        for (binder, slot) in tail.binders.iter().zip(slots.into_iter()) {
            let value = bound_value(binder.tier, slot);
            self.bind_materialized(BindingEntry {
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
        turn: tidepool_runtime::session::SessionTurnResult,
        inner_type: Option<String>,
        handlers: &mut H,
        captured: &CapturedOutput,
    ) -> ItemStep {
        if turn.warnings.has_io {
            return ItemStep::Done(io_type_fail());
        }
        if let Err(e) = self.merge_table(&turn.table) {
            return ItemStep::Done(TurnOutcome::Error(tag_failure(
                FailureClass::Runtime,
                Phase::Run,
                e,
            )));
        }
        self.ensure_effect_machine();
        if !self.core.is_bootstrapped() {
            if let Err(e) = self.core.bootstrap_if_needed(&turn.expr, &turn.table) {
                return ItemStep::Done(TurnOutcome::Error(run_fail("JIT compile error", e)));
            }
            self.publish_cancel();
        }
        let referenced = tidepool_repr::free_vars::free_vars(&turn.expr);
        let env = self.core.seed_external_env(&referenced);
        let fid = match self.core.add_fragment_session("repl_ref", &turn.expr, &env) {
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
        inject: &[String],
        handlers: &mut H,
        captured: &CapturedOutput,
    ) -> ItemStep {
        let preamble = self.patched_preamble();
        let g = self.core.val_gen().next();
        let eval_input = self.eval_input.clone();
        // TWO names, matching the `(it, toWire it)` tuple `result` now yields:
        // rides the same multi-binder `splitTupleType` path
        // `run_multi_bind`/`wrap_multi_bind_source` use, so `emitBindArtifacts`
        // splits `result`'s `(T, Value)` type into `it :: T` + `__it_render ::
        // Value` instead of wrongly taking the whole tuple as `it`'s type.
        // `__it_render`'s binder metadata is discarded below (only
        // `turn.binders[0]`, i.e. `it`, is used).
        let it_names = vec!["it".to_string(), "__it_render".to_string()];

        let monadic_src = wrap_bare_it_monadic(
            &preamble,
            &self.cfg.effect_stack,
            imports,
            expr_text,
            eval_input.as_ref(),
        );
        let monadic_result = {
            let include = self.turn_include();
            compile_session_turn(
                &monadic_src,
                &include,
                self.session_root(),
                inject,
                Some(SessionBind {
                    names: &it_names,
                    gen: g.0,
                    probe_only: false,
                }),
            )
        };

        let turn = match monadic_result {
            Ok(t) => t,
            Err(_monadic_err) => {
                let pure_src = wrap_bare_it_pure(
                    &preamble,
                    &self.cfg.effect_stack,
                    imports,
                    expr_text,
                    eval_input.as_ref(),
                );
                let include = self.turn_include();
                let user_lines = user_code_line_range(&pure_src, expr_text);
                match compile_session_turn(
                    &pure_src,
                    &include,
                    self.session_root(),
                    inject,
                    Some(SessionBind {
                        names: &it_names,
                        gen: g.0,
                        probe_only: false,
                    }),
                ) {
                    Ok(t) => t,
                    Err(pure_err) => {
                        return ItemStep::Done(TurnOutcome::Error(compile_fail(
                            &pure_err, &pure_src, user_lines,
                        )))
                    }
                }
            }
        };

        if turn.warnings.has_io {
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
        if let Err(e) = self.merge_table(&turn.table) {
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
            if let Err(e) = self.core.bootstrap_if_needed(&turn.expr, &turn.table) {
                return ItemStep::Done(TurnOutcome::Error(run_fail("JIT compile error", e)));
            }
            self.publish_cancel();
        }

        let referenced = tidepool_repr::free_vars::free_vars(&turn.expr);
        let env = self.core.seed_external_env(&referenced);
        let fid = match self.core.add_fragment_session("repl_it", &turn.expr, &env) {
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
        self.bind_materialized(BindingEntry {
            name: BindingName("it".to_string()),
            id: SessionVarId::from_extract(tail.var_id),
            module: SessionModule::val(tail.g),
            value: it_value,
            type_display: Some(tail.type_display.clone()),
            defining_expr: Some(tail.defining_expr),
            scope: ScopeId::ROOT,
        });

        let rendered = value_to_json(&rendered_value, self.core.session_table(), 0);
        self.value_outcome_bound_it(rendered, Some(tail.type_display))
    }

    /// Compile-only type query: returns the inner value type `a` for a monadic
    /// expression of type `M a` / `Eff '[…] a`. Hoists the expr to a module-level
    /// `__probe` binding then binds `__t <- __probe` so `__t :: a` (the monadic
    /// bind peels the Eff head) — see `wrap_probe_source` for why the module-level
    /// binding matters (a trailing `where` can attach there but not on a
    /// do-statement). Consumes a throwaway generation to avoid iface collisions
    /// with subsequent real binds. Returns `None` if the compile fails (e.g. a
    /// non-monadic expression, which has no inner type to peel).
    fn query_inner_type(&mut self, expr_text: &str) -> Option<String> {
        let g = self.core.val_gen().next();
        self.core.set_val_gen(g);
        let preamble = self.patched_preamble();
        let inject = self.live_val_modules();
        let imports = self.turn_imports(expr_text);
        let eval_input = self.eval_input.clone();
        let wrapped = wrap_probe_source(
            &preamble,
            &self.cfg.effect_stack,
            &imports,
            expr_text,
            eval_input.as_ref(),
        );
        let include = self.turn_include();
        let names = vec!["__t".to_string()];
        compile_session_turn(
            &wrapped,
            &include,
            self.session_root(),
            &inject,
            Some(SessionBind {
                names: &names,
                gen: g.0,
                probe_only: false,
            }),
        )
        .ok()
        .and_then(|turn| turn.binders.into_iter().next())
        .map(|b| b.type_display)
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
                        let effect_names = self
                            .cfg
                            .roster
                            .decls()
                            .iter()
                            .map(|d| d.type_name.to_string())
                            .collect();
                        self.core = PersistentSession::new(
                            Some(lib),
                            self.cfg.roster.suspend_tag(),
                            effect_names,
                            self.cfg.nursery_size,
                        );
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
                let preamble = self.patched_preamble();
                // Consume a throwaway generation to prevent an iface collision
                // with the next real bind (compile_session_turn writes a Val.G<g>.hi
                // even for the discard path). We do NOT add to self.core.bindings().
                let throwaway_gen = self.core.val_gen().next();
                self.core.set_val_gen(throwaway_gen);
                let inject = self.live_val_modules();
                let imports = self.turn_imports(expr);
                let eval_input = self.eval_input.clone();
                let turn_text = format!("let __t = {expr}");
                let wrapped = wrap_bind_source(
                    &preamble,
                    &self.cfg.effect_stack,
                    &imports,
                    &turn_text,
                    "__t",
                    eval_input.as_ref(),
                );
                let include = self.turn_include();
                let names: Vec<String> = vec!["__t".to_string()];
                let user_lines = user_code_line_range(&wrapped, &turn_text);
                let turn = match compile_session_turn(
                    &wrapped,
                    &include,
                    self.session_root(),
                    &inject,
                    Some(SessionBind {
                        names: &names,
                        gen: throwaway_gen.0,
                        // `:t` is pure introspection: read the type, print it,
                        // discard the bind — never registered in session scope,
                        // so it must never trip the cross-row guard meant for
                        // binds that persist across turns.
                        probe_only: true,
                    }),
                ) {
                    Ok(t) => t,
                    Err(e) => {
                        return TurnOutcome::Meta(serde_json::json!({
                            "error": format!(
                                "compile error: {}",
                                render_compile_fail_body(&e, &wrapped, user_lines)
                            )
                        }))
                    }
                };
                match turn.binders.into_iter().next() {
                    Some(binder) => TurnOutcome::Meta(serde_json::json!({
                        "type": binder.type_display
                    })),
                    None => TurnOutcome::Meta(serde_json::json!({
                        "error": "no type information captured"
                    })),
                }
            }
            MetaCommand::Info(name) => {
                // 1. Bound value lookup (highest priority — a session binding shadows types).
                if let Some((_, entry)) = self
                    .core
                    .bindings()
                    .iter_current()
                    .find(|(n, _)| n.0 == *name)
                {
                    return TurnOutcome::Meta(serde_json::json!({
                        "name": name,
                        "type": entry.type_display.clone().unwrap_or_default(),
                        "tier": if entry.value.is_forced() { "Tier0Data" } else { "Tier1Closure" },
                        "module": entry.module.module_name(),
                    }));
                }
                // 1b. Pure bind (decl-backed) — part of the environment too.
                if let Some(pb) = self.pure_binds.get(name) {
                    return TurnOutcome::Meta(serde_json::json!({
                        "name": name,
                        "type": pb.type_display,
                        "tier": "DeclBacked",
                        "module": tidepool_repr::SessionModule::lib(pb.gen).module_name(),
                    }));
                }
                // 2. Built-in effect decl type_defs (data/newtype/type) and GADT constructors.
                for decl in self.cfg.roster.decls() {
                    for type_def in decl.type_defs {
                        if type_def_head(type_def) == Some(name.as_str()) {
                            return TurnOutcome::Meta(serde_json::json!({
                                "name": name,
                                "shape": *type_def,
                            }));
                        }
                    }
                    for con in decl.constructors {
                        if con.split("::").next().map(str::trim) == Some(name.as_str()) {
                            return TurnOutcome::Meta(serde_json::json!({
                                "name": name,
                                "shape": *con,
                                "effect": decl.type_name,
                            }));
                        }
                    }
                }
                // 3. Session-defined types (data/newtype/type/class from declaration items).
                if let Some(src) = self.core.lib().decl_type_source(name) {
                    return TurnOutcome::Meta(serde_json::json!({
                        "name": name,
                        "shape": src,
                        "source": "session",
                    }));
                }
                // 3b. Session-defined values/functions (`f x = …`). These are
                // decls, not bindings or types, so they need their own lookup
                // here rather than falling through to a total miss. (#318)
                if let Some(src) = self.core.lib().decl_value_source(name) {
                    return TurnOutcome::Meta(serde_json::json!({
                        "name": name,
                        "shape": src,
                        "source": "session",
                    }));
                }
                // 4. Stdlib/preamble types (`Proc`, `Hit`, … — source-scanned
                // from the same include dirs the session compiles against).
                if let Some(info) = crate::introspect::stdlib_info(&self.cfg.base_include, name) {
                    return TurnOutcome::Meta(info);
                }
                // 4b. Stdlib/library VALUES (`findDef`, … — lowercase names
                // `:vocab` already lists via the same signature scanner, but
                // step 4 above is type-only and bails immediately on a
                // lowercase name).
                if let Some(info) =
                    crate::introspect::stdlib_value_info(&self.cfg.base_include, name)
                {
                    return TurnOutcome::Meta(info);
                }
                // 5. Total miss.
                TurnOutcome::Meta(serde_json::json!({
                    "error": "not a bound value or known type",
                    "name": name,
                    "hint": "searched session bindings, effect types, session declarations, \
                             and the stdlib/library sources; for an expression's type use \
                             `:t <expr>`",
                }))
            }
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
        for (name, gen) in self.core.lib().current_decl_heads() {
            if self.pure_binds.contains_key(&name) {
                continue;
            }
            entries.push(serde_json::json!({
                "name": name,
                "type": "",
                "kind": "decl",
                "generation": gen,
            }));
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
        let inject = self.live_val_modules();
        let eval_input = self.eval_input.clone();
        // `template_haskell_show_default` (the non-session `eval`-tool
        // template, target `result`) is the WRONG shape here — it's compiled
        // via `compile_session_turn`, whose extractor invocation always
        // targets the scaffold-reserved `__result` (see `turn.rs`). Reuse
        // `wrap_probe_source` instead: it already compiles to a properly
        // `Eff`-typed `__result :: Eff {effect_stack} _` binding (forcing the
        // same freer-simple constructor requirement this bootstrap exists
        // for), just via an extra `__probe`/`__t` monadic peel we don't need
        // the result of — only `turn.table`/`turn.expr` are read below.
        let src = wrap_probe_source(
            &preamble,
            &self.cfg.effect_stack,
            &imports,
            "pure ()",
            eval_input.as_ref(),
        );
        let compiled = {
            let include = self.turn_include();
            compile_session_turn(&src, &include, self.session_root(), &inject, None)
        };
        if let Ok(turn) = compiled {
            let _ = self.merge_table(&turn.table);
            if self
                .core
                .bootstrap_if_needed(&turn.expr, &turn.table)
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
/// formats. The original message is embedded verbatim, so string sniffs over it
/// (e.g. `is_parse_error`) still match.
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
/// The body is the ORIGINAL `"<prefix>: <err>"` text (not the classifier's
/// re-messaging), so the Auto decl→stmt fallback's `is_parse_error` sniff still
/// finds the "binder extraction failed" marker; the envelope supplies only the
/// class/phase tag.
fn session_fail(err: &SessionError, prefix: &str) -> String {
    let env = classify_session(err);
    tag_failure(env.class, env.phase, format!("{prefix}: {err}"))
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
/// Insert `NoMonomorphismRestriction` into a preamble's `LANGUAGE` pragma so a
/// probe compile generalizes a constrained pure bind instead of failing to
/// monomorphize it. Idempotent — a no-op if NMR is already present.
fn to_nmr_pragmas(preamble: &str) -> String {
    if preamble.contains("NoMonomorphismRestriction") {
        return preamble.to_string();
    }
    preamble.replacen(
        "NoImplicitPrelude,",
        "NoImplicitPrelude, NoMonomorphismRestriction,",
        1,
    )
}

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

/// Whether `expr_text` is a MONADIC pure bind (`name <- pure e` /
/// `name <- return e`) whose RHS `e` references `name`. Such a bind must read
/// the PRIOR `name` and shadow — GHCi `>>=` semantics (`pure e >>= \name -> …`
/// evaluates `e` in the outer scope, so `name` there is the old binding). The
/// decl route ([`pure_bind_to_decl`]) would instead emit a top-level
/// `name = e`, which in Haskell is RECURSIVE (`n = n + 1` self-forces to a
/// blackhole). So these divert to the materialize/shadow path.
///
/// `let name = e` is deliberately EXCLUDED: Haskell `let` is recursive, so
/// `let n = n + 1` looping matches GHCi — only the `<-` form shadows.
fn self_referential_monadic_pure_bind(expr_text: &str, name: &str) -> bool {
    let t = expr_text.trim();
    if t.starts_with("let ") {
        return false;
    }
    let Some(rhs) = t
        .strip_prefix(name)
        .map(str::trim_start)
        .and_then(|a| a.strip_prefix("<-"))
        .map(str::trim_start)
    else {
        return false;
    };
    ["pure ", "return "]
        .iter()
        .find_map(|kw| rhs.strip_prefix(kw))
        .is_some_and(|e| mentions_word(e, name))
}

/// Whether `text` contains `word` as a whole identifier (Haskell ident
/// boundaries: alnum, `_`, `'`). Used to find binds that reference a
/// redefined decl (the `stale:` field).
fn mentions_word(text: &str, word: &str) -> bool {
    if word.is_empty() {
        return false;
    }
    let ident = |c: char| c.is_alphanumeric() || c == '_' || c == '\'';
    let bytes = text.as_bytes();
    let mut start = 0;
    while let Some(rel) = text[start..].find(word) {
        let i = start + rel;
        let before_ok = i == 0 || !text[..i].chars().next_back().is_some_and(ident);
        let after = i + word.len();
        let after_ok = after >= bytes.len() || !text[after..].chars().next().is_some_and(ident);
        if before_ok && after_ok {
            return true;
        }
        start = i + 1;
    }
    false
}

/// Whether a decl item's text contains a DEFINING equation for `head` (as
/// opposed to only a type signature `head :: T`). Drives the within-block
/// redefinition split in the decl batcher (#320): sig+binding pairs must stay
/// batched; two defining items for one head must not. A heuristic over lines
/// (operator heads in prefix parens are not detected — GHC's verdict stays
/// authoritative for what actually compiles).
fn defines_head(text: &str, head: &str) -> bool {
    if head.is_empty() {
        return false;
    }
    text.lines().any(|l| {
        let l = l.trim_start();
        match l.strip_prefix(head) {
            Some(rest) => {
                let boundary_ok = rest
                    .chars()
                    .next()
                    .is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '\''));
                boundary_ok && !rest.trim_start().starts_with("::")
            }
            None => false,
        }
    })
}

/// Strip leading blank lines and `--` line comments so `decl_head` extracts
/// the real token instead of the comment marker. A leading `-- slugify a
/// title\nslug t = ...` must classify head `"slug"`, not `"--"` — a stray
/// `"--"` head poisons the type-probe gate (`defines_head`), the within-block
/// redefinition splitter, and `mentions_word`'s stale-bind detection, since
/// all three key off the (wrong) head text.
///
/// Follows the Haskell lexical rule that a dash run immediately followed by
/// another symbol character is an OPERATOR, not a comment (`-->`, `|--`), so
/// such lines are left alone for the caller's own parsing.
fn strip_leading_comments(text: &str) -> &str {
    const SYMBOL_CHARS: &str = "!#$%&*+./<=>?@\\^|~:-";
    let mut s = text;
    loop {
        let trimmed = s.trim_start_matches(char::is_whitespace);
        match trimmed.strip_prefix("--") {
            Some(rest) if !rest.starts_with(|c: char| SYMBOL_CHARS.contains(c)) => {
                s = match rest.find('\n') {
                    Some(nl) => &rest[nl + 1..],
                    None => "",
                };
            }
            _ => return trimmed,
        }
    }
}

/// Extract the declared head identifier from a Haskell declaration string,
/// for the slim `{"decl":"name"}` block item result. Strips keyword prefixes
/// for type/class/instance declarations; for function definitions returns the
/// first identifier. Returns `""` for empty or unrecognised text.
fn decl_head(text: &str) -> &str {
    let s = strip_leading_comments(text).trim();
    // Import: name the module being imported, not the `import` keyword. (#317)
    if let Some(rest) = s.strip_prefix("import ") {
        let rest = rest.trim_start();
        let rest = rest.strip_prefix("qualified ").unwrap_or(rest).trim_start();
        let end = rest
            .find(|c: char| c.is_whitespace() || c == '(')
            .unwrap_or(rest.len());
        return rest[..end].trim_end();
    }
    // Fixity: name the operator(s) being fixed, not the `infixl`/`infixr`/`infix`
    // keyword (skip the optional precedence digits). (#317)
    for kw in &["infixl ", "infixr ", "infix "] {
        if let Some(rest) = s.strip_prefix(kw) {
            return rest
                .trim_start()
                .trim_start_matches(|c: char| c.is_ascii_digit())
                .trim();
        }
    }
    for kw in &["data ", "newtype ", "type ", "class ", "instance "] {
        if let Some(rest) = s.strip_prefix(kw) {
            let end = rest
                .find(|c: char| c.is_whitespace() || c == '(' || c == '=')
                .unwrap_or(rest.len());
            return rest[..end].trim_end();
        }
    }
    // Prefix operator definition `(<>) x y = …` / `(<>) = …`: name the operator
    // rather than returning "" (the `(` used to zero the token). (#317)
    if let Some(rest) = s.strip_prefix('(') {
        if let Some(close) = rest.find(')') {
            return rest[..close].trim();
        }
    }
    let end = s
        .find(|c: char| c.is_whitespace() || c == '(' || c == ':' || c == '=')
        .unwrap_or(s.len());
    &s[..end]
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
        TurnOutcome::Defined {
            head,
            type_display,
            stale,
            ..
        } => {
            let mut obj = serde_json::json!({ "decl": head });
            // Paint the inferred type the server had at compile time, so
            // `{decl:"heatOf"}` doesn't cost the caller a `:t` (#317). Omitted
            // (best-effort) for non-value decls and probe failures.
            if let Some(ty) = type_display.as_deref().filter(|t| !t.is_empty()) {
                obj["type"] = serde_json::json!(ty);
            }
            if !stale.is_empty() {
                obj["stale"] = serde_json::json!(stale);
            }
            obj
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
        TurnOutcome::Error(e) => serde_json::json!({ "error": e }),
        TurnOutcome::Block { .. } => serde_json::json!({ "error": "nested block" }),
    }
}

/// Return `true` when a `run_def` error message indicates a GHC parse (not
/// type or scope) error, so the try-cascade in `run_block` can fall back from
/// `run_def` to `run_eval` for items that are expressions, not declarations.
/// Case-insensitive to tolerate minor GHC version variation.
///
/// Also treats "binder extraction failed" as a not-a-declaration signal:
/// that's the parse/scope STAGE (pre-typecheck), so a failure there —
/// including on a non-declaration input like `123 :: Int` — means "try it as
/// an expression." A genuine-but-type-broken declaration parses fine here and
/// fails later as a "declaration type-check failed" error instead, which does
/// NOT match and so surfaces as a decl error.
fn is_parse_error(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    lower.contains("parse error")
        || lower.contains("lexical error")
        || lower.contains("binder extraction failed")
}

/// Extract the declared head name from a Haskell type declaration string.
/// Returns `Some(name)` when the string starts with `data`/`newtype`/`type`
/// and the next token is the type name; `None` for functions, instances, etc.
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
                let verbs: Vec<String> = d.helpers.iter().filter_map(|h| helper_sig(h)).collect();
                serde_json::json!({
                    "effect": d.type_name,
                    "description": d.description,
                    "verbs": verbs,
                    "constructors": d.constructors,
                    // Real field names, straight from the type_def — the doc
                    // prose above can drift, this can't (#346).
                    "types": d.type_defs,
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
    use super::self_referential_monadic_pure_bind;
    use super::{browse_effects, EffectDecl};
    use super::{decl_head, pure_bind_to_decl, slim_item_result, strip_leading_comments};

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

    #[test]
    fn self_ref_monadic_pure_bind_detected() {
        // The accumulator idiom: `<-` form referencing the prior binding — must
        // divert to materialize (shadow), NOT the recursive decl route.
        assert!(self_referential_monadic_pure_bind("n <- pure (n + 1)", "n"));
        assert!(self_referential_monadic_pure_bind(
            "xs <- return (0 : xs)",
            "xs"
        ));
        // Non-self-referential `<-` pure binds still take the decl route.
        assert!(!self_referential_monadic_pure_bind("xs <- pure []", "xs"));
        assert!(!self_referential_monadic_pure_bind(
            "n <- pure (m + 1)",
            "n"
        ));
        // `let` is recursive in GHCi — left on the decl route intentionally.
        assert!(!self_referential_monadic_pure_bind("let n = n + 1", "n"));
        // A substring of the name is not a self-reference (whole-word only).
        assert!(!self_referential_monadic_pure_bind(
            "n <- pure (nn + 1)",
            "n"
        ));
    }
    use crate::command::TurnOutcome;

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
    fn decl_head_extracts_names() {
        assert_eq!(decl_head("slug t = T.replace \" \" \"-\" t"), "slug");
        assert_eq!(decl_head("data Foo = Bar | Baz"), "Foo");
        assert_eq!(
            decl_head("newtype Wrapper a = Wrapper { unwrap :: a }"),
            "Wrapper"
        );
        assert_eq!(decl_head("type Name = Text"), "Name");
        assert_eq!(decl_head("class MyClass a where"), "MyClass");
        assert_eq!(decl_head("  f x = x + 1"), "f");
        assert_eq!(decl_head(""), "");
        // #317: import → module, fixity → operator, prefix-op def → operator.
        assert_eq!(decl_head("import Data.Char"), "Data.Char");
        assert_eq!(decl_head("import qualified Data.Map as M"), "Data.Map");
        assert_eq!(decl_head("infixl 6 <+>"), "<+>");
        assert_eq!(decl_head("infixr 5 >>>"), ">>>");
        assert_eq!(decl_head("(<+>) = (++)"), "<+>");
        assert_eq!(decl_head("(<>) x y = x <> y"), "<>");
    }

    #[test]
    fn decl_head_skips_leading_comments() {
        // A leading `-- comment` line must not poison the head as "--".
        assert_eq!(
            decl_head("-- slugify a title\nslug t = T.replace \" \" \"-\" t"),
            "slug"
        );
        // Blank lines + multiple comment lines before the real decl.
        assert_eq!(
            decl_head("\n-- first note\n-- second note\ndata Foo = Bar"),
            "Foo"
        );
        // A dash-run immediately followed by a symbol char is an OPERATOR, not
        // a comment (Haskell lexical rule) — left untouched.
        assert_eq!(decl_head("(-->) x y = x"), "-->");
        assert_eq!(strip_leading_comments("--> merge x y"), "--> merge x y");
    }

    #[test]
    fn defines_head_sig_vs_binding() {
        use super::defines_head;
        // A binding defines; a bare signature does not.
        assert!(defines_head("rf x = x + 1", "rf"));
        assert!(!defines_head("rf :: Int -> Int", "rf"));
        // Sig+binding in one item defines.
        assert!(defines_head("rf :: Int -> Int\nrf x = x + 1", "rf"));
        // Identifier-boundary: `rfoo` does not define `rf`.
        assert!(!defines_head("rfoo x = 1", "rf"));
        // Multi-clause single item defines (once).
        assert!(defines_head("f 0 = 0\nf n = n", "f"));
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
            head: "slug".into(),
            type_display: Some("Text -> Text".into()),
            stale: Vec::new(),
        };
        let r = slim_item_result(&defined);
        assert_eq!(r["decl"], "slug");
        assert_eq!(r["type"], "Text -> Text", "inferred type painted (#317)");
        assert!(r.get("stale").is_none(), "no stale key when nothing stale");
        assert!(r.get("generation").is_none(), "no generation in slim decl");
        assert!(r.get("module").is_none(), "no module in slim decl");

        // A non-value decl (or a probe failure) carries no `type` field.
        let defined_no_type = TurnOutcome::Defined {
            generation: 1,
            module: "Tidepool.Session.Lib.G1".into(),
            head: "Node".into(),
            type_display: None,
            stale: Vec::new(),
        };
        let r = slim_item_result(&defined_no_type);
        assert_eq!(r["decl"], "Node");
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
                defining_expr: "42".to_string(),
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
