//! `PersistentSession` — the resident-JIT session core shared by
//! `tidepool-repl` and `tidepool-harness`.
//!
//! Both crates drive a long-lived [`JitEffectMachine`] turn-by-turn, accumulate
//! declarations (the [`SessionLib`] decl plane) and value bindings (the
//! [`BindingTable`] value plane), and union each turn's constructor metadata into
//! one growing [`DataConTable`]. That substrate — machine lifecycle, the two
//! planes, the accumulated table, and the fragment-run primitives — is identical
//! between them and lives here.
//!
//! There is ONE suspend mechanism, and it is **threadless**: an `Ask` stows the
//! whole machine as DATA ([`JitEffectMachine`] is `Send` precisely because it is
//! stowed-XOR-running), the eval thread exits, and a fresh thread re-enters via
//! `resume_suspended`. No OS thread is parked per suspended session — neither in
//! the harness (a TREE of many simultaneously-suspended nodes cannot pin N+1
//! threads) nor in the repl (see `plans/unpark/feasibility-map.md`).
//!
//! Every run entry here therefore reports either a completion or a suspension,
//! and every one has a `resume_*` sibling that re-enters the stowed continuation
//! with an answer or an abort. The four result-materialization policies the JIT
//! supports — plain `Value`, single `Bind`, projected multi-bind, and
//! bind+render — each appear as such a pair, each carrying what it actually
//! produces ([`SuspendableOutcome`] for the two `Value`-completing policies,
//! [`Suspendable`] over the roots for the other two). See the module docstrings
//! of [`super::resident`] (the harness's single-node consumer) and
//! `tidepool-repl`'s `session.rs` (the repl's block-cursor consumer) for the
//! orchestration around this core.

use std::path::Path;

use tidepool_codegen::binding_table::{BindingEntry, BindingTable};
use tidepool_codegen::emit::ExternalEnv;
use tidepool_codegen::jit_machine::{
    FuncId, JitEffectMachine, ResumeInput, Suspendable, SuspendableOutcome,
};
use tidepool_codegen::old_space::RootSlot;
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_eval::value::Value;
use tidepool_repr::{CoreExpr, DataCon, DataConTable, Generation, SessionModule, VarId};

use super::engine::OutputSink;
use super::{SessionError, SessionLib};
use crate::JitError;

// ---------------------------------------------------------------------------
// The shared session core
// ---------------------------------------------------------------------------

/// The resident-session substrate both servers own: one live [`JitEffectMachine`]
/// (`None` until the first turn bootstraps it), the accumulated constructor
/// [`DataConTable`], the [`SessionLib`] decl plane, the [`BindingTable`] value
/// plane, and the value-binding generation.
///
/// The consumers keep their own higher-level turn orchestration (source
/// wrapping, decl/pure-bind routing, output draining, continuation-id minting)
/// and delegate the machine + plane operations here.
pub struct PersistentSession {
    /// The resident machine — `None` before the first turn bootstraps it,
    /// `Some` when idle/suspended, and moved out onto the eval thread for a
    /// turn's duration (stowed-XOR-running).
    machine: Option<JitEffectMachine>,
    /// The constructor metadata unioned across turns (`insert_checked`, monotone:
    /// later turns are a subset), so an ADT value bound earlier renders with real
    /// con names later.
    session_table: DataConTable,
    /// The declaration plane: user `data`/`class`/`f x = …` accumulated as source
    /// across turns, imported by later turns through the gen-versioned module.
    /// `None` for a session with no decl plane; `Some` for the repl and the
    /// accumulating harness.
    lib: Option<SessionLib>,
    /// The value plane: `name → (SessionVarId, RootSlot, Val.G<g>)` for each
    /// materialized bind, seeded into a later fragment's [`ExternalEnv`].
    bindings: BindingTable,
    /// Monotonic value-binding generation. Each materialized bind mints a fresh
    /// `Val.G<g>` so its `stableVarId` is collision-free and a rebind shadows
    /// without clobbering the prior root.
    val_gen: Generation,
    /// Monotonic per-turn counter → unique fragment function names.
    turn_counter: u64,
    /// The `Ask` union tag intercepted at the suspend boundary.
    ask_tag: u64,
    /// JIT nursery size for the resident machine.
    nursery_size: usize,
}

