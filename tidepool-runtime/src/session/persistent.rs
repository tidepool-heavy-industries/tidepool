//! `PersistentSession<S>` — the resident-JIT session core shared by
//! `tidepool-repl` and `tidepool-harness`.
//!
//! Both crates drive a long-lived [`JitEffectMachine`] turn-by-turn, accumulate
//! declarations (the [`SessionLib`] decl plane) and value bindings (the
//! [`BindingTable`] value plane), and union each turn's constructor metadata into
//! one growing [`DataConTable`]. That substrate — machine lifecycle, the two
//! planes, the accumulated table, and the fragment-run primitives — is identical
//! between them and lives here.
//!
//! The ONE axis on which they differ is the **suspend boundary**, captured by
//! [`SuspensionMechanism`]:
//!
//! * [`ParkedThread`] (repl): an `Ask` parks the resident worker thread on an
//!   answer channel with the native stack intact. The caller wraps the handler
//!   stack (`ReplAskDispatcher`) so the `Ask` is serviced inline — from the
//!   machine's view a fragment always runs to completion, and resume happens
//!   out-of-band by waking the parked thread. Fine for ONE session pinned to one
//!   OS thread.
//! * [`Threadless`] (harness): an `Ask` stows the whole machine as DATA
//!   ([`JitEffectMachine`] is `Send` precisely because it is stowed-XOR-running),
//!   the eval thread exits, and a fresh thread re-enters via `resume_suspended`.
//!   A TREE of many simultaneously-suspended nodes (a parent parked on a fork
//!   while N children run) cannot pin N+1 threads, so threadless is load-bearing.
//!
//! The suspension MECHANISMS are deliberately NOT unified — only this
//! accumulation + turn-run core is. See the module docstrings of
//! [`super::resident`] (threadless) and `tidepool-repl`'s `ask.rs` (parked) for
//! the two consumers' orchestration around this core.

use std::marker::PhantomData;
use std::path::Path;

use tidepool_codegen::binding_table::{BindingEntry, BindingTable};
use tidepool_codegen::emit::ExternalEnv;
use tidepool_codegen::jit_machine::{FuncId, JitEffectMachine, ResumeInput, SuspendableOutcome};
use tidepool_codegen::old_space::RootSlot;
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_eval::value::Value;
use tidepool_repr::{CoreExpr, DataCon, DataConTable, Generation, SessionModule, VarId};

use super::engine::OutputSink;
use super::{SessionError, SessionLib};
use crate::JitError;

// ---------------------------------------------------------------------------
// The suspend-boundary axis
// ---------------------------------------------------------------------------

/// How a persistent session drives a turn to its first boundary and re-enters a
/// suspended one. The only behavior that differs between the parked-thread (repl)
/// and threadless (harness) session models; everything else is shared in
/// [`PersistentSession`].
///
/// The three methods correspond exactly to the three [`JitEffectMachine`] run
/// entries a turn can take: the machine's original entry ([`Self::run_entry`]),
/// an added fragment ([`Self::run_fragment`]), and re-entry of a stowed
/// continuation ([`Self::resume`]). The bind / child-run primitives do NOT go
/// through here — they are mechanism-agnostic (an `Ask` inside a bind parks the
/// worker under `ParkedThread`, and threadless bind-suspension is out of scope).
pub trait SuspensionMechanism {
    /// Drive the machine's ORIGINAL entry to its first boundary. `ParkedThread`
    /// runs it to completion (`Ask` serviced inline by the caller-wrapped
    /// dispatcher); `Threadless` runs it through the suspend driver, yielding
    /// `Suspended` with the continuation stowed on the machine.
    fn run_entry<O, H>(
        machine: &mut JitEffectMachine,
        table: &DataConTable,
        handlers: &mut H,
        captured: &O,
        ask_tag: u64,
    ) -> Result<SuspendableOutcome, JitError>
    where
        O: OutputSink,
        H: DispatchEffect<O>;

