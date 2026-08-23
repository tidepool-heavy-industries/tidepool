//! The high-level JIT effect machine ([`JitEffectMachine`]) and the effect-drive
//! loop at the JIT↔Rust boundary.
//!
//! # Nested child runs on a suspended machine
//!
//! A parent turn suspended at a typed yield (`runLLMTurn`/`Ask`) can host an
//! arbitrary number of SEQUENTIAL child fragment runs — including ones that
//! force GC and heap doubling — and resume correctly afterward.
//!
//! INVARIANT: while a child runs, the parent's stowed continuation must be a
//! registered GC root, because the child allocates and collects.
//! [`JitEffectMachine::enter_nested_child`] moves the continuation pointer out
//! of `suspended_continuation` into a heap-stable `Box` cell and registers
//! that cell's address in `stowed_roots`, which `perform_gc` folds into its
//! root assembly; a child collection evacuates the parent's continuation tree
//! and rewrites the cell in place, and teardown reads the GC-current pointer
//! back out. The L7 `suspended_continuation.is_none()` asserts on every plain
//! run entry (`run`/`run_pure`/`run_fragment`/`*_and_bind`) stay UNCHANGED and
//! still fire if a plain entry runs while a continuation is stowed and
//! UNREGISTERED — [`JitEffectMachine::run_child_fragment`] and its pure
//! sibling are the only sanctioned way to run while suspended, since they
//! empty `suspended_continuation` for the child's duration so the L7 asserts
//! pass naturally.
//!
//! ORDERING INVARIANT: a child turn's [`RegistryGuard::drop`] reclaims the
//! session heap buffer + high-water cursor into `self.session` (the buffer
//! may have been swapped/doubled by a child GC). `NestedChildGuard` must drop
//! AFTER the child's `RegistryGuard`, so it reads the continuation pointer
//! back out of the stowed cell AFTER the reclaim — observing the POST-child
//! heap. The pointer stays valid across the reclaim because moving a
//! `Vec<u64>` moves its 24-byte header, not its heap data.
//!
//! `nested_child_depth` counts children currently running against the parent;
//! a parent resume is rejected while it is > 0, because the heap holds exactly
//! one computation at a time (sequential-isolated).
//!
//! # The parked-continuation registry (realm prototype)
//!
//! [`JitEffectMachine::run_suspendable_parked`] and [`JitEffectMachine::resume_parked`]
//! generalize the single `suspended_continuation` slot to a map of many. A
//! parked continuation is a registered `stowed_roots` entry from the moment it
//! parks until it resumes, so a collection triggered by ANY later computation
//! on the machine evacuates it and rewrites its cell.
//!
//! The parked path deliberately leaves `suspended_continuation` as `None`, so
//! the L7 asserts on every plain run entry keep passing. The two paths never
//! interact: a machine using the parked registry never stows into the slot,
//! and a machine using the slot never populates the registry.
//!
//! # Completion policies on the suspend/park family
//!
//! A suspendable turn completes under one of the SAME four
//! [`ResultMaterialization`] policies the plain routes use, through one shared
//! implementation ([`JitEffectMachine::materialize`]):
//!
//! | policy | run entry | resume entry | completion type | products |
//! |---|---|---|---|---|
//! | `Value` | [`JitEffectMachine::run_fragment_suspendable`] | [`JitEffectMachine::resume_suspended`] | [`SuspendableOutcome`] | the bridged value |
//! | `Bind { forced }` | [`JitEffectMachine::run_fragment_suspendable_binding`] | [`JitEffectMachine::resume_suspended_binding`] | [`SuspendableOutcome`] | bridged value, plus one tenured root stashed for [`JitEffectMachine::take_last_bound_root`] |
//! | `Project { n_fields }` | [`JitEffectMachine::run_fragment_suspendable_projected`] | [`JitEffectMachine::resume_suspended_projected`] | `Suspendable<Vec<RootSlot>>` | the N tenured roots, IN the completion |
//! | `Render { field0_forced }` | [`JitEffectMachine::run_fragment_suspendable_render`] | [`JitEffectMachine::resume_suspended_render`] | `Suspendable<(RootSlot, Value)>` | field 0's tenured root + field 1's render, IN the completion |
//!
//! The PARKED (registry) path covers all four — [`ParkKind`] mirrors
//! `Value`/`Bind`/`Project`/`Render` (one-session plan, Phase 0: `Project`/
//! `Render` joined `Plain`/`Binding` so the registry path can serve every
//! session lane).
//!
//! CONTRACT (see `plans/unpark/`, §6.2, for why this split is load-bearing):
//! `Project`/`Render` return their tenured [`crate::old_space::RootSlot`]s
//! inline in the completion; `Bind` cannot, because `RootSlot` is a bare
//! `*mut *mut u8` newtype that is deliberately NOT `unsafe impl Send` (unlike
//! its containers, which are blessed under the stowed-XOR-running argument).
//! `Bind` is the one policy `tidepool_runtime::session::ResidentSession`
//! drives across `on_eval_thread`'s scoped-thread join, which requires the
//! completion to be `Send` — a slot riding out inline fails `E0277` there. So
//! `last_bound_root` (and `suspended_finalized_root`, same reason) LAUNDERS a
//! `!Send` slot across that boundary by riding inside the machine, which is
//! already blessed `Send`. Removing the stash would require `unsafe impl Send
//! for RootSlot` — a new standalone soundness claim, left open deliberately.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub use cranelift_module::FuncId;
use tidepool_effect::{DispatchEffect, EffectContext, EffectError};
use tidepool_eval::value::Value;
use tidepool_repr::{CoreExpr, DataConTable};

use crate::context::VMContext;
use crate::effect_machine::{CompiledEffectMachine, ConTags};
use crate::heap_bridge;
use crate::machine_state::{machine_state, MachineState};
use crate::nursery::Nursery;
use crate::pipeline::CodegenPipeline;
use crate::yield_type::Yield;

/// Why an incoming handled prefix was refused against the machine's
/// established one ([`JitEffectMachine::check_prefix_compatible`]). Two
/// non-empty prefixes must be EXACTLY EQUAL, so a disagreement is either a
/// length difference (not reducible to any shared position — naming one
/// would be misleading) or, at equal length, a content disagreement at a
/// specific position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrefixMismatch {
    /// Both prefixes are non-empty but of different lengths.
    Length,
    /// Same length, but the prefixes disagree at this 0-based position.
    Position(usize),
}

impl std::fmt::Display for PrefixMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PrefixMismatch::Length => write!(f, "differing lengths"),
            PrefixMismatch::Position(position) => write!(f, "position {position}"),
        }
    }
}

/// Error type for JIT compilation/execution failures.
#[derive(Debug, thiserror::Error)]
pub enum JitError {
    #[error("JIT compilation error: {0}")]
    Compilation(#[from] crate::emit::EmitError),
    #[error("pipeline error: {0}")]
    Pipeline(#[from] crate::pipeline::PipelineError),
    #[error("missing freer-simple constructor '{0}' in DataConTable")]
    MissingConTags(&'static str),
    #[error("effect dispatch error: {0}")]
    Effect(#[from] EffectError),
    #[error("yield error: {0}")]
    Yield(#[from] crate::yield_type::YieldError),
    #[error("heap bridge error: {0}")]
    HeapBridge(#[from] crate::heap_bridge::BridgeError),
    // Transparent: this variant wraps signals from EVERY protected JIT call
    // site (step/resume/apply/bridge), so a site-specific prefix here would
    // lie about the phase.
    #[error(transparent)]
    Signal(#[from] crate::signal_safety::SignalError),
    #[error("Effect handler response too large ({nodes} value nodes, max {limit}). Narrow your query to return fewer results.")]
    EffectResponseTooLarge { nodes: usize, limit: usize },
    #[error(
        "VarId collision at load: {0}. This indicates a Haskell-side VarId-scheme regression."
    )]
    VarIdCollision(#[from] tidepool_repr::VarIdCollision),
    /// Refused at ENTRY to the parked path, before the machine is driven at
    /// all (never a machine invariant violation — a caller/configuration
    /// error, so `Err`, not a panic): the realm's handled-effect prefix
    /// disagrees with the prefix the machine already established from an
    /// earlier entry. Two non-empty prefixes must be EXACTLY EQUAL — an
    /// empty prefix never produces this error, it is compatible with
    /// anything. See [`JitEffectMachine::check_prefix_compatible`] for why
    /// exact equality is the sound check and what it does, and does not,
    /// verify.
    #[error(
        "realm handled-effect prefix disagrees with the machine's established prefix \
         ({mismatch}): established {established:?}, incoming {incoming:?}"
    )]
    IncompatibleHandledPrefix {
        established: Vec<String>,
        incoming: Vec<String>,
        mismatch: PrefixMismatch,
    },
}

/// A pending first-cause `RuntimeError` surfaces as a yield error — the shape
/// `host_fns::surface_error` resolves to at `Result<_, JitError>` boundaries.
impl From<crate::host_fns::RuntimeError> for JitError {
    fn from(err: crate::host_fns::RuntimeError) -> Self {
        JitError::Yield(err.into())
    }
}

/// A read-only snapshot of one machine's heap/GC counters
/// ([`JitEffectMachine::heap_stats`]) — plain numbers, no GC/rooting
/// internals exposed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeapStats {
    /// Total capacity of the machine's nursery, in bytes.
    pub nursery_bytes: usize,
    /// The session heap's bump high-water mark, in bytes (0 for a one-shot
    /// machine, or a session machine that hasn't run a turn yet).
    pub live_bytes: usize,
    /// Number of collections this machine has run ([`MachineState::gc_generation`]).
    pub gc_count: u64,
    /// Number of fragments ever compiled into this machine's JITModule
    /// ([`JitEffectMachine::add_function`]) — MONOTONIC, never reclaimed
    /// (cranelift leaks finalized code by design), so this is the
    /// bounded-lifetime ceiling's primary signal (one-session plan, Phase 4:
    /// rotation, not immortality).
    pub fragments: u64,
}

/// Identity of one continuation parked in a machine's continuation registry.
/// Minted by [`JitEffectMachine::run_suspendable_parked`], consumed by
/// [`JitEffectMachine::resume_parked`]. Ids are never reused within a machine:
/// a resume that suspends AGAIN mints a fresh id (same realm).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContinuationId(pub u64);

/// Identity of a realm — the ownership scope a parked continuation belongs to
/// (an outer loop turn, one answerer subtree, …). Carried on the frame so a
/// caller can group, cancel, or drain a realm's parks without tracking ids
/// externally. The machine itself attaches no semantics to it beyond
/// ownership: [`JitEffectMachine::close_realm`] is scope exit — every frame
/// and [`ValueHandle`] the realm owns is released together, structured-
/// concurrency style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RealmId(pub u64);

/// Opaque, `Send`-able identity of a machine-side rooted heap value (one-
/// session plan, pillar B — the embedder handle). Minted by
/// [`JitEffectMachine::handle_from_finalized`] (more sources in later
/// phases), observed via [`JitEffectMachine::observe_handle`], delivered via
/// [`ResumeInput::Handle`], released by [`JitEffectMachine::close_realm`] of
/// the owning realm. The `!Send` [`crate::old_space::RootSlot`] underneath
/// never crosses an API layer.
///
/// Stays `Copy`/freely re-usable at THIS layer on purpose: this machine-level
/// primitive is also exercised directly by tests that read a handle
/// non-linearly (`observe_handle`, `handle_realm`, repeated
/// `ResumeInput::Handle` — all borrows, never a consuming transfer). The
/// session layer (`tidepool_runtime::session::resident::RootCustody`) wraps
/// this type in a linear, non-`Clone` custody token at the ONE seam where a
/// caller-visible obligation to consume-exactly-once actually exists
/// (`ResidentSession::finalized_handle` → `resume_handle`/`mount_handle`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ValueHandle(pub u64);

/// One live [`ValueHandle`]'s machine-side entry: the persistent-rooted slot
/// and the realm that owns (and will release) it.
struct HandleEntry {
    slot: crate::old_space::RootSlot,
    realm: RealmId,
}

/// What kind of turn parked a continuation — the registry's spelling of the
/// completion policy, covering ALL FOUR of the [`ResultMaterialization`]
/// policies (one-session plan, Phase 0: `Project`/`Render` joined
/// `Plain`/`Binding` so the registry path can serve every session lane and
/// the slot path can eventually retire).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParkKind {
    /// A plain suspendable turn: on completion the `Done` pointer is bridged
    /// and returned (`ResultMaterialization::Value`).
    Plain,
    /// A value-plane BIND turn: on completion the result is tenured into
    /// old-space and its [`crate::old_space::RootSlot`] returned inline
    /// (`ResultMaterialization::Bind`). `forced` deep-forces to NF before
    /// tenuring (Tier0 data) vs tenuring a Tier1 closure as-is.
    Binding { forced: bool },
    /// A multi-binder BIND turn: on completion the `Done` tuple is
    /// deep-forced and each of `n_fields` fields is tenured in order, the
    /// roots returned inline (`ResultMaterialization::Project`). `n_fields`
    /// is a [`NonZeroUsize`] — a zero-field projection is meaningless (there
    /// is no product to bind) and is rejected once, at the public-method
    /// boundary, rather than asserted on every route that carries it.
    Project { n_fields: NonZeroUsize },
    /// The single-compile `it`-binding epilogue: field 1 is bridged FIRST
    /// (aliasing-safe ordering, see [`ResultMaterialization::Render`]), then
    /// field 0 optionally forced + tenured; both products returned inline.
    Render { field0_forced: bool },
}

impl ParkKind {
    /// Project to the [`ResultMaterialization`] the shared suspendable
    /// epilogue takes — a one-to-one spelling since Phase 0 of the
    /// one-session plan.
    fn materialization(self) -> ResultMaterialization {
        match self {
            ParkKind::Plain => ResultMaterialization::Value,
            ParkKind::Binding { forced } => ResultMaterialization::Bind { forced },
            ParkKind::Project { n_fields } => ResultMaterialization::Project { n_fields },
            ParkKind::Render { field0_forced } => ResultMaterialization::Render { field0_forced },
        }
    }
}

/// One parked continuation in a machine's continuation registry.
///
/// `cell` is a heap-stable `Box` holding the continuation pointer, exactly the
/// `stowed_root_cell` pattern: the machine itself moves between threads (stow
/// XOR run), but the `Box`'s POINTEE address is a stable heap allocation, so
/// the `stowed_roots` registration (the cell's address) stays valid across the
/// move. The GC reads and rewrites `*cell` in place on every collection, so the
/// pointer read back out at resume is the GC-current one.
pub struct ContinuationFrame {
    /// Heap-stable cell holding the (GC-current) continuation pointer.
    cell: Box<*mut u8>,
    /// The realm this park belongs to.
    realm: RealmId,
    /// The union tag the turn suspended at, replayed on resume so the caller
    /// does not have to remember it per-park.
    suspend_tag: u64,
    /// Plain park vs value-plane binding park.
    kind: ParkKind,
    /// The persistent root of a closure-valued `finalize`'s finalized value,
    /// tenured at park time. `Some` only for a frame parked while
    /// suspended on a closure-valued finalize; taken via
    /// [`JitEffectMachine::take_parked_finalized_root`], which leaves the
    /// frame itself parked and rooted.
    finalized_root: Option<crate::old_space::RootSlot>,
    /// This frame's realm's cancel flag, cloned at park time so
    /// [`JitEffectMachine::resume_parked`] installs it without a second
    /// per-realm cancel-flag lookup.
    cancel_flag: Arc<AtomicBool>,
    /// The [`DataConTable`] this frame's continuation suspended against,
    /// cloned once at park time (not per collection, not per resume) so a
    /// resume decodes exclusively against the row it was compiled for — a
    /// caller cannot resume a frame against a foreign table.
    table: Arc<DataConTable>,
    /// This realm's handled prefix — the effect names for tags
    /// `[0, suspend_tag)`, in position order — checked (and possibly
    /// establishing) at ENTRY to the parked path
    /// ([`JitEffectMachine::enter_parked_path`]), stored here purely so
    /// [`JitEffectMachine::resume_parked`] can replay it through that same
    /// entry check on a re-suspension.
    handled_prefix: Arc<[String]>,
}

/// Outcome of a run/resume on the parked path — [`SuspendableOutcome`] plus the
/// [`ContinuationId`] a suspension parked under.
#[derive(Debug)]
pub enum ParkedOutcome {
    /// A [`ParkKind::Plain`] turn ran to completion: just the bridged result.
    CompletedValue(tidepool_eval::value::Value),
    /// A [`ParkKind::Binding`] turn ran to completion: the bridged result
    /// plus the tenured root of its value-plane BIND, returned INLINE in the
    /// same call that observed completion. A completed park leaves no frame
    /// in the registry (a frame exists only while parked), so there is
    /// nowhere for a machine-level stash to live between write and read — no
    /// window for a second realm's completion to overwrite it before the
    /// caller reads it.
    CompletedBinding {
        /// The bridged result.
        value: tidepool_eval::value::Value,
        /// The tenured root of the value-plane BIND's result.
        root: crate::old_space::RootSlot,
    },
    /// A [`ParkKind::Project`] park ran to completion: the tenured field
    /// roots, in field order, returned INLINE (same no-machine-stash argument
    /// as [`Self::CompletedBinding`]'s `root`).
    CompletedProject {
        /// The tenured roots of each projected field, in field order.
        roots: Vec<crate::old_space::RootSlot>,
    },
    /// A [`ParkKind::Render`] park ran to completion: field 0's tenured root
    /// plus field 1's already-bridged render, returned INLINE.
    CompletedRender {
        /// Field 0's tenured root.
        root: crate::old_space::RootSlot,
        /// Field 1's bridged render.
        rendered: tidepool_eval::value::Value,
    },
    /// The turn suspended and its continuation was PARKED in the registry as a
    /// registered GC root. Resume it with [`JitEffectMachine::resume_parked`].
    Suspended {
        /// The registry key this continuation parked under.
        id: ContinuationId,
        /// The bridged suspend request.
        request: tidepool_eval::value::Value,
        /// See [`SuspendableOutcome::Suspended::has_finalized_closure`].
        has_finalized_closure: bool,
    },
}

/// Where the shared suspendable epilogue puts a continuation when a turn
/// suspends. Internal: the public entries pick one and project the result.
#[derive(Debug, Clone)]
enum ParkTarget {
    /// The single `suspended_continuation` slot (every pre-existing entry).
    Slot,
    /// The continuation registry, under a fresh id in this realm.
    Registry {
        realm: RealmId,
        kind: ParkKind,
        /// This realm's handled prefix, already checked (and possibly
        /// established) at entry to the parked path — carried here only to
        /// be stored on the frame if this run suspends.
        handled_prefix: Arc<[String]>,
    },
}

/// Result of the shared suspendable body before it is projected into whichever
/// public outcome type the caller's entry returns. `id` is `Some` exactly when
/// the park target was [`ParkTarget::Registry`].
///
/// `Completed` carries a [`ParkedOutcome`] directly — always one of its four
/// `Completed*` variants, never `Suspended` — so the registry-path projection
/// ([`Self::into_parked`]) is a plain pass-through and each slot-path
/// projection matches the one `Completed*` variant its policy produces.
enum ParkedRaw {
    Completed(ParkedOutcome),
    Suspended {
        request: tidepool_eval::value::Value,
        has_finalized_closure: bool,
        id: Option<ContinuationId>,
    },
}

impl ParkedRaw {
    /// Split a suspension's payload out, asserting the slot-path invariant that
    /// no continuation id was minted. Shared by every slot-path projection.
    fn expect_slot_suspension(
        request: tidepool_eval::value::Value,
        has_finalized_closure: bool,
        id: Option<ContinuationId>,
    ) -> (tidepool_eval::value::Value, bool) {
        debug_assert!(id.is_none(), "slot park target must not mint an id");
        (request, has_finalized_closure)
    }

    /// Project onto the single-slot path's `Value`-completing outcome type
    /// (the `Value` and `Bind` policies).
    fn into_suspendable(self) -> SuspendableOutcome {
        match self {
            ParkedRaw::Completed(ParkedOutcome::CompletedValue(value))
            | ParkedRaw::Completed(ParkedOutcome::CompletedBinding { value, .. }) => {
                SuspendableOutcome::Completed(value)
            }
            ParkedRaw::Completed(_) => unreachable!(
                "into_suspendable is only reached by the Value/Bind entries, \
                 whose policies produce ParkedOutcome::CompletedValue/CompletedBinding"
            ),
            ParkedRaw::Suspended {
                request,
                has_finalized_closure,
                id,
            } => {
                let (request, has_finalized_closure) =
                    Self::expect_slot_suspension(request, has_finalized_closure, id);
                SuspendableOutcome::Suspended {
                    request,
                    has_finalized_closure,
                }
            }
        }
    }

    /// Project onto the multi-binder entries' outcome: the tenured field roots.
    fn into_projected(self) -> Suspendable<Vec<crate::old_space::RootSlot>> {
        match self {
            ParkedRaw::Completed(ParkedOutcome::CompletedProject { roots }) => {
                Suspendable::Completed(roots)
            }
            ParkedRaw::Completed(_) => unreachable!(
                "into_projected is only reached by the Project entries, \
                 whose policy produces ParkedOutcome::CompletedProject"
            ),
            ParkedRaw::Suspended {
                request,
                has_finalized_closure,
                id,
            } => {
                let (request, has_finalized_closure) =
                    Self::expect_slot_suspension(request, has_finalized_closure, id);
                Suspendable::Suspended {
                    request,
                    has_finalized_closure,
                }
            }
        }
    }

    /// Project onto the bind-and-render entries' outcome: field 0's tenured
    /// root paired with field 1's render.
    fn into_render(self) -> Suspendable<(crate::old_space::RootSlot, tidepool_eval::value::Value)> {
        match self {
            ParkedRaw::Completed(ParkedOutcome::CompletedRender { root, rendered }) => {
                Suspendable::Completed((root, rendered))
            }
            ParkedRaw::Completed(_) => unreachable!(
                "into_render is only reached by the Render entries, \
                 whose policy produces ParkedOutcome::CompletedRender"
            ),
            ParkedRaw::Suspended {
                request,
                has_finalized_closure,
                id,
            } => {
                let (request, has_finalized_closure) =
                    Self::expect_slot_suspension(request, has_finalized_closure, id);
                Suspendable::Suspended {
                    request,
                    has_finalized_closure,
                }
            }
        }
    }

    /// Project onto the registry path's outcome type. `Completed` already IS
    /// a [`ParkedOutcome`], so this only has to mint the `Suspended` variant.
    fn into_parked(self) -> ParkedOutcome {
        match self {
            ParkedRaw::Completed(outcome) => outcome,
            #[allow(
                clippy::expect_used,
                reason = "registry park target mints an id on suspension"
            )]
            ParkedRaw::Suspended {
                request,
                has_finalized_closure,
                id,
            } => ParkedOutcome::Suspended {
                id: id.expect("registry park target mints an id on suspension"),
                request,
                has_finalized_closure,
            },
        }
    }
}

/// Every public run variant's drive/session/materialization shape, all folded
/// as thin wrappers over [`JitEffectMachine::with_active_run`]:
///
/// | route | drive | session | materialization |
/// |---|---|---|---|
/// | [`JitEffectMachine::run`] / [`JitEffectMachine::run_fragment`] | effectful (`drive_to_done`) | optional | `Value` |
/// | [`JitEffectMachine::run_pure`] / [`JitEffectMachine::run_fragment_pure`] | pure (raw call, no `Yield` decoding) | optional | `Value` |
/// | [`JitEffectMachine::run_pure_and_bind`] | pure | REQUIRED | `Bind { forced: true }` (unconditional) |
/// | [`JitEffectMachine::run_fragment_and_bind`] | effectful | REQUIRED | `Bind { forced: <caller> }` |
/// | [`JitEffectMachine::run_fragment_and_bind_projected`] | effectful | REQUIRED | `Project { n_fields }` |
/// | [`JitEffectMachine::run_fragment_and_bind_render`] | effectful | REQUIRED | `Render { field0_forced }` |
/// | [`JitEffectMachine::run_child_fragment`] / `_pure` | → `run_with_entry` / `run_pure_with_entry`, wrapped in [`JitEffectMachine::enter_nested_child`] | requires a suspended parent | `Value` |
///
/// The suspend/park family (`run_suspendable*`, `resume_suspended*`,
/// `run_suspendable_parked`, `run_fragment_suspendable_parked`,
/// `resume_parked`) is NOT in this table: it funnels through its own shared
/// setup body ([`JitEffectMachine::run_suspendable_shared`] /
/// [`JitEffectMachine::resume_applied`]) and one shared epilogue
/// ([`JitEffectMachine::finish_suspendable`]), which also calls
/// [`JitEffectMachine::materialize`] — see the completion-policy table in the
/// module docs.
///
/// `with_active_run` owns registry install/reclaim and
/// `VMContext`/`CompiledEffectMachine` lifecycle once, drives through
/// [`JitEffectMachine::drive_active`], and hands the `Done` pointer to
/// [`JitEffectMachine::materialize`] — an explicit [`ResultMaterialization`]
/// enum so the four semantic policies (`Value`/`Bind`/`Project`/`Render`)
/// stay visible as named variants rather than folding into one opaque
/// callback.
///
/// ORDERING INVARIANT: reclaim is armed exactly ONCE, in `with_active_run`,
/// always AFTER `materialize` returns (Ok or Err) — arming it before a
/// fallible materialize step can reclaim a heap state materialize is still
/// reading.
enum ResultMaterialization {
    /// Bridge `Done` to an owned [`Value`] — the plain (non-bind) routes.
    Value,
    /// Value-plane BIND: optionally deep-force to NF (`forced`), tenure into
    /// old-space, return the persistent [`crate::old_space::RootSlot`].
    Bind { forced: bool },
    /// Multi-binder BIND: deep-force the WHOLE `Done` tuple, then project and
    /// tenure each of `n_fields` fields in order. `n_fields` is a
    /// [`NonZeroUsize`], parsed once at the public-method boundary (a
    /// zero-field projection has no product to bind and is rejected there,
    /// not re-asserted on every route that carries it).
    Project { n_fields: NonZeroUsize },
    /// The single-compile `it`-binding epilogue: bridge field 1 (the render)
    /// into an owned [`Value`] FIRST — field0/field1 may alias, and the
    /// bridge is a full owned copy immune to whatever tenuring field0 does
    /// afterward — then optionally force + tenure field 0 alone. Field 1 is
    /// never tenured.
    Render { field0_forced: bool },
}

/// The materialized result of [`JitEffectMachine::materialize`], one variant
/// per [`ResultMaterialization`] policy — a plain closed sum, not a
/// generic/sealed-trait correlation to [`ResultMaterialization`] (a settled
/// design choice: no per-policy associated type). Every thin route wrapper
/// requests exactly one policy and extracts exactly the matching variant via
/// one of the `expect_*` accessors below — those, not a `match … { _ =>
/// unreachable!() }` repeated at each call site, are the single place a
/// request/result mismatch would panic. A new policy variant must update
/// those accessors (an exhaustive match with no wildcard arm), which is the
/// point: the compiler forces every accessor to be revisited, not just the
/// ones a change happens to touch.
enum MaterializeResult {
    Value(Value),
    Bind(crate::old_space::RootSlot),
    /// The tenured field roots, in field order. Both routes want exactly these
    /// — a projection has no result value of its own.
    Project(Vec<crate::old_space::RootSlot>),
    Render(crate::old_space::RootSlot, Value),
}

