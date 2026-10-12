# tidepool-runtime — compile/run API and resident sessions

This crate owns the high-level Haskell compile/run facade, resident sessions,
session checkout, turn supervision, and classification of runtime/session
failures.

Toolchain discovery, validation, fingerprinting, diagnostics, timing, and the
compiled-artifact cache belong to `tidepool-toolchain`. The corresponding
modules re-exported here are compatibility surfaces, not policy boundaries.

Session state has one mutable owner at a time. `SessionRegistry` controls
checkout admission; `PersistentSession` owns the resident execution contract;
frontends choose whether admission waits or fails immediately. JIT resource
ownership remains below this layer in `tidepool-codegen`.

Checkout epochs fence stale timeout, panic, or cancellation settlement; every
path settles exactly once. Temporary root chunks keep stable addresses;
reusable DAG handles remain rooted until parent publication, and no borrowed
heap view may survive collection or forcing. Static-region indices select
candidates only; exact object/tag validation and transactional installation
and retirement are authoritative.

`session::workbench` owns frontend-neutral source classification,
meta-command tokenization, canonical resident turn templates, and
ordered cursors that retain completed execution evidence. Frontends own command meaning, execution
settlement, presentation, provider loops, and lifecycle policy.

An admitted cell publishes its declarations and bindings together on successful
completion. Failure or cancellation before publication publishes no cell names.
Neither failure nor rejection rolls back earlier external effects; preserve effect receipts rather than
inferring that a missing binding means nothing happened. Exact transport
retries return retained receipts, while newly submitted source is new intent.
Keep workbench observation formatting separate from execution and authority;
retain full results and expose expansion without rerunning effects.

Original program provenance owns lazy activation renderer specialization. Its
slots key issued canonical witness and protected recipe/budget; original context
facts stay with one retained owner. Renderer compilation and asynchronous waiting
hold no machine checkout. Reacquisition validates the mounted input and fresh
affine admission before invocation. Only `Clean` or `NotStarted` compiler
settlement permits publication or retry; successful code survives its first
consumer's exit. Unconfirmed closure and producer panic wake waiters with terminal
refusal without admitting another producer. Renderable slots publish only after
native target, reachable original-group images and literal storage are prepared.
They retain immutable native custody, never rendered values or scoped installation
plans. Each consumer resolves imports and source domains against its fresh scope
before installation. Ready installation carries the complete native bundle and
only looks up its exact keys; absent, in-flight, expired or unheld images are typed
integrity refusals before installation. Opaque and unavailable outcomes retain no
native image bundle.

Compiled turns retain immutable provenance selections and context identities.
Installation creates fresh runtime execution observations and still resolves
current lexical domains, generations, bindings and snapshots. Static plans
must not retain runtime provenance or renderer owners: a renderer can retain its
compiled turn, so that reverse ownership would create a cycle. Reconstructing
compiler inputs issues a new plan owner; borrowing or owning the same immutable
inputs preserves its plan.

Recovery publication reuses canonical interface references emitted by native
product materialization only for the exact artifact identity in that same
compiler context. The materialization owner preserves durable filenames and
independent interfaces; callers do not supply unverified references to skip IO.
Private authored and public joined recovery records retain full-body live-binding
custody requirements independently of selected executable groups. An interface-only
lexical projection does not discharge the retained original body's lifetime needs.

Notebook compilation first admits physical targets and checked-item receipts,
then prepares existing cumulative declaration projections under one compiler
ticket. The ticket closes normally before contextual item sealing, prefix
assembly and candidate publication. `PreparedRuntimeCell` retains its physical
scratch owner and checked facts across that boundary; an explicit
`HostAdmissionCancellation` preserves invocation cancellation after END.
Convenience callers use `compile_cell_program_admitted`, which owns the same
prepare/close/seal pipeline. Ticket owners use `prepare_cell_program_admitted`
and pass its independent action/close result to `finish_cell_program_admitted`.

Execution intent retains the exact original declaration and value identities.
A declaration publication captures its relevant public predecessors before
issuing an affine join reservation. Reservation and durable allocation changes
do not alter declaration facts or compiler-view currency. Relevant public changes
require compiler recertification against the retained private intent; metadata
changes retain the same receipt and reservation while applying the delta to the
latest complete durable graph. Owner retirement or private-intent mutation is
terminal. Abandoned, refused and cancelled reservations remain burned without
rerunning private execution. Binding-only capture issues no join reservation.
Recovery declarations remain an append-only causal DAG:
parents and implementation references identify previously issued declarations.