impl PersistentSession {
    /// Build an idle session core. `lib` is the decl plane (`Some` for the repl
    /// and the accumulating harness; `None` for a value-plane-only session). The
    /// machine is not bootstrapped until the first turn.
    pub fn new(lib: Option<SessionLib>, ask_tag: u64, nursery_size: usize) -> Self {
        PersistentSession {
            machine: None,
            session_table: DataConTable::new(),
            lib,
            bindings: BindingTable::new(),
            val_gen: Generation(0),
            turn_counter: 0,
            ask_tag,
            nursery_size,
        }
    }

    // -- accessors ---------------------------------------------------------

    /// The decl-plane library (read). Panics if the session has no decl plane —
    /// a repl invariant; the harness only calls this once a decl plane has been
    /// installed.
    pub fn lib(&self) -> &SessionLib {
        self.lib.as_ref().expect("decl plane present")
    }
    /// The decl-plane library (mutate — e.g. `define_batch_with_vals`). Panics if
    /// the session has no decl plane (see [`Self::lib`]).
    pub fn lib_mut(&mut self) -> &mut SessionLib {
        self.lib.as_mut().expect("decl plane present")
    }
    /// Whether this session has a decl plane.
    pub fn has_lib(&self) -> bool {
        self.lib.is_some()
    }
    /// The value-plane binding table (read).
    pub fn bindings(&self) -> &BindingTable {
        &self.bindings
    }
    /// The value-plane binding table (mutate).
    pub fn bindings_mut(&mut self) -> &mut BindingTable {
        &mut self.bindings
    }
    /// The accumulated constructor table.
    pub fn session_table(&self) -> &DataConTable {
        &self.session_table
    }
    /// The current value-binding generation.
    pub fn val_gen(&self) -> Generation {
        self.val_gen
    }
    /// Set the value-binding generation (a consumer advances it when a bind
    /// materializes at a freshly-minted `Val.G<g>`).
    pub fn set_val_gen(&mut self, g: Generation) {
        self.val_gen = g;
    }
    /// The `Ask` union tag this session suspends on.
    pub fn ask_tag(&self) -> u64 {
        self.ask_tag
    }
    /// Whether the resident machine has been bootstrapped (first turn run).
    pub fn is_bootstrapped(&self) -> bool {
        self.machine.is_some()
    }
    /// The resident machine, if bootstrapped (read — e.g. `heap_stats`).
    pub fn machine(&self) -> Option<&JitEffectMachine> {
        self.machine.as_ref()
    }
    /// The resident machine, if bootstrapped (mutate).
    pub fn machine_mut(&mut self) -> Option<&mut JitEffectMachine> {
        self.machine.as_mut()
    }

    // -- table accumulation ------------------------------------------------

    /// Seed the accumulated session table wholesale (the bootstrap turn's table
    /// becomes the base; later turns [`Self::merge_table`] onto it).
    pub fn seed_session_table(&mut self, table: DataConTable) {
        self.session_table = table;
    }

    /// Union `table`'s constructors into the accumulated session table
    /// (`extend_checked`; loud on a genuine `stableVarId` collision — gen-versioned
    /// names make that a real bug, not churn).
    ///
    /// A turn's table is normally a SUBSET of what earlier turns already
    /// accumulated, so entries already present with identical metadata are
    /// filtered out before touching the table at all — no clone, no index
    /// work, no sort for the steady-state no-new-constructors turn. What
    /// remains is batched through [`DataConTable::extend_checked`], which
    /// sorts each affected `by_type_name` bucket once instead of once per
    /// insert.
    pub fn merge_table(&mut self, table: &DataConTable) -> Result<(), String> {
        let turn_cons = table.iter().count();
        let incoming: Vec<DataCon> = table
            .iter()
            .filter(|&dc| self.session_table.get(dc.id) != Some(dc))
            .cloned()
            .collect();
        log::debug!(
            target: "tidepool::session",
            "merge_table turn_cons={turn_cons} skipped={} applied={} session_cons_before={}",
            turn_cons - incoming.len(),
            incoming.len(),
            self.session_table.len(),
        );
        self.session_table
            .extend_checked(incoming)
            .map_err(|e| format!("session DataConTable collision: {e}"))
    }