impl MaterializeResult {
    /// Extract the `Value` policy's payload. Every caller already requested
    /// [`ResultMaterialization::Value`], so the other arms are unreachable by
    /// construction — but written out, not `_`, so a fifth policy variant
    /// fails to compile here until this is updated.
    fn expect_value(self) -> Value {
        match self {
            MaterializeResult::Value(v) => v,
            MaterializeResult::Bind(_)
            | MaterializeResult::Project(_)
            | MaterializeResult::Render(..) => {
                unreachable!("ResultMaterialization::Value always yields MaterializeResult::Value")
            }
        }
    }

    /// Extract the `Bind` policy's payload — see [`Self::expect_value`].
    fn expect_bind(self) -> crate::old_space::RootSlot {
        match self {
            MaterializeResult::Bind(slot) => slot,
            MaterializeResult::Value(_)
            | MaterializeResult::Project(_)
            | MaterializeResult::Render(..) => {
                unreachable!("ResultMaterialization::Bind always yields MaterializeResult::Bind")
            }
        }
    }

    /// Extract the `Project` policy's payload — see [`Self::expect_value`].
    fn expect_project(self) -> Vec<crate::old_space::RootSlot> {
        match self {
            MaterializeResult::Project(slots) => slots,
            MaterializeResult::Value(_)
            | MaterializeResult::Bind(_)
            | MaterializeResult::Render(..) => {
                unreachable!(
                    "ResultMaterialization::Project always yields MaterializeResult::Project"
                )
            }
        }
    }

    /// Extract the `Render` policy's payload — see [`Self::expect_value`].
    fn expect_render(self) -> (crate::old_space::RootSlot, Value) {
        match self {
            MaterializeResult::Render(slot, rendered) => (slot, rendered),
            MaterializeResult::Value(_)
            | MaterializeResult::Bind(_)
            | MaterializeResult::Project(_) => {
                unreachable!(
                    "ResultMaterialization::Render always yields MaterializeResult::Render"
                )
            }
        }
    }
}

/// Which driver [`JitEffectMachine::with_active_run`] uses, and — for the
/// effectful case — the handler triple it dispatches through. Pure programs
/// skip the freer-simple effect loop entirely (the compiled function returns
/// a raw value directly — no `Yield` decoding); effectful programs step
/// through [`drive_to_done`], dispatching each request through `handlers`.
enum RunTarget<'a, U, H: DispatchEffect<U>> {
    Pure,
    Effectful {
        table: &'a DataConTable,
        handlers: &'a mut H,
        user: &'a U,
    },
}

/// Uninhabited marker instantiating [`RunTarget`]'s `H` type parameter for a
/// [`RunTarget::Pure`] call, where no handler is ever dispatched.
/// [`RunTarget::Pure`] carries no handlers value, so a caller driving a pure
/// run still has to name SOME concrete `H: DispatchEffect<U>` to monomorphize
/// [`JitEffectMachine::with_active_run`]; this type exists purely to fill
/// that slot. It can never be constructed, so `dispatch` is unreachable by
/// construction — [`JitEffectMachine::drive_active`]'s pure branch never
/// calls it either.
enum NoHandlers {}

impl DispatchEffect<()> for NoHandlers {
    fn dispatch(
        &mut self,
        _tag: u64,
        _request: &Value,
        _cx: &EffectContext<'_, ()>,
    ) -> Result<tidepool_effect::Response, EffectError> {
        match *self {}
    }
}

/// The live driving context [`JitEffectMachine::with_active_run`] owns for one
/// run: a bare [`VMContext`] for [`RunTarget::Pure`], or a
/// [`CompiledEffectMachine`] (which owns its own `VMContext`) for
/// [`RunTarget::Effectful`].
enum ActiveContext {
    Pure(VMContext),
    Effectful(CompiledEffectMachine),
}

impl ActiveContext {
    fn vmctx_mut(&mut self) -> &mut VMContext {
        match self {
            ActiveContext::Pure(vmctx) => vmctx,
            ActiveContext::Effectful(machine) => machine.vmctx_mut(),
        }
    }
}

/// High-level JIT effect machine.
///
/// Compiles a `CoreExpr` (Haskell effect program) into native code via Cranelift
/// and runs it as a coroutine: the machine yields effect requests, the caller
/// dispatches them through an HList of [`EffectHandler`]s, and resumes with responses.
///
/// ```no_run
/// # use tidepool_codegen::jit_machine::JitEffectMachine;
/// # use tidepool_repr::{CoreExpr, CoreFrame, DataConTable, RecursiveTree, Literal};
/// # let expr: CoreExpr = RecursiveTree { nodes: vec![CoreFrame::Lit(Literal::LitInt(42))] };
/// # let table = DataConTable::new();
/// let mut vm = JitEffectMachine::compile(&expr, &table, 1 << 20)?;
/// let result = vm.run_pure()?;
/// # Ok::<(), tidepool_codegen::jit_machine::JitError>(())
/// ```
///
/// Owns the compiled code, nursery (GC heap), and freer-simple constructor tags.
/// The nursery size (in bytes) controls how much heap is available before GC triggers.
///
/// [`EffectHandler`]: tidepool_effect::EffectHandler
pub struct JitEffectMachine {
    pipeline: CodegenPipeline,
    nursery: Nursery,
    tags: Result<ConTags, &'static str>,
    func_id: FuncId,
    /// aeson-`Value` constructor ids for the `JsonDecode` primop, resolved once
    /// at compile from the `DataConTable` and installed into a host-fn
    /// thread-local at each run entry. `None` if the closure isn't in scope.
    json_con_ids: Option<tidepool_eval::json::JsonConIds>,
    /// `Either`/`I#`/`Text` constructor ids for the `ParseISO8601` primop's host
    /// fn — resolved at compile and installed into the machine state at each run
    /// entry. `None` if those constructors aren't in scope.
    time_con_ids: Option<tidepool_eval::time::TimeConIds>,
    /// External cancellation flag. The JIT installs a thread-local clone of this
    /// `Arc` via `set_cancel_flag` before entering compiled code; the next
    /// GC safepoint observes the flag and aborts execution with
    /// `YieldError::Cancelled` if it has been set. See [`Self::cancel_handle`].
    cancel_flag: Arc<AtomicBool>,
    /// Session state for GHCi-style persistent machines.
    /// `None` for one-shot machines created by [`Self::compile`].
    session: Option<SessionState>,
    /// Per-machine ambient state (cancel flag, JSON con ids, stack-map
    /// registry, call depth, diagnostics). Reached at run time via
    /// `(*vmctx).machine_state`, pointed at this field by
    /// `install_registries`/the run entries.
    machine_state: MachineState,
    /// The freer-simple continuation heap pointer of a turn that suspended at
    /// the ask boundary (`run_suspendable` → `SuspendableOutcome::Suspended`),
    /// waiting for `resume_suspended`. `None` for a running or completed
    /// machine. The pointer is into this machine's retained session heap.
    ///
    /// INVARIANT: while nested CHILD runs execute against the suspended
    /// parent, this pointer is a REGISTERED GC ROOT, not just protected by
    /// "no GC runs on a suspended machine" — [`Self::enter_nested_child`]
    /// copies it into `stowed_root_cell` and registers that heap-stable cell
    /// in the machine's `stowed_roots` set, so any child collection evacuates
    /// the continuation tree and rewrites the cell in place; on child
    /// teardown the (GC-current) pointer is read back out. `resume_suspended`
    /// still re-roots via `materialize_response_and_resume` for its own
    /// answer materialization.
    suspended_continuation: Option<*mut u8>,
    /// Heap-stable cell holding the stowed continuation pointer WHILE a
    /// nested child is running. A `Box` (not the `suspended_continuation`
    /// field directly) because the machine itself moves between threads under
    /// the stow-XOR-run discipline: the `Box`'s POINTEE address is a stable heap
    /// allocation that does NOT move with the struct, so the `stowed_roots`
    /// registration (the cell's address) stays valid across the move — exactly
    /// the `OldSpace` slots pattern. `None` unless a child is mid-run.
    stowed_root_cell: Option<Box<*mut u8>>,
    /// Number of nested child runs currently executing against this suspended
    /// parent. Zero when idle, suspended-but-no-child, or running its own
    /// turn. A parent resume is rejected while this is > 0 (exactly one
    /// computation on the heap at a time — sequential-isolated). Incremented by
    /// [`Self::enter_nested_child`], decremented on guard drop; the stowed root
    /// is registered on 0→1 and deregistered on 1→0.
    nested_child_depth: usize,
    /// A value-plane bind whose fragment ran through the SUSPENDABLE
    /// path (`run_fragment_suspendable_binding`/`resume_suspended_binding`) tenures
    /// its `Done` result into old-space and stashes the persistent [`RootSlot`]
    /// here, for the caller to read out AFTER the machine moves back off the eval
    /// thread. A `RootSlot` (`*mut *mut u8`) is `!Send`, so it cannot cross the
    /// eval-thread scope boundary as a bare value — it rides home INSIDE the
    /// machine (already `Send` under stowed-XOR-running, same as
    /// `suspended_continuation`; see the module docs for the full contract).
    /// `None` except in the window between a bind fragment completing and the
    /// caller taking it via [`Self::take_last_bound_root`]. A fork bind lands
    /// here on the eventual `resume`, not the initial (suspending) run.
    ///
    /// SLOT PATH ONLY: the parked path never writes this field — its
    /// `ParkedOutcome::CompletedBinding::root` returns the tenured root inline
    /// instead, so a second realm's completion cannot overwrite a first
    /// realm's still-unread root.
    last_bound_root: Option<crate::old_space::RootSlot>,
    /// Finalize-by-reference: the persistent root slot of a suspended
    /// `finalize @T closure`'s finalized VALUE (field 1 of the request Con),
    /// tenured at suspend time by [`Self::tenure_finalized_payload`]. `Some`
    /// only while suspended on a closure-valued finalize; read out by
    /// [`Self::take_finalized_root`] when the harness applies the closure by
    /// reference. Rides inside the machine for the same `!Send` reason as
    /// `last_bound_root`.
    ///
    /// SLOT PATH ONLY: the parked path stashes this on the
    /// [`ContinuationFrame`] instead (`ContinuationFrame::finalized_root`,
    /// taken via [`Self::take_parked_finalized_root`]) — a frame exists for
    /// the whole parked lifetime, so there is nowhere for a race to land.
    suspended_finalized_root: Option<crate::old_space::RootSlot>,
    /// REALM PROTOTYPE — the many-continuation generalization of
    /// `suspended_continuation`: every continuation parked by
    /// [`Self::run_suspendable_parked`], keyed by [`ContinuationId`] and tagged
    /// with the [`RealmId`] that owns it. Empty on every machine that only uses
    /// the single-slot path.
    ///
    /// THE INVARIANT: a frame's `cell` is registered in `stowed_roots` from the
    /// moment it is parked until the moment it is resumed — not just while a
    /// child runs. The single-slot path protects an idle-suspended continuation
    /// by a TEMPORAL argument (no GC can run on a suspended machine, enforced by
    /// the L7 `suspended_continuation.is_none()` asserts) and only falls back to
    /// a registered root for the window a nested child occupies. A parked frame
    /// has no such window: it is a root for its whole parked lifetime, so any
    /// collection — from a sibling park's turn, a plain fragment, another
    /// realm's resume, or a heap doubling in any of them — evacuates its
    /// continuation tree and rewrites `*cell` in place. Dropping the temporal
    /// argument is exactly what lets several continuations coexist on one heap
    /// while unrelated computation keeps running.
    ///
    /// Consequently `stowed_roots_count()` equals `continuations.len()` at
    /// every quiescent point on the parked path (plus one transiently while a
    /// `run_child_fragment*` guard is alive on the single-slot path).
    continuations: HashMap<ContinuationId, ContinuationFrame>,
    /// Monotonic source of [`ContinuationId`]s for `continuations`. Never
    /// rewound — a resumed id is not reused, so a stale id from a caller is a
    /// clean "unknown continuation" error rather than a silent aliasing of some
    /// later park.
    next_continuation_id: u64,
    /// Per-realm cancel flags, lazily minted on first park-path run/resume
    /// entry for a realm ([`Self::realm_cancel_flag`]). The parked entries
    /// install a realm's own flag into [`MachineState`] instead of
    /// `cancel_flag`, so cancelling one realm cannot abort a sibling realm's
    /// run — cancellation is realm-scoped, not machine-scoped, because a
    /// realm's continuation ids change on every re-suspension and cannot live
    /// only on the frame. Never shrinks during a realm's life: a cancelled
    /// realm's flag is not removed, only cleared (see
    /// [`Self::realm_cancel_handle`]'s doc for whether a completed run clears
    /// it); [`Self::close_realm`] removes the closed realm's entry.
    realm_cancel_flags: HashMap<RealmId, Arc<AtomicBool>>,
    /// The VALUE-HANDLE registry (one-session plan, pillar B): opaque,
    /// Send-able ids over machine-side persistent roots, so upper layers pass
    /// heap values — closures included — WITHOUT eagerly bridging them into a
    /// Rust [`Value`] (the eager bridge substitutes `CLOSURE_SENTINEL` and is
    /// the closure-killer on delivery paths; it remains only in
    /// [`Self::observe_handle`], where an opaque view of an opaque value is
    /// honest). Handles are SCOPE-OWNED BORROWS: minting does not consume the
    /// underlying root, observing and delivering do not consume the handle,
    /// and [`Self::close_realm`] releases every handle its realm minted.
    value_handles: HashMap<u64, HandleEntry>,
    /// Monotonic count of fragments compiled into the JITModule (its
    /// executable memory is never reclaimed) — [`HeapStats::fragments`].
    fragments_added: u64,
    /// Monotonic source of [`ValueHandle`] ids — same never-rewound
    /// discipline as `next_continuation_id` (a released handle's id is a
    /// clean "unknown handle" error, never a silent alias).
    next_value_handle: u64,
    /// The machine's ESTABLISHED handled-effect prefix: the machine cannot
    /// introspect its own handler stack (`H` is a compile-time monomorphized
    /// type parameter, not runtime data), so this is the machine's runtime
    /// record of what `H` is, in its stead. Set from the first NON-EMPTY
    /// handled prefix any realm ENTERS the parked path with
    /// ([`Self::enter_parked_path`], called before the machine is driven —
    /// deliberately not deferred to an actual park, since a realm whose turn
    /// completes without ever suspending still dispatches every effect
    /// through `H`); `None` until then. MONOTONIC — never cleared or
    /// overwritten afterward, including on resume: every realm on a machine
    /// is driven through the same single `H` for the machine's whole life,
    /// so a realm that resumed and completed does not release the
    /// constraint. See [`Self::check_prefix_compatible`].
    established_prefix: Option<Arc<[String]>>,
}

// SAFETY: a `JitEffectMachine` is only ever touched by ONE thread at a time —
// it is either stowed as data (E2 suspension) or running on exactly one eval
// thread, never both. This mirrors the existing `unsafe impl Send` on
// `CompiledEffectMachine`/`MachineState`/`GcState`: the JIT executable mappings
// and heap buffers are process-global address space, valid on any thread, and
// the raw `suspended_continuation` pointer is a heap offset into an owned
// buffer that moves with the machine. Concurrent access is prevented by the
// SessionEngine registry (a machine is stowed XOR running), so sending
// ownership across the suspend/resume thread boundary is sound.
unsafe impl Send for JitEffectMachine {}

/// External handle for cancelling a running `JitEffectMachine`.
///
/// `CancelHandle` is `Send + Sync + Clone`, so callers can hand clones to
/// watchdog threads. Cancellation is observed at the next GC safepoint
/// (heap check), which fires on essentially every non-trivial allocation in
/// Haskell code. The running program unwinds via the normal error path with
/// `JitError::Yield(YieldError::Cancelled)`.
///
/// The flag is per-`JitEffectMachine`, not per-run: call [`Self::reset`]
/// between runs if you intend to reuse the machine after a cancellation.
#[derive(Clone, Debug)]
pub struct CancelHandle(Arc<AtomicBool>);

impl CancelHandle {
    /// Request cancellation of the associated `JitEffectMachine`. The running
    /// program (if any) will abort at its next GC safepoint with
    /// `YieldError::Cancelled`.
    pub fn cancel(&self) {
        // SeqCst is overkill for correctness here (the JIT thread's relaxed
        // load will observe the store eventually), but this is not a hot path
        // — it is called once from a watchdog — so we prefer the stronger
        // ordering for debuggability.
        self.0.store(true, Ordering::SeqCst);
    }

