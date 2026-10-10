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
