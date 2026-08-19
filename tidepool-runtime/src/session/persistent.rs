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
//! Suspension is **threadless** everywhere: an `Ask` stows the machine (or, on
//! the parked lane below, one continuation of many) as DATA — the eval thread
//! exits, and a fresh thread re-enters to resume. No OS thread is parked per
//! suspended session — neither in the harness (a TREE of many
//! simultaneously-suspended nodes cannot pin N+1 threads) nor in the repl (see
//! `plans/unpark/feasibility-map.md`).
//!
//! There are TWO suspend mechanisms, one per lane (one-session plan; do not
//! conflate them — a machine never mixes both, asserted both directions):
//!
//! - **The SLOT path — implemented in this file.** [`JitEffectMachine`] is
//!   `Send` precisely because it is stowed-XOR-running: the whole machine
//!   stows as one `suspended_continuation` slot, and `resume_suspended`
//!   re-enters it. This is the pair-per-policy family below. It remains the
//!   live mechanism for the repl and one-shot eval lanes; on the harness lane
//!   it is legacy, superseded by the parked path, and its deletion is gated
//!   on the parked path's production soak (`plans/one-session.md` Phase 6).
//! - **The PARKED path — `super::resident::ResidentSession`.** A machine's
//!   continuation REGISTRY holds many independently parked frames at once,
//!   each a registered GC root, resumable by identity in any order. This is
//!   the primary suspension mechanism for the harness (resident-session)
//!   lane as of the one-session collapse; see that module's docstring for
//!   the fragment × suspend shape.
//!
//! Every run entry here therefore reports either a completion or a suspension,
//! and every one has a `resume_*` sibling that re-enters the stowed continuation
//! with an answer or an abort. The four result-materialization policies the JIT
//! supports — plain `Value`, single `Bind`, projected multi-bind, and
//! bind+render — each appear as such a pair, each carrying what it actually
//! produces ([`SuspendableOutcome`] for the two `Value`-completing policies,
//! [`Suspendable`] over the roots for the other two). See the module docstrings
//! of [`super::resident`] (the parked path built on this same core, and the
//! harness's single-node consumer) and `tidepool-repl`'s `session.rs` (the
//! repl's block-cursor consumer, still on the slot path) for the orchestration
//! around this core.

use std::path::Path;