    /// Returns `true` if cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// Clear a previous cancellation request. Call this between runs if the
    /// same `JitEffectMachine` is reused after a cancelled run.
    pub fn reset(&self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Session-level heap + cursor retained across runs.
///
/// `heap` is `None` until the first GC fires and migrates the live set off
/// the machine's `Nursery` into a `Vec<u64>` owned here.
///
/// INVARIANT: `Vec<u64>`, not `Vec<u8>` — heap objects are read/written
/// assuming 8-byte alignment, which `Vec<u64>` guarantees structurally
/// (unlike `Vec<u8>`, whose element alignment is 1 — any alignment it
/// happens to have is an allocator implementation detail).
///
/// `cursor` is the bump high-water mark (bytes from the start of `heap`, or
/// from `nursery.start()` when `heap` is None) at the end of the last run —
/// the next run resumes allocation from there.
struct SessionState {
    heap: Option<Vec<u64>>,
    cursor: usize,
    /// Populated at bind time; read by the four `*_and_bind*` run entries to
    /// tenure NF values into stable old-space slots.
    old_space: crate::old_space::OldSpace,
}

/// Ensures thread-local JIT registries are cleaned up even on early error return.
///
/// For one-shot machines (`is_session = false`), Drop behaves as before.
/// For session machines (`is_session = true`):
///   - `reclaim` is set by `arm_reclaim` after the vmctx is at its final
///     location; Drop reads `alloc_ptr` from the vmctx and calls
///     `reclaim_session_heap` to move `active_buffer` back onto the machine
///     BEFORE `clear_run_scratch` takes the GcState.
///   - `clear_run_scratch` (not `clear_gc_state`) runs per-run; it drops
///     only the GcState shell + RUST_ROOTS, leaving PERSISTENT_ROOTS alone.
pub(crate) struct RegistryGuard {
    is_session: bool,
    /// Raw pointers captured by `arm_reclaim`. Both point into the same
    /// stack frame as this guard (run / run_pure), which cannot have
    /// returned by the time Drop runs. VMContext has no custom Drop, so its
    /// bytes are valid on the stack even after the value is logically dropped.
    reclaim: Option<ReclaimTargets>,
    /// Points at the owning `JitEffectMachine::machine_state`, set by
    /// `install_registries`. Outlives this guard (same call frame).
    machine_state: *mut MachineState,
    /// The thread's `CURRENT_MACHINE` value before `install_registries`
    /// installed `machine_state` (null unless runs nest) — restored on drop.
    prev_machine: *mut MachineState,
}

/// The two raw pointers `arm_reclaim` captures for the Drop-time heap reclaim.
/// A named struct (not a bare tuple) so the `session_slot` and `vmctx` fields —
/// both raw pointers — cannot be silently transposed at a call site.
struct ReclaimTargets {
    /// Points to `JitEffectMachine::session` (same call frame as the guard).
    session_slot: *mut Option<SessionState>,
    /// Points to the VMContext used for this run (same call frame; no Drop).
    vmctx: *const crate::context::VMContext,
}

impl RegistryGuard {
    /// Arm the reclaim step for session machines. Called after the vmctx is
    /// at its final stable location (local `vmctx` in run_pure, inside
    /// `CompiledEffectMachine` in run).
    ///
    /// # Safety
    /// - `session` must point to `JitEffectMachine::session` and remain
    ///   valid until this guard drops (it's in the same call frame).
    /// - `vmctx` must point to the VMContext used for this run and remain
    ///   readable until Drop (no custom Drop on VMContext, so the stack
    ///   bytes persist until the enclosing frame returns).
    unsafe fn arm_reclaim(
        &mut self,
        session: *mut Option<SessionState>,
        vmctx: *const crate::context::VMContext,
    ) {
        if self.is_session {
            self.reclaim = Some(ReclaimTargets {
                session_slot: session,
                vmctx,
            });
        }
    }
}

impl Drop for RegistryGuard {
    fn drop(&mut self) {
        // Reclaim the live heap buffer back onto the machine BEFORE
        // clear_run_scratch takes GcState (which would free active_buffer).
        if let Some(ReclaimTargets {
            session_slot,
            vmctx,
        }) = self.reclaim
        {
            // SAFETY: vmctx points into the enclosing run/run_pure stack
            // frame which is still live. VMContext has no custom Drop so its
            // bytes are intact even after the value is logically dropped.
            // session_slot points to JitEffectMachine::session in the same frame.
            unsafe {
                let ap = (*vmctx).alloc_ptr;
                let (buf, cur) = (*self.machine_state).reclaim_session_heap(ap);
                if let Some(s) = (*session_slot).as_mut() {
                    s.heap = buf;
                    s.cursor = cur;
                }
            }
        }
        // SAFETY: machine_state was set by install_registries and outlives
        // this guard (points at the owning JitEffectMachine's field). Clean
        // the per-run cells (including the GC-cluster's clear_run_scratch)
        // directly through the machine (not the ambient free-fn shims)
        // BEFORE restoring CURRENT_MACHINE below — otherwise the free fns
        // would see a cleared/stale current-machine pointer.
        unsafe {
            (*self.machine_state).clear_run_scratch();
            (*self.machine_state).clear_stack_map_registry();
            (*self.machine_state).clear_cancel_flag();
            let _ = (*self.machine_state).take_runtime_error();
            let _ = (*self.machine_state).drain_diagnostics();
            (*self.machine_state).reset_call_depth();
        }
        // D7: this drops only the thread-local's Rc *handle* to the lambda
        // registry this run installed — the accumulated registry itself lives
        // in `self.pipeline` (an `Rc<LambdaRegistry>` field) and is untouched.
        // Dropping the handle here is what lets the NEXT `install_registries`
        // call's `build_lambda_registry` extend that shared registry in place
        // (refcount back to 1) instead of falling back to a clone.
        crate::debug::clear_lambda_registry();
        crate::host_fns::set_exec_context("");
        crate::machine_state::restore_current_machine(self.prev_machine);
    }
}

/// The compiled artifacts produced by [`JitEffectMachine::compile_inner`],
/// shared by the one-shot (`compile`) and session (`compile_session`) ctors.
type CompiledParts = (
    CodegenPipeline,
    Nursery,
    Result<ConTags, &'static str>,
    FuncId,
    Option<tidepool_eval::json::JsonConIds>,
    Option<tidepool_eval::time::TimeConIds>,
);

impl JitEffectMachine {
    /// Shared compilation body: normalise, emit, finalise.
    fn compile_inner(
        expr: &CoreExpr,
        table: &DataConTable,
        nursery_size: usize,
    ) -> Result<CompiledParts, JitError> {
        crate::debug::init_logging();
        // #313 defense: a duplicate VarId on the top-level Let spine means two
        // distinct top-level bindings silently shadow each other — fail loudly
        // at load instead. Runs on the raw deserialized tree (the wrapAllBinds
        // Let-nest), before normalize/datacon wrapping reshape it.
        tidepool_repr::check_toplevel_varids(expr)?;
        let expr = tidepool_repr::normalize(expr, table);
        let expr = crate::datacon_env::wrap_with_datacon_env(expr, table);
        // Defensive precondition restore: the real Haskell pipeline never emits
        // a Jump crossing a Lam boundary (Translate.hs's `jumpCrossesLam` rewrites
        // it first), but hand-built/synthetic CoreExpr producers can. Re-check
        // after normalize/datacon-env wrapping so whatever final shape reaches
        // emission satisfies codegen's join-registration invariant.
        let expr = crate::lower::lower_jump_crosses_lam(&expr);
        let mut pipeline = CodegenPipeline::new(&crate::host_fns::host_fn_symbols())?;
        // Give data-case dispatch runtime tolerance for bare Lit scrutinees of
        // boxed-literal wrapper constructors (e.g. a Rust-materialized aeson
        // `Number`'s LitDouble reaching `case x of { D# ds -> .. }`).
        pipeline.lit_wrappers = crate::emit::LitWrapperIds::from_table(table);
        // No session bindings on initial compile, so the external env is empty.
        let func_id = crate::emit::expr::compile_expr(
            &mut pipeline,
            &expr,
            "main",
            &crate::emit::ExternalEnv::new(),
        )
        .map_err(JitError::Compilation)?;
        pipeline.finalize()?;
        let tags = ConTags::from_table(table).map_err(|kind| kind.name());
        let nursery = Nursery::new(nursery_size);
        // Cache the aeson-`Value` constructor ids for the `JsonDecode` primop's
        // host fn, and the `Either`/`I#`/`Text` ids for `ParseISO8601` (both
        // installed into the machine state before each run).
        let json_con_ids = tidepool_eval::json::JsonConIds::from_table(table);
        let time_con_ids = tidepool_eval::time::TimeConIds::from_table(table);
        Ok((pipeline, nursery, tags, func_id, json_con_ids, time_con_ids))
    }

    /// Compile a CoreExpr for one-shot JIT execution.
    ///
    /// The returned machine has no session state: the heap lives in the
    /// machine's `Nursery` and is discarded after each run.
    pub fn compile(
        expr: &CoreExpr,
        table: &DataConTable,
        nursery_size: usize,
    ) -> Result<Self, JitError> {
        let (pipeline, nursery, tags, func_id, json_con_ids, time_con_ids) =
            Self::compile_inner(expr, table, nursery_size)?;
        Ok(Self {
            pipeline,
            nursery,
            tags,
            func_id,
            json_con_ids,
            time_con_ids,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            session: None,
            machine_state: MachineState::new(),
            suspended_continuation: None,
            stowed_root_cell: None,
            nested_child_depth: 0,
            last_bound_root: None,
            suspended_finalized_root: None,
            continuations: HashMap::new(),
            next_continuation_id: 0,
            realm_cancel_flags: HashMap::new(),
            value_handles: HashMap::new(),
            next_value_handle: 0,
            fragments_added: 0,
            established_prefix: None,
        })
    }

    /// Compile a CoreExpr for GHCi-style session execution.
    ///
    /// The returned machine retains its heap across multiple runs: the live
    /// heap after the first GC is moved into `SessionState::heap` and
    /// re-installed on every subsequent `run`/`run_pure` call. Persistent
    /// GC roots (registered via [`Self::register_persistent_root`]) survive
    /// across runs and are cleared only when the machine is dropped.
    pub fn compile_session(
        expr: &CoreExpr,
        table: &DataConTable,
        nursery_size: usize,
    ) -> Result<Self, JitError> {
        let (pipeline, nursery, tags, func_id, json_con_ids, time_con_ids) =
            Self::compile_inner(expr, table, nursery_size)?;
        Ok(Self {
            pipeline,
            nursery,
            tags,
            func_id,
            json_con_ids,
            time_con_ids,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            session: Some(SessionState {
                heap: None,
                cursor: 0,
                old_space: crate::old_space::OldSpace::new(),
            }),
            machine_state: MachineState::new(),
            suspended_continuation: None,
            stowed_root_cell: None,
            nested_child_depth: 0,
            last_bound_root: None,
            suspended_finalized_root: None,
            continuations: HashMap::new(),
            next_continuation_id: 0,
            realm_cancel_flags: HashMap::new(),
            value_handles: HashMap::new(),
            next_value_handle: 0,
            fragments_added: 0,
            established_prefix: None,
        })
    }

    /// Obtain a clone-able, thread-safe handle for requesting cancellation of
    /// this machine's next (or in-flight) run. The handle remains valid for
    /// the lifetime of the machine; multiple handles may be held concurrently.
    pub fn cancel_handle(&self) -> CancelHandle {
        CancelHandle(self.cancel_flag.clone())
    }

    /// Obtain a clone-able cancellation handle scoped to ONE realm,
    /// lazily minting that realm's flag on first request. Cancelling this
    /// handle aborts only runs/resumes parked-path-entered under `realm` —
    /// a sibling realm's run on the same machine is unaffected, because the
    /// parked entries install the ACTIVE realm's flag into [`MachineState`]
    /// (see [`Self::realm_cancel_flag`]), not the machine-level
    /// [`Self::cancel_flag`].
    ///
    /// A cancelled realm's flag is NOT auto-cleared after the cancelled run
    /// completes — same discipline as the machine-level [`CancelHandle`]
    /// (whose own doc says "call `reset` between runs if you intend to
    /// reuse"): the caller decides when a realm is done retrying and calls
    /// `CancelHandle::reset` explicitly.
    pub fn realm_cancel_handle(&mut self, realm: RealmId) -> CancelHandle {
        CancelHandle(self.realm_cancel_flag(realm))
    }

    /// This realm's cancel flag, lazily minted on first park-path
    /// run/resume entry for `realm`. Never removed once minted, so the same
    /// `Arc` identity is returned for the machine's whole life — a
    /// [`ContinuationFrame`] cloning it at park time and a later
    /// [`Self::realm_cancel_handle`] call always observe the same flag.
    fn realm_cancel_flag(&mut self, realm: RealmId) -> Arc<AtomicBool> {
        self.realm_cancel_flags
            .entry(realm)
            .or_insert_with(|| Arc::new(AtomicBool::new(false)))
            .clone()
    }

    /// Drain this machine's accumulated diagnostics. The machine-scoped
    /// sibling of the ambient `host_fns::drain_diagnostics` free-fn shim
    /// (per #340).
    pub fn drain_diagnostics(&self) -> Vec<String> {
        self.machine_state.drain_diagnostics()
    }

    /// Install per-run thread-local registries (using the machine-level
    /// cancel flag) and return a drop guard. Every entry outside the parked
    /// run/resume path calls this.
    ///
    /// For session machines: re-points the GC state at the retained heap
    /// buffer (if a GC has already run) OR at `nursery.start()` (first run
    /// only). For one-shot machines: always points at `nursery.start()`.
    pub(crate) fn install_registries(&mut self) -> RegistryGuard {
        let flag = self.cancel_flag.clone();
        self.install_registries_with_cancel_flag(flag)
    }

    /// Shared body of [`Self::install_registries`]: install per-run
    /// thread-local registries using the given `cancel_flag` rather than
    /// unconditionally `self.cancel_flag` — the parked run/resume entries
    /// pass the ACTIVE realm's flag here instead, via
    /// [`Self::realm_cancel_flag`], so cancelling one realm cannot abort a
    /// sibling realm's run on the same machine.
    fn install_registries_with_cancel_flag(
        &mut self,
        cancel_flag: Arc<AtomicBool>,
    ) -> RegistryGuard {
        crate::debug::set_lambda_registry(self.pipeline.build_lambda_registry());
        self.machine_state
            .set_stack_map_registry(&self.pipeline.stack_maps);
        match &mut self.session {
            Some(s) => match s.heap.take() {
                Some(buf) => self.machine_state.install_session_buffer(buf),
                None => self
                    .machine_state
                    .set_gc_state(self.nursery.start() as *mut u8, self.nursery.size()),
            },
            None => self
                .machine_state
                .set_gc_state(self.nursery.start() as *mut u8, self.nursery.size()),
        }
        self.machine_state.set_cancel_flag(cancel_flag);
        // Make the aeson-`Value` constructor ids (JsonDecode) and the
        // Either/I#/Text ids (ParseISO8601) visible to those primops' host fns
        // for the duration of this run.
        self.machine_state.set_json_con_ids(self.json_con_ids);
        self.machine_state.set_time_con_ids(self.time_con_ids);
        let machine_state_ptr = &mut self.machine_state as *mut MachineState;
        // Install this machine as the thread's reach target for vmctx-less
        // host fns and the external ambient shims; RegistryGuard::drop
        // restores whatever was installed before (null unless runs nest).
        let prev_machine = crate::machine_state::install_current_machine(machine_state_ptr);
        RegistryGuard {
            is_session: self.session.is_some(),
            reclaim: None,
            machine_state: machine_state_ptr,
            prev_machine,
        }
    }

    /// Build a `VMContext` for a session run, re-pointing alloc_ptr at the
    /// persistent cursor.
    ///
    /// Reads the active GC region from `GC_STATE` (installed by
    /// `install_registries` immediately before this call) and sets
    /// `alloc_ptr = start + cursor` so the run resumes from the last
    /// run's high-water mark rather than overwriting live data.
    ///
    /// # Panics
    /// Panics if called without GC state installed or on a non-session machine.
    fn make_session_vmctx(&self) -> crate::context::VMContext {
        #[allow(
            clippy::expect_used,
            reason = "GC state must be installed before make_session_vmctx"
        )]
        let (start, size) = self
            .machine_state
            .gc_active_range()
            .expect("GC state must be installed before make_session_vmctx");
        #[allow(
            clippy::expect_used,
            reason = "make_session_vmctx called on non-session machine"
        )]
        let cursor = self
            .session
            .as_ref()
            .expect("make_session_vmctx called on non-session machine")
            .cursor;
        // SAFETY: start..start+size is the session heap installed by
        // install_registries. cursor <= size is maintained by reclaim_session_heap.
        let mut vmctx = unsafe {
            crate::context::VMContext::new(start, start.add(size), crate::host_fns::gc_trigger)
        };
        // SAFETY: cursor <= size guaranteed by the reclaim invariant.
        vmctx.alloc_ptr = unsafe { start.add(cursor) };
        vmctx
    }

    /// The shared executor every plain (non-suspending) run route is a thin
    /// wrapper over — see the route table above and [`ResultMaterialization`].
    /// Owns, exactly once: the L7/session asserts, signal-handler install,
    /// registry install (+ its drop-guard), `VMContext`/`CompiledEffectMachine`
    /// construction, and — structurally, not by convention — reclaim arming
    /// LAST, after [`Self::materialize`] (the only place a
    /// `Bind`/`Project`/`Render` epilogue touches `self.session` via
    /// `tenure`), unconditionally on every exit path.
    #[allow(clippy::too_many_arguments)]
    fn with_active_run<U, H: DispatchEffect<U>>(
        &mut self,
        func_id: FuncId,
        mode: RunTarget<'_, U, H>,
        materialization: ResultMaterialization,
        l7_msg: &str,
        exec_start: &str,
        resume_suffix: &str,
    ) -> Result<MaterializeResult, JitError> {
        // L7: shared by every plain entry — starting a new turn while a prior
        // one is still parked at resume_suspended isn't a GC-rooted invariant
        // anything else enforces.
        assert!(self.suspended_continuation.is_none(), "{l7_msg}");
        let tags = match &mode {
            RunTarget::Pure => None,
            RunTarget::Effectful { .. } => Some(self.tags.map_err(JitError::MissingConTags)?),
        };

        // Per-thread signal handler + altstack; idempotent.
        crate::signal_safety::install();
        let mut _guard = self.install_registries();

        // SAFETY: get_function_ptr returns a finalized JIT code pointer.
        // Transmuting to the expected calling convention is correct per our
        // compilation contract.
        let func_ptr: unsafe extern "C" fn(*mut VMContext) -> *mut u8 =
            unsafe { std::mem::transmute(self.pipeline.get_function_ptr(func_id)) };

        let raw_vmctx = if self.session.is_some() {
            self.make_session_vmctx()
        } else {
            self.nursery.make_vmctx(crate::host_fns::gc_trigger)
        };
        let mut ctx = match tags {
            None => {
                let mut vmctx = raw_vmctx;
                // SAFETY: machine_state outlives this run (owned by self).
                vmctx.machine_state = &mut self.machine_state as *mut MachineState;
                ActiveContext::Pure(vmctx)
            }
            Some(tags) => {
                let mut machine = CompiledEffectMachine::new(func_ptr, raw_vmctx, tags);
                // SAFETY: machine_state outlives this run (owned by self).
                machine.vmctx_mut().machine_state = &mut self.machine_state as *mut MachineState;
                ActiveContext::Effectful(machine)
            }
        };

        let result = self
            .drive_active(&mut ctx, func_ptr, mode, exec_start, resume_suffix)
            .and_then(|done_ptr| self.materialize(ctx.vmctx_mut(), done_ptr, materialization));

        // Arm reclaim LAST, unconditionally, on every exit path — this one
        // call site is the whole point of the fold: no route-local judgment
        // call about ordering survives to be gotten wrong.
        //
        // SAFETY: `ctx` (a `VMContext` or a `CompiledEffectMachine`, which
        // owns its own `VMContext`) is a local in THIS frame, live until this
        // function returns; neither type has a custom Drop, so its bytes are
        // valid when `_guard` drops immediately after (same frame).
        unsafe {
            _guard.arm_reclaim(&mut self.session as *mut _, ctx.vmctx_mut() as *const _);
        }
        result
    }

    /// Drive `ctx` to its `Done` heap pointer: a raw call (no `Yield`
    /// decoding) for [`RunTarget::Pure`], or the shared freer-simple step
    /// loop ([`drive_to_done`]) for [`RunTarget::Effectful`]. The explicit
    /// pre-bridge `take_runtime_error`/null checks below are PURE-ONLY: the
    /// effectful loop already surfaces a runtime error via its own
    /// `Yield::Error` arm before ever reaching `Done`, so `drive_to_done`
    /// needs no separate check here — this is not an oversight, see the
    /// route table above.
    fn drive_active<U, H: DispatchEffect<U>>(
        &mut self,
        ctx: &mut ActiveContext,
        func_ptr: unsafe extern "C" fn(*mut VMContext) -> *mut u8,
        mode: RunTarget<'_, U, H>,
        exec_start: &str,
        resume_suffix: &str,
    ) -> Result<*mut u8, JitError> {
        match (ctx, mode) {
            (ActiveContext::Pure(vmctx), RunTarget::Pure) => {
                self.machine_state.reset_call_depth();
                crate::host_fns::set_exec_context(exec_start);
                let vmctx_ptr = vmctx as *mut VMContext;
                // SAFETY: calling the JIT function through a valid function
                // pointer with signal protection for crash recovery; vmctx is
                // freshly constructed.
                let result_ptr: *mut u8 =
                    unsafe { crate::signal_safety::with_signal_protection(|| func_ptr(vmctx_ptr)) }
                        .map_err(|e| JitError::Yield(runtime_error_or_signal(e.0)))?;
                // SAFETY: resolving pending tail calls; vmctx.tail_callee/
                // tail_arg are valid heap pointers set by JIT tail-call sites.
                let result_ptr = unsafe { resolve_tail_calls_protected(vmctx, result_ptr)? };
                // Runtime error now returns a poison object instead of null,
                // so the null check alone is not enough — check first.
                if let Some(err) = crate::host_fns::take_runtime_error() {
                    return Err(JitError::Yield(crate::yield_type::YieldError::from(err)));
                }
                if result_ptr.is_null() {
                    return Err(JitError::Yield(crate::yield_type::YieldError::NullPointer));
                }
                Ok(result_ptr)
            }
            (
                ActiveContext::Effectful(machine),
                RunTarget::Effectful {
                    table,
                    handlers,
                    user,
                },
            ) => drive_to_done(
                machine,
                &self.cancel_flag,
                table,
                handlers,
                user,
                exec_start,
                resume_suffix,
            ),
            _ => unreachable!(
                "with_active_run always constructs ActiveContext to match RunTarget's variant"
            ),
        }
    }

    /// The shared epilogue for EVERY completion, plain or suspendable —
    /// [`ResultMaterialization`]'s four policies.
    ///
    /// The parameter is a bare `&mut VMContext` rather than the
    /// [`ActiveContext`] the plain routes own, because that is the context
    /// BOTH callers can supply: `with_active_run` passes `ctx.vmctx_mut()`,
    /// and [`Self::finish_suspendable`] — which holds a
    /// [`CompiledEffectMachine`], not an `ActiveContext` — passes
    /// `machine.vmctx_mut()`. Nothing here ever needed more than the vmctx.
    ///
    /// ORDERING (load-bearing, and the reason this is one function): every
    /// policy that tenures touches `self.session`, so a caller MUST NOT have
    /// armed reclaim before calling this — `RegistryGuard::arm_reclaim` stores
    /// a `*mut self.session` that would alias the `tenure` below. Both
    /// families arm strictly AFTER this returns (`with_active_run`'s single
    /// arm-last call; `run_suspendable_shared`/`resume_applied`'s tail arm).
    fn materialize(
        &mut self,
        vmctx: &mut VMContext,
        done_ptr: *mut u8,
        materialization: ResultMaterialization,
    ) -> Result<MaterializeResult, JitError> {
        // One address, used by every arm below: `vmctx` is a live `&mut` for
        // this whole call, so the pointer stays valid throughout.
        let vmctx_ptr: *mut VMContext = vmctx;
        match materialization {
            ResultMaterialization::Value => {
                // SAFETY: done_ptr is a valid heap pointer returned by the
                // JIT; vmctx_ptr is valid for forcing thunks; signal
                // protection guards against crashes.
                let bridge_res = unsafe {
                    crate::signal_safety::with_signal_protection(|| {
                        heap_bridge::heap_to_value_forcing(done_ptr, vmctx_ptr)
                    })
                }
                .map_err(JitError::Signal)?;
                // A cancel observed during forcing (gc_trigger) records the
                // first cause; the bridge outcome — even a successful bridge
                // of a poison value — is only its symptom.
                let value =
                    crate::host_fns::surface_error(bridge_res.map_err(JitError::HeapBridge))?;
                Ok(MaterializeResult::Value(value))
            }
            ResultMaterialization::Bind { forced } => {
                if let Some(err) = crate::host_fns::take_runtime_error() {
                    return Err(JitError::Yield(crate::yield_type::YieldError::from(err)));
                }
                if done_ptr.is_null() {
                    return Err(JitError::Yield(crate::yield_type::YieldError::NullPointer));
                }
                // Optionally deep-force to NF before tenuring (Tier0 data);
                // Tier1 closures tenure as-is (callable code, not data).
                let nf_ptr = if forced {
                    let nf = unsafe {
                        crate::signal_safety::with_signal_protection(|| {
                            crate::host_fns::deep_force(vmctx_ptr, done_ptr)
                        })
                    }
                    .map_err(JitError::Signal)?;
                    if let Some(err) = crate::host_fns::take_runtime_error() {
                        return Err(JitError::Yield(crate::yield_type::YieldError::from(err)));
                    }
                    nf
                } else {
                    done_ptr
                };
                // E/D — tenure the (optionally forced) closure out of the
                // nursery into old-space and register its persistent root.
                // gc_active_range is the nursery from-range (still installed;
                // the guard has not dropped). SAFETY: nf_ptr is a live heap
                // object in the nursery from-range; tenure evacuates its
                // closure and registers the returned slot as a persistent
                // root valid for the machine's life. `self.session` is
                // unaliased here — reclaim is armed strictly after this
                // method returns (`with_active_run`'s single arm-last call).
                #[allow(clippy::expect_used, reason = "GC state installed for the bind run")]
                let from = self
                    .machine_state
                    .gc_active_range()
                    .expect("GC state installed for the bind run");
                let from_range = (from.0 as *const u8, unsafe {
                    from.0.add(from.1) as *const u8
                });
                let slot = unsafe {
                    #[allow(clippy::expect_used, reason = "session machine")]
                    self.session
                        .as_mut()
                        .expect("session machine")
                        .old_space
                        .tenure(vmctx_ptr, nf_ptr, from_range)
                };
                Ok(MaterializeResult::Bind(slot))
            }
            ResultMaterialization::Project { n_fields } => {
                if let Some(err) = crate::host_fns::take_runtime_error() {
                    return Err(JitError::Yield(crate::yield_type::YieldError::from(err)));
                }
                if done_ptr.is_null() {
                    return Err(JitError::Yield(crate::yield_type::YieldError::NullPointer));
                }
                // GC-safe projection protocol:
                // 1. deep_force the WHOLE TUPLE first. deep_force internally
                //    registers every pending parent as a Rust GC root and
                //    re-reads field slots from the live (possibly relocated)
                //    parent after each heap_force — so no pointer is cached
                //    across a GC. Closures (TAG_CLOSURE) are forced to WHNF
                //    and left as-is.
                let nf_tuple = unsafe {
                    crate::signal_safety::with_signal_protection(|| {
                        crate::host_fns::deep_force(vmctx_ptr, done_ptr)
                    })
                }
                .map_err(JitError::Signal)?;
                if let Some(err) = crate::host_fns::take_runtime_error() {
                    return Err(JitError::Yield(crate::yield_type::YieldError::from(err)));
                }
                // 2. Validate arity from the NF (post-GC) object.
                let n_actual = unsafe {
                    *(nf_tuple.add(crate::layout::CON_NUM_FIELDS_OFFSET as usize) as *const u16)
                        as usize
                };
                if n_actual != n_fields.get() {
                    return Err(JitError::Yield(crate::yield_type::YieldError::Runtime(
                        crate::host_fns::RuntimeError::UserErrorMsg(format!(
                            "multi-bind: result tuple has {} fields, expected {}",
                            n_actual, n_fields
                        )),
                    )));
                }
                // 3. Root nf_tuple across the per-field tenure loop below.
                //    `tenure()` now folds a real minor collection into every
                //    call that actually evacuates something (see
                //    `OldSpace::tenure`'s doc) to fix up sibling references,
                //    which can relocate other live nursery objects —
                //    including nf_tuple itself, read again on every
                //    iteration.
                let mut nf_tuple = nf_tuple;
                // SAFETY: vmctx_ptr is the active run's VMContext; the scope
                // covers the whole per-field tenure loop below.
                let _root_tuple = unsafe { heap_bridge::RootScope::new(vmctx_ptr) };
                // SAFETY: the slot lives on this frame until _root_tuple drops.
                unsafe {
                    crate::host_fns::register_rust_root(vmctx_ptr, &mut nf_tuple as *mut *mut u8);
                }
                // 4. Project each field from nf_tuple and tenure. Both
                //    nf_tuple (kept current by the registered root above)
                //    and from_range are re-read fresh every iteration: a
                //    fixup collection inside any tenure() call can relocate
                //    nf_tuple and/or grow/replace the active nursery region
                //    (heap doubling), invalidating a snapshot taken before
                //    the loop.
                let mut slots = Vec::with_capacity(n_fields.get());
                for i in 0..n_fields.get() {
                    let field_ptr = unsafe {
                        *(nf_tuple.add(crate::layout::CON_FIELDS_OFFSET as usize + 8 * i)
                            as *const *mut u8)
                    };
                    #[allow(clippy::expect_used, reason = "GC state installed for the bind run")]
                    let from = self
                        .machine_state
                        .gc_active_range()
                        .expect("GC state installed for the bind run");
                    let from_range = (from.0 as *const u8, unsafe {
                        from.0.add(from.1) as *const u8
                    });
                    let slot = unsafe {
                        #[allow(clippy::expect_used, reason = "session machine")]
                        self.session
                            .as_mut()
                            .expect("session machine")
                            .old_space
                            .tenure(vmctx_ptr, field_ptr, from_range)
                    };
                    slots.push(slot);
                }
                Ok(MaterializeResult::Project(slots))
            }
            ResultMaterialization::Render { field0_forced } => {
                if let Some(err) = crate::host_fns::take_runtime_error() {
                    return Err(JitError::Yield(crate::yield_type::YieldError::from(err)));
                }
                if done_ptr.is_null() {
                    return Err(JitError::Yield(crate::yield_type::YieldError::NullPointer));
                }
                // `Yield::Done` (effect_machine::parse_result) already forces
                // the Val field to WHNF before returning it, so done_ptr is
                // guaranteed a real Con here (never a thunk) — safe to read
                // its header directly, no additional WHNF force needed.
                let tag = unsafe { *done_ptr };
                if tag != crate::layout::TAG_CON {
                    return Err(JitError::Yield(
                        crate::yield_type::YieldError::UnexpectedTag(tag),
                    ));
                }
                let n_actual = unsafe {
                    *(done_ptr.add(crate::layout::CON_NUM_FIELDS_OFFSET as usize) as *const u16)
                        as usize
                };
                if n_actual != 2 {
                    return Err(JitError::Yield(crate::yield_type::YieldError::Runtime(
                        crate::host_fns::RuntimeError::UserErrorMsg(format!(
                            "bind-render: result tuple has {} fields, expected 2",
                            n_actual
                        )),
                    )));
                }
                // Read-only, no GC-capable calls in between — both field
                // pointers are consistent with the (already-WHNF) done_ptr.
                let field0_ptr = unsafe {
                    *(done_ptr.add(crate::layout::CON_FIELDS_OFFSET as usize) as *const *mut u8)
                };
                let field1_ptr = unsafe {
                    *(done_ptr.add(crate::layout::CON_FIELDS_OFFSET as usize + 8) as *const *mut u8)
                };

                // Root field0_ptr across the field1 bridge below: bridging
                // can force thunks reachable from field1's subtree, which can
                // allocate and trigger a minor GC that relocates field0's
                // object (whether or not it aliases field1).
                let mut field0_ptr = field0_ptr;
                // SAFETY: vmctx_ptr is the active run's VMContext; the scope
                // covers exactly the field1 bridge call below.
                let _root0 = unsafe { heap_bridge::RootScope::new(vmctx_ptr) };
                // SAFETY: the slot lives on this frame until _root0 drops.
                unsafe {
                    crate::host_fns::register_rust_root(vmctx_ptr, &mut field0_ptr as *mut *mut u8);
                }

                // READ-BEFORE-TENURE (load-bearing): bridge field1 (the
                // render) into a fully OWNED Value before field0 is forced or
                // tenured. heap_to_value_forcing's result retains no pointer
                // into the JIT heap, so it is unaffected by whatever tenure()
                // below does to field0's object — even when field0 and
                // field1 alias.
                //
                // This ordering is STRUCTURAL, not a convention any route has
                // to re-observe: the suspendable render route reaches this
                // same code (via `finish_suspendable`), so a turn that
                // suspends at an ask and completes on a later resume gets the
                // identical bridge-then-tenure sequence.
                let bridge_res = unsafe {
                    crate::signal_safety::with_signal_protection(|| {
                        heap_bridge::heap_to_value_forcing(field1_ptr, vmctx_ptr)
                    })
                }
                .map_err(JitError::Signal)?;
                if let Some(err) = crate::host_fns::take_runtime_error() {
                    return Err(JitError::Yield(crate::yield_type::YieldError::from(err)));
                }
                let rendered =
                    crate::host_fns::surface_error(bridge_res.map_err(JitError::HeapBridge))?;

                // field0_ptr is no longer needed as a GC root past this
                // point: deep_force (if field0_forced) roots its own
                // traversal, and the only remaining use of field0_ptr is as
                // tenure()'s OWN argument below (`nf_field0`), which tenure
                // roots internally — nothing else here needs to survive
                // whatever collection tenure() may now fold in.
                drop(_root0);

                // Force (iff field0_forced) and tenure field0 ONLY. field1 is
                // never tenured — it was already fully consumed into
                // `rendered` above.
                let nf_field0 = if field0_forced {
                    let nf = unsafe {
                        crate::signal_safety::with_signal_protection(|| {
                            crate::host_fns::deep_force(vmctx_ptr, field0_ptr)
                        })
                    }
                    .map_err(JitError::Signal)?;
                    if let Some(err) = crate::host_fns::take_runtime_error() {
                        return Err(JitError::Yield(crate::yield_type::YieldError::from(err)));
                    }
                    nf
                } else {
                    field0_ptr
                };

                // Capture from_range AFTER any forcing above (GC may have
                // changed the active region) — same ordering as Project.
                #[allow(clippy::expect_used, reason = "GC state installed for the bind run")]
                let from = self
                    .machine_state
                    .gc_active_range()
                    .expect("GC state installed for the bind run");
                let from_range = (from.0 as *const u8, unsafe {
                    from.0.add(from.1) as *const u8
                });
                let slot = unsafe {
                    #[allow(clippy::expect_used, reason = "session machine")]
                    self.session
                        .as_mut()
                        .expect("session machine")
                        .old_space
                        .tenure(vmctx_ptr, nf_field0, from_range)
                };
                Ok(MaterializeResult::Render(slot, rendered))
            }
        }
    }

    /// Run to completion, dispatching effects through the handler HList.
    pub fn run<U, H: DispatchEffect<U>>(
        &mut self,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
    ) -> Result<Value, JitError> {
        let func_id = self.func_id;
        self.run_with_entry(func_id, table, handlers, user)
    }

    /// Shared effectful-run body, parametrized by the entry `func_id`.
    ///
    /// [`Self::run`] passes the machine's original entry; [`Self::run_fragment`]
    /// passes an [`Self::add_function`]-minted fragment id. The lifecycle is
    /// identical either way (session vmctx, reclaim arming, effect loop).
    fn run_with_entry<U, H: DispatchEffect<U>>(
        &mut self,
        func_id: FuncId,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
    ) -> Result<Value, JitError> {
        Ok(self
            .with_active_run(
                func_id,
                RunTarget::Effectful {
                    table,
                    handlers,
                    user,
                },
                ResultMaterialization::Value,
                "run/run_fragment called while a continuation is suspended — \
                 resume_suspended it first",
                "stepping main function",
                "",
            )?
            .expect_value())
    }

    // ----------------------------------------------------------------------
    // Threadless suspension at the ask boundary.
    // ----------------------------------------------------------------------

    /// Drive an effectful turn until it COMPLETES or SUSPENDS at `suspend_tag`
    /// (the `Ask` union tag). Additive sibling of [`Self::run`]: a turn that
    /// never reaches `suspend_tag` drives byte-identically — the effect loop's
    /// suspend branch is simply never taken (see [`drive_effect_loop`]).
    ///
    /// On suspension the machine's heap is retained through the session
    /// machinery (`RegistryGuard::drop` → `reclaim_session_heap`) and the
    /// continuation is stowed inside `self`; the whole `JitEffectMachine` can
    /// then be moved off this thread and parked as data. Call
    /// [`Self::resume_suspended`] with the answer to continue on ANY thread.
    ///
    /// # Panics
    /// Panics on a non-session machine — heap retention across the suspension
    /// requires [`Self::compile_session`].
    pub fn run_suspendable<U, H: DispatchEffect<U>>(
        &mut self,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
        suspend_tag: u64,
    ) -> Result<SuspendableOutcome, JitError> {
        let func_id = self.func_id;
        self.run_suspendable_with_entry(
            func_id,
            table,
            handlers,
            user,
            suspend_tag,
            ResultMaterialization::Value,
        )
        .map(ParkedRaw::into_suspendable)
    }

    /// Suspend-capable sibling of [`Self::run_fragment`]: drive an
    /// [`Self::add_function`]-minted fragment through the same threadless
    /// suspend path [`Self::run_suspendable`] uses for the machine's original
    /// entry. A fragment that reaches `suspend_tag` (an `Ask`) mid-computation
    /// stows its continuation on `self` exactly as the entry path does; the
    /// binding it was computing lands on [`Self::resume_suspended`] to
    /// completion. This is the composition of the fragment plane (C2 session
    /// re-entry) with E2 threadless suspension — same shared
    /// [`drive_effect_loop`], only the entry `func_id` differs.
    ///
    /// # Panics
    /// Panics on a non-session machine — heap retention across the suspension
    /// requires [`Self::compile_session`].
    pub fn run_fragment_suspendable<U, H: DispatchEffect<U>>(
        &mut self,
        func_id: FuncId,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
        suspend_tag: u64,
    ) -> Result<SuspendableOutcome, JitError> {
        self.run_suspendable_with_entry(
            func_id,
            table,
            handlers,
            user,
            suspend_tag,
            ResultMaterialization::Value,
        )
        .map(ParkedRaw::into_suspendable)
    }

    /// Value-plane BIND sibling of [`Self::run_fragment_suspendable`]: drive a
    /// bind fragment (`x <- e`) through the same threadless suspend path, and — on
    /// `Done` — tenure the result into old-space, stashing its [`RootSlot`] on the
    /// machine (read via [`Self::take_last_bound_root`] after the machine moves off
    /// the eval thread). `forced` deep-forces the result to NF first (Tier0 data)
    /// vs tenuring a Tier1 closure as-is. A fork bind SUSPENDS here (no tenure yet);
    /// its value is bound on the eventual [`Self::resume_suspended_binding`].
    pub fn run_fragment_suspendable_binding<U, H: DispatchEffect<U>>(
        &mut self,
        func_id: FuncId,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
        suspend_tag: u64,
        forced: bool,
    ) -> Result<SuspendableOutcome, JitError> {
        self.run_suspendable_with_entry(
            func_id,
            table,
            handlers,
            user,
            suspend_tag,
            ResultMaterialization::Bind { forced },
        )
        .map(ParkedRaw::into_suspendable)
    }

    /// MULTI-binder sibling of [`Self::run_fragment_suspendable_binding`], and
    /// the suspendable sibling of [`Self::run_fragment_and_bind_projected`]:
    /// drive a multi-bind fragment (`(a, b) <- e`) through the threadless
    /// suspend path, and — on `Done` — deep-force the WHOLE result tuple and
    /// tenure each of its `n_fields` fields into old-space.
    ///
    /// Completion IS the tenured roots, in field order: the caller zips them
    /// with its binder metadata. A projection has no result value of its own —
    /// no bridge of the bound fields (which would import the bridge's
    /// depth/size failure modes into a path that cannot fail after a successful
    /// tenure), and nothing invented to fill a `Value`-shaped hole either,
    /// because [`Suspendable`] is generic over what the policy produces. A turn
    /// that suspends at an ask tenures NOTHING yet — its fields are bound on the
    /// eventual [`Self::resume_suspended_projected`].
    ///
    /// # Panics
    /// Panics on a non-session machine, on `n_fields == 0` (same precondition
    /// as [`Self::run_fragment_and_bind_projected`]), or if a continuation is
    /// already suspended.
    pub fn run_fragment_suspendable_projected<U, H: DispatchEffect<U>>(
        &mut self,
        func_id: FuncId,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
        suspend_tag: u64,
        n_fields: usize,
    ) -> Result<Suspendable<Vec<crate::old_space::RootSlot>>, JitError> {
        #[allow(
            clippy::expect_used,
            reason = "run_fragment_suspendable_projected requires at least one field"
        )]
        let n_fields = NonZeroUsize::new(n_fields)
            .expect("run_fragment_suspendable_projected requires at least one field");
        self.run_suspendable_with_entry(
            func_id,
            table,
            handlers,
            user,
            suspend_tag,
            ResultMaterialization::Project { n_fields },
        )
        .map(ParkedRaw::into_projected)
    }

    /// BIND-AND-RENDER sibling of [`Self::run_fragment_suspendable_binding`],
    /// and the suspendable sibling of [`Self::run_fragment_and_bind_render`]:
    /// drive the repl's bare-expression fragment — a wrapped
    /// `pure (it, toWire it)` — through the threadless suspend path, binding
    /// field 0 and rendering field 1 in ONE run. Completion carries BOTH
    /// products together — field 0's tenured root and field 1's render — the
    /// same pair `run_fragment_and_bind_render` returns directly.
    ///
    /// `field0_forced` mirrors the bind flag: `true` (Tier0 data) deep-forces
    /// field 0 to NF before tenuring, `false` (Tier1 closure) tenures as-is.
    ///
    /// The aliasing discipline — bridge field 1 BEFORE tenuring field 0,
    /// because an identity `toWire` makes them the SAME heap object and the
    /// bridge is the owned deep copy that survives the tenure — is not
    /// restated here: this route reaches the one copy of it inside
    /// [`Self::materialize`], so it holds identically whether the turn
    /// completed in its first run or after any number of suspensions.
    ///
    /// # Panics
    /// Panics on a non-session machine, or if a continuation is already
    /// suspended.
    pub fn run_fragment_suspendable_render<U, H: DispatchEffect<U>>(
        &mut self,
        func_id: FuncId,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
        suspend_tag: u64,
        field0_forced: bool,
    ) -> Result<Suspendable<(crate::old_space::RootSlot, Value)>, JitError> {
        self.run_suspendable_with_entry(
            func_id,
            table,
            handlers,
            user,
            suspend_tag,
            ResultMaterialization::Render { field0_forced },
        )
        .map(ParkedRaw::into_render)
    }

    /// Shared suspend-capable run body, parametrized by the entry `func_id`.
    /// [`Self::run_suspendable`] passes the machine's original entry;
    /// [`Self::run_fragment_suspendable`] passes an [`Self::add_function`]-minted
    /// fragment id. The lifecycle is identical either way (session vmctx, reclaim
    /// arming, suspend-capable effect loop).
    fn run_suspendable_with_entry<U, H: DispatchEffect<U>>(
        &mut self,
        func_id: FuncId,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
        suspend_tag: u64,
        materialization: ResultMaterialization,
    ) -> Result<ParkedRaw, JitError> {
        self.run_suspendable_shared(
            func_id,
            table,
            handlers,
            user,
            suspend_tag,
            materialization,
            ParkTarget::Slot,
        )
    }

    /// Shared suspendable run body for BOTH suspension paths, parametrized by
    /// the entry `func_id` and by `park` — where a suspension puts its
    /// continuation (the single `suspended_continuation` slot, or the
    /// continuation registry). Everything before the epilogue is identical, so
    /// the pre-existing entries stay byte-identical to the pre-registry body.
    #[allow(clippy::too_many_arguments)]
    fn run_suspendable_shared<U, H: DispatchEffect<U>>(
        &mut self,
        func_id: FuncId,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
        suspend_tag: u64,
        materialization: ResultMaterialization,
        park: ParkTarget,
    ) -> Result<ParkedRaw, JitError> {
        assert!(
            self.session.is_some(),
            "run_suspendable requires a session machine (compile_session)"
        );
        // L7: see run_with_entry's doc.
        assert!(
            self.suspended_continuation.is_none(),
            "run_suspendable called while a continuation is already suspended — \
             resume_suspended it first"
        );
        // Guard (plans/post-restart/codex-review-2026-08-08.md item 11): the
        // slot-vs-registry exclusion — no machine holds both a slot
        // continuation and parked ones — is enforced here. The sibling
        // direction (a parked-path entry while the SLOT is occupied) is
        // already caught by the `suspended_continuation.is_none()` assert
        // above, since every parked-path entry funnels through this same
        // method too.
        //
        // HAZARD: without this check, parking a realm and then calling a
        // legacy slot-path entry reaches a mixed state that isn't caught
        // until `resume_parked` panics later, after running with the
        // slot-held continuation unrooted in the meantime. Reject it HERE
        // instead, before anything is driven.
        //
        // A clean `Err`, deliberately NOT a panic/assert: parking a realm and
        // then calling a legacy entry is ordinary caller misuse (a plausible
        // sequencing mistake, not a violated internal invariant), and it must
        // reject the same way in every build, debug or release — a
        // `debug_assert!` here would panic before this Err is ever reached in
        // the debug builds tests run under, making the "clean rejection" this
        // guard exists to provide untestable and unreachable in practice.
        if let ParkTarget::Slot = park {
            if !self.continuations.is_empty() {
                return Err(JitError::Effect(EffectError::Handler(format!(
                    "run_suspendable/run_fragment_suspendable* called while {} \
                     continuation(s) are parked in the registry — the slot path and the \
                     registry path must not mix (a slot-held continuation is unrooted; \
                     parked-path activity can collect while this ran). Use the parked \
                     path (run_suspendable_parked / run_fragment_suspendable_parked) \
                     instead.",
                    self.continuations.len()
                ))));
            }
        }
        let tags = self.tags.map_err(JitError::MissingConTags)?;
        crate::signal_safety::install();
        // The parked path installs THAT realm's cancel flag (lazily minted)
        // instead of the machine-level one; the slot path installs
        // `self.cancel_flag`.
        let park_cancel_flag: Arc<AtomicBool> = match park {
            ParkTarget::Slot => self.cancel_flag.clone(),
            ParkTarget::Registry { realm, .. } => self.realm_cancel_flag(realm),
        };
        let mut _guard = self.install_registries_with_cancel_flag(park_cancel_flag.clone());
        // SAFETY: finalized JIT code pointer; calling convention per contract.
        let func_ptr: unsafe extern "C" fn(*mut VMContext) -> *mut u8 =
            unsafe { std::mem::transmute(self.pipeline.get_function_ptr(func_id)) };
        let vmctx = self.make_session_vmctx();
        let mut machine = CompiledEffectMachine::new(func_ptr, vmctx, tags);
        // SAFETY: machine_state outlives this run (owned by self).
        machine.vmctx_mut().machine_state = &mut self.machine_state as *mut MachineState;
        // Reclaim is armed LAST (after finish_suspendable), NOT here: every
        // materialization policy except `Value` tenures into self.session
        // (inside `materialize`), and arm_reclaim's stored *mut self.session
        // would alias it — the same arm-last ordering `with_active_run` owns
        // for the plain routes. Safe for the `Value` case too — nothing touches
        // self.session before the arm, and the arm runs unconditionally (even on
        // a run error) so the guard still reclaims the session buffer on drop.
        let yield_result = initial_step(&mut machine, "stepping main function");
        let finished = match drive_effect_loop(
            &mut machine,
            &park_cancel_flag,
            table,
            handlers,
            user,
            "",
            Some(suspend_tag),
            yield_result,
        ) {
            Ok(outcome) => self.finish_suspendable(
                &mut machine,
                outcome,
                materialization,
                park,
                suspend_tag,
                table,
                park_cancel_flag,
            ),
            Err(e) => Err(e),
        };
        // SAFETY: machine.vmctx_mut() points into `machine` on this frame;
        // CompiledEffectMachine has no custom Drop so the bytes are valid when
        // _guard drops (machine drops first but the frame is still live). The
        // guard's reclaim reads the post-run buffer/cursor back into self.session.
        unsafe {
            _guard.arm_reclaim(&mut self.session as *mut _, machine.vmctx_mut() as *const _);
        }
        finished
    }

    /// Re-enter a turn suspended by [`Self::run_suspendable`], feeding the
    /// (already schema-validated, bridged) answer — or an abort — into the
    /// stowed ask and driving to the next suspension or completion.
    ///
    /// Runs on ANY thread: [`Self::install_registries`] re-installs this
    /// machine's per-thread reach (`CURRENT_MACHINE`, stack-map/lambda
    /// registry, cancel flag) and re-points the GC state at the RETAINED
    /// session heap. It must NOT reset the nursery — the session heap-retention
    /// path (heap `Some` → `install_session_buffer`, or `None` → nursery at the
    /// preserved cursor) preserves the mid-ask heap; a nursery reset would
    /// silently discard it.
    ///
    /// Errors if the machine is not currently suspended.
    pub fn resume_suspended<U, H: DispatchEffect<U>>(
        &mut self,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
        suspend_tag: u64,
        input: ResumeInput,
    ) -> Result<SuspendableOutcome, JitError> {
        self.resume_suspended_inner(
            table,
            handlers,
            user,
            suspend_tag,
            input,
            ResultMaterialization::Value,
        )
        .map(ParkedRaw::into_suspendable)
    }

    /// Value-plane BIND sibling of [`Self::resume_suspended`]: re-enter a suspended
    /// bind turn (`x <- e` that stowed at a fork) and, on `Done`, tenure the bound
    /// result — stashing its [`RootSlot`] on the machine
    /// ([`Self::take_last_bound_root`]). `forced` deep-forces to NF (Tier0) vs
    /// tenuring a Tier1 closure as-is.
    pub fn resume_suspended_binding<U, H: DispatchEffect<U>>(
        &mut self,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
        suspend_tag: u64,
        input: ResumeInput,
        forced: bool,
    ) -> Result<SuspendableOutcome, JitError> {
        self.resume_suspended_inner(
            table,
            handlers,
            user,
            suspend_tag,
            input,
            ResultMaterialization::Bind { forced },
        )
        .map(ParkedRaw::into_suspendable)
    }

    /// MULTI-binder sibling of [`Self::resume_suspended_binding`]: re-enter a
    /// suspended multi-bind turn (`(a, b) <- e` that stowed at an ask) and, on
    /// `Done`, deep-force the whole result tuple and tenure each of its
    /// `n_fields` fields — completing with those roots. `n_fields` is supplied
    /// by the caller (which is holding the binder metadata across the
    /// suspension), exactly as `forced` is on the binding resume. See
    /// [`Self::run_fragment_suspendable_projected`] for why the completion
    /// carries slots rather than a value.
    ///
    /// # Panics
    /// Panics on `n_fields == 0`. Errors (does not panic) if the machine is
    /// not currently suspended.
    pub fn resume_suspended_projected<U, H: DispatchEffect<U>>(
        &mut self,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
        suspend_tag: u64,
        input: ResumeInput,
        n_fields: usize,
    ) -> Result<Suspendable<Vec<crate::old_space::RootSlot>>, JitError> {
        #[allow(
            clippy::expect_used,
            reason = "resume_suspended_projected requires at least one field"
        )]
        let n_fields = NonZeroUsize::new(n_fields)
            .expect("resume_suspended_projected requires at least one field");
        self.resume_suspended_inner(
            table,
            handlers,
            user,
            suspend_tag,
            input,
            ResultMaterialization::Project { n_fields },
        )
        .map(ParkedRaw::into_projected)
    }

    /// BIND-AND-RENDER sibling of [`Self::resume_suspended_binding`]: re-enter
    /// a suspended bare-expression turn (`pure (it, toWire it)` that stowed at
    /// an ask) and, on `Done`, complete with field 0's tenured root paired with
    /// field 1's bridged render. The field1-before-field0-tenure ordering that
    /// makes an aliased `(it, toWire it)` safe lives in [`Self::materialize`]
    /// and is therefore identical on this path and on the non-suspending
    /// [`Self::run_fragment_and_bind_render`].
    pub fn resume_suspended_render<U, H: DispatchEffect<U>>(
        &mut self,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
        suspend_tag: u64,
        input: ResumeInput,
        field0_forced: bool,
    ) -> Result<Suspendable<(crate::old_space::RootSlot, Value)>, JitError> {
        self.resume_suspended_inner(
            table,
            handlers,
            user,
            suspend_tag,
            input,
            ResultMaterialization::Render { field0_forced },
        )
        .map(ParkedRaw::into_render)
    }

    /// Shared body of every slot-path resume, parametrized by the
    /// [`ResultMaterialization`] the completing turn's policy calls for —
    /// `Value` (plain), `Bind`, `Project`, or `Render`.
    fn resume_suspended_inner<U, H: DispatchEffect<U>>(
        &mut self,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
        suspend_tag: u64,
        input: ResumeInput,
        materialization: ResultMaterialization,
    ) -> Result<ParkedRaw, JitError> {
        // PEEK the continuation — do NOT consume it yet. The A5 NF-force
        // rejects a bottom-bearing answer WITHOUT consuming the continuation,
        // so the caller can retry with a corrected answer; only after the
        // answer is verified NF do we `.take()` (below).
        if self.suspended_continuation.is_none() {
            return Err(JitError::Effect(EffectError::Handler(
                "resume_suspended called on a machine that is not suspended".into(),
            )));
        }
        // Defense-in-depth companion to the entry guard in
        // `run_suspendable_shared` (codex-review-2026-08-08.md item 11): by
        // construction, the registry can only gain entries while the slot is
        // empty (the same shared method's L7 assert), so reaching a slot
        // resume with the registry non-empty should be unreachable. Not a
        // caller-facing error path — a caller cannot trigger this from the
        // public API — so `debug_assert!`, not a typed `Err`, matching
        // `assert_rooting_receipt`'s discipline for an internal invariant.
        debug_assert!(
            self.continuations.is_empty(),
            "slot resume reached with {} realm(s) parked in the registry — \
             the entry guard in run_suspendable_shared should have prevented this",
            self.continuations.len()
        );
        // A5 — NF-force the data-kinded answer BEFORE consuming the
        // continuation. A bottom anywhere in the answer (a residual unforced
        // thunk — an `undefined`/`⊥` the child-answer bridge would have raised,
        // caught here as defense-in-depth) fails the answer as a retryable
        // error and leaves `suspended_continuation` intact. Function-bearing
        // answer types are rejected at extract, so every field of a
        // data-kinded answer is walkable by construction; the walk terminates
        // on a visited-set (cyclic data).
        if let ResumeInput::Answer(val) = &input {
            answer_force_nf(val).map_err(|reason| {
                JitError::Effect(EffectError::Handler(format!(
                    "resume answer is not in normal form (bottom in the answer): {reason}"
                )))
            })?;
        }
        // Answer verified NF (or this is an Abort) — NOW consume the
        // continuation. Every early return above left it stowed.
        #[allow(
            clippy::expect_used,
            reason = "suspended_continuation present (checked is_some above)"
        )]
        let continuation = self
            .suspended_continuation
            .take()
            .expect("suspended_continuation present (checked is_some above)");
        let flag = self.cancel_flag.clone();
        self.resume_applied(
            continuation,
            table,
            handlers,
            user,
            suspend_tag,
            input,
            materialization,
            ParkTarget::Slot,
            flag,
        )
    }

    /// Apply an already-acquired continuation to a resume `input` and drive to
    /// the next suspension or completion. Shared by BOTH resume paths: the
    /// single-slot [`Self::resume_suspended_inner`] (which `take()`s the slot)
    /// and the registry [`Self::resume_parked`] (which removes the frame and
    /// deregisters its root). Both callers have already run the A5 NF-force, so
    /// by the time control reaches here the continuation is committed.
    #[allow(clippy::too_many_arguments)]
    fn resume_applied<U, H: DispatchEffect<U>>(
        &mut self,
        continuation: *mut u8,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
        suspend_tag: u64,
        input: ResumeInput,
        materialization: ResultMaterialization,
        park: ParkTarget,
        // A3: the flag to install for THIS run — `self.cancel_flag` for the
        // slot path, or (`resume_parked`'s) already-cloned
        // `ContinuationFrame::cancel_flag` for the registry path, so a
        // resume never does a second `realm_cancel_flags` lookup.
        cancel_flag: Arc<AtomicBool>,
    ) -> Result<ParkedRaw, JitError> {
        let tags = self.tags.map_err(JitError::MissingConTags)?;
        crate::signal_safety::install();
        // Re-points GC state at the retained heap (heap `Some` → session buffer,
        // else nursery at the preserved cursor) — NOT a nursery reset.
        let mut _guard = self.install_registries_with_cancel_flag(cancel_flag.clone());
        // SAFETY: finalized JIT code pointer. The entry func is not re-called on
        // resume (the continuation is applied via `machine.resume`), but
        // CompiledEffectMachine needs a func_ptr for its own tail-call resolution.
        let func_ptr: unsafe extern "C" fn(*mut VMContext) -> *mut u8 =
            unsafe { std::mem::transmute(self.pipeline.get_function_ptr(self.func_id)) };
        let vmctx = self.make_session_vmctx();
        let mut machine = CompiledEffectMachine::new(func_ptr, vmctx, tags);
        // SAFETY: machine_state outlives this run (owned by self).
        machine.vmctx_mut().machine_state = &mut self.machine_state as *mut MachineState;
        // Reclaim is armed LAST (after finish_suspendable), NOT here — see
        // run_suspendable_shared: a Bind/Project/Render finish tenures into
        // self.session (inside `materialize`) and arm_reclaim's *mut
        // self.session would alias it. The abort branch arms explicitly before
        // it returns (it never reaches the tail arm below).

        let payload = match input {
            ResumeInput::Answer(val) => {
                ResumePayload::Response(tidepool_effect::Response::Complete(val))
            }
            ResumeInput::Handle(h) => match self.value_handles.get(&h.0) {
                // SAFETY: the slot is persistent-rooted until released, and a
                // released handle is absent from the map — so `current()`
                // reads the GC-current pointer of a still-rooted value.
                Some(entry) => ResumePayload::HeapPtr(unsafe { entry.slot.current() }),
                None => {
                    // Same discipline as the Abort arm: nothing has run, but
                    // the guard must still be armed so the session buffer is
                    // restored on this early return.
                    unsafe {
                        _guard.arm_reclaim(
                            &mut self.session as *mut _,
                            machine.vmctx_mut() as *const _,
                        );
                    }
                    return Err(JitError::Effect(EffectError::Handler(format!(
                        "resume: unknown or released ValueHandle({})",
                        h.0
                    ))));
                }
            },
            ResumeInput::Abort(reason) => {
                // A stowed machine has no thread. We do NOT run the
                // continuation — the ask itself fails, returning
                // `EffectError::Handler("ask aborted by caller: …")` from the
                // dispatcher (no `Cancelled` first cause — that is only the
                // gate/timeout abort). `install_registries` above already
                // installed this machine as `CURRENT_MACHINE`, so any
                // machine-scoped state stays reachable, but this early return
                // surfaces the error directly without touching the first-cause
                // cell.
                // Abort does not run the continuation, so it never reaches the
                // tail arm; arm+drop the guard here to restore the session buffer.
                unsafe {
                    _guard
                        .arm_reclaim(&mut self.session as *mut _, machine.vmctx_mut() as *const _);
                }
                return Err(JitError::Effect(EffectError::Handler(format!(
                    "ask aborted by caller: {reason}"
                ))));
            }
        };

        // Feed the answer as a Complete response through the SAME materialization
        // + resume path the effect loop uses, then continue driving. Capture the
        // result WITHOUT `?` so the tail arm runs on every path (a bind finish
        // tenures into self.session, so the arm must follow finish_suspendable).
        let finished = match materialize_response_and_resume(
            &mut machine,
            continuation,
            payload,
            suspend_tag,
            "",
        ) {
            Ok(yield_result) => match drive_effect_loop(
                &mut machine,
                &cancel_flag,
                table,
                handlers,
                user,
                "",
                Some(suspend_tag),
                yield_result,
            ) {
                Ok(outcome) => self.finish_suspendable(
                    &mut machine,
                    outcome,
                    materialization,
                    park,
                    suspend_tag,
                    table,
                    cancel_flag,
                ),
                Err(e) => Err(e),
            },
            Err(e) => Err(e),
        };
        // SAFETY: machine.vmctx_mut() points into `machine` on this frame; the
        // guard's reclaim reads the post-run buffer/cursor into self.session.
        unsafe {
            _guard.arm_reclaim(&mut self.session as *mut _, machine.vmctx_mut() as *const _);
        }
        finished
    }

    /// Shared epilogue for the suspendable path: materialize a `Done` pointer
    /// under `materialization` (via the same [`Self::materialize`] the plain
    /// routes use — see the completion-policy table in the module docs), or
    /// stow the continuation on `self` and surface the suspension.
    ///
    /// What stays HERE, because it is the suspendable path's own concern, in
    /// this order:
    ///
    /// 1. the machine-level stash — on the SLOT path a `Bind`'s tenured root
    ///    goes to `self.last_bound_root`, because
    ///    [`SuspendableOutcome::Completed`] is fixed at a bare `Value` and
    ///    cannot carry it; on the REGISTRY path it is returned INLINE via
    ///    `ParkedOutcome::CompletedBinding::root` instead, so that two realms'
    ///    binds completing before either is drained cannot overwrite one
    ///    machine-level slot. `Project`/`Render` never stash: they complete
    ///    as `Suspendable<T>` and return their roots in the outcome. `Bind`
    ///    is slated to join them once `SuspendableOutcome` is widened;
    /// 2. the bridge that turns a tenured root into the `Value` a
    ///    [`SuspendableOutcome::Completed`] must carry — for `Bind`, that is
    ///    the bridge of `slot.current()` (the TENURED, rooted pointer, not
    ///    `done_ptr`). `Render` needs no bridge here (field 1 was already
    ///    bridged inside `materialize`) and `Project` needs none at all.
    ///
    /// ORDERING: [`Self::materialize`] touches `self.session` via `tenure`, so
    /// a caller MUST NOT have armed reclaim before this call — the guard's
    /// stored `*mut self.session` would alias it. Both callers
    /// (`run_suspendable_shared`, `resume_applied`) arm strictly AFTER this
    /// returns, on every exit path.
    ///
    /// `park` selects where a SUSPENSION puts its continuation: the single
    /// `suspended_continuation` slot ([`ParkTarget::Slot`], every pre-existing
    /// entry) or the continuation registry ([`ParkTarget::Registry`]). `table`
    /// and `park_cancel_flag` are only consulted on the registry path, to
    /// populate the newly-parked [`ContinuationFrame`] (A3/A4) — the slot path
    /// ignores both, unchanged. Enforced constraint 1 (realm-lanes/B-prefix-
    /// compat) is checked and established at ENTRY to the parked path
    /// ([`Self::enter_parked_path`], called from the public
    /// `run_fragment_suspendable_parked`/`resume_parked` entries) — by the
    /// time this method runs, that check has already passed, regardless of
    /// whether the turn is about to complete or suspend.
    #[allow(clippy::too_many_arguments)]
    fn finish_suspendable(
        &mut self,
        machine: &mut CompiledEffectMachine,
        outcome: DriveOutcome,
        materialization: ResultMaterialization,
        park: ParkTarget,
        suspend_tag: u64,
        table: &DataConTable,
        park_cancel_flag: Arc<AtomicBool>,
    ) -> Result<ParkedRaw, JitError> {
        match outcome {
            DriveOutcome::Done(done_ptr) => {
                // ONE epilogue for both families. `materialize` owns the null/
                // runtime-error checks, the optional deep-force, the tenure,
                // and (for Render) the load-bearing bridge-field1-BEFORE-
                // tenure-field0 ordering — the suspendable path no longer
                // re-states any of it. Reclaim is armed by the caller strictly
                // after this returns, because the tenure inside touches
                // `self.session`.
                match self.materialize(machine.vmctx_mut(), done_ptr, materialization)? {
                    MaterializeResult::Value(value) => {
                        Ok(ParkedRaw::Completed(ParkedOutcome::CompletedValue(value)))
                    }
                    MaterializeResult::Bind(slot) => {
                        // Laundering a `!Send` `RootSlot` across the eval-thread
                        // boundary — see the module docstring's `RootSlot: !Send`
                        // contract for why. Only the SLOT path stashes here; the
                        // registry path returns its root inline in
                        // `ParkedOutcome::CompletedBinding::root` instead.
                        if let ParkTarget::Slot = park {
                            self.last_bound_root = Some(slot);
                        }
                        // Bridge the TENURED (rooted, stable) value for the
                        // turn's rendered result — never `done_ptr`, which
                        // tenure has just forwarded. SAFETY: slot.current() is
                        // the live old-space pointer; forcing is a no-op on the
                        // already-NF Tier0 case. TOLERANT, not strict: a Tier1
                        // bind (tenured as-is, never forced — a bare closure,
                        // OR any Tier0-shaped type that merely CONTAINS one,
                        // e.g. a record with a function field, PRD 21 lane
                        // C1's mounted-value shape) has a real `TAG_CLOSURE`
                        // reachable here, which the strict bridge rejects. The
                        // same substitution `finalize`'s closure path already
                        // uses for its rendered value: the REAL value stays
                        // live at `slot` regardless (that root is what
                        // `MaterializeResult::Bind`'s caller actually resolves a
                        // later reference through), this bridge only needs to
                        // produce SOMETHING renderable. Strictly a superset of
                        // the old behavior — a Tier0 value never reaches a
                        // `TAG_CLOSURE` (nothing left to substitute), so this
                        // is a no-op there.
                        let vmctx_ptr = machine.vmctx_mut() as *mut VMContext;
                        let bridge_res = unsafe {
                            let live = slot.current();
                            crate::signal_safety::with_signal_protection(|| {
                                heap_bridge::heap_to_value_forcing_tolerant(live, vmctx_ptr)
                            })
                        }
                        .map_err(JitError::Signal)?;
                        let value = crate::host_fns::surface_error(
                            bridge_res.map_err(JitError::HeapBridge),
                        )?;
                        Ok(ParkedRaw::Completed(ParkedOutcome::CompletedBinding {
                            value,
                            root: slot,
                        }))
                    }
                    MaterializeResult::Project(slots) => {
                        // The slots ARE the products; they ride out in the
                        // completion on BOTH paths (slot: `Suspendable<T>`;
                        // registry: `ParkedOutcome::CompletedProject`), so
                        // nothing is stashed and nothing has to be taken back
                        // off the machine.
                        Ok(ParkedRaw::Completed(ParkedOutcome::CompletedProject {
                            roots: slots,
                        }))
                    }
                    MaterializeResult::Render(slot, rendered) => {
                        // No second bridge: `rendered` IS field 1, already
                        // bridged inside `materialize` BEFORE field 0 was
                        // tenured (the aliasing-safe ordering). Both products
                        // ride out in the completion on both paths.
                        Ok(ParkedRaw::Completed(ParkedOutcome::CompletedRender {
                            root: slot,
                            rendered,
                        }))
                    }
                }
            }
            DriveOutcome::Suspended {
                request,
                request_ptr,
                continuation,
            } => {
                // Finalize-by-reference: when the bridged request carries a
                // CLOSURE_SENTINEL placeholder, its value field (field 1 of the
                // request Con) is a live closure with no data representation.
                // Tenure it into old-space NOW — while we still hold the run's
                // active GC range and a valid vmctx — so it survives any later
                // child GC as a persistent root, and hand the slot up so the
                // harness can apply it by reference via `run_child`.
                let has_finalized_closure = request_carries_closure_sentinel(&request);
                // On the slot path the tenured slot goes to
                // `self.suspended_finalized_root` (only written when
                // `has_finalized_closure`, so an unrelated suspension never
                // clobbers a stale value there); on the registry path it
                // rides to `park_continuation` instead and lands on the
                // frame — never on a machine-level field a second realm
                // could overwrite.
                let mut parked_finalized_root = None;
                // `continuation` is a raw heap pointer used AFTER this block
                // (stored below, or handed to `park_continuation`).
                // `tenure_finalized_payload` now folds a real minor
                // collection into its own tenure call (see
                // `OldSpace::tenure`'s doc) to fix up sibling references, so
                // it can relocate other live nursery objects — root
                // `continuation` across it exactly like `field0_ptr` is
                // rooted across the field1 bridge in `materialize`'s Render
                // arm.
                let mut continuation = continuation;
                if has_finalized_closure {
                    let vmctx_ptr = machine.vmctx_mut() as *mut VMContext;
                    // SAFETY: vmctx_ptr is the active run's VMContext; the
                    // scope covers exactly the tenure call below.
                    let _root_cont = unsafe { heap_bridge::RootScope::new(vmctx_ptr) };
                    // SAFETY: the slot lives on this frame until _root_cont drops.
                    unsafe {
                        crate::host_fns::register_rust_root(
                            vmctx_ptr,
                            &mut continuation as *mut *mut u8,
                        );
                    }
                    let slot = self.tenure_finalized_payload(machine, request_ptr)?;
                    match park {
                        ParkTarget::Slot => self.suspended_finalized_root = Some(slot),
                        ParkTarget::Registry { .. } => parked_finalized_root = Some(slot),
                    }
                }
                let id = match park {
                    ParkTarget::Slot => {
                        self.suspended_continuation = Some(continuation);
                        None
                    }
                    ParkTarget::Registry {
                        realm,
                        kind,
                        handled_prefix,
                    } => Some(self.park_continuation(
                        continuation,
                        realm,
                        kind,
                        suspend_tag,
                        park_cancel_flag,
                        Arc::new(table.clone()),
                        parked_finalized_root,
                        handled_prefix,
                    )),
                };
                Ok(ParkedRaw::Suspended {
                    request,
                    has_finalized_closure,
                    id,
                })
            }
        }
    }

    /// Tenure the finalized VALUE (field 1) out of a suspended `finalize @T x`
    /// request Con into old-space, returning its persistent GC root slot.
    /// The finalized value stays LIVE in the session heap (never deep-forced to
    /// data) and is applied later by reference. Runs during the suspending turn,
    /// so `gc_active_range`/`vmctx` are valid.
    fn tenure_finalized_payload(
        &mut self,
        machine: &mut CompiledEffectMachine,
        request_ptr: *mut u8,
    ) -> Result<crate::old_space::RootSlot, JitError> {
        if request_ptr.is_null() {
            return Err(JitError::Yield(crate::yield_type::YieldError::NullPointer));
        }
        // FinalizeWith(site, value): the value is field index 1. Read its pointer
        // out of the (WHNF Con) request. SAFETY: request_ptr is the rooted request
        // Con from the suspend arm; a `FinalizeWith` always has >= 2 fields.
        let value_ptr = unsafe {
            let nf = *(request_ptr.add(crate::layout::CON_NUM_FIELDS_OFFSET as usize) as *const u16)
                as usize;
            if nf < 2 {
                return Err(JitError::Yield(crate::yield_type::YieldError::Runtime(
                    crate::host_fns::RuntimeError::UserErrorMsg(format!(
                        "finalize request Con has {nf} fields, expected >= 2 (FinalizeWith site value)"
                    )),
                )));
            }
            *(request_ptr.add(crate::layout::CON_FIELDS_OFFSET as usize + 8) as *const *mut u8)
        };
        #[allow(
            clippy::expect_used,
            reason = "GC state installed for the suspending finalize run"
        )]
        let from = self
            .machine_state
            .gc_active_range()
            .expect("GC state installed for the suspending finalize run");
        let from_range = (from.0 as *const u8, unsafe {
            from.0.add(from.1) as *const u8
        });
        let vmctx_ptr = machine.vmctx_mut() as *mut VMContext;
        // SAFETY: value_ptr is a live heap object in the nursery from-range; tenure
        // evacuates it into old-space and registers the returned slot as a
        // persistent root valid for the machine's life. `self.session` is
        // unaliased (reclaim not yet armed on the suspend path).
        let slot = unsafe {
            #[allow(clippy::expect_used, reason = "session machine for a finalize tenure")]
            self.session
                .as_mut()
                .expect("session machine for a finalize tenure")
                .old_space
                .tenure(vmctx_ptr, value_ptr, from_range)
        };
        Ok(slot)
    }

    /// Take the [`RootSlot`] a value-plane bind tenured on its last suspendable
    /// completion (`run_fragment_suspendable_binding`/`resume_suspended_binding`),
    /// clearing it. `None` if the last run was not a bind or has already been
    /// taken. The caller reads this AFTER the machine moves back off the eval
    /// thread and records the `BindingEntry` against it.
    pub fn take_last_bound_root(&mut self) -> Option<crate::old_space::RootSlot> {
        self.last_bound_root.take()
    }

    /// Take the persistent root slot of a suspended `finalize @T closure`'s
    /// finalized VALUE (finalize-by-reference), tenured at suspend time. The
    /// slot stays a registered persistent root for the machine's life (taking
    /// it here only removes the machine's own handle, not the registration), so
    /// a subsequent `run_child` that references it by slot address is GC-safe.
    /// `None` unless the machine suspended on a closure-valued finalize.
    pub fn take_finalized_root(&mut self) -> Option<crate::old_space::RootSlot> {
        self.suspended_finalized_root.take()
    }

    /// Run a pure (non-effectful) program to completion.
    ///
    /// Skips freer-simple effect dispatch entirely — calls the compiled function
    /// and converts the raw heap result directly to a Value. Use this for programs
    /// that don't use an `Eff` wrapper.
    pub fn run_pure(&mut self) -> Result<Value, JitError> {
        let func_id = self.func_id;
        self.run_pure_with_entry(func_id)
    }

    /// Shared pure-run body, parametrized by the entry `func_id`. [`Self::run_pure`]
    /// uses the machine's original entry; [`Self::run_fragment_pure`] passes an
    /// [`Self::add_function`]-minted fragment id. Same session lifecycle either way.
    fn run_pure_with_entry(&mut self, func_id: FuncId) -> Result<Value, JitError> {
        Ok(self
            .with_active_run::<(), NoHandlers>(
                func_id,
                RunTarget::Pure,
                ResultMaterialization::Value,
                "run_pure/run_fragment_pure called while a continuation is suspended — \
                 resume_suspended it first",
                "running pure computation",
                "",
            )?
            .expect_value())
    }

    // ----------------------------------------------------------------------
    // GHCi-style session re-entry.
    //
    // These freeze the codegen contracts the tidepool-repl session manager
    // builds on, atop the session lifecycle seam (buffer retention,
    // persistent roots, tenuring).
    // ----------------------------------------------------------------------

    /// Compile an additional `CoreExpr` fragment into this machine's *live*
    /// `JITModule` and return its `FuncId`, without tearing down the existing
    /// code or heap. The new fragment may reference session bindings via
    /// `external_env` (Var-miss resolution to seeded heap pointers).
    ///
    /// Declare + define the fragment and re-run
    /// `finalize_definitions` (multi-round-safe in cranelift 0.129.1 — a new
    /// `FuncId` post-finalize carves a fresh arena segment, leaving round-1 code
    /// stable). `table` shapes the fragment exactly like the one-shot entry
    /// (`normalize` + datacon-env wrap + lit-wrapper tolerance), so re-entry is
    /// emission-identical to the original compile, only the destination differs.
    /// Returns the id for a later [`Self::run_fragment`] / [`Self::run_fragment_pure`].
    pub fn add_function(
        &mut self,
        name: &str,
        expr: &CoreExpr,
        table: &DataConTable,
        external_env: &crate::emit::ExternalEnv,
    ) -> Result<FuncId, JitError> {
        self.fragments_added += 1;
        // Mirror compile_inner's tree shaping so the fragment is emitted exactly
        // like the original entry; only the JITModule destination differs (it is
        // already finalized — we add a fresh round).
        let expr = tidepool_repr::normalize(expr, table);
        let expr = crate::datacon_env::wrap_with_datacon_env(expr, table);
        // Boxed-literal wrapper tolerance is per-compile; refresh from this
        // fragment's table (see compile_inner). Runtime-inert — read only during
        // emission — so refreshing it does not perturb already-compiled code.
        self.pipeline.lit_wrappers = crate::emit::LitWrapperIds::from_table(table);
        // GLOBAL-ID INVARIANT (codex-review-2026-08-08.md item 3): `json_con_ids`,
        // `time_con_ids`, and `tags` (`ConTags`) below are MACHINE-GLOBAL —
        // one slot each on `JitEffectMachine`/`MachineState`, not one per
        // realm or per parked frame. Every `add_function` call on this
        // machine (any realm) accumulates into the SAME three slots, and a
        // later call's successfully-resolved ids OVERWRITE an earlier one's
        // (re-resolved, not merged — see the `tags` comment below for the
        // exact Err/Ok transition table). `resume_applied` reads `self.tags`
        // (not a per-frame copy) to interpret a resumed continuation's own
        // freer-simple envelope (`Val`/`E`/`Union`/`Leaf`/`Node`).
        //
        // This is safe ONLY because every realm sharing one machine is
        // expected to agree on these ids: `Val`/`E`/`Union`/`Leaf`/`Node`
        // (and, if used, the JSON `Either`/`I#`/`Text` / time constructors)
        // come from the SAME fixed library modules for every realm compiled
        // through this process, so in practice every table resolves them to
        // the SAME numeric tags — this is what makes "last writer wins"
        // harmless rather than a silent tag-confusion hazard. It is NOT
        // guaranteed by any check here: a table that assigned a DIFFERENT
        // numeric tag to one of these shared constructors would silently
        // corrupt how an already-parked SIBLING realm's continuation gets
        // interpreted on its next resume. What IS guarded, and load-bearing
        // for realms in general, is that a realm's OWN domain constructors
        // never go through this machine-global cache at all: `resume_parked`
        // decodes exclusively against `ContinuationFrame::table` (A4, cloned
        // once at park time), so two realms may freely reuse the SAME numeric
        // `DataConId`/tag for DIFFERENT domain constructors without collision
        // or shadowing — see `realm_global_id_isolation.rs` for the pinning
        // test.
        //
        // ACCUMULATE the primop constructor-id bundles (JsonDecode / ParseISO8601)
        // as fragments introduce constructors: upgrade None -> Some, never clobber
        // a resolved bundle. Each turn's table is a SUBSET of the session, so a
        // later turn that merely FORCES a primop-produced thunk — its own Core
        // may not reference Either/Value/I#/Text at all — still sees the ids a
        // turn that DID reference them resolved. (Without this, forcing a
        // JsonDecode/ParseISO8601 result in a sparse turn failed with
        // "constructors not in scope".) The machine reads these fields at every
        // run entry via `install_registries`.
        if let Some(ids) = tidepool_eval::json::JsonConIds::from_table(table) {
            self.json_con_ids = Some(ids);
        }
        if let Some(ids) = tidepool_eval::time::TimeConIds::from_table(table) {
            self.time_con_ids = Some(ids);
        }
        // Refresh `tags` too — re-resolve ConTags against THIS fragment's table
        // rather than leaving it frozen at whatever `compile_inner` saw at
        // bootstrap. The asymmetry is deliberate, not an oversight:
        //   Err -> Ok: install. Mirrors json_con_ids/time_con_ids' accumulate-
        //     never-clobber intent — a later turn's table may supply a freer
        //     constructor (Val/E/Union/Leaf/Node) that bootstrap's table lacked,
        //     and without this a session stays permanently `MissingConTags`
        //     even once the table can classify.
        //   Ok -> Ok (re-resolved): install. An accumulated session table is a
        //     superset of the bootstrap one, so this is a no-op in practice,
        //     but re-resolving against the turn's own table rather than
        //     assuming stability is the honest rule.
        //   Ok -> Err: do NOT clobber. Overwriting an established `Ok` with a
        //     fresh `Err` would break a session whose later turn happens to
        //     carry a sparser table than a prior turn did.
        if let Ok(refreshed) = ConTags::from_table(table) {
            if self.tags.is_err() {
                // NAMED heal event (plans/post-restart/extract-wave/boot's
                // lazy-boot item): a machine bootstrapped from a ConTags-free
                // expr (e.g. the outer session's pure `render` seed) starts
                // `Err(MissingConTags)`; this fragment's table is the first to
                // resolve. Must be loud — the whole point of naming it is that
                // a future regression that stops the heal surfaces HERE, not
                // three files away as a confusing dispatch failure.
                log::info!(
                    target: "tidepool::codegen",
                    "ConTags healed on add_function(name={name}): was MissingConTags, now resolved from this fragment's table",
                );
            }
            self.tags = Ok(refreshed);
        }

        let func_id =
            crate::emit::expr::compile_expr(&mut self.pipeline, &expr, name, external_env)
                .map_err(JitError::Compilation)?;

        // Multi-round finalize: finalize_definitions is safe to re-run; finalize()
        // drains only THIS round's pending stack maps and appends them to the
        // registry (round-1 maps were drained on the first finalize).
        self.pipeline.finalize()?;

        Ok(func_id)
    }

    /// Run a previously-[`add_function`](Self::add_function)ed fragment against
    /// this machine's live, machine-owned heap, dispatching effects through the
    /// handler HList exactly as [`Self::run`] does for the one-shot entry.
    ///
    /// Like `run`, but targets `func_id` instead of the machine's original
    /// entry, reusing the persistent heap (`install_registries` re-points GC
    /// state at the retained buffer).
    pub fn run_fragment<U, H: DispatchEffect<U>>(
        &mut self,
        func_id: FuncId,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
    ) -> Result<Value, JitError> {
        self.run_with_entry(func_id, table, handlers, user)
    }

    /// Pure sibling of [`Self::run_fragment`]: run an `add_function`-minted
    /// fragment whose result is a plain value (no `Eff` wrapper) against the
    /// retained session heap. Mirrors [`Self::run_pure`]. Used by the converge
    /// proof, where a reference fragment (`case x of C n -> n`) resolves a
    /// tenured session value purely.
    pub fn run_fragment_pure(&mut self, func_id: FuncId) -> Result<Value, JitError> {
        self.run_pure_with_entry(func_id)
    }

    /// The value-plane **bind primitive**: run a pure entry, deep-force its
    /// result to normal form, tenure the NF value into the session old-space,
    /// register its persistent GC root, and return the stable
    /// [`RootSlot`](crate::old_space::RootSlot) a later fragment resolves
    /// through its `ExternalEnv`.
    ///
    /// The tenure happens while GC state is still installed and the result
    /// pointer is live (before the per-run `RegistryGuard` reclaims the
    /// nursery buffer), so the tenured copy and its slot outlive the run.
    ///
    /// # Panics
    /// Panics if called on a non-session machine (no old-space to tenure into).
    pub fn run_pure_and_bind(
        &mut self,
        func_id: FuncId,
    ) -> Result<crate::old_space::RootSlot, JitError> {
        assert!(
            self.session.is_some(),
            "run_pure_and_bind requires a session machine (compile_session)"
        );
        // run_pure_and_bind always forces to NF (Tier0 data) before tenuring
        // — unlike its effectful sibling `run_fragment_and_bind`, it takes no
        // `forced` flag.
        Ok(self
            .with_active_run::<(), NoHandlers>(
                func_id,
                RunTarget::Pure,
                ResultMaterialization::Bind { forced: true },
                "run_pure_and_bind called while a continuation is suspended — \
                 resume_suspended it first",
                "running pure computation (bind)",
                "",
            )?
            .expect_bind())
    }

    /// The effectful value-plane **bind primitive**: run `func_id` through the
    /// freer-simple effect step loop (dispatching through `handlers`), and at
    /// `Yield::Done(ptr)` apply the BIND sequence from `run_pure_and_bind`:
    /// optionally `deep_force` to NF (`forced = true` → Tier0 data; `false` →
    /// Tier1 closure, tenure as-is), tenure into old-space, register the
    /// persistent root, and return the stable
    /// [`RootSlot`](crate::old_space::RootSlot) a later fragment resolves via
    /// `ExternalEnv`.
    ///
    /// **Why this must exist (not reusing `run_pure_and_bind`):** a bind turn
    /// compiles `result = do { x <- action; pure x } :: Eff stack T`. The Core
    /// is a freer-simple `Eff` tree, NOT a bare `T`. `run_pure_and_bind` calls
    /// the entry once and roots the immediate return — for an `Eff` result that
    /// is the `Val`-leaf wrapper, not the underlying value. The value only
    /// appears at `Yield::Done(ptr)` AFTER the effect step loop reduces the
    /// tree. This method runs the loop and then applies the bind sequence.
    ///
    /// # Panics
    /// Panics if called on a non-session machine (no old-space to tenure into).
    pub fn run_fragment_and_bind<U, H: DispatchEffect<U>>(
        &mut self,
        func_id: FuncId,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
        forced: bool,
    ) -> Result<crate::old_space::RootSlot, JitError> {
        assert!(
            self.session.is_some(),
            "run_fragment_and_bind requires a session machine (compile_session)"
        );
        Ok(self
            .with_active_run(
                func_id,
                RunTarget::Effectful {
                    table,
                    handlers,
                    user,
                },
                ResultMaterialization::Bind { forced },
                "run_fragment_and_bind called while a continuation is suspended — \
                 resume_suspended it first",
                "stepping effectful computation (bind)",
                "",
            )?
            .expect_bind())
    }

    /// Multi-binder effectful bind: run `func_id` through the effect step loop,
    /// deep-force the WHOLE `Yield::Done(tuple_ptr)` result tuple (Tier-0 —
    /// every field is forced, unconditionally, unlike the single-binder
    /// `run_fragment_and_bind`'s per-call `forced` choice), then project and
    /// tenure each of `n_fields` fields into old-space. The caller
    /// (session.rs `run_multi_bind`) zips the returned slots with the binder
    /// metadata, in the same source order as the `pure (a, b, …)` wrapper and
    /// the binders in the JSON sidecar; the assertion on `n_actual` guards
    /// against shape mismatches.
    ///
    /// # Panics
    /// Panics if called on a non-session machine.
    pub fn run_fragment_and_bind_projected<U, H: DispatchEffect<U>>(
        &mut self,
        func_id: FuncId,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
        n_fields: usize,
    ) -> Result<Vec<crate::old_space::RootSlot>, JitError> {
        assert!(
            self.session.is_some(),
            "run_fragment_and_bind_projected requires a session machine"
        );
        #[allow(
            clippy::expect_used,
            reason = "run_fragment_and_bind_projected requires at least one field"
        )]
        let n_fields = NonZeroUsize::new(n_fields)
            .expect("run_fragment_and_bind_projected requires at least one field");
        Ok(self
            .with_active_run(
                func_id,
                RunTarget::Effectful {
                    table,
                    handlers,
                    user,
                },
                ResultMaterialization::Project { n_fields },
                "run_fragment_and_bind_projected called while a continuation is \
                 suspended — resume_suspended it first",
                "stepping effectful computation (multi-bind)",
                " (multi-bind)",
            )?
            .expect_project())
    }

    /// The single-compile `it`-binding primitive: run `func_id` through the
    /// effect step loop, and at `Yield::Done(tuple_ptr)` — the result of the
    /// wrapped `pure (it, toWire it)` — bridge field 1 (the render) into an
    /// OWNED [`Value`] first, then tenure field 0 (`it` itself) alone,
    /// returning both.
    ///
    /// **Why field1-before-field0-tenure is load-bearing:** when `toWire` is
    /// the identity (`toWire :: Aeson.Value -> Aeson.Value`, e.g. a bare
    /// `pure input`), field 0 and field 1 resolve to the exact SAME heap
    /// object. [`heap_bridge::heap_to_value_forcing`] returns a COMPLETE DEEP
    /// COPY — every leaf is owned Rust data, no pointer into the JIT heap
    /// survives the call — so bridging field 1 into `rendered` FIRST makes it
    /// immune to whatever `tenure` does to that shared object afterward.
    /// Tenuring field 0 SECOND (and ONLY field 0 — field 1 is never tenured)
    /// means at most one object in this call ever gets forwarded, so the
    /// aliasing corruption a naive `pure (it, toWire it)` +
    /// `run_fragment_and_bind_projected` hit (independently tenuring both
    /// fields of a shared object — see that method's doc and
    /// `old_space::tenure`'s forward-skip fix) cannot recur here.
    ///
    /// `field0_forced`: mirrors `run_fragment_and_bind`'s `forced` flag —
    /// `true` (Tier0Data) deep-forces field 0 to NF before tenuring; `false`
    /// (Tier1 closure) tenures field 0 as-is, unforced.
    ///
    /// # Panics
    /// Panics if called on a non-session machine.
    pub fn run_fragment_and_bind_render<U, H: DispatchEffect<U>>(
        &mut self,
        func_id: FuncId,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
        field0_forced: bool,
    ) -> Result<(crate::old_space::RootSlot, Value), JitError> {
        assert!(
            self.session.is_some(),
            "run_fragment_and_bind_render requires a session machine"
        );
        Ok(self
            .with_active_run(
                func_id,
                RunTarget::Effectful {
                    table,
                    handlers,
                    user,
                },
                ResultMaterialization::Render { field0_forced },
                "run_fragment_and_bind_render called while a continuation is \
                 suspended — resume_suspended it first",
                "stepping effectful computation (bind-render)",
                " (bind-render)",
            )?
            .expect_render())
    }

    /// Register a session-scoped GC root slot that survives across runs (i.e.
    /// across `RegistryGuard` drops), unlike the per-run rust roots.
    ///
    /// Fills this machine's persistent-roots registry so a tenured binding's
    /// root is appended to `perform_gc`'s root set and is NOT cleared by the
    /// per-run `clear_run_scratch`. Takes a slot pointer (`*mut *mut u8`)
    /// like `host_fns::register_rust_root`.
    ///
    /// # Safety
    /// The caller guarantees that `slot` is non-null, points to a valid
    /// `*mut u8` heap-pointer location, and remains valid and dereferenceable
    /// until the session ends (the `JitEffectMachine` is dropped) — the copying
    /// GC will read and rewrite `*slot` in place on every collection until then.
    /// A slot freed or moved before machine teardown is a use-after-free.
    pub unsafe fn register_persistent_root(&self, slot: *mut *mut u8) {
        // Delegates directly to this machine's own MachineState (not through
        // the vmctx-gated free fn) — `self.machine_state` IS the handle that
        // fn would otherwise have to look up. `free_session_heap` (machine
        // drop) clears the registry. SAFETY: forwarded to the caller's
        // contract documented above.
        self.machine_state.register_persistent_root(slot);
    }

    /// Number of persistent GC roots currently registered on this machine
    /// (test/diagnostic accessor). Reads `self.machine_state` directly —
    /// unlike the vmctx-gated `host_fns::persistent_roots_count` free fn,
    /// this works whether or not a run is currently in flight, since a
    /// `JitEffectMachine` always owns its `MachineState`.
    pub fn persistent_roots_count(&self) -> usize {
        self.machine_state.persistent_roots_count()
    }

    /// Number of write-barrier remembered slots currently registered on this
    /// machine (test/diagnostic accessor).
    pub fn remembered_slots_count(&self) -> usize {
        self.machine_state.remembered_slots_count()
    }

    /// Session-lifetime count of Cranelift functions successfully compiled
    /// into this machine's `JITModule` (test/diagnostic accessor).
    pub fn functions_defined(&self) -> u64 {
        self.pipeline.functions_defined()
    }

    /// Total bytes currently tenured in this session's old-space (test/
    /// diagnostic accessor). 0 for a one-shot machine (no session, no
    /// old-space).
    pub fn old_space_bytes_used(&self) -> usize {
        self.session
            .as_ref()
            .map(|s| s.old_space.bytes_used())
            .unwrap_or(0)
    }

    /// Whether this machine is currently suspended at a typed yield (`Ask`),
    /// holding a stowed continuation awaiting `resume_suspended`.
    pub fn is_suspended(&self) -> bool {
        self.suspended_continuation.is_some()
    }

    /// Number of stowed GC roots currently registered (test/diagnostic
    /// accessor — 1 while a nested child is running against a suspended parent,
    /// 0 otherwise).
    pub fn stowed_roots_count(&self) -> usize {
        self.machine_state.stowed_roots_count()
    }

    /// Read-only heap/GC snapshot (observatory heap pane) — EXISTING counters
    /// only, no new instrumentation inside the collector. `nursery_bytes` is
    /// the nursery's total capacity; `live_bytes` is the session heap's bump
    /// high-water mark (`SessionState::cursor` — bytes allocated since the
    /// last GC, or since bootstrap if none has run yet); `gc_count` is
    /// [`MachineState::gc_generation`], bumped once per actual collection.
    pub fn heap_stats(&self) -> HeapStats {
        let live_bytes = self.session.as_ref().map(|s| s.cursor).unwrap_or(0);
        HeapStats {
            nursery_bytes: self.nursery.size(),
            live_bytes,
            gc_count: self.machine_state.gc_generation(),
            fragments: self.fragments_added,
        }
    }

    /// Test/debug-only: force a real Cheney minor collection against this
    /// session's retained heap, without running any compiled code. Not part
    /// of the public API.
    ///
    /// Production code has no reason to trigger a collection deterministically
    /// between two calls — every real collection fires from inside compiled
    /// code, mid-allocation (`gc_trigger`). This exists to close that gap for
    /// diagnosing whether a collection LANDING BETWEEN a suspend-time tenure
    /// (`Self::tenure_finalized_payload`) and a later resume corrupts a parked
    /// frame's own reference into what tenuring evacuated (the tenure-then-
    /// resume rooting family — see `tidepool-codegen/CLAUDE.md`'s diagnostics
    /// table). Calling it with the machine suspended (slot path) or holding
    /// parked frames (registry path) is exactly the intended use.
    ///
    /// Installs registries and builds an ordinary session `VMContext` (the
    /// same construction `with_active_run` uses), calls
    /// `host_fns::gc_trigger` directly with NO compiled frame on the stack —
    /// `frame_walker::walk_frames` degrades gracefully in that case (finds no
    /// JIT stack maps, contributes zero stack roots; see its doc), so this
    /// collection's root set is exactly {persistent, stowed, remembered}, the
    /// same classes a real mid-allocation collection would fold in — then
    /// reclaims the (possibly relocated) heap buffer back onto the session,
    /// mirroring `RegistryGuard`'s ordinary drop-time reclaim.
    ///
    /// # Panics
    /// Panics if this is not a session machine (`compile_session`).
    #[doc(hidden)]
    pub fn force_gc_for_test(&mut self) {
        assert!(
            self.session.is_some(),
            "force_gc_for_test requires a session machine (compile_session)"
        );
        let mut guard = self.install_registries();
        let mut vmctx = self.make_session_vmctx();
        // SAFETY: machine_state outlives this call (owned by self), matching
        // every other run entry's vmctx construction.
        vmctx.machine_state = &mut self.machine_state as *mut MachineState;
        let vmctx_ptr = &mut vmctx as *mut crate::context::VMContext;
        // gc_trigger reads the caller's own frame pointer to start its stack
        // walk, which is sound to call from plain Rust (no JIT frame on the
        // stack) per `walk_frames`'s doc — degrades to zero stack roots.
        crate::host_fns::gc_trigger(vmctx_ptr);
        // SAFETY: vmctx is a local in this frame, live until `guard` drops at
        // the end of this function — the same arm-last discipline
        // `with_active_run` uses. Reclaims the (possibly relocated) heap
        // buffer back into `self.session`.
        unsafe {
            guard.arm_reclaim(&mut self.session as *mut _, vmctx_ptr as *const _);
        }
    }

    // ----------------------------------------------------------------------
    // Nested child runs on a suspended machine — see the module docstring's
    // "Nested child runs on a suspended machine" section for the invariant.
    // ----------------------------------------------------------------------

    /// Enter nested-child mode: MOVE the stowed continuation out of
    /// `suspended_continuation` into a heap-stable `Box` cell, register that
    /// cell in `stowed_roots`, and increment `nested_child_depth`. Returns a
    /// [`NestedChildGuard`] whose `Drop` reads the (GC-current) pointer back out
    /// and restores it into `suspended_continuation`, deregisters the root, and
    /// decrements the depth.
    ///
    /// Moving the pointer OUT of `suspended_continuation` for the child's
    /// duration is load-bearing two ways: (1) `suspended_continuation` reads
    /// `None` while the child runs, so the plain run entries' L7 asserts pass
    /// naturally — the child fragment goes through `run_fragment*` exactly like
    /// any turn — and (2) the continuation is protected NOT by the (now-absent)
    /// temporal argument but by the `stowed_roots` registration on the
    /// heap-stable cell, which every child collection traces and rewrites in
    /// place. On guard drop the machine's `suspended_continuation` again points
    /// at the (possibly relocated) continuation.
    ///
    /// # Panics
    /// Panics if the machine is not suspended (no continuation to root) — a
    /// nested child requires a suspended parent by construction.
    fn enter_nested_child(&mut self) -> NestedChildGuard {
        #[allow(
            clippy::expect_used,
            reason = "enter_nested_child on a machine that is not suspended"
        )]
        let cont = self
            .suspended_continuation
            .take()
            .expect("enter_nested_child on a machine that is not suspended");
        // Heap-stable cell: the machine moves between threads (stow XOR run),
        // but the Box's pointee address is a stable heap allocation, so the
        // registered slot address stays valid across the move.
        let mut cell = Box::new(cont);
        let slot: *mut *mut u8 = &mut *cell;
        self.stowed_root_cell = Some(cell);
        // SAFETY: `slot` is the address of the Box's inner cell, stable for the
        // Box's life (until the guard drops and puts the pointer back). The GC
        // reads and rewrites `*slot` in place on every collection until then.
        self.machine_state.register_stowed_root(slot);
        self.nested_child_depth += 1;
        NestedChildGuard {
            machine_state: &self.machine_state as *const MachineState,
            suspended_continuation: &mut self.suspended_continuation as *mut Option<*mut u8>,
            stowed_root_cell: &mut self.stowed_root_cell as *mut Option<Box<*mut u8>>,
            nested_child_depth: &mut self.nested_child_depth as *mut usize,
            slot,
        }
    }

    /// Run a fragment as a CHILD against this suspended parent, dispatching
    /// effects through the handler HList exactly as [`Self::run_fragment`] does.
    /// The parent's stowed continuation is GC-rooted for the child's duration
    /// (see [`Self::enter_nested_child`]); a child collection — including heap
    /// doubling — evacuates it, so the parent resumes correctly afterward.
    ///
    /// The child fragment reads the parent's session bindings zero-copy through
    /// its `external_env` (resolved when the fragment was `add_function`-minted),
    /// against the SAME retained session heap. Module accretion is inert for the
    /// parent: adding a child fragment does not perturb the parent's stowed
    /// continuation.
    ///
    /// # Panics
    /// Panics if the machine is not currently suspended.
    pub fn run_child_fragment<U, H: DispatchEffect<U>>(
        &mut self,
        func_id: FuncId,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
    ) -> Result<Value, JitError> {
        assert!(
            self.suspended_continuation.is_some(),
            "run_child_fragment requires a suspended parent (call run_fragment on an idle machine)"
        );
        let _nested = self.enter_nested_child();
        // With the continuation moved into the registered stowed cell,
        // `suspended_continuation` is None — the plain run entry's L7 assert
        // passes, and the fragment drives byte-identically to any turn.
        self.run_with_entry(func_id, table, handlers, user)
    }

    /// Pure sibling of [`Self::run_child_fragment`] — run an `add_function`-minted
    /// pure fragment as a child against the suspended parent's retained heap.
    ///
    /// # Panics
    /// Panics if the machine is not currently suspended.
    pub fn run_child_fragment_pure(&mut self, func_id: FuncId) -> Result<Value, JitError> {
        assert!(
            self.suspended_continuation.is_some(),
            "run_child_fragment_pure requires a suspended parent"
        );
        let _nested = self.enter_nested_child();
        self.run_pure_with_entry(func_id)
    }

    // ----------------------------------------------------------------------
    // REALM PROTOTYPE — the parked-continuation registry. See the module
    // docstring's "The parked-continuation registry (realm prototype)"
    // section for the invariant.
    // ----------------------------------------------------------------------

    /// Check `incoming` — a realm's handled prefix, the effect names for tags
    /// `[0, suspend_tag)` in position order — against the machine's
    /// established prefix. `DispatchEffect` is positional over an `HList`
    /// and the suspend test is `tag >= suspend_tag`; both are correct only
    /// relative to one effect row whose handled effects occupy a contiguous
    /// low prefix, so two realms sharing a machine must agree on that prefix
    /// exactly, not merely where they happen to overlap.
    ///
    /// An EMPTY `incoming` prefix is compatible with anything — this is the
    /// outer driver's threshold-zero row (`vec![runllmturn_decl()]`, handled
    /// prefix empty: nothing handled, nothing ever dispatched, so it cannot
    /// misroute). If the machine has not established a prefix yet, any
    /// `incoming` prefix is compatible (it may go on to become the
    /// establishing one). Otherwise, two non-empty prefixes must be EXACTLY
    /// EQUAL: a length difference is refused as [`PrefixMismatch::Length`]
    /// (a strict EXTENSION, e.g. `[FileIO, Proc]` established against
    /// `[FileIO, Proc, Memory]` incoming, is REFUSED, not accepted — see
    /// below for why); equal length but disagreeing content is refused as
    /// [`PrefixMismatch::Position`] at the first differing index.
    ///
    /// **Why exact equality, not agreement-up-to-the-shorter-length.** The
    /// established prefix is CALLER-SUPPLIED metadata, not something read off
    /// the machine's actual (compile-time monomorphized, runtime-opaque) `H`.
    /// A realm declaring a shorter prefix says nothing about how many
    /// handlers `H` really has — it may simply use a lower suspend
    /// threshold. So accepting an extension is unsound: if `H` really does
    /// have a handler at the extended position, the extending realm's tag
    /// there is BELOW ITS OWN threshold (dispatched, not suspended), and it
    /// reaches that handler — a silent misroute, exactly what this check
    /// exists to prevent.
    ///
    /// **Why exact equality IS sound.** With every non-empty established
    /// prefix on a machine equal to every other, and an empty prefix
    /// dispatching nothing, every tag that is ever DISPATCHED (as opposed to
    /// suspended) is strictly below the one common prefix length, and every
    /// realm agrees on what sits at every position below that length. No
    /// dispatched tag can therefore reach a position two realms disagree
    /// about.
    ///
    /// **The residual, stated rather than glossed.** This check enforces
    /// agreement AMONG realms sharing a machine; it cannot verify a declared
    /// prefix against the actual, opaque `H` — a single realm parking alone,
    /// or every realm agreeing with each other while all of them are wrong
    /// about `H`, is not caught here. A wrong declared prefix, undetected by
    /// any other realm's disagreement, remains the caller's responsibility.
    fn check_prefix_compatible(&self, incoming: &[String]) -> Result<(), JitError> {
        if incoming.is_empty() {
            return Ok(());
        }
        if let Some(established) = &self.established_prefix {
            if established.len() != incoming.len() {
                return Err(JitError::IncompatibleHandledPrefix {
                    established: established.to_vec(),
                    incoming: incoming.to_vec(),
                    mismatch: PrefixMismatch::Length,
                });
            }
            for (position, (e, i)) in established.iter().zip(incoming.iter()).enumerate() {
                if e != i {
                    return Err(JitError::IncompatibleHandledPrefix {
                        established: established.to_vec(),
                        incoming: incoming.to_vec(),
                        mismatch: PrefixMismatch::Position(position),
                    });
                }
            }
        }
        Ok(())
    }

    /// Enter the parked path with `incoming` — a realm's handled prefix.
    /// Checks it against the machine's established prefix
    /// ([`Self::check_prefix_compatible`]) and, if compatible, ESTABLISHES it
    /// when this is the first non-empty prefix to enter. Two non-empty
    /// prefixes must be EXACTLY EQUAL to be compatible — see
    /// [`Self::check_prefix_compatible`] for why that is sound (every
    /// dispatched tag sits below the one common prefix length every realm
    /// agrees on) and its residual (agreement AMONG realms, not verification
    /// against the actual opaque `H`).
    ///
    /// `H` is fixed for the machine's life regardless of whether the
    /// entering turn goes on to suspend or complete, so establishing must
    /// happen HERE — at entry, before the machine is driven at all — not at
    /// park: a realm that runs a turn to completion without ever suspending
    /// dispatches every one of its effects through `H` exactly the same as
    /// one that suspends, so establishing only on suspension would leave
    /// such a realm's non-empty prefix never recorded, after which an
    /// incompatible realm could park successfully because nothing was
    /// established.
    ///
    /// Called at the TOP of every parked-path entry
    /// ([`Self::run_fragment_suspendable_parked`], [`Self::resume_parked`]),
    /// before anything is driven — a refusal here leaves the machine
    /// untouched because nothing has run yet, which is a strictly easier
    /// property to hold than checking after a run has already suspended.
    fn enter_parked_path(&mut self, incoming: &Arc<[String]>) -> Result<(), JitError> {
        self.check_prefix_compatible(incoming)?;
        if self.established_prefix.is_none() && !incoming.is_empty() {
            self.established_prefix = Some(incoming.clone());
        }
        Ok(())
    }

    /// The rooting receipt: every parked continuation must be a
    /// registered GC root for its whole parked lifetime, so
    /// `stowed_roots_count() == parked_count()` at every quiescent point the
    /// registry passes through. `debug_assert_eq!` rather than a hard
    /// assertion — a violation is a soundness bug worth crashing a debug or
    /// test build over, but no release caller should pay a counting cost for
    /// it. Call after every registry mutation: a park, a re-park during a
    /// resume, a rejected resume that leaves the frame parked, a successful
    /// removal, and a drain.
    fn assert_rooting_receipt(&self) {
        debug_assert_eq!(
            self.machine_state.stowed_roots_count(),
            self.continuations.len(),
            "rooting receipt violated: stowed_roots_count() must equal the parked \
             continuation count at every quiescent point"
        );
    }

    /// Park a suspended continuation into the registry as a registered GC root
    /// and mint its [`ContinuationId`]. The heap-stable `Box` cell is the same
    /// pattern [`Self::enter_nested_child`] uses; the difference is lifetime —
    /// this registration is released by [`Self::resume_parked`], not by a guard
    /// at the end of the next child run.
    ///
    /// The caller has already checked AND established `handled_prefix` via
    /// [`Self::enter_parked_path`] at entry to the parked path — before the
    /// machine was driven at all. This method performs no check or establish
    /// of its own; `handled_prefix` is stored on the new frame purely so a
    /// later [`Self::resume_parked`] can replay it through that same entry
    /// check.
    #[allow(clippy::too_many_arguments)]
    fn park_continuation(
        &mut self,
        continuation: *mut u8,
        realm: RealmId,
        kind: ParkKind,
        suspend_tag: u64,
        cancel_flag: Arc<AtomicBool>,
        table: Arc<DataConTable>,
        finalized_root: Option<crate::old_space::RootSlot>,
        handled_prefix: Arc<[String]>,
    ) -> ContinuationId {
        let mut cell = Box::new(continuation);
        let slot: *mut *mut u8 = &mut *cell;
        // SAFETY: `slot` is the address of the Box's inner cell — a stable heap
        // allocation that does not move when the Box moves into the map or the
        // machine moves between threads. It stays valid until `resume_parked`
        // deregisters it and drops the frame. The GC reads and rewrites `*slot`
        // in place on every collection until then.
        self.machine_state.register_stowed_root(slot);
        let id = ContinuationId(self.next_continuation_id);
        self.next_continuation_id += 1;
        self.continuations.insert(
            id,
            ContinuationFrame {
                cell,
                realm,
                suspend_tag,
                kind,
                finalized_root,
                cancel_flag,
                table,
                handled_prefix,
            },
        );
        self.assert_rooting_receipt();
        id
    }

    /// Parked sibling of [`Self::run_suspendable`]: drive the machine's entry
    /// through the same suspend path, but PARK a suspension in the continuation
    /// registry under `realm` instead of stowing it in the single slot.
    ///
    /// `suspended_continuation` is left `None` throughout, so the machine stays
    /// usable: further fragments, further parked turns, and resumes of OTHER
    /// parked continuations all run against it while this one waits.
    ///
    /// `handled_prefix` must be EXACTLY EQUAL to every other non-empty
    /// prefix already on this machine, or empty — see
    /// [`Self::check_prefix_compatible`] for why that is the sound check
    /// (not merely agreement up to a shared length) and its residual (it
    /// enforces agreement AMONG realms, not verification against the
    /// actual, opaque `H`).
    ///
    /// # Panics
    /// Panics on a non-session machine — heap retention across the suspension
    /// requires [`Self::compile_session`].
    pub fn run_suspendable_parked<U, H: DispatchEffect<U>>(
        &mut self,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
        suspend_tag: u64,
        realm: RealmId,
        handled_prefix: &[String],
    ) -> Result<ParkedOutcome, JitError> {
        let func_id = self.func_id;
        self.run_fragment_suspendable_parked(
            func_id,
            table,
            handlers,
            user,
            suspend_tag,
            realm,
            ParkKind::Plain,
            handled_prefix,
        )
    }

    /// Parked sibling of [`Self::run_fragment_suspendable`] /
    /// [`Self::run_fragment_suspendable_binding`]: drive an
    /// [`Self::add_function`]-minted fragment through the suspend path, parking
    /// a suspension in the registry under `realm`. `kind` picks the completion
    /// discipline — [`ParkKind::Plain`] bridges the `Done` pointer,
    /// [`ParkKind::Binding`] tenures it as a value-plane bind.
    ///
    /// `handled_prefix` is this realm's handled prefix — the effect names for
    /// tags `[0, suspend_tag)`, in position order (the caller builds the
    /// decls row, so it has the names). Checked against the machine's
    /// established prefix, and established if this is the first non-empty
    /// prefix to enter, BEFORE the machine is driven at all
    /// ([`Self::enter_parked_path`]) — an incompatible realm never executes a
    /// single effect against a foreign handler stack, whether or not it
    /// would go on to suspend or complete. Two non-empty prefixes must be
    /// EXACTLY EQUAL to be compatible — a strict extension of the
    /// established prefix is REFUSED, not accepted, because the established
    /// prefix is caller-supplied metadata, not a read of the machine's
    /// actual (opaque) handler stack (see
    /// [`Self::check_prefix_compatible`] for the full argument and its
    /// residual: this enforces agreement AMONG realms, not verification
    /// against the real `H`). A disagreement refuses with
    /// `JitError::IncompatibleHandledPrefix` and leaves the machine
    /// untouched (nothing has run yet).
    ///
    /// # Panics
    /// Panics on a non-session machine.
    #[allow(clippy::too_many_arguments)]
    pub fn run_fragment_suspendable_parked<U, H: DispatchEffect<U>>(
        &mut self,
        func_id: FuncId,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
        suspend_tag: u64,
        realm: RealmId,
        kind: ParkKind,
        handled_prefix: &[String],
    ) -> Result<ParkedOutcome, JitError> {
        let handled_prefix: Arc<[String]> = Arc::from(handled_prefix);
        self.enter_parked_path(&handled_prefix)?;
        self.run_suspendable_shared(
            func_id,
            table,
            handlers,
            user,
            suspend_tag,
            kind.materialization(),
            ParkTarget::Registry {
                realm,
                kind,
                handled_prefix,
            },
        )
        .map(ParkedRaw::into_parked)
    }

    /// Re-enter the continuation parked under `id`, feeding the answer (or an
    /// abort) and driving to the next suspension or completion. The frame's own
    /// `suspend_tag` and [`ParkKind`] are replayed — the caller supplies only
    /// the id and the input.
    ///
    /// Resumes in ANY order: the registry imposes none. A re-suspension parks
    /// again under a FRESH id in the same realm, replaying the frame's own
    /// `handled_prefix` — re-checked (and, if still unestablished, re-offered
    /// to establish) via [`Self::enter_parked_path`] at the TOP of this
    /// method, before the continuation is driven at all — same discipline as
    /// [`Self::run_fragment_suspendable_parked`]'s entry check. The frame's
    /// own prefix was already checked EXACTLY EQUAL to the machine's
    /// established one when it first parked, and the established prefix is
    /// monotonic, so this re-check cannot newly disagree — see
    /// [`Self::check_prefix_compatible`] for why exact equality is sound and
    /// its residual (agreement AMONG realms, not verification against the
    /// real `H`).
    ///
    /// A5 discipline, same as [`Self::resume_suspended`]: the answer is
    /// NF-forced BEFORE the frame is taken out of the map, so a bottom-bearing
    /// answer leaves the frame PARKED and still ROOTED and the caller can retry
    /// with a corrected answer.
    ///
    /// A4: the frame's own `table` (cloned at park time, see
    /// [`ContinuationFrame::table`]) is what gets decoded against — this
    /// method no longer accepts a caller-supplied table at all, so resuming a
    /// frame against a foreign row is impossible by construction.
    ///
    /// # Panics
    /// Panics if the single `suspended_continuation` slot is occupied. The two
    /// suspension paths must not be MIXED on one machine: a slot-held
    /// continuation is unregistered, so driving a parked resume against it
    /// would let this run's collections free the slot-held one. This is the L7
    /// assert's sibling for the registry path, and it is why a caller one level
    /// up (`ResidentSession::run_child`'s `ChildSuspended` wall) has to convert
    /// its parent to the registry too rather than park only the child.
    pub fn resume_parked<U, H: DispatchEffect<U>>(
        &mut self,
        id: ContinuationId,
        handlers: &mut H,
        user: &U,
        input: ResumeInput,
    ) -> Result<ParkedOutcome, JitError> {
        assert!(
            self.suspended_continuation.is_none(),
            "resume_parked called while the single-slot continuation is occupied — \
             the slot-held continuation is UNREGISTERED and this run's collections \
             would free it. Convert the caller to the parked path; do not mix."
        );
        // PEEK the frame — do NOT remove it yet (A5).
        let (realm, kind, suspend_tag, handled_prefix) = match self.continuations.get(&id) {
            Some(frame) => (
                frame.realm,
                frame.kind,
                frame.suspend_tag,
                frame.handled_prefix.clone(),
            ),
            None => {
                return Err(JitError::Effect(EffectError::Handler(format!(
                    "resume_parked: no continuation parked under {id:?}"
                ))))
            }
        };
        // Entry check, BEFORE the continuation is driven at all (same
        // discipline as run_fragment_suspendable_parked). The established
        // prefix is monotonic, so this frame's own prefix — already checked
        // compatible when it first parked — stays compatible forever; this
        // re-confirmation is a no-op in practice, kept for the same
        // before-anything-runs discipline rather than because it can newly
        // disagree.
        self.enter_parked_path(&handled_prefix)?;
        if let ResumeInput::Answer(val) = &input {
            if let Err(reason) = answer_force_nf(val) {
                // A5: rejected WITHOUT consuming — the frame stays parked and
                // still rooted, so the rooting receipt must still hold.
                self.assert_rooting_receipt();
                return Err(JitError::Effect(EffectError::Handler(format!(
                    "resume answer is not in normal form (bottom in the answer): {reason}"
                ))));
            }
        }
        // Answer verified NF (or this is an Abort) — NOW take the frame and
        // release its root. Every early return above left it parked and rooted.
        #[allow(
            clippy::expect_used,
            reason = "frame present (peeked above, &mut self held throughout)"
        )]
        let mut frame = self
            .continuations
            .remove(&id)
            .expect("frame present (peeked above, &mut self held throughout)");
        let slot: *mut *mut u8 = &mut *frame.cell;
        self.machine_state.deregister_stowed_root(slot);
        self.assert_rooting_receipt();
        // Read the GC-CURRENT pointer out of the cell: collections since the
        // park rewrote it in place through the registered slot.
        let continuation = *frame.cell;
        // A3/A4: the frame's own cancel flag and table — no `realm_cancel_flags`
        // or caller lookup needed, this IS the second-lookup avoidance the
        // frame exists for.
        let cancel_flag = frame.cancel_flag.clone();
        let table = frame.table.clone();
        // Any finalized root never taken via `take_parked_finalized_root` is
        // dropped here with the frame; its persistent-root registration lives
        // independently for the machine's life regardless (same as
        // `take_finalized_root`'s doc), so this is not a leak — just an
        // explicit acknowledgment rather than a silent field drop.
        let _ = frame.finalized_root.take();
        drop(frame);
        self.resume_applied(
            continuation,
            &table,
            handlers,
            user,
            suspend_tag,
            input,
            kind.materialization(),
            ParkTarget::Registry {
                realm,
                kind,
                handled_prefix,
            },
            cancel_flag,
        )
        .map(ParkedRaw::into_parked)
    }

    /// Frame-scoped sibling of [`Self::take_finalized_root`] for the registry
    /// path: take the persistent root of a PARKED frame's closure-valued
    /// `finalize` payload, tenured at park time. The frame stays parked and
    /// rooted — only the frame's own handle to the slot is cleared, not its
    /// persistent-root registration, which lives for the machine's life
    /// regardless (same as `take_finalized_root`'s doc). `None` unless `id`
    /// names a frame that parked while suspended on a closure-valued
    /// finalize, or its slot was already taken.
    pub fn take_parked_finalized_root(
        &mut self,
        id: ContinuationId,
    ) -> Option<crate::old_space::RootSlot> {
        self.continuations
            .get_mut(&id)
            .and_then(|frame| frame.finalized_root.take())
    }

    /// Number of continuations currently parked in the registry. Equal to
    /// [`Self::stowed_roots_count`] at every quiescent point on the parked path
    /// — that equality IS the rooting receipt.
    pub fn parked_count(&self) -> usize {
        self.continuations.len()
    }

    /// The ids currently parked, ascending. Ordering is imposed here (a
    /// `HashMap` has none) purely so callers and tests can enumerate
    /// deterministically.
    pub fn parked_ids(&self) -> Vec<ContinuationId> {
        let mut ids: Vec<ContinuationId> = self.continuations.keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    /// The realm owning the continuation parked under `id`, if any.
    pub fn parked_realm(&self, id: ContinuationId) -> Option<RealmId> {
        self.continuations.get(&id).map(|f| f.realm)
    }

    // --- value handles + scope exit (one-session plan, pillars A/B) --------

    /// Mint a [`ValueHandle`] over the closure-valued `finalize` payload of
    /// the frame parked under `id` (tenured + persistent-rooted at park time).
    /// The frame STAYS parked and rooted; only its own stash of the slot is
    /// moved into the handle registry, owned by the frame's realm — so
    /// [`Self::close_realm`] of that realm releases the payload exactly once,
    /// whether or not the handle was ever observed or delivered. `None`
    /// unless `id` names a parked frame holding an untaken finalized payload.
    ///
    /// Supersedes [`Self::take_parked_finalized_root`] for new callers: a
    /// handle is `Send`, releasable, and deliverable via
    /// [`ResumeInput::Handle`]; a raw `RootSlot` is none of those.
    ///
    /// Raw [`ValueHandle`] on purpose, not [`RootCustody`]: this machine-level
    /// primitive is also exercised directly by tests that read a handle
    /// non-linearly (`observe_handle`, `handle_realm`, repeated
    /// `ResumeInput::Handle` — all borrows, never a consuming transfer). The
    /// session layer (`tidepool_runtime`'s `ResidentSession::finalized_handle`)
    /// is where a caller-visible custody obligation actually begins — see
    /// [`RootCustody`]'s doc — and that is where the linear wrapper is
    /// applied.
    pub fn handle_from_finalized(&mut self, id: ContinuationId) -> Option<ValueHandle> {
        let frame = self.continuations.get_mut(&id)?;
        let realm = frame.realm;
        let slot = frame.finalized_root.take()?;
        let h = ValueHandle(self.next_value_handle);
        self.next_value_handle += 1;
        self.value_handles.insert(h.0, HandleEntry { slot, realm });
        Some(h)
    }

    /// The realm owning `handle`, if it is live (minted and not yet released
    /// by [`Self::close_realm`]).
    pub fn handle_realm(&self, handle: ValueHandle) -> Option<RealmId> {
        self.value_handles.get(&handle.0).map(|e| e.realm)
    }

    /// Mint a [`ValueHandle`] over an ALREADY-ROOTED slot (a tenured bind
    /// root returned inline by a parked completion). Exists because the
    /// `!Send` [`crate::old_space::RootSlot`] cannot ride an outcome across
    /// the session layer's eval-thread boundary — the eval-thread closure
    /// mints the handle machine-side and the `Send` id crosses instead
    /// (`ResidentSession`'s laundering, pillar-B flavored). The slot's
    /// persistent-root registration is unchanged; the handle just records
    /// realm ownership over it.
    pub fn mint_handle_from_root(
        &mut self,
        slot: crate::old_space::RootSlot,
        realm: RealmId,
    ) -> ValueHandle {
        let h = ValueHandle(self.next_value_handle);
        self.next_value_handle += 1;
        self.value_handles.insert(h.0, HandleEntry { slot, realm });
        h
    }

    /// The rooted slot behind a live handle — for the session layer's OWN
    /// bookkeeping (a value-plane `BindingTable` stores `RootSlot`s on the
    /// session thread, same as it always has). The handle stays live; pairing
    /// this with [`Self::release_handle`] transfers ownership to the caller.
    pub fn handle_slot(&self, handle: ValueHandle) -> Option<crate::old_space::RootSlot> {
        self.value_handles.get(&handle.0).map(|e| e.slot)
    }

    /// Release ONE handle without closing its realm — for a caller that
    /// consumed the underlying slot into its own lifetime discipline (the
    /// value plane). Does NOT deregister the persistent root (ownership
    /// transferred, not dropped); a later `close_realm` no longer sees it.
    pub fn release_handle(&mut self, handle: ValueHandle) -> bool {
        self.value_handles.remove(&handle.0).is_some()
    }

    /// Number of live value handles (test/diagnostic accessor).
    pub fn value_handle_count(&self) -> usize {
        self.value_handles.len()
    }

    /// OBSERVE a handle's payload: bridge its GC-current heap value through
    /// the TOLERANT bridge into an owned [`tidepool_eval::value::Value`] —
    /// data bridges fully (forcing thunks as needed), a closure field renders
    /// as the `CLOSURE_SENTINEL` stub. This is the ONE place a handle's
    /// payload is ever serialized (pillar B: observation by serialization),
    /// and it is honest — an opaque view of an opaque value — never a lossy
    /// delivery. The handle is not consumed.
    ///
    /// Forcing executes JIT code, so this installs the same run shell a
    /// resume does (signal safety, registries, a `CompiledEffectMachine`) and
    /// re-arms session-buffer reclaim on exit; a forced thunk may allocate
    /// and collect, which is safe because the handle's slot is a persistent
    /// root and every parked frame is a stowed root.
    ///
    /// # Panics
    /// Panics on a non-session machine (same constraint as every parked
    /// entry — heap retention requires [`Self::compile_session`]).
    pub fn observe_handle(
        &mut self,
        handle: ValueHandle,
    ) -> Result<tidepool_eval::value::Value, JitError> {
        let entry = match self.value_handles.get(&handle.0) {
            Some(e) => e,
            None => {
                return Err(JitError::Effect(EffectError::Handler(format!(
                    "observe: unknown or released ValueHandle({})",
                    handle.0
                ))))
            }
        };
        let slot = entry.slot;
        let tags = self.tags.map_err(JitError::MissingConTags)?;
        crate::signal_safety::install();
        let mut _guard = self.install_registries_with_cancel_flag(self.cancel_flag.clone());
        // SAFETY: finalized JIT code pointer; the entry func is never called
        // here (no drive), but `CompiledEffectMachine` needs one for its own
        // tail-call resolution if a forced thunk trampolines.
        let func_ptr: unsafe extern "C" fn(*mut VMContext) -> *mut u8 =
            unsafe { std::mem::transmute(self.pipeline.get_function_ptr(self.func_id)) };
        let vmctx = self.make_session_vmctx();
        let mut machine = CompiledEffectMachine::new(func_ptr, vmctx, tags);
        // SAFETY: machine_state outlives this observation (owned by self).
        machine.vmctx_mut().machine_state = &mut self.machine_state as *mut MachineState;
        let vmctx_ptr = machine.vmctx_mut() as *mut VMContext;
        // SAFETY: the slot is persistent-rooted until release; `current()` is
        // the GC-current pointer, re-read AFTER the registries are installed.
        let bridge_res = unsafe {
            let live = slot.current();
            crate::signal_safety::with_signal_protection(|| {
                heap_bridge::heap_to_value_forcing_tolerant(live, vmctx_ptr)
            })
        }
        .map_err(JitError::Signal);
        // SAFETY: as in `resume_applied` — the guard's reclaim reads the
        // post-run buffer/cursor (forcing may have allocated) into
        // self.session.
        unsafe {
            _guard.arm_reclaim(&mut self.session as *mut _, machine.vmctx_mut() as *const _);
        }
        let value = bridge_res?.map_err(JitError::HeapBridge)?;
        Ok(value)
    }

    /// SCOPE EXIT (pillar A — structured concurrency): close `realm`,
    /// releasing everything it owns, atomically from the caller's view:
    ///
    /// - every parked frame owned by the realm is removed and its stowed
    ///   continuation root deregistered;
    /// - each such frame's untaken finalized payload root, and every
    ///   [`ValueHandle`] the realm owns, has its persistent-root registration
    ///   deregistered (the 8-byte slot cell stays with `OldSpace` for the
    ///   machine's life; the VALUE it pinned becomes collectable once nothing
    ///   else reaches it);
    /// - the realm's cancel flag entry is dropped;
    /// - sibling realms and their frames/handles are untouched;
    /// - the rooting receipt (`stowed_roots_count() == parked_count()`) holds
    ///   before and after.
    ///
    /// Returns `(frames_dropped, handles_released)`. Closing a realm that
    /// owns nothing is a no-op `(0, 0)` — idempotent by construction, so a
    /// retirement path that can race a wholesale teardown stays safe.
    pub fn close_realm(&mut self, realm: RealmId) -> (usize, usize) {
        let ids: Vec<ContinuationId> = self
            .continuations
            .iter()
            .filter(|(_, f)| f.realm == realm)
            .map(|(&id, _)| id)
            .collect();
        for id in &ids {
            #[allow(
                clippy::expect_used,
                reason = "id collected from the map above; &mut self held throughout"
            )]
            let mut frame = self
                .continuations
                .remove(id)
                .expect("id collected from the map above; &mut self held throughout");
            let slot: *mut *mut u8 = &mut *frame.cell;
            self.machine_state.deregister_stowed_root(slot);
            if let Some(root) = frame.finalized_root.take() {
                self.machine_state.deregister_persistent_root(root.addr());
            }
        }
        let hids: Vec<u64> = self
            .value_handles
            .iter()
            .filter(|(_, e)| e.realm == realm)
            .map(|(&k, _)| k)
            .collect();
        for k in &hids {
            #[allow(
                clippy::expect_used,
                reason = "key collected from the map above; &mut self held throughout"
            )]
            let entry = self
                .value_handles
                .remove(k)
                .expect("key collected from the map above; &mut self held throughout");
            self.machine_state
                .deregister_persistent_root(entry.slot.addr());
        }
        self.realm_cancel_flags.remove(&realm);
        self.assert_rooting_receipt();
        (ids.len(), hids.len())
    }

    /// SCOPE RETIREMENT (PRD 21 lane C2): deregister one value-plane binding's
    /// persistent GC root, because the scope that solely owned it is retiring.
    ///
    /// **This is a scope-retirement primitive, not a general "drop a root"
    /// tool**, and it is named that way on purpose. Its invariant, which the
    /// caller carries and this method cannot check:
    ///
    /// - `slot` belongs to a binding the RETIRING SCOPE SOLELY OWNS — no other
    ///   live `BindingEntry`, in any scope, holds the same
    ///   [`crate::old_space::RootSlot::addr`], and no live [`ValueHandle`]
    ///   still holds it (see [`Self::handle_holds_root`]);
    /// - it is called EXACTLY ONCE per root;
    /// - the release is WITNESSED by a [`Self::persistent_roots_count`]
    ///   decrement, asserted by the caller — a retirement that reports a
    ///   release without moving that counter is a false receipt.
    ///
    /// A caller that cannot name which scope owns the root is not a legitimate
    /// caller. The value plane's owner is
    /// `tidepool_runtime::session::PersistentSession::retire_scope`; nothing
    /// else should reach for this.
    ///
    /// Removal is memory-safe here for the same reasons it is in
    /// [`Self::close_realm`]: `perform_gc` rebuilds its root vector per
    /// collection (`extend_persistent_roots`), so nothing holds an index
    /// across collections. It does **not** touch `OldSpace::slots` — the `Box`
    /// cell stays allocated for the machine's life, so an already-compiled
    /// fragment that `iconst`ed that address still `load`s. What is released
    /// is the root's place in the GC TRACE LIST, not its bytes; see
    /// `tidepool-codegen/CLAUDE.md`'s root-accounting section for the honest
    /// bound.
    ///
    /// Idempotent (the underlying deregistration is a `Vec::remove` by
    /// position, a no-op if absent), but a caller relying on that is a caller
    /// violating the exactly-once clause above.
    pub fn retire_scope_root(&mut self, slot: crate::old_space::RootSlot) {
        self.machine_state.deregister_persistent_root(slot.addr());
    }

    /// Whether any LIVE [`ValueHandle`] still holds `slot` — the
    /// handle-registry half of [`Self::retire_scope_root`]'s sole-ownership
    /// clause, so a retirement site can debug-assert it rather than assume it.
    ///
    /// A mount transfers ownership out of the handle registry
    /// (`release_handle`) before the value plane records the binding, so this
    /// answers `false` for every properly-mounted root; a `true` means the
    /// handle registry and the value plane both believe they own the slot,
    /// which is the state retirement must not act on.
    #[must_use]
    pub fn handle_holds_root(&self, slot: crate::old_space::RootSlot) -> bool {
        self.value_handles
            .values()
            .any(|e| std::ptr::eq(e.slot.addr(), slot.addr()))
    }
}