    // -- machine lifecycle -------------------------------------------------
    //
    // Threading note: [`BindingTable`] holds `RootSlot(*mut *mut u8)` and
    // [`ExternalEnv`] holds raw slot addresses. Both the table and the
    // [`JitEffectMachine`] carry an `unsafe impl Send` justified by the
    // stowed-XOR-running discipline, so a whole `PersistentSession` can be moved
    // to another thread as long as exactly one thread owns it at a time — which
    // is what the repl does (the session is moved into a `spawn_blocking` turn
    // and returned out of it). The harness instead runs the deep-recursion turn
    // on a fresh big-stack thread while keeping the rest of the session on the
    // caller's frame: it [`Self::take_machine`]s the machine over with the
    // accumulated table and [`Self::restore_machine`]s it afterwards.
    // `add_function` (which needs the raw-pointer env) therefore always happens
    // on the thread that owns the session.

    /// Bootstrap the resident machine from `expr`/`table` if it is not already
    /// live (a session machine, so its heap is retained across turns). No-op when
    /// already bootstrapped. The bootstrap `expr` seeds the machine's ConTags; it
    /// is NOT run (the harness bootstrap seed, and the repl bind path, both
    /// bootstrap-then-fragment without running the seed).
    pub fn bootstrap_if_needed(
        &mut self,
        expr: &CoreExpr,
        table: &DataConTable,
    ) -> Result<(), JitError> {
        if self.machine.is_none() {
            self.machine = Some(JitEffectMachine::compile_session(
                expr,
                table,
                self.nursery_size,
            )?);
        }
        Ok(())
    }

    /// Move the resident machine out (to run a turn on a fresh big-stack eval
    /// thread — the machine is `Send`, the rest of the session is not). Pair with
    /// [`Self::restore_machine`]. Panics if the machine is not bootstrapped or is
    /// already taken.
    pub fn take_machine(&mut self) -> JitEffectMachine {
        self.machine
            .take()
            .expect("machine present (idle or suspended) before a turn")
    }

    /// Move a previously [`Self::take_machine`]d machine back into the session.
    pub fn restore_machine(&mut self, machine: JitEffectMachine) {
        self.machine = Some(machine);
    }

    // There is deliberately no `drop_machine`: tearing a session down means
    // dropping the whole `PersistentSession` (which frees the heap through the
    // machine's own `Drop`). A method that emptied the machine slot in place
    // would leave a live session whose value-plane `RootSlot`s all dangle — a
    // state with no legitimate use and no way to detect from the outside.

    // -- fragment preparation (CALLER thread; touches the `!Send` env) ------

    /// Add `expr` as a fragment against the ACCUMULATED session table, seeded by
    /// `env`, minting the fragment name `"{name_hint}_{turn}"`. Returns the
    /// `FuncId` a later run drives. Merge the turn's table via [`Self::merge_table`]
    /// first. Requires the machine bootstrapped.
    pub fn add_fragment_session(
        &mut self,
        name_hint: &str,
        expr: &CoreExpr,
        env: &ExternalEnv,
    ) -> Result<FuncId, JitError> {
        self.turn_counter += 1;
        let frag_name = format!("{name_hint}_{}", self.turn_counter);
        let PersistentSession {
            machine,
            session_table,
            ..
        } = self;
        let machine = machine
            .as_mut()
            .expect("machine bootstrapped before add_fragment_session");
        machine.add_function(&frag_name, expr, session_table, env)
    }