    /// Drive a freshly-added fragment (`func_id`) to its first boundary — the
    /// fragment sibling of [`Self::run_entry`].
    fn run_fragment<O, H>(
        machine: &mut JitEffectMachine,
        func_id: FuncId,
        table: &DataConTable,
        handlers: &mut H,
        captured: &O,
        ask_tag: u64,
    ) -> Result<SuspendableOutcome, JitError>
    where
        O: OutputSink,
        H: DispatchEffect<O>;

    /// Re-enter a suspended turn with an answer or abort. Only `Threadless`
    /// supports in-band resume (the machine holds the stowed continuation);
    /// `ParkedThread` resumes out-of-band by waking the parked worker, so a
    /// [`PersistentSession<ParkedThread>`] never calls this.
    fn resume<O, H>(
        machine: &mut JitEffectMachine,
        table: &DataConTable,
        handlers: &mut H,
        captured: &O,
        ask_tag: u64,
        input: ResumeInput,
    ) -> Result<SuspendableOutcome, JitError>
    where
        O: OutputSink,
        H: DispatchEffect<O>;
}

/// Repl mechanism: the resident worker thread parks on an `Ask` (native stack
/// intact) and the machine stays pinned to it. The `Ask` is serviced inline by
/// the caller-installed `ReplAskDispatcher`, so a fragment run always completes.
pub struct ParkedThread;

impl SuspensionMechanism for ParkedThread {
    fn run_entry<O, H>(
        machine: &mut JitEffectMachine,
        table: &DataConTable,
        handlers: &mut H,
        captured: &O,
        _ask_tag: u64,
    ) -> Result<SuspendableOutcome, JitError>
    where
        O: OutputSink,
        H: DispatchEffect<O>,
    {
        machine
            .run(table, handlers, captured)
            .map(SuspendableOutcome::Completed)
    }

    fn run_fragment<O, H>(
        machine: &mut JitEffectMachine,
        func_id: FuncId,
        table: &DataConTable,
        handlers: &mut H,
        captured: &O,
        _ask_tag: u64,
    ) -> Result<SuspendableOutcome, JitError>
    where
        O: OutputSink,
        H: DispatchEffect<O>,
    {
        machine
            .run_fragment(func_id, table, handlers, captured)
            .map(SuspendableOutcome::Completed)
    }

    fn resume<O, H>(
        _machine: &mut JitEffectMachine,
        _table: &DataConTable,
        _handlers: &mut H,
        _captured: &O,
        _ask_tag: u64,
        _input: ResumeInput,
    ) -> Result<SuspendableOutcome, JitError>
    where
        O: OutputSink,
        H: DispatchEffect<O>,
    {
        // The parked-thread mechanism resumes out-of-band: the server wakes the
        // worker blocked in `ReplAskDispatcher`'s answer-channel `recv`. A
        // `PersistentSession<ParkedThread>` never sees `Suspended`, so this is
        // unreachable by construction.
        unreachable!(
            "ParkedThread resumes out-of-band by waking the parked worker; \
             PersistentSession::resume is only reachable under Threadless"
        )
    }
}

/// Harness mechanism: an `Ask` stows the machine as DATA (no parked thread), the
/// eval thread exits, and a fresh thread re-enters via `resume_suspended`.
/// Load-bearing for a tree of many simultaneously-suspended nodes.
pub struct Threadless;

impl SuspensionMechanism for Threadless {
    fn run_entry<O, H>(
        machine: &mut JitEffectMachine,
        table: &DataConTable,
        handlers: &mut H,
        captured: &O,
        ask_tag: u64,
    ) -> Result<SuspendableOutcome, JitError>
    where
        O: OutputSink,
        H: DispatchEffect<O>,
    {
        machine.run_suspendable(table, handlers, captured, ask_tag)
    }