/// RAII proof that a nested child is running against a suspended parent.
/// On drop it reads the (GC-current) continuation pointer back
/// out of the heap-stable stowed cell and restores it into the machine's
/// `suspended_continuation`, deregisters the stowed root, drops the cell, and
/// decrements the nested-child depth — leaving the machine exactly as suspended
/// as it was on entry, but with the continuation pointer updated to wherever the
/// child's collections relocated it.
///
/// All four raw pointers point into the owning `JitEffectMachine`. The guard is
/// a local in `run_child_fragment*` and drops at that method's end, strictly
/// within the method's `&mut self` scope — so `self` cannot have moved or
/// dropped while the guard is alive. This mirrors `RegistryGuard`, which
/// likewise holds raw pointers into its owning call frame rather than a borrow
/// (so `self` stays free for the run call it wraps).
struct NestedChildGuard {
    machine_state: *const MachineState,
    suspended_continuation: *mut Option<*mut u8>,
    stowed_root_cell: *mut Option<Box<*mut u8>>,
    nested_child_depth: *mut usize,
    slot: *mut *mut u8,
}

impl Drop for NestedChildGuard {
    fn drop(&mut self) {
        // SAFETY: all pointers target the owning JitEffectMachine's fields,
        // live for the guard's whole scope (the guard is a local in the child
        // run method, which holds `&mut self`). The stowed cell holds the
        // GC-current continuation pointer (rewritten in place by any child
        // collection through the registered slot); read it back out and restore
        // it so the parent stays suspended on the relocated continuation.
        unsafe {
            (*self.machine_state).deregister_stowed_root(self.slot);
            #[allow(
                clippy::expect_used,
                reason = "stowed cell present for the guard's life"
            )]
            let cell = (*self.stowed_root_cell)
                .take()
                .expect("stowed cell present for the guard's life");
            *self.suspended_continuation = Some(*cell);
            debug_assert!(
                *self.nested_child_depth > 0,
                "nested_child_depth underflow — double drop"
            );
            *self.nested_child_depth = (*self.nested_child_depth).saturating_sub(1);
        }
    }
}