    /// Nested-child sibling of [`Self::add_fragment_session`], minting the name
    /// `"{name_hint}_child_{turn}"`.
    pub fn add_child_fragment_session(
        &mut self,
        name_hint: &str,
        expr: &CoreExpr,
        env: &ExternalEnv,
    ) -> Result<FuncId, JitError> {
        self.turn_counter += 1;
        let frag_name = format!("{name_hint}_child_{}", self.turn_counter);
        let PersistentSession {
            machine,
            session_table,
            ..
        } = self;
        let machine = machine
            .as_mut()
            .expect("machine bootstrapped before add_child_fragment_session");
        machine.add_function(&frag_name, expr, session_table, env)
    }

    /// Add `expr` as a fragment against an EXTERNAL per-turn table (not the
    /// accumulated one) — the repl's plain-expression path, where a standalone
    /// turn carries its own table. Requires the machine bootstrapped.
    pub fn add_fragment_with_table(
        &mut self,
        name_hint: &str,
        expr: &CoreExpr,
        run_table: &DataConTable,
        env: &ExternalEnv,
    ) -> Result<FuncId, JitError> {
        self.turn_counter += 1;
        let frag_name = format!("{name_hint}_{}", self.turn_counter);
        let machine = self
            .machine
            .as_mut()
            .expect("machine bootstrapped before add_fragment_with_table");
        machine.add_function(&frag_name, expr, run_table, env)
    }

    // -- in-place suspendable runs (machine owned here) ---------------------
    //
    // Each of the four result-materialization policies gets a `run_*` entry and
    // a `resume_*` sibling; both return a [`SuspendableOutcome`], so a caller
    // handles completion and suspension with the SAME code whichever run it came
    // from. A suspension leaves the continuation stowed on the machine — nothing
    // is parked, nothing blocks — and the caller re-enters through the matching
    // `resume_*`, which must be the sibling of the entry that suspended (the
    // policy decides what the completing turn materializes).

    /// Run the resident machine's ORIGINAL entry (the seed compiled by
    /// [`Self::bootstrap_if_needed`]) to its first boundary. The repl's first
    /// bare-expression turn, where the seed IS the program. `run_table` is the
    /// seed's table; [`Self::resume_with_table`] re-enters with the same one.
    /// Requires the machine bootstrapped.
    pub fn run_entry<O, H>(
        &mut self,
        run_table: &DataConTable,
        handlers: &mut H,
        captured: &O,
    ) -> Result<SuspendableOutcome, JitError>
    where
        O: OutputSink,
        H: DispatchEffect<O>,
    {
        let ask_tag = self.ask_tag;
        let machine = self
            .machine
            .as_mut()
            .expect("machine bootstrapped before run_entry");
        machine.run_suspendable(run_table, handlers, captured, ask_tag)
    }

    /// Drive a fragment (already added) to its first boundary against an
    /// EXTERNAL table. The repl plain-expression path; resumed by
    /// [`Self::resume_with_table`].
    pub fn run_funcid_with_table<O, H>(
        &mut self,
        func_id: FuncId,
        run_table: &DataConTable,
        handlers: &mut H,
        captured: &O,
    ) -> Result<SuspendableOutcome, JitError>
    where
        O: OutputSink,
        H: DispatchEffect<O>,
    {
        let ask_tag = self.ask_tag;
        let machine = self
            .machine
            .as_mut()
            .expect("machine bootstrapped before run_funcid_with_table");
        machine.run_fragment_suspendable(func_id, run_table, handlers, captured, ask_tag)
    }

    /// Re-enter a turn suspended by [`Self::run_entry`] or
    /// [`Self::run_funcid_with_table`], against the SAME external `run_table`
    /// the run used (the suspend request and the completed value are both
    /// bridged against it).
    pub fn resume_with_table<O, H>(
        &mut self,
        run_table: &DataConTable,
        handlers: &mut H,
        captured: &O,
        input: ResumeInput,
    ) -> Result<SuspendableOutcome, JitError>
    where
        O: OutputSink,
        H: DispatchEffect<O>,
    {
        let ask_tag = self.ask_tag;
        let machine = self
            .machine
            .as_mut()
            .expect("machine present before resume_with_table");
        machine.resume_suspended(run_table, handlers, captured, ask_tag, input)
    }