    fn run_fragment<O, H>(
        machine: &mut JitEffectMachine,
        func_id: FuncId,
        table: &DataConTable,
        handlers: &mut H,
        captured: &O,
        ask_tag: u64,
    ) -> Result<SuspendableOutcome, JitError>
    where
        O: OutputSink,
        H: DispatchEffect<O>,
    {
        machine.run_fragment_suspendable(func_id, table, handlers, captured, ask_tag)
    }

    fn resume<O, H>(
        machine: &mut JitEffectMachine,
        table: &DataConTable,
        handlers: &mut H,
        captured: &O,
        ask_tag: u64,
        input: ResumeInput,
    ) -> Result<SuspendableOutcome, JitError>
    where
        O: OutputSink,
        H: DispatchEffect<O>,
    {
        machine.resume_suspended(table, handlers, captured, ask_tag, input)
    }
}

// ---------------------------------------------------------------------------
// The shared session core
// ---------------------------------------------------------------------------

/// The resident-session substrate both servers own: one live [`JitEffectMachine`]
/// (`None` until the first turn bootstraps it), the accumulated constructor
/// [`DataConTable`], the [`SessionLib`] decl plane, the [`BindingTable`] value
/// plane, and the value-binding generation. Parameterized over the
/// [`SuspensionMechanism`] so `run_entry`/`run_fragment`/`resume` dispatch to the
/// parked-thread or threadless machine calls without the caller branching.
///
/// The consumers keep their own higher-level turn orchestration (source
/// wrapping, decl/pure-bind routing, output draining, continuation-id minting)
/// and delegate the machine + plane operations here.
pub struct PersistentSession<S: SuspensionMechanism> {
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
    /// `None` for a session with no decl plane (the harness before W1b turns on
    /// accumulation); `Some` for the repl and the accumulating harness.
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
    _mech: PhantomData<S>,
}