impl Drop for JitEffectMachine {
    fn drop(&mut self) {
        // REALM PROTOTYPE: deregister every parked continuation's stowed root
        // BEFORE its `Box` cell is freed (the `HashMap` drops with `self` after
        // this body returns). `free_session_heap` below also clears stowed
        // roots, but only on a session machine — doing it here makes the
        // "registered from park until resume, and no longer" invariant hold on
        // every drop path.
        for (_, mut frame) in self.continuations.drain() {
            let slot: *mut *mut u8 = &mut *frame.cell;
            self.machine_state.deregister_stowed_root(slot);
        }
        self.assert_rooting_receipt();
        // Clear this machine's persistent-root registry (whose slots point
        // into the session heap Vec, which drops with self after this).
        // Harmless for one-shot machines (free_session_heap does nothing if
        // GC state is already absent, and no persistent roots are
        // registered). Operates directly on self.machine_state — always
        // clears exactly this machine's own registries, never a different
        // one (see the `free_session_heap` doc on `MachineState`).
        if self.session.is_some() {
            // Retire every old-space arena BEFORE the arena Vec<u8>s
            // themselves drop (which happens when `self.session` drops,
            // after this fn body returns): forgets any remembered write-
            // barrier slot pointing into that arena so it never outlives the
            // memory it points into, and deregisters the range so a
            // diagnostic pass never reads freed memory as live old-space.
            for (start, end) in self.machine_state.old_space_arena_ranges() {
                self.machine_state.retire_old_space_arena(start, end);
            }
            self.machine_state.free_session_heap();
        }
    }
}