    /// Drive a fragment (already added) to its first boundary against the
    /// ACCUMULATED session table. The repl's effectful session-reference path;
    /// resumed by [`Self::resume_session`].
    pub fn run_funcid_session<O, H>(
        &mut self,
        func_id: FuncId,
        handlers: &mut H,
        captured: &O,
    ) -> Result<SuspendableOutcome, JitError>
    where
        O: OutputSink,
        H: DispatchEffect<O>,
    {
        let PersistentSession {
            machine,
            session_table,
            ask_tag,
            ..
        } = self;
        let machine = machine
            .as_mut()
            .expect("machine bootstrapped before run_funcid_session");
        machine.run_fragment_suspendable(func_id, session_table, handlers, captured, *ask_tag)
    }

    /// Re-enter a turn suspended by [`Self::run_funcid_session`], against the
    /// accumulated session table.
    pub fn resume_session<O, H>(
        &mut self,
        handlers: &mut H,
        captured: &O,
        input: ResumeInput,
    ) -> Result<SuspendableOutcome, JitError>
    where
        O: OutputSink,
        H: DispatchEffect<O>,
    {
        let PersistentSession {
            machine,
            session_table,
            ask_tag,
            ..
        } = self;
        let machine = machine
            .as_mut()
            .expect("machine present before resume_session");
        machine.resume_suspended(session_table, handlers, captured, *ask_tag, input)
    }

    /// Run a PURE fragment (no effect tree) to a value against the accumulated
    /// table. The repl's pure session-reference path (`run_fragment_pure`).
    /// Pure means no effects, hence no `Ask`, hence no suspension — this is the
    /// one run entry with no `resume_*` sibling.
    pub fn run_funcid_pure(&mut self, func_id: FuncId) -> Result<Value, JitError> {
        let machine = self
            .machine
            .as_mut()
            .expect("machine bootstrapped before run_funcid_pure");
        machine.run_fragment_pure(func_id)
    }

    /// Drive a fragment (already added) as an effectful VALUE BIND against the
    /// accumulated table: run the effect tree and, on completion, deep-force
    /// (`forced` → Tier-0 data) or tenure-as-is (Tier-1 closure) and register the
    /// persistent root. Read the tenured root with [`Self::take_bound_root`]; a
    /// suspension tenures NOTHING yet, and lands its value on
    /// [`Self::resume_bind`] with the SAME `forced` flag.
    pub fn bind_funcid<O, H>(
        &mut self,
        func_id: FuncId,
        handlers: &mut H,
        captured: &O,
        forced: bool,
    ) -> Result<SuspendableOutcome, JitError>
    where
        O: OutputSink,
        H: DispatchEffect<O>,
    {
        let PersistentSession {
            machine,
            session_table,
            ask_tag,
            ..
        } = self;
        let machine = machine
            .as_mut()
            .expect("machine bootstrapped before bind_funcid");
        machine.run_fragment_suspendable_binding(
            func_id,
            session_table,
            handlers,
            captured,
            *ask_tag,
            forced,
        )
    }

    /// Re-enter a turn suspended by [`Self::bind_funcid`]. `forced` is the SAME
    /// flag the run carried — the caller threads it across the suspension on the
    /// binder metadata it is already holding.
    pub fn resume_bind<O, H>(
        &mut self,
        handlers: &mut H,
        captured: &O,
        input: ResumeInput,
        forced: bool,
    ) -> Result<SuspendableOutcome, JitError>
    where
        O: OutputSink,
        H: DispatchEffect<O>,
    {
        let PersistentSession {
            machine,
            session_table,
            ask_tag,
            ..
        } = self;
        let machine = machine
            .as_mut()
            .expect("machine present before resume_bind");
        machine.resume_suspended_binding(session_table, handlers, captured, *ask_tag, input, forced)
    }