use tidepool_codegen::binding_table::{BindingEntry, BindingTable};
use tidepool_codegen::emit::ExternalEnv;
use tidepool_codegen::jit_machine::{
    FuncId, JitEffectMachine, ResumeInput, Suspendable, SuspendableOutcome,
};
use tidepool_codegen::old_space::RootSlot;
use tidepool_codegen::scope::{ScopeId, ScopeTree};
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
    /// THE scope tree (PRD 21 lane C2) — one per session, shared by BOTH
    /// planes. The value plane hangs [`BindingTable`] frames off these ids and
    /// the decl plane keys its per-scope tips off the SAME ids, which is why
    /// neither owns a tree of its own: two trees would be two answers to "is
    /// this scope live", and a scoped decl and a scoped binding would drift.
    /// [`ScopeId::ROOT`] is the flat session every pre-C2 caller lives in.
    scopes: ScopeTree,
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
            scopes: ScopeTree::new(),
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
        #[allow(clippy::expect_used, reason = "decl plane present")]
        self.lib.as_ref().expect("decl plane present")
    }
    /// The decl-plane library (mutate — e.g. `define_batch_with_vals`). Panics if
    /// the session has no decl plane (see [`Self::lib`]).
    pub fn lib_mut(&mut self) -> &mut SessionLib {
        #[allow(clippy::expect_used, reason = "decl plane present")]
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
        // MONOTONIC MAX, not assignment: with any-order resume (one-session
        // plan), two in-flight bind turns can materialize out of mint order —
        // gen 7 completing before gen 6. A plain assignment would REWIND the
        // counter on the late gen-6 materialization, and the next mint would
        // re-issue 7, colliding with the live Val.G7. Generations are only
        // ever bumped, never reused (`Generation::next`'s contract) — this
        // enforces it at the one write site.
        if g.0 > self.val_gen.0 {
            self.val_gen = g;
        }
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
    // caller's frame: it [`Self::lease_machine`]s the machine over with the
    // accumulated table and lets the [`MachineLease`] restore it on `Drop`.
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

    /// Move the resident machine out onto a [`MachineLease`] (to run a turn on
    /// a fresh big-stack eval thread — the machine is `Send`, the rest of the
    /// session is not). The lease mutably borrows this session for its whole
    /// lifetime and restores the SAME machine into it on `Drop` — there is no
    /// way to reach the emptied-slot state through a public method, and no way
    /// to hand the lease's machine to a different session's restore (the lease
    /// borrows the session it took from and nothing else). Panics if the
    /// machine is not bootstrapped or is already leased.
    pub fn lease_machine(&mut self) -> MachineLease<'_> {
        #[allow(
            clippy::expect_used,
            reason = "machine present (idle or suspended) before a turn"
        )]
        let machine = self
            .machine
            .take()
            .expect("machine present (idle or suspended) before a turn");
        MachineLease {
            session: self,
            machine: Some(machine),
        }
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
        #[allow(
            clippy::expect_used,
            reason = "machine bootstrapped before add_fragment_session"
        )]
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
        #[allow(
            clippy::expect_used,
            reason = "machine bootstrapped before add_child_fragment_session"
        )]
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
        #[allow(
            clippy::expect_used,
            reason = "machine bootstrapped before add_fragment_with_table"
        )]
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
        #[allow(clippy::expect_used, reason = "machine bootstrapped before run_entry")]
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
        #[allow(
            clippy::expect_used,
            reason = "machine bootstrapped before run_funcid_with_table"
        )]
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
        #[allow(
            clippy::expect_used,
            reason = "machine present before resume_with_table"
        )]
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
        #[allow(
            clippy::expect_used,
            reason = "machine bootstrapped before run_funcid_session"
        )]
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
        #[allow(clippy::expect_used, reason = "machine present before resume_session")]
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
        #[allow(
            clippy::expect_used,
            reason = "machine bootstrapped before run_funcid_pure"
        )]
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
        #[allow(
            clippy::expect_used,
            reason = "machine bootstrapped before bind_funcid"
        )]
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
        #[allow(clippy::expect_used, reason = "machine present before resume_bind")]
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
        #[allow(
            clippy::expect_used,
            reason = "machine bootstrapped before bind_funcid_projected"
        )]
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
        #[allow(
            clippy::expect_used,
            reason = "machine present before resume_bind_projected"
        )]
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
        #[allow(
            clippy::expect_used,
            reason = "machine bootstrapped before bind_funcid_render"
        )]
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
        #[allow(
            clippy::expect_used,
            reason = "machine present before resume_bind_render"
        )]
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

    /// Scoped [`Self::current_lib_module`]: the `Lib.G<g>` module at `scope`'s
    /// OWN tip — the module a turn compiled in that scope imports, and the head
    /// of a re-export chain that already runs up through its ancestors.
    /// `current_lib_module() == current_lib_module_in(ScopeId::ROOT)`.
    pub fn current_lib_module_in(&self, scope: ScopeId) -> Option<SessionModule> {
        self.lib.as_ref().and_then(|l| l.current_module_in(scope))
    }

    /// The decl-plane include directory (where `Lib.G<g>.hs` modules live), for
    /// a later turn's compile search path. `None` when the session has no decl
    /// plane.
    /// Move the decl plane OUT (machine rotation, one-session living
    /// structure): the plane is SOURCE-side state (gen modules on disk +
    /// the in-memory decl log), independent of any machine's heap, so it
    /// transfers wholesale into a freshly-built session while the old
    /// machine (and its value plane, whose roots die with its heap) drops.
    /// KNOWN EDGE: a gen module that imports `Val.G<g>` (a decl rendered
    /// while value binds were live) will fail its next recompile after the
    /// transfer with an ordinary module-not-found — legible, not silent.
    pub fn take_lib(&mut self) -> Option<SessionLib> {
        self.lib.take()
    }

    pub fn lib_include_dir(&self) -> Option<&Path> {
        self.lib.as_ref().map(|l| l.include_dir())
    }

    /// Define decl text(s) scoped against live session values: the current
    /// `Val.G<g>` per still-live name are imported unqualified, every live
    /// `Val.G<g>` is injected for validation. The decl-plane analogue of GHCi
    /// seeing earlier bindings from a new top-level definition.
    pub fn define_scoped(&mut self, decl_texts: &[&str]) -> Result<Generation, SessionError> {
        self.define_scoped_in(ScopeId::ROOT, decl_texts)
    }

    /// Scoped [`Self::define_scoped`]: append to `scope`'s own decl tip,
    /// validated against the value bindings VISIBLE at `scope` (its frame plus
    /// every ancestor's). `define_scoped(d) == define_scoped_in(ScopeId::ROOT,
    /// d)`.
    ///
    /// Injection stays the FULL live set — `--inject-val` only has to make the
    /// referenced `Val.G<g>` modules findable, and restricting it by scope
    /// would buy nothing while risking a missing module for a shadowed gen.
    /// Visibility is decided by the IMPORT list, which is scoped.
    pub fn define_scoped_in(
        &mut self,
        scope: ScopeId,
        decl_texts: &[&str],
    ) -> Result<Generation, SessionError> {
        if !self.scopes.is_live(scope) {
            return Err(SessionError::DeadScope(scope));
        }
        let import_modules = self.current_val_modules_in(scope);
        let inject_modules = self.live_val_modules();
        #[allow(clippy::expect_used, reason = "decl plane present")]
        self.lib
            .as_mut()
            .expect("decl plane present")
            .define_batch_with_vals_in(scope, decl_texts, &import_modules, &inject_modules)
    }

    /// Retract `name` from the decl plane (its binding migrated to the value
    /// plane). No-op when `name` is not a current decl head.
    pub fn retract(&mut self, name: &str) -> Result<(), SessionError> {
        self.retract_in(ScopeId::ROOT, name)
    }

    /// Scoped [`Self::retract`]: retract `name` from `scope`'s decl tip only.
    /// `retract(n) == retract_in(ScopeId::ROOT, n)`.
    ///
    /// This is the cross-plane rule (design doc §1.3) made scope-correct: a
    /// name lives in at most one plane *per scope*, so a CHILD binding
    /// `helper` on the value plane must not retract the PARENT's decl-plane
    /// `helper` — the parent's name is still the parent's, and nothing ever
    /// walks downward.
    pub fn retract_in(&mut self, scope: ScopeId, name: &str) -> Result<(), SessionError> {
        if !self.scopes.is_live(scope) {
            return Err(SessionError::DeadScope(scope));
        }
        match self.lib.as_mut() {
            Some(lib) => lib.retract_in(scope, name),
            None => Ok(()),
        }
    }

    // -- scopes (PRD 21 lane C2) -------------------------------------------

    /// Mint a fresh child scope of `parent`. `None` if `parent` is not live
    /// (never minted, or already retired) — a scope is never born under a dead
    /// ancestor.
    ///
    /// This is also where the DECL plane learns the new scope's inherited
    /// environment: `PersistentSession` owns the one [`ScopeTree`], so it is
    /// the only place that knows a scope's parent, and `SessionLib` keys its
    /// tips by [`ScopeId`] without owning the tree. Seeding the child from its
    /// parent's tip HERE is what makes "children read parent declarations"
    /// (locked decision 4) true at the moment of minting rather than at first
    /// use — the difference matters exactly when a sibling pushes a turn in
    /// between, which would otherwise leak into this scope.
    pub fn mint_scope(&mut self, parent: ScopeId) -> Option<ScopeId> {
        let child = self.scopes.mint_child(parent)?;
        if let Some(lib) = self.lib.as_mut() {
            let inherited = lib.scope_tip(parent);
            lib.seed_scope(child, inherited);
        }
        Some(child)
    }

    /// The session's one scope tree — read by both planes for their lookup
    /// walks. There is no `_mut` sibling on purpose: minting and retiring are
    /// the only writes, and both go through this type so the value plane's
    /// frames and roots are released in the same step as the tree edge.
    pub fn scope_tree(&self) -> &ScopeTree {
        &self.scopes
    }

    /// Record a materialized value binding in `scope`'s frame.
    /// `bind(e) == bind_in(ScopeId::ROOT, e)`.
    ///
    /// Rejects a dead `scope` (never minted, or already retired) BEFORE
    /// touching the binding table: a binding written under a dead scope
    /// would sit in a frame no lookup chain ever walks and
    /// [`Self::retire_scope`] can never drain — for a mounted persistent
    /// root, a permanent GC root by construction. Every caller must check
    /// liveness before consuming whatever custody transfer led here (a
    /// [`super::resident::RootCustody`], a released [`ValueHandle`](tidepool_codegen::jit_machine::ValueHandle))
    /// — this check is the backstop, not the first line, since `bind_in`
    /// failing here is too late to undo a handle already released from the
    /// machine's registry.
    pub fn bind_in(&mut self, scope: ScopeId, entry: BindingEntry) -> Result<(), SessionError> {
        if !self.scopes.is_live(scope) {
            return Err(SessionError::DeadScope(scope));
        }
        self.bindings.bind_in(scope, entry);
        Ok(())
    }

    /// Resolve `name` as seen FROM `scope`: local frame first, then each
    /// ancestor up to ROOT.
    pub fn resolve_in(&self, scope: ScopeId, name: &str) -> Option<&BindingEntry> {
        self.bindings.resolve_in(&self.scopes, scope, name)
    }

    /// Scoped [`Self::current_val_modules`]: the `Val.G<g>` module per name
    /// VISIBLE at `scope` (child frames shadowing parent ones) — what a turn
    /// compiled in that scope imports unqualified.
    pub fn current_val_modules_in(&self, scope: ScopeId) -> Vec<String> {
        let mut v: Vec<String> = self
            .bindings
            .iter_current_in(&self.scopes, scope)
            .into_iter()
            .map(|(_, entry)| entry.module.module_name())
            .collect();
        v.sort();
        v.dedup();
        v
    }

    /// How many names `scope`'s own frame currently binds (accounting class 3,
    /// per scope). `scope_binding_count(ScopeId::ROOT)` is the flat session's
    /// `current_val_modules`/`binding_names` population.
    pub fn scope_binding_count(&self, scope: ScopeId) -> usize {
        self.bindings.scope_binding_count(scope)
    }

    /// Number of persistent GC roots registered on the resident machine
    /// (accounting class 4 — the GC ROOT LEDGER, the witness that a retirement
    /// actually released what it claims). 0 before the machine bootstraps.
    pub fn persistent_roots_count(&self) -> usize {
        self.machine
            .as_ref()
            .map_or(0, |m| m.persistent_roots_count())
    }

    /// Retire `scope` and its whole subtree: drop each scope's value-plane
    /// frame and RELEASE the GC roots those bindings solely owned.
    ///
    /// Walks [`ScopeTree::retire`]'s deepest-first order so a child's frames
    /// are gone before its parent's, drains each frame
    /// ([`BindingTable::drain_scope`]), and for every drained entry applies the
    /// **sole-ownership rule**: its root is deregistered
    /// ([`JitEffectMachine::retire_scope_root`]) only when no OTHER live
    /// `BindingEntry` — in any scope, including the not-yet-drained ancestors
    /// of this same retirement — holds the same slot address, and no live
    /// `ValueHandle` still does.
    ///
    /// That rule is what makes the escaped-closure case safe: a value produced
    /// in a child and mounted into a PARENT-scope binding is still owned by
    /// that surviving entry when the child retires, so its root stays
    /// registered and its captured heap subgraph stays traced transitively.
    ///
    /// Retiring ROOT, or a scope that is already retired, is a no-op returning
    /// an all-zero receipt.
    ///
    /// # What this does and does not reclaim
    /// Deregistration removes a root from the GC TRACE LIST. It does not free
    /// `OldSpace` bytes — no major/compacting pass exists — so a long-resident
    /// session's old space still grows monotonically with total mounts ever
    /// made, reclaimed only at machine drop. See
    /// `tidepool-codegen/CLAUDE.md`'s root-accounting section.
    pub fn retire_scope(&mut self, scope: ScopeId) -> ScopeRetirement {
        let roots_before = self.persistent_roots_count();
        let doomed = self.scopes.retire(scope);
        let mut receipt = ScopeRetirement {
            scopes_retired: doomed.len(),
            bindings_retired: 0,
            roots_released: 0,
        };
        // EXACTLY ONCE PER ROOT is `retire_scope_root`'s stated invariant, and
        // two names in the SAME frame can share one slot (two mounts from one
        // handle). The alias check below cannot see that — both entries are
        // already drained — so already-released addresses are tracked here.
        // Without this the second release is a ledger no-op while the receipt
        // counts two, which is precisely the false receipt this lane rejects.
        let mut released: Vec<*mut *mut u8> = Vec::new();
        for dead in &doomed {
            for entry in self.bindings.drain_scope(*dead) {
                receipt.bindings_retired += 1;
                let slot = entry.value.root();
                // SOLE OWNERSHIP, checked against what is STILL live: the
                // drained entry is already out of `live`, and deeper scopes
                // drained before this one, so an alias found here belongs to a
                // survivor (a parent-scope mount, a sibling, or an ancestor
                // retiring later in this same walk — which then releases it).
                let aliased_by_binding = self
                    .bindings
                    .iter_live()
                    .any(|e| std::ptr::eq(e.value.root().addr(), slot.addr()));
                let Some(machine) = self.machine.as_mut() else {
                    // No machine, hence no registered roots: the frames are
                    // still dropped, and the receipt honestly reports zero
                    // releases rather than claiming one it did not make.
                    continue;
                };
                // A LIVE HANDLE over a retiring value-plane root is a genuine
                // bug, not a legitimate co-owner: mounting releases the handle
                // (ownership transfers to the value plane) BEFORE the binding
                // exists, so a handle still holding this slot means that
                // transfer never happened. Asserted, then also honored — the
                // root stays registered rather than being pulled out from
                // under the registry in release builds.
                let held_by_handle = machine.handle_holds_root(slot);
                debug_assert!(
                    !held_by_handle,
                    "retire_scope: slot {:p} is still held by a live ValueHandle — \
                     ownership never transferred to the value plane",
                    slot.addr()
                );
                // Aliasing by a surviving BINDING is the opposite: entirely
                // legitimate (the escaped-closure case), and precisely what
                // the sole-ownership rule exists to skip.
                let already_released = released.iter().any(|&a| std::ptr::eq(a, slot.addr()));
                if !aliased_by_binding && !held_by_handle && !already_released {
                    machine.retire_scope_root(slot);
                    released.push(slot.addr());
                    receipt.roots_released += 1;
                }
            }
        }
        debug_assert_eq!(
            roots_before - self.persistent_roots_count(),
            receipt.roots_released,
            "retire_scope receipt must be witnessed by the GC root ledger",
        );
        receipt
    }
}