/// Resolve pending tail calls with signal protection.
///
/// # Safety
/// vmctx must have valid tail_callee/tail_arg if non-null.
unsafe fn resolve_tail_calls_protected(
    vmctx: &mut VMContext,
    result: *mut u8,
) -> Result<*mut u8, JitError> {
    let mut ptr = result;
    while ptr.is_null() && !vmctx.tail_callee.is_null() {
        // External cancellation safepoint — see the rationale in
        // `host_fns::trampoline_resolve`. Without this check, an infinite
        // tail-recursive loop never yields control back to the caller even
        // when cancellation has been requested.
        if crate::host_fns::check_cancel_and_set_error(vmctx) {
            vmctx.tail_callee = std::ptr::null_mut();
            vmctx.tail_arg = std::ptr::null_mut();
            ptr = crate::host_fns::error_poison_ptr();
            break;
        }

        let callee = vmctx.tail_callee;
        let arg = vmctx.tail_arg;
        vmctx.tail_callee = std::ptr::null_mut();
        vmctx.tail_arg = std::ptr::null_mut();
        machine_state(vmctx).reset_call_depth();
        let code_ptr =
            *(callee.add(crate::layout::CLOSURE_CODE_PTR_OFFSET as usize) as *const usize);
        let func: unsafe extern "C" fn(*mut VMContext, *mut u8, *mut u8) -> *mut u8 =
            std::mem::transmute(code_ptr);
        ptr = crate::signal_safety::with_signal_protection(|| func(vmctx, callee, arg))
            .map_err(|e| JitError::Yield(runtime_error_or_signal(e.0)))?;
    }
    Ok(ptr)
}