impl<S: SuspensionMechanism> PersistentSession<S> {
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
            _mech: PhantomData,
        }
    }

    // -- accessors ---------------------------------------------------------

    /// The decl-plane library (read). Panics if the session has no decl plane —
    /// a repl invariant; the harness only calls this once W1b has installed one.
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
    // [`ExternalEnv`] holds raw slot addresses, so `PersistentSession` (and the
    // env a fragment seeds) is `!Send` — only the [`JitEffectMachine`] is `Send`
    // (stowed-XOR-running). The repl drives every turn on ITS pinned worker
    // thread, so it runs in place ([`Self::run_fragment_session`] etc.). The
    // harness must run the deep-recursion turn on a fresh big-stack thread, so it
    // [`Self::take_machine`]s the machine (Send) over, calls the
    // [`SuspensionMechanism`] trait methods on it there with the accumulated
    // table (also Send), and [`Self::restore_machine`]s it. `add_function` (which
    // needs the `!Send` env) therefore always happens on the CALLING thread.

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

    /// Drop the resident machine, freeing the session heap (its `Drop` reclaims
    /// the persistent roots). The value-plane [`RootSlot`]s become dangling, so
    /// only call this when the session is being torn down.
    pub fn drop_machine(&mut self) {
        self.machine = None;
    }

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

    // -- same-thread runs (repl; machine in place, accumulated table) -------

    /// Run the resident machine's ORIGINAL entry (the seed compiled by
    /// [`Self::bootstrap_if_needed`]) to the first boundary
    /// ([`SuspensionMechanism::run_entry`]). The repl's first bare-expression
    /// turn, where the seed IS the program. `run_table` is the seed's table.
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
        let machine = self
            .machine
            .as_mut()
            .expect("machine bootstrapped before run_entry");
        S::run_entry(machine, run_table, handlers, captured, self.ask_tag)
    }

    /// Drive a fragment (already added) to the first boundary against an EXTERNAL
    /// table, on the calling thread ([`SuspensionMechanism::run_fragment`]). The
    /// repl plain-expression path.
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
        let machine = self
            .machine
            .as_mut()
            .expect("machine bootstrapped before run_funcid_with_table");
        S::run_fragment(
            machine,
            func_id,
            run_table,
            handlers,
            captured,
            self.ask_tag,
        )
    }

    /// Drive a fragment (already added) to the first boundary against the
    /// ACCUMULATED session table, on the calling thread
    /// ([`SuspensionMechanism::run_fragment`]). The repl's effectful
    /// session-reference path.
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
        S::run_fragment(
            machine,
            func_id,
            session_table,
            handlers,
            captured,
            *ask_tag,
        )
    }

    /// Run a PURE fragment (no effect tree) to a value against the accumulated
    /// table. The repl's pure session-reference path (`run_fragment_pure`).
    pub fn run_funcid_pure(&mut self, func_id: FuncId) -> Result<Value, JitError> {
        let machine = self
            .machine
            .as_mut()
            .expect("machine bootstrapped before run_funcid_pure");
        machine.run_fragment_pure(func_id)
    }

    /// Drive a fragment (already added) as an effectful VALUE BIND against the
    /// accumulated table, on the calling thread: run the effect tree, deep-force
    /// (`forced` → Tier-0 data) or tenure-as-is (Tier-1 closure), register the
    /// persistent root, return its stable [`RootSlot`]. The repl bind path.
    pub fn bind_funcid<O, H>(
        &mut self,
        func_id: FuncId,
        handlers: &mut H,
        captured: &O,
        forced: bool,
    ) -> Result<RootSlot, JitError>
    where
        O: OutputSink,
        H: DispatchEffect<O>,
    {
        let PersistentSession {
            machine,
            session_table,
            ..
        } = self;
        let machine = machine
            .as_mut()
            .expect("machine bootstrapped before bind_funcid");
        machine.run_fragment_and_bind(func_id, session_table, handlers, captured, forced)
    }

    /// Multi-binder sibling of [`Self::bind_funcid`]: project `n_fields` tuple
    /// components, tenuring each as a separate root.
    pub fn bind_funcid_projected<O, H>(
        &mut self,
        func_id: FuncId,
        handlers: &mut H,
        captured: &O,
        n_fields: usize,
    ) -> Result<Vec<RootSlot>, JitError>
    where
        O: OutputSink,
        H: DispatchEffect<O>,
    {
        let PersistentSession {
            machine,
            session_table,
            ..
        } = self;
        let machine = machine
            .as_mut()
            .expect("machine bootstrapped before bind_funcid_projected");
        machine.run_fragment_and_bind_projected(
            func_id,
            session_table,
            handlers,
            captured,
            n_fields,
        )
    }

    /// Bind-and-render sibling of [`Self::bind_funcid`]: run the fragment ONCE,
    /// binding field 0 (`field0_forced` → Tier-0 data) and rendering field 1 in
    /// the same run. Returns the bound value's [`RootSlot`] and the rendered
    /// value. The repl's bare-expression `it` path.
    pub fn bind_funcid_render<O, H>(
        &mut self,
        func_id: FuncId,
        handlers: &mut H,
        captured: &O,
        field0_forced: bool,
    ) -> Result<(RootSlot, Value), JitError>
    where
        O: OutputSink,
        H: DispatchEffect<O>,
    {
        let PersistentSession {
            machine,
            session_table,
            ..
        } = self;
        let machine = machine
            .as_mut()
            .expect("machine bootstrapped before bind_funcid_render");
        machine.run_fragment_and_bind_render(
            func_id,
            session_table,
            handlers,
            captured,
            field0_forced,
        )
    }

    // -- value-plane bookkeeping (delegating over the two planes) ----------

    /// Build the [`ExternalEnv`] a later fragment consults at a `Var`-miss:
    /// the `SessionVarId → RootSlot` of every live binding `referenced`
    /// names (D9) — typically `tidepool_repr::free_vars(&fragment)`. Not
    /// every live binding: a binding absent from `referenced` still stays a
    /// GC root (registered at bind time, independent of this call) but is
    /// not seeded into this particular fragment's env.
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