// ---------------------------------------------------------------------------
// MachineLease — the only way to move a session's machine onto another thread
// ---------------------------------------------------------------------------

/// An exclusive, scoped loan of a [`PersistentSession`]'s machine, minted by
/// [`PersistentSession::lease_machine`]. The lease mutably borrows the session
/// it came from for its entire lifetime, so the session's machine slot cannot
/// be observed or touched by anything else while the lease is outstanding, and
/// on `Drop` it restores EXACTLY the machine it took — never an arbitrary one,
/// and never into a different session. There is no public constructor and no
/// public field: the empty-slot state this replaces (the audited
/// `take_machine`/`restore_machine` pair) is not reachable through any safe
/// call, by construction rather than by convention.
pub struct MachineLease<'a> {
    session: &'a mut PersistentSession,
    machine: Option<JitEffectMachine>,
}

// The exclusive-borrow guarantee this type exists for ("the session's machine
// slot cannot be observed or touched by anything else while the lease is
// outstanding") is a `&mut` the borrow checker already enforces — a
// `#[derive(Clone)]` could never actually compile against the `&'a mut
// PersistentSession` field as written, but a future refactor that swapped
// that field for something Clone-able (e.g. an `Rc`/raw pointer) would make
// the derive compile silently, losing the guarantee this pin exists to catch.
static_assertions::assert_not_impl_any!(MachineLease<'static>: Clone, Copy);

impl MachineLease<'_> {
    /// The leased machine and the session's accumulated constructor table, on
    /// loan together for a turn run on another thread. Panics if called after
    /// the lease's machine has somehow already been consumed — unreachable
    /// through this type's own API, kept as a `debug_assert`-strength backstop
    /// rather than an `unwrap` a reviewer has to re-verify by hand.
    pub fn parts(&mut self) -> (&mut JitEffectMachine, &DataConTable) {
        #[allow(
            clippy::expect_used,
            reason = "lease holds its machine for its whole lifetime"
        )]
        let machine = self
            .machine
            .as_mut()
            .expect("lease holds its machine for its whole lifetime");
        let table = self.session.session_table();
        (machine, table)
    }
}

impl Drop for MachineLease<'_> {
    fn drop(&mut self) {
        if let Some(machine) = self.machine.take() {
            self.session.machine = Some(machine);
        }
    }
}

/// What a [`PersistentSession::retire_scope`] actually released — counts, not
/// booleans, so a caller can assert the accounting rather than trust it.
///
/// `roots_released` is the number of persistent GC roots deregistered, and it
/// is exactly the drop a caller must observe in
/// [`PersistentSession::persistent_roots_count`] across the call. It is `<=`
/// `bindings_retired`: a binding whose slot is still owned by a survivor (the
/// sole-ownership rule) retires its NAME without releasing its ROOT.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ScopeRetirement {
    /// Scopes removed from the tree — the retired scope plus every live
    /// descendant.
    pub scopes_retired: usize,
    /// `live` entries evicted across all of those scopes' frames, shadowed
    /// older gens included.
    pub bindings_retired: usize,
    /// Persistent GC roots deregistered — the sole-owner subset of the above.
    pub roots_released: usize,
}