/// Normalized effect-response materialization: a Value to convert eagerly,
/// or an already-materialized heap pointer (the iterative list path).
/// Shared by the one effect-drive loop below.
enum ResponsePlan {
    Eager(tidepool_eval::value::Value),
    Ready(*mut u8),
}

/// Outcome of the shared effect step loop ([`drive_effect_loop`]): the turn
/// completed with a Done heap pointer, or it SUSPENDED at the caller's
/// `suspend_tag` (threadless suspension). `Suspended` carries the bridged
/// request `Value` (the caller extracts prompt/meta) and the raw continuation
/// heap pointer (the caller stows it; the machine's session heap is retained
/// across the suspension). The non-suspend callers pass `suspend_tag = None`
/// and never observe `Suspended`.
enum DriveOutcome {
    Done(*mut u8),
    Suspended {
        request: tidepool_eval::value::Value,
        /// The raw heap pointer to the request `Con` (rooted for the arm). A
        /// `finalize`'s value field crosses by reference, so `finish_suspendable`
        /// reaches back into this Con to tenure the finalized value when the
        /// bridged `request` carries a [`heap_bridge::CLOSURE_SENTINEL`] placeholder.
        request_ptr: *mut u8,
        continuation: *mut u8,
    },
}

/// Whether a bridged suspend request carries a [`heap_bridge::CLOSURE_SENTINEL`]
/// placeholder among its top-level Con fields — the tolerant bridge's marker
/// that a field was a live closure it declined to materialize. Only the
/// direct fields of the request Con are checked: a `finalize`'s value is field
/// 1 of `FinalizeWith`, and no other suspend request (`Ask`/`RunLLMTurn`) can
/// legally contain a closure, so a nested sentinel would itself be a bug.
///
/// DEEP scan (codex review 2026-08-12, finding 3): a closure NESTED inside a
/// finalized product — a record of functions, a pair of lenses — must trigger
/// by-reference tenure exactly like a top-level closure, or the payload takes
/// the lossy bridge path and its nested closures arrive as sentinels. The
/// request's own Con head is skipped (only its FIELDS can carry the payload);
/// everything below is walked by [`heap_bridge::contains_closure_sentinel`].
fn request_carries_closure_sentinel(request: &tidepool_eval::value::Value) -> bool {
    match request {
        tidepool_eval::value::Value::Con(_, fields) => {
            fields.iter().any(heap_bridge::contains_closure_sentinel)
        }
        _ => false,
    }
}

/// A suspendable turn's outcome, generic over what its
/// result-materialization policy actually PRODUCES on completion. The
/// projected/render entries return their real product — N tenured roots, or
/// a root paired with a render — rather than inventing a `Value` to satisfy
/// a fixed shape.
pub enum Suspendable<T> {
    /// The turn ran to completion, producing `T`.
    Completed(T),
    /// The turn suspended at the ask boundary; the machine holds the
    /// continuation internally.
    Suspended {
        request: tidepool_eval::value::Value,
        /// Finalize-by-reference: `true` when the suspend request was a
        /// `finalize @T closure` — the finalized VALUE (field 1 of the request
        /// Con) has been tenured into old-space and its persistent root slot
        /// stashed on the machine ([`JitEffectMachine::take_finalized_root`]).
        /// The bridged `request` carries a [`heap_bridge::CLOSURE_SENTINEL`] in
        /// that field's place. `false` for an ordinary `Ask`/`RunLLMTurn`
        /// suspension (or a `finalize` of a plain DATA value, which bridges
        /// fully and needs no by-reference handoff).
        has_finalized_closure: bool,
    },
}

/// Result of a suspendable turn ([`JitEffectMachine::run_suspendable`] /
/// [`JitEffectMachine::resume_suspended`]): the turn produced a bridged
/// `Value`, or it suspended at the ask boundary carrying the bridged request
/// (the continuation is stowed inside the machine, ready for
/// `resume_suspended`). The `T = Value` specialization of [`Suspendable`],
/// kept as its own name because it predates the generic and has consumers
/// matching on it.
pub type SuspendableOutcome = Suspendable<tidepool_eval::value::Value>;

/// How a suspended turn is re-entered ([`JitEffectMachine::resume_suspended`]).
pub enum ResumeInput {
    /// Feed the (already-validated, bridged) answer value into the suspended
    /// ask and continue driving.
    Answer(tidepool_eval::value::Value),
    /// Feed a machine-side rooted heap value — a [`ValueHandle`] minted on
    /// THIS machine — as the answer, WITHOUT materializing a bridged
    /// [`tidepool_eval::value::Value`] into the heap: the handle's GC-current
    /// pointer is the response, verbatim. This is how a closure (or any
    /// opaque value) is DELIVERED into a sibling continuation on the same
    /// heap (one-session plan, pillar B). No A5 NF-force applies: the payload
    /// is already a real heap value whose thunks are ordinary lazy structure,
    /// not a bridged answer that could smuggle a bottom past validation. The
    /// handle is NOT consumed (scope-owned borrow; released by
    /// [`JitEffectMachine::close_realm`]). An unknown/released handle is a
    /// clean typed error before anything runs.
    Handle(ValueHandle),
    /// Abort the suspended ask WITHOUT running the continuation (a stowed
    /// machine has no thread) — returns `JitError::Effect(EffectError::
    /// Handler("ask aborted by caller: {reason}"))` directly, the same
    /// terminal outcome a pre-E2 caller-abort produced. This does NOT touch
    /// the first-cause cell / record `RuntimeError::Cancelled` — that cause
    /// is reserved for the gate/timeout abort path, not a caller-supplied
    /// abort reason.
    Abort(String),
}

/// Drive the freer-simple effect step loop to `Yield::Done`: step the machine,
/// bridge + dispatch each effect request, materialize the response (lazy park
/// or eager), and resume — returning the final Done heap pointer for the
/// caller's epilogue (value bridging in `run_with_entry`; deep-force/tenure in
/// the `*_and_bind` variants; tuple projection in `_projected`).
///
/// This is THE effect-boundary body, shared by all three run methods — they
/// differ only in their Done epilogues, exec-context labels, and
/// reclaim-arming position. It owns the continuation GC-rooting, the request
/// bridge + runtime-error precedence, the effect-dispatch cancellation
/// safepoint, and the Stream/Complete response planning with the lazy-spine
/// re-park.
fn drive_to_done<U, H: DispatchEffect<U>>(
    machine: &mut CompiledEffectMachine,
    cancel_flag: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    table: &DataConTable,
    handlers: &mut H,
    user: &U,
    exec_start: &str,
    resume_suffix: &str,
) -> Result<*mut u8, JitError> {
    let yield_result = initial_step(machine, exec_start);
    // suspend_tag = None: the ask-suspend branch is never taken, so this drives
    // the plain non-suspend path. Threadless suspension is opt-in via
    // `JitEffectMachine::run_suspendable`, which passes `Some(ask_tag)`.
    match drive_effect_loop(
        machine,
        cancel_flag,
        table,
        handlers,
        user,
        resume_suffix,
        None,
        yield_result,
    )? {
        DriveOutcome::Done(ptr) => Ok(ptr),
        DriveOutcome::Suspended { .. } => {
            unreachable!("drive_to_done passes suspend_tag=None; the effect loop never suspends")
        }
    }
}

/// The initial `machine.step()` for a fresh drive: reset call depth, set the
/// exec-context label, step under signal protection. Shared by
/// [`drive_to_done`] and [`JitEffectMachine::run_suspendable`].
fn initial_step(machine: &mut CompiledEffectMachine, exec_start: &str) -> Yield {
    // SAFETY: machine.vmctx_mut()'s machine_state was set by the caller before
    // entering the effect loop.
    unsafe { machine_state(machine.vmctx_mut() as *mut VMContext) }.reset_call_depth();
    crate::host_fns::set_exec_context(exec_start);
    // SAFETY: with_signal_protection wraps the JIT call with sigsetjmp for
    // crash recovery; machine.step() calls the JIT function through a valid
    // function pointer.
    match unsafe { crate::signal_safety::with_signal_protection(|| machine.step()) } {
        Ok(y) => y,
        Err(e) => signal_error_to_yield(e),
    }
}

/// The shared freer-simple effect step loop, factored out of [`drive_to_done`]
/// so the same body serves the non-suspending run AND threadless suspension.
///
/// `suspend_tag = Some(t)`: a `Yield::Request` with `tag >= t` unwinds as
/// [`DriveOutcome::Suspended`] (after bridging the request and while the
/// continuation is still valid), instead of dispatching to a handler. `t` is
/// the first INTERPOSED (unhandled) tag — every tag from there on is
/// unhandled by construction, so this threshold test is what lets
/// `Ask`/`RunLLMTurn`/`Finalize` share this one suspend arm without each
/// needing its own comparison.
/// `suspend_tag = None`: every effect dispatches through the plain,
/// non-suspend path.
///
/// `yield_result` is the entry Yield: a fresh [`initial_step`] for a new turn,
/// or a `machine.resume(..)` of the stowed continuation for a re-entry.
#[allow(clippy::too_many_arguments)]
fn drive_effect_loop<U, H: DispatchEffect<U>>(
    machine: &mut CompiledEffectMachine,
    cancel_flag: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    table: &DataConTable,
    handlers: &mut H,
    user: &U,
    resume_suffix: &str,
    suspend_tag: Option<u64>,
    mut yield_result: Yield,
) -> Result<DriveOutcome, JitError> {
    loop {
        match yield_result {
            Yield::Done(ptr) => return Ok(DriveOutcome::Done(ptr)),
            Yield::Request {
                tag,
                request,
                continuation,
            } => {
                // Root the continuation for the whole arm: request-forcing
                // (heap_force runs thunk code that can allocate → GC) and
                // response materialization (host_alloc_gc in
                // materialize_cons_list / build_cons_cells) can collect
                // while the JIT stack is unwound — an UNROOTED continuation
                // tree is not evacuated and from-space is freed, so
                // `machine.resume(continuation, …)` would read freed memory.
                // The GC rewrites the rooted slot in place; resume reads the
                // updated pointer.
                let mut continuation = continuation;
                let vmctx_ptr = machine.vmctx_mut() as *mut VMContext;
                // SAFETY: vmctx_ptr is the active run's VMContext.
                let _cont_root = unsafe { heap_bridge::RootScope::new(vmctx_ptr) };
                // SAFETY: the slot lives on this frame until the arm ends
                // (after resume); _cont_root truncates the registry on drop.
                unsafe {
                    crate::host_fns::register_rust_root(
                        vmctx_ptr,
                        &mut continuation as *mut *mut u8,
                    );
                }
                // Root the raw request pointer too, alongside the continuation:
                // a suspended `finalize @T closure` passes the finalized value
                // by REFERENCE, so the harness reaches
                // BACK into this request Con's value field live after the suspend.
                // The bridge below may itself GC (thunk forcing); registering the
                // request slot keeps field(1)'s subtree evacuated + GC-updated.
                let mut request = request;
                // SAFETY: the slot lives on this frame until the arm ends; the
                // request subtree is also reachable from the rooted continuation,
                // so it survives the suspension regardless.
                unsafe {
                    crate::host_fns::register_rust_root(vmctx_ptr, &mut request as *mut *mut u8);
                }
                // SAFETY: request is a valid heap pointer from the JIT effect dispatch.
                // A suspend request uses the TOLERANT bridge: a `finalize`'s value
                // field may be a closure (`TAG_CLOSURE`), which has no data `Value`
                // representation — the tolerant bridge substitutes a placeholder
                // (`CLOSURE_SENTINEL`) so the leading `site`/`prompt` fields still
                // bridge for the classifier, while the real closure crosses by
                // reference (`request_ptr` below). `Ask`/`RunLLMTurn` requests carry
                // no closure, so the policy is behavior-preserving for them.
                let bridge_res = unsafe {
                    let vmctx_ptr = machine.vmctx_mut() as *mut VMContext;
                    crate::signal_safety::with_signal_protection(|| {
                        heap_bridge::heap_to_value_forcing_tolerant(request, vmctx_ptr)
                    })
                }
                .map_err(JitError::Signal)?;
                // Request forcing can record a first cause (e.g. a cancel in
                // `gc_trigger`); the bridge outcome is only its symptom.
                let req_val =
                    crate::host_fns::surface_error(bridge_res.map_err(JitError::HeapBridge))?;
                log::debug!(target: "tidepool::effects", "effect tag={} request={:?}", tag, req_val);
                // Threadless suspension: `suspend_tag` is the FIRST interposed
                // (unhandled) tag — the position right after the last effect with
                // a real handler. Every tag at or beyond it is unhandled by
                // construction (Ask, and RunLLMTurn/Finalize, always appended
                // consecutively after the handled stack), so the threshold test
                // `tag >= suspend_tag` catches ALL of them through this one arm:
                // no per-effect duplication, `Ask`/`RunLLMTurn`/`Finalize` share
                // this exact suspend path. A stack with only one interposed
                // effect (today's ordinary eval/repl stacks) has `suspend_tag`
                // as the only tag >= it, so this is behavior-preserving there.
                // Unwind carrying the bridged request + the
                // continuation instead of dispatching. `continuation` here is the
                // post-request-bridge value (the arm's `register_rust_root`
                // updated it in place through any GC during forcing). The arm's
                // `_cont_root` drops as we return, releasing the run-scoped root;
                // the raw pointer stays valid because the session heap buffer is
                // retained across the suspension (no GC runs while stowed), and
                // `resume_suspended` re-roots it before its answer materialization
                // can collect.
                if suspend_tag.is_some_and(|t| tag >= t) {
                    return Ok(DriveOutcome::Suspended {
                        request: req_val,
                        request_ptr: request,
                        continuation,
                    });
                }
                let cx = EffectContext::with_user(table, user);
                // A dispatcher that aborts at its `PauseGate` checkpoint
                // records `RuntimeError::Cancelled` as the first cause before
                // returning `EffectError::Handler`, so a gate-fired timeout
                // surfaces the same cause as a flag-fired one. Ordinary
                // handler errors record no cause and pass through unchanged.
                let response = crate::host_fns::surface_error(
                    handlers
                        .dispatch(tag, &req_val, &cx)
                        .map_err(JitError::from),
                )?;

                // External cancellation safepoint at the effect-dispatch
                // boundary. The handler we just called may itself have flipped
                // the cancel flag (a watchdog handler is the canonical case);
                // the JIT-internal safepoints (gc_trigger, trampoline_resolve)
                // only fire on tail-recursive or heavy-allocating Haskell, so
                // freer-simple effect loops would otherwise observe the cancel
                // only as an eventual unrelated error. Checking here gives
                // prompt unwind for the realistic handler-driven scenario
                // without depending on the shape of the compiled program.
                if cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
                    return Err(JitError::Yield(crate::yield_type::YieldError::Runtime(
                        crate::host_fns::RuntimeError::Cancelled,
                    )));
                }

                // Materialize the handler response and resume the continuation.
                // Extracted so `resume_suspended` re-enters a stowed turn
                // through the identical path (see the helper's doc).
                yield_result = materialize_response_and_resume(
                    machine,
                    continuation,
                    ResumePayload::Response(response),
                    tag,
                    resume_suffix,
                )?;
            }
            Yield::Error(e) => return Err(JitError::Yield(e)),
        }
    }
}