    /// Multi-binder sibling of [`Self::bind_funcid`]: on completion, project
    /// `n_fields` tuple components and tenure each as a separate root.
    ///
    /// Completion IS those roots. A projection has no result value of its own,
    /// and the outcome type says so — there is no bridged tuple to render and
    /// none to accidentally render.
    pub fn bind_funcid_projected<O, H>(
        &mut self,
        func_id: FuncId,
        handlers: &mut H,
        captured: &O,
        n_fields: usize,
    ) -> Result<Suspendable<Vec<RootSlot>>, JitError>
    where
        O: OutputSink,
        H: DispatchEffect<O>,
    {
        let PersistentSession {
            machine,
            session_table,
            ask_tag,
            ..
        } = self;
        let machine = machine
            .as_mut()
            .expect("machine bootstrapped before bind_funcid_projected");
        machine.run_fragment_suspendable_projected(
            func_id,
            session_table,
            handlers,
            captured,
            *ask_tag,
            n_fields,
        )
    }

    /// Re-enter a turn suspended by [`Self::bind_funcid_projected`]. `n_fields`
    /// is the SAME arity the run carried (the caller is holding the binder
    /// vector across the suspension).
    pub fn resume_bind_projected<O, H>(
        &mut self,
        handlers: &mut H,
        captured: &O,
        input: ResumeInput,
        n_fields: usize,
    ) -> Result<Suspendable<Vec<RootSlot>>, JitError>
    where
        O: OutputSink,
        H: DispatchEffect<O>,
    {
        let PersistentSession {
            machine,
            session_table,
            ask_tag,
            ..
        } = self;
        let machine = machine
            .as_mut()
            .expect("machine present before resume_bind_projected");
        machine.resume_suspended_projected(
            session_table,
            handlers,
            captured,
            *ask_tag,
            input,
            n_fields,
        )
    }

    /// Bind-and-render sibling of [`Self::bind_funcid`]: run the fragment ONCE,
    /// binding field 0 (`field0_forced` → Tier-0 data) and rendering field 1 in
    /// the same run. Completion carries BOTH — field 0's tenured root and field
    /// 1's rendered value. The repl's bare-expression `it` path.
    pub fn bind_funcid_render<O, H>(
        &mut self,
        func_id: FuncId,
        handlers: &mut H,
        captured: &O,
        field0_forced: bool,
    ) -> Result<Suspendable<(RootSlot, Value)>, JitError>
    where
        O: OutputSink,
        H: DispatchEffect<O>,
    {
        let PersistentSession {
            machine,
            session_table,
            ask_tag,
            ..
        } = self;
        let machine = machine
            .as_mut()
            .expect("machine bootstrapped before bind_funcid_render");
        machine.run_fragment_suspendable_render(
            func_id,
            session_table,
            handlers,
            captured,
            *ask_tag,
            field0_forced,
        )
    }

    /// Re-enter a turn suspended by [`Self::bind_funcid_render`]. The
    /// field1-before-field0-tenure ordering that keeps an aliased
    /// `(it, toWire it)` intact lives in the machine's one `materialize`, so it
    /// holds identically here and on the first run.
    pub fn resume_bind_render<O, H>(
        &mut self,
        handlers: &mut H,
        captured: &O,
        input: ResumeInput,
        field0_forced: bool,
    ) -> Result<Suspendable<(RootSlot, Value)>, JitError>
    where
        O: OutputSink,
        H: DispatchEffect<O>,
    {
        let PersistentSession {
            machine,
            session_table,
            ask_tag,
            ..
        } = self;
        let machine = machine
            .as_mut()
            .expect("machine present before resume_bind_render");
        machine.resume_suspended_render(
            session_table,
            handlers,
            captured,
            *ask_tag,
            input,
            field0_forced,
        )
    }