/// What a resume feeds the continuation: a bridged handler
/// [`tidepool_effect::Response`] to MATERIALIZE into the heap, or an
/// already-in-heap pointer (a [`ValueHandle`]'s persistent-rooted payload)
/// used VERBATIM — the delivery half of pillar B's "delivery by handle,
/// observation by serialization".
enum ResumePayload {
    Response(tidepool_effect::Response),
    HeapPtr(*mut u8),
}

/// Materialize a resume payload into a heap pointer and resume the machine's
/// `continuation` with it, returning the next [`Yield`].
///
/// Shared so [`JitEffectMachine::resume_suspended`] re-enters a stowed turn
/// through the EXACT same materialization path (lazy `Stream` park,
/// long-spine re-park, eager `value_to_heap`) — one body, no drift-prone
/// second copy. A [`ResumePayload::HeapPtr`] skips materialization entirely
/// (the pointer is already a rooted heap object).
///
/// `continuation` is GC-rooted here for the duration: response materialization
/// (`value_to_heap` / `materialize_cons_list`) can
/// allocate and collect, which would move the continuation out from under the
/// `machine.resume` below. The in-loop caller also holds its own arm root
/// across request forcing; this extra registration harmlessly overlaps it
/// (both slots track the same pointer through a GC).
fn materialize_response_and_resume(
    machine: &mut CompiledEffectMachine,
    mut continuation: *mut u8,
    response: ResumePayload,
    tag: u64,
    resume_suffix: &str,
) -> Result<Yield, JitError> {
    let vmctx_ptr = machine.vmctx_mut() as *mut VMContext;
    // SAFETY: vmctx_ptr is the active run's VMContext; the slot lives on this
    // frame until _root truncates the registry on drop.
    let _root = unsafe { heap_bridge::RootScope::new(vmctx_ptr) };
    // SAFETY: &mut continuation is a stable stack slot for the duration below.
    unsafe {
        crate::host_fns::register_rust_root(vmctx_ptr, &mut continuation as *mut *mut u8);
    }

    // Response materialization. A list response (or a long list spine
    // inside a Complete value) goes through ITERATIVE dismantle +
    // `materialize_cons_list` — a deep spine must never reach a recursive
    // Drop or recursive value_to_heap (~3 stack frames per cell overflow
    // the eval thread; the fault lands outside signal protection and
    // silently kills the thread). Everything else converts eagerly via
    // value_to_heap. The node cap bounds every channel.
    const LONG_SPINE_THRESHOLD_NODES: usize = 2_000;
    const MAX_EFFECT_RESPONSE_NODES: usize = 100_000;

    let plan = match response {
        // A ValueHandle's payload: already a real (old-space, persistent-
        // rooted) heap object — no materialization, no size caps, the pointer
        // IS the response. This is pillar B's delivery path: a closure
        // crosses into the continuation verbatim, where the eager bridge
        // would have substituted CLOSURE_SENTINEL.
        ResumePayload::HeapPtr(p) => ResponsePlan::Ready(p),
        ResumePayload::Response(tidepool_effect::Response::List {
            items,
            cons_id,
            nil_id,
        }) => {
            let nodes = 3 * items.len() + items.iter().map(|v| v.node_count()).sum::<usize>();
            if nodes > MAX_EFFECT_RESPONSE_NODES {
                return Err(JitError::EffectResponseTooLarge {
                    nodes,
                    limit: MAX_EFFECT_RESPONSE_NODES,
                });
            }
            let p = unsafe {
                crate::signal_safety::with_signal_protection(|| {
                    crate::host_fns::materialize_cons_list(
                        machine.vmctx_mut(),
                        cons_id.0,
                        nil_id.0,
                        &items,
                    )
                })
            }
            .map_err(JitError::Signal)?;
            if let Some(err) = crate::host_fns::take_runtime_error() {
                return Err(JitError::Yield(crate::yield_type::YieldError::from(err)));
            }
            ResponsePlan::Ready(p)
        }
        ResumePayload::Response(tidepool_effect::Response::Complete(resp_val)) => {
            let spine =
                probe_list_spine(&resp_val).filter(|&(_, _, len)| len > LONG_SPINE_THRESHOLD_NODES);
            match spine {
                Some((cons_tag, nil_tag, len)) => {
                    let items = dismantle_list_spine(resp_val, len);
                    let nodes = 3 * len + items.iter().map(|v| v.node_count()).sum::<usize>();
                    if nodes > MAX_EFFECT_RESPONSE_NODES {
                        return Err(JitError::EffectResponseTooLarge {
                            nodes,
                            limit: MAX_EFFECT_RESPONSE_NODES,
                        });
                    }
                    let p = unsafe {
                        crate::signal_safety::with_signal_protection(|| {
                            crate::host_fns::materialize_cons_list(
                                machine.vmctx_mut(),
                                cons_tag,
                                nil_tag,
                                &items,
                            )
                        })
                    }
                    .map_err(JitError::Signal)?;
                    if let Some(err) = crate::host_fns::take_runtime_error() {
                        return Err(JitError::Yield(crate::yield_type::YieldError::from(err)));
                    }
                    ResponsePlan::Ready(p)
                }
                None => ResponsePlan::Eager(resp_val),
            }
        }
    };
    let resp_ptr = match plan {
        ResponsePlan::Ready(p) => p,
        ResponsePlan::Eager(resp_val) => {
            let nodes = resp_val.node_count();
            if nodes > MAX_EFFECT_RESPONSE_NODES {
                return Err(JitError::EffectResponseTooLarge {
                    nodes,
                    limit: MAX_EFFECT_RESPONSE_NODES,
                });
            }
            // SAFETY: Converting a Value back to a heap object in the
            // nursery, with one GC-and-retry via the shared `gc_retry`
            // helper (matching every other value_to_heap call site:
            // primops.rs eitherDecode/parseISO8601, streaming.rs
            // build_cons_cells/stream_element): `continuation` is already a
            // registered rust_root (above), so the retry's collection
            // evacuates it safely, and a transient nursery-full at
            // response-materialization time is recoverable rather than
            // fatal.
            let conv = unsafe {
                crate::signal_safety::with_signal_protection(|| {
                    heap_bridge::gc_retry(
                        vmctx_ptr,
                        |r: &Result<*mut u8, heap_bridge::BridgeError>| {
                            matches!(r, Err(heap_bridge::BridgeError::NurseryExhausted))
                        },
                        || heap_bridge::value_to_heap(&resp_val, machine.vmctx_mut()),
                    )
                })
            }
            .map_err(JitError::Signal)?;
            match conv {
                Ok(p) => p,
                Err(e) => return Err(JitError::HeapBridge(e)),
            }
        }
    };
    // SAFETY: as above.
    unsafe { machine_state(machine.vmctx_mut() as *mut VMContext) }.reset_call_depth();
    crate::host_fns::set_exec_context(&format!(
        "resuming after effect tag={}{}",
        tag, resume_suffix
    ));
    // SAFETY: continuation and resp_ptr are valid nursery heap pointers.
    // resume applies the continuation tree to the response.
    Ok(
        match unsafe {
            crate::signal_safety::with_signal_protection(|| machine.resume(continuation, resp_ptr))
        } {
            Ok(y) => y,
            Err(e) => signal_error_to_yield(e),
        },
    )
}

/// Signal-boundary adapter for `host_fns::surface_error`: the raw signal is
/// the symptomatic fallback (a first cause like `BadFunPtrTag` is recorded by
/// `debug_app_check` before the JIT crashes and outranks it). Names the
/// faulting JIT function in the diagnostics when the fault address is known.
fn runtime_error_or_signal(sig: i32) -> crate::yield_type::YieldError {
    let fault_addr = crate::signal_safety::FAULTING_ADDR.with(|c| c.get());
    if fault_addr != 0 {
        if let Some(name) = crate::debug::lookup_lambda_by_address(fault_addr) {
            crate::host_fns::push_diagnostic(format!(
                "Signal {} in JIT function: {} (addr=0x{:x})",
                sig, name, fault_addr
            ));
        }
    }
    crate::host_fns::surface_error::<std::convert::Infallible, _>(Err(
        crate::yield_type::YieldError::Signal(sig),
    ))
    .unwrap_err()
}

/// Detect a cons-list spine by reference: a chain of 2-field Cons sharing one
/// DataConId, terminated by a 0-field Con. Returns (cons_tag, nil_tag, len).
/// Tags are read from the spine itself — no DataConTable lookup needed.
/// Iterative, walks the full spine to validate the terminator.
fn probe_list_spine(val: &tidepool_eval::value::Value) -> Option<(u64, u64, usize)> {
    use tidepool_eval::value::Value;
    let mut len = 0usize;
    let mut cons_tag: Option<u64> = None;
    let mut cur = val;
    loop {
        match cur {
            Value::Con(id, fields) if fields.len() == 2 => {
                match cons_tag {
                    None => cons_tag = Some(id.0),
                    Some(t) if t == id.0 => {}
                    Some(_) => return None, // mixed 2-field constructors: not a list
                }
                len += 1;
                cur = &fields[1];
            }
            Value::Con(id, fields) if fields.is_empty() => {
                return cons_tag.map(|c| (c, id.0, len));
            }
            _ => return None,
        }
    }
}

/// Dismantle a probe-validated cons spine BY VALUE: each element is moved out
/// and each cell freed iteratively, one at a time. This is the load-bearing
/// detail — letting a deep spine hit `Value`'s recursive destructor costs ~3
/// stack frames per cons cell, which overflows the eval thread's stack on
/// responses past a few thousand elements (SIGSEGV outside signal protection
/// → silent thread exit → caller hang).
fn dismantle_list_spine(
    val: tidepool_eval::value::Value,
    len: usize,
) -> Vec<tidepool_eval::value::Value> {
    use tidepool_eval::value::Value;
    let mut items = Vec::with_capacity(len);
    let mut cur = val;
    loop {
        // `ref mut` + pop: Value implements Drop, so fields can't move out
        // by pattern. (Value's Drop is itself iterative, so even handing a
        // deep spine to the destructor is safe now — this dismantle just
        // avoids building the worklist twice.)
        match cur {
            Value::Con(_, ref mut fields) if fields.len() == 2 => {
                #[allow(clippy::expect_used, reason = "len checked")]
                let tail = fields.pop().expect("len checked");
                #[allow(clippy::expect_used, reason = "len checked")]
                let head = fields.pop().expect("len checked");
                items.push(head);
                // The emptied cell (and its Vec) drops shallowly here.
                cur = tail;
            }
            // Probe validated the terminator: nothing deep remains.
            _ => break,
        }
    }
    items
}

fn signal_error_to_yield(e: crate::signal_safety::SignalError) -> Yield {
    Yield::Error(runtime_error_or_signal(e.0))
}

/// A5 — the deepseq-style NF check on a data-kinded resume answer.
///
/// A bridged answer `Value` is produced by `heap_to_value_forcing`, which forces
/// each node to WHNF as it walks — so a genuine bottom (`undefined`/`⊥`, a lazy
/// poison closure) is already raised at that bridge boundary as a `JitError`,
/// never reaching this point as a `Value`. This walk is the defense-in-depth
/// backstop the spec mandates: it rejects any answer carrying a residual
/// **unforced thunk** (`ThunkRef`) — the shape a not-fully-forced bottom would
/// take — as a retryable error, so the caller's continuation is NOT consumed.
///
/// Iterative (explicit work stack — data can be arbitrarily deep) with an
/// address-keyed visited set on `Con` payloads so shared/cyclic data terminates.
/// `Con`/`Lit`/`ByteArray` are normal-form data; `ThunkRef` is a bottom-reject;
/// `Closure`/`JoinCont`/`ConFun` cannot occur in a data-kinded answer
/// (function-bearing types are rejected at extract) but are treated
/// as a reject too, since they are not first-order NF data.
fn answer_force_nf(root: &tidepool_eval::value::Value) -> Result<(), String> {
    use tidepool_eval::value::Value;
    let mut work: Vec<&Value> = vec![root];
    let mut visited: std::collections::HashSet<*const Vec<Value>> =
        std::collections::HashSet::new();
    while let Some(v) = work.pop() {
        match v {
            Value::Lit(_) | Value::ByteArray(_) => {}
            Value::Con(_, fields) => {
                // Dedup shared/cyclic sub-graphs by the payload Vec's address.
                if visited.insert(fields as *const Vec<Value>) {
                    for f in fields {
                        work.push(f);
                    }
                }
            }
            Value::ThunkRef(id) => {
                return Err(format!("unforced thunk {id} in answer"));
            }
            Value::Closure { .. } => {
                return Err("function-bearing value (closure) in answer".to_string());
            }
            Value::JoinCont { .. } => {
                return Err("join-point value in answer".to_string());
            }
            Value::ConFun(id, arity, args) => {
                return Err(format!(
                    "partially-applied constructor (Con#{} {}/{}) in answer",
                    id.0,
                    args.len(),
                    arity
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::yield_type::YieldError;
    use serial_test::serial;

    /// Regression test: when a RuntimeError is pending and a signal fires,
    /// prefer the RuntimeError (more specific) over the raw signal number.
    /// This prevents "JIT signal: unknown signal" when the real cause is
    /// something like BadFunPtrTag(255).
    #[test]
    fn test_runtime_error_preferred_over_signal() {
        crate::machine_state::test_support::with_test_machine(|| {
            // Set a pending runtime error via public API (kind=0 = DivisionByZero)
            crate::host_fns::runtime_error(0);

            // Signal fires after the runtime error was set
            let err = runtime_error_or_signal(libc::SIGBUS);

            // Should get DivisionByZero, not Signal(SIGBUS)
            assert_eq!(
                err,
                YieldError::Runtime(crate::host_fns::RuntimeError::DivisionByZero)
            );
        });
    }

    /// When no RuntimeError is pending, the signal number comes through.
    #[test]
    fn test_signal_passthrough_without_runtime_error() {
        // Ensure no pending error
        crate::host_fns::take_runtime_error();

        let err = runtime_error_or_signal(libc::SIGILL);
        assert_eq!(err, YieldError::Signal(libc::SIGILL));
    }

    #[test]
    fn test_varid_check_rejects_duplicate_toplevel_binder() {
        use tidepool_repr::tree::RecursiveTree;
        use tidepool_repr::types::Literal;
        use tidepool_repr::{CoreFrame, VarId};

        // let v1 = 1 in let v1 = 2 in v1
        let expr = RecursiveTree {
            nodes: vec![
                CoreFrame::Lit(Literal::LitInt(1)), // 0
                CoreFrame::Lit(Literal::LitInt(2)), // 1
                CoreFrame::Var(VarId(1)),           // 2
                CoreFrame::LetNonRec {
                    binder: VarId(1),
                    rhs: 1,
                    body: 2,
                }, // 3
                CoreFrame::LetNonRec {
                    binder: VarId(1),
                    rhs: 0,
                    body: 3,
                }, // 4 (root)
            ],
        };
        let table = DataConTable::new();

        let res = JitEffectMachine::compile(&expr, &table, 1 << 20);
        assert!(
            matches!(res, Err(JitError::VarIdCollision(_))),
            "Expected VarIdCollision, got success"
        );
    }

    /// `suspended_continuation` is not a GC root, so every plain run entry
    /// must assert it is `None` (the L7 assert) rather than rely on external
    /// discipline. Rather than drive a real `Ask`-boundary suspension (heavy
    /// effect-machine setup), this directly sets the private field to
    /// simulate "already suspended" and confirms each entry's assert fires —
    /// a clean panic, not silent corruption of a live continuation.
    #[test]
    fn run_entries_assert_when_a_continuation_is_already_suspended() {
        use tidepool_repr::tree::RecursiveTree;
        use tidepool_repr::types::Literal;
        use tidepool_repr::CoreFrame;

        let expr = RecursiveTree {
            nodes: vec![CoreFrame::Lit(Literal::LitInt(42))],
        };
        let table = DataConTable::new();
        let mut machine = JitEffectMachine::compile_session(&expr, &table, 1 << 16)
            .expect("compile_session failed");
        let func_id = machine.func_id;

        macro_rules! assert_panics_while_suspended {
            ($name:expr, $call:expr) => {
                machine.suspended_continuation = Some(std::ptr::null_mut());
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe($call));
                assert!(
                    result.is_err(),
                    "{} must panic while a continuation is suspended (L7)",
                    $name
                );
                machine.suspended_continuation = None;
            };
        }

        assert_panics_while_suspended!("run_pure", || {
            let _ = machine.run_pure();
        });
        assert_panics_while_suspended!("run_fragment_pure", || {
            let _ = machine.run_fragment_pure(func_id);
        });
        assert_panics_while_suspended!("run_pure_and_bind", || {
            let _ = machine.run_pure_and_bind(func_id);
        });
    }

    // ---------------------------------------------------------------------------
    // Session seam tests
    // ---------------------------------------------------------------------------

    /// Build a Con-chain expr + DataConTable that forces >=1 GC under a small nursery.
    ///
    /// INVARIANT: cfg(test) code in this crate must NOT reference tidepool-testing.
    /// tidepool-testing links its own (non-test) copy of tidepool-codegen, so if this
    /// crate's lib-test binary pulls tidepool-testing in, it ends up linking TWO
    /// copies of tidepool-codegen's `#[no_mangle] extern "C"` host fns (the JIT ABI
    /// requires unmangled names) — a hard duplicate-symbol link error. Integration
    /// tests under tests/*.rs don't have this problem (they're a separate binary that
    /// links tidepool-codegen only once, without `--test` on it), so they're free to
    /// share tidepool_testing::gen::make_gc_forcing_setup; this cfg(test) helper can't.
    fn make_gc_forcing_setup(
        depth: usize,
    ) -> (tidepool_repr::CoreExpr, tidepool_repr::DataConTable) {
        use tidepool_repr::datacon::DataCon;
        use tidepool_repr::types::{DataConId, Literal, VarId};
        use tidepool_repr::{CoreFrame, DataConTable, TreeBuilder};

        let mut bld = TreeBuilder::new();
        let var_x = bld.push(CoreFrame::Var(VarId(0)));
        let g1_rhs = bld.push(CoreFrame::Con {
            tag: DataConId(1),
            fields: vec![var_x],
        });
        let var_g1 = bld.push(CoreFrame::Var(VarId(1)));
        let g2_rhs = bld.push(CoreFrame::Con {
            tag: DataConId(1),
            fields: vec![var_g1],
        });
        let final_con = bld.push(CoreFrame::Con {
            tag: DataConId(1),
            fields: vec![var_x],
        });
        let let_g2 = bld.push(CoreFrame::LetNonRec {
            binder: VarId(2),
            rhs: g2_rhs,
            body: final_con,
        });
        let let_g1 = bld.push(CoreFrame::LetNonRec {
            binder: VarId(1),
            rhs: g1_rhs,
            body: let_g2,
        });
        let lam_x = bld.push(CoreFrame::Lam {
            binder: VarId(0),
            body: let_g1,
        });
        let mut current = bld.push(CoreFrame::Lit(Literal::LitInt(42)));
        for _ in 0..depth {
            let f_var = bld.push(CoreFrame::Var(VarId(99)));
            current = bld.push(CoreFrame::App {
                fun: f_var,
                arg: current,
            });
        }
        bld.push(CoreFrame::LetRec {
            bindings: vec![(VarId(99), lam_x)],
            body: current,
        });
        let expr = bld.build();

        let mut table = DataConTable::new();
        table.insert(DataCon {
            id: DataConId(1),
            name: "C1".to_string(),
            tag: 1,
            rep_arity: 1,
            field_bangs: vec![],
            qualified_name: None,
            type_name: String::new(),
        });
        for (i, kind) in crate::effect_machine::EffContKind::ALL.iter().enumerate() {
            table.insert(DataCon {
                id: DataConId(1000 + i as u64),
                name: kind.name().to_string(),
                tag: (1000 + i) as u32,
                rep_arity: if matches!(
                    kind,
                    crate::effect_machine::EffContKind::Node
                        | crate::effect_machine::EffContKind::Union
                ) {
                    2
                } else {
                    1
                },
                field_bangs: vec![],
                qualified_name: None,
                type_name: String::new(),
            });
        }
        (expr, table)
    }

    /// Persistent roots survive a RegistryGuard drop; per-run rust
    /// roots and GC state are cleared.
    ///
    /// Bare-VMContext harness (the GC cluster reaches exclusively via
    /// `vmctx.machine_state`, never `CURRENT_MACHINE`): owns a `MachineState`,
    /// wires it onto a hand-built `VMContext`, and drives the vmctx-gated
    /// free fns through it.
    #[test]
    #[serial]
    fn test_persistent_root_survives_guard_drop() {
        let machine_state = MachineState::new();
        let mut vmctx = VMContext {
            alloc_ptr: std::ptr::null_mut(),
            alloc_limit: std::ptr::null_mut(),
            gc_trigger: crate::host_fns::gc_trigger,
            tail_callee: std::ptr::null_mut(),
            tail_arg: std::ptr::null_mut(),
            machine_state: &machine_state as *const MachineState as *mut MachineState,
        };
        let vmctx_ptr = &mut vmctx as *mut VMContext;

        // Register a persistent root (null heap ptr — GC skips null slots)
        let mut persistent_slot: *mut u8 = std::ptr::null_mut();
        unsafe {
            crate::host_fns::register_persistent_root(
                vmctx_ptr,
                &mut persistent_slot as *mut *mut u8,
            );
        }
        assert_eq!(
            unsafe { crate::host_fns::persistent_roots_count(vmctx_ptr) },
            1
        );

        // Register a per-run rust root
        let mut rust_slot: *mut u8 = std::ptr::null_mut();
        unsafe {
            crate::host_fns::register_rust_root(vmctx_ptr, &mut rust_slot as *mut *mut u8);
        }
        assert_eq!(unsafe { crate::host_fns::rust_roots_mark(vmctx_ptr) }, 1);

        // Simulate what RegistryGuard::drop does for the per-run half
        machine_state.clear_run_scratch();

        // Persistent root must survive; rust roots and GC state must be gone
        assert_eq!(
            unsafe { crate::host_fns::persistent_roots_count(vmctx_ptr) },
            1,
            "persistent root must survive clear_run_scratch"
        );
        assert_eq!(
            unsafe { crate::host_fns::rust_roots_mark(vmctx_ptr) },
            0,
            "rust roots must be cleared by clear_run_scratch"
        );
        assert!(
            machine_state.gc_active_range().is_none(),
            "GC state must be cleared by clear_run_scratch"
        );

        // Cleanup
        machine_state.clear_persistent_roots();
    }

    /// THE SEAM TEST — compile_session, run, verify heap retention,
    /// verify install re-points, verify persistent root survives second run.
    #[test]
    #[serial]
    fn test_session_heap_seam() {
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                crate::host_fns::reset_test_counters();

                let (expr, table) = make_gc_forcing_setup(40);
                // 2 KiB nursery: forces >=1 GC for the 40-deep chain
                let mut machine = JitEffectMachine::compile_session(&expr, &table, 2048)
                    .expect("compile_session");

                // --- Run 1 ---
                let result1 = machine.run_pure().expect("run 1 should succeed");

                assert!(
                    crate::host_fns::gc_trigger_call_count() > 0,
                    "GC must have fired during run 1 with 2 KiB nursery"
                );
                assert!(
                    machine.session.as_ref().unwrap().heap.is_some(),
                    "session.heap must be Some after GC (heap migrated off nursery)"
                );
                assert!(
                    machine.session.as_ref().unwrap().cursor > 0,
                    "session.cursor must be >0 after run 1"
                );

                // Capture the retained heap ptr BEFORE install takes it
                let retained_heap_ptr = machine
                    .session
                    .as_ref()
                    .unwrap()
                    .heap
                    .as_ref()
                    .unwrap()
                    .as_ptr() as *const u8;

                // install_registries must RE-POINT at the retained buffer (not nursery.start())
                let guard = machine.install_registries();
                let (active_start, _) = machine
                    .machine_state
                    .gc_active_range()
                    .expect("GC state installed");
                assert_eq!(
                    active_start as *const u8, retained_heap_ptr,
                    "install_registries must re-point GC state at the retained heap"
                );
                assert_ne!(
                    active_start as *const u8,
                    machine.nursery.start(),
                    "install must NOT reset to nursery.start()"
                );
                // Drop the guard so reclaim runs and buffer goes back to session
                drop(guard);

                // --- Register a persistent root before run 2 ---
                // Via the machine's own accessor (not the vmctx-gated free fn:
                // there is no live vmctx between runs) — same MachineState
                // cell either way.
                let mut persistent_slot: *mut u8 = std::ptr::null_mut();
                unsafe {
                    machine.register_persistent_root(&mut persistent_slot as *mut *mut u8);
                }
                assert_eq!(machine.persistent_roots_count(), 1);

                // --- Run 2 ---
                let result2 = machine.run_pure().expect("run 2 should succeed");

                // Results must be structurally equivalent (same program, same heap)
                assert_eq!(
                    format!("{:?}", result1),
                    format!("{:?}", result2),
                    "second run must produce the same value"
                );

                // Persistent root must have survived run 2's teardown
                assert_eq!(
                    machine.persistent_roots_count(),
                    1,
                    "persistent root must survive run-2 teardown (clear_run_scratch)"
                );

                // Drop the machine: free_session_heap clears persistent roots.
                // Per-machine ownership means this is structural — the
                // MachineState (and its persistent_roots Vec) is deallocated
                // with `machine`, so there is nothing left to query;
                // `persistent_roots_count()` on a freed handle would be UB,
                // not a check.
                drop(machine);
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