    /// Take the tenured root a completed [`Self::bind_funcid`] (or
    /// [`Self::resume_bind`]) stashed on the machine. `None` when no bind
    /// completed — the caller treats that as the infra error it is.
    ///
    /// Only the single-bind policy needs this, and NOT because of its outcome
    /// type's shape. `RootSlot` is `!Send`; bind is the one policy
    /// [`super::ResidentSession`] drives, and it does so through
    /// `on_eval_thread`, which returns the completion across a scoped-thread
    /// join. A slot riding out inline does not compile there — stashing it
    /// inside the (`Send`-blessed) machine is what carries it across. The
    /// projected and render policies return their roots inline only because
    /// nothing drives them across a thread; their sole caller is this
    /// single-threaded repl path.
    ///
    /// Removing this was attempted and reverted: it requires
    /// `unsafe impl Send for RootSlot`, a standalone soundness claim on a raw
    /// pointer rather than a refactor (`plans/unpark/` §6.2).
    pub fn take_bound_root(&mut self) -> Option<RootSlot> {
        self.machine.as_mut().and_then(|m| m.take_last_bound_root())
    }

    /// Whether the machine currently holds a stowed continuation (a turn
    /// suspended at an `Ask` and has not been resumed or aborted).
    pub fn is_suspended(&self) -> bool {
        self.machine.as_ref().is_some_and(|m| m.is_suspended())
    }

    // -- value-plane bookkeeping (delegating over the two planes) ----------

    /// Build the [`ExternalEnv`] a later fragment consults at a `Var`-miss:
    /// the `SessionVarId → RootSlot` of every live binding `referenced`
    /// names — typically `tidepool_repr::free_vars(&fragment)`. Not every
    /// live binding: a binding absent from `referenced` still stays a GC
    /// root (registered at bind time, independent of this call) but is not
    /// seeded into this particular fragment's env.
    pub fn seed_external_env(&self, referenced: &[VarId]) -> ExternalEnv {
        self.bindings.seed_external_env(referenced)
    }

    /// Record a materialized value binding on the value plane.
    pub fn bind(&mut self, entry: BindingEntry) {
        self.bindings.bind(entry);
    }

    /// Module names of every live value binding — injected (`--inject-val`) AND
    /// so already-compiled fragments / closure captures keep resolving. Includes
    /// shadowed older gens.
    pub fn live_val_modules(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .bindings
            .live_modules()
            .map(|m| m.module_name())
            .collect();
        v.sort();
        v.dedup();
        v
    }

    /// The CURRENT (newest) `Val.G<g>` module per still-live name — what a turn
    /// IMPORTS unqualified (excludes shadowed older gens, which would make a
    /// rebound name an ambiguous occurrence).
    pub fn current_val_modules(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .bindings
            .iter_current()
            .map(|(_, entry)| entry.module.module_name())
            .collect();
        v.sort();
        v.dedup();
        v
    }

    /// The current decl-plane module (`Lib.G<g>`), if any (also `None` when the
    /// session has no decl plane at all).
    pub fn current_lib_module(&self) -> Option<SessionModule> {
        self.lib.as_ref().and_then(|l| l.current_module())
    }

    /// The decl-plane include directory (where `Lib.G<g>.hs` modules live), for
    /// a later turn's compile search path. `None` when the session has no decl
    /// plane.
    pub fn lib_include_dir(&self) -> Option<&Path> {
        self.lib.as_ref().map(|l| l.include_dir())
    }

    /// Define decl text(s) scoped against live session values: the current
    /// `Val.G<g>` per still-live name are imported unqualified, every live
    /// `Val.G<g>` is injected for validation. The decl-plane analogue of GHCi
    /// seeing earlier bindings from a new top-level definition.
    pub fn define_scoped(&mut self, decl_texts: &[&str]) -> Result<Generation, SessionError> {
        let import_modules = self.current_val_modules();
        let inject_modules = self.live_val_modules();
        self.lib
            .as_mut()
            .expect("decl plane present")
            .define_batch_with_vals(decl_texts, &import_modules, &inject_modules)
    }

    /// Retract `name` from the decl plane (its binding migrated to the value
    /// plane). No-op when `name` is not a current decl head.
    pub fn retract(&mut self, name: &str) -> Result<(), SessionError> {
        match self.lib.as_mut() {
            Some(lib) => lib.retract(name),
            None => Ok(()),
        }
    }
}
