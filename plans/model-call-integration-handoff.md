# ModelCall: engine integration handoff

This branch adds a Haskell-authored, bounded single model turn with supplied
`AgentSpec` tools. It reuses the companion harness Engine. It does **not** enable
ModelCall for running actors; the execution owner must supply its per-cell service.

## The one runtime hook

At cell admission, install a `ModelHandler::new(Arc<CellModelService<...>>)`
into that execution's concrete handler row. Construct the service once with the
kernel principal, exact admitted cell identity, existing Store and scheduler,
captured Tokio handle, existing authenticated transport factory and host policy.
Call the concrete service's `cancel()` when the execution is cancelled or
disposed; dropping the last Arc alone cannot signal a deferred method that still
owns a clone. Cancellation seals new admission and signals all active turns
without waiting for an admitted callback to finish. The lazy cell budget starts
on the first invocation. Reuse this exact service
through suspension, publication continuations and nested callbacks; never build
one per effect request. Do not infer a cell from an actor principal or a current
thread-local cell. Distinct admitted cells get distinct services.

`ModelHandler::default()` returns typed `ModelUnavailable`. The generated handler
runs service work deferred outside machine checkout. Callbacks return to the
original Haskell continuation and execute its full effect row; the service
never reenters JIT. The admitted model/effort choices come from host policy,
not an authored grant. No actor/worktree is allocated for a model turn.

Add ModelCall to the appropriate concrete rows, effect attenuation vocabulary
and model-facing import guide **only after this hook is wired and tested**.
The universal type vocabulary is generated now; naming an effect grants no
runtime service. The model-selection type `Model` already exists, hence the
new effect's distinct name.

## Budgets, completion and cancellation

Default cell allowance: 16 provider requests, 64 tool attempts, 128000 reported
input+output tokens and 300 seconds since first invocation. Each invocation may
narrow its own allowance. Its receipt excludes nested invocation usage; all
invocations charge the shared cell allowance. Any latched exhaustion stops new
model work. Reported tokens can overshoot, missing usage is explicit, and this
is not a monetary ceiling.

A pending provider request can be cancelled. An admitted Haskell callback
finishes cooperatively and its actual result is retained even after exhaustion.
This wrapper cannot promise that an arbitrary Haskell callback terminates by the
model deadline. Parent-cell cancellation follows the engine contract; it is not
caught and reclassified as ordinary completion. Close requests are not evidence
that external effects were undone.

After-tool hooks run only for this supplied spec and only after durable output.
Pruning changes the model's view of the exact operation; retained original output
remains evidence. Evidence references must remain resolvable after the in-memory
invocation registry entry is removed.

## Required joined acceptance

1. One real resident cell invokes ModelCall with a supplied typed tool that uses
   a second effect in its caller row. Confirm its returned answer and retained
   child-operation evidence; no actor appears in the tree.
2. Nested and concurrent invocations on one cell share its allowance. A second
   admitted cell has an independent allowance, even on the same actor.
3. Suspension releases machine checkout. Provider completion resumes the exact
   invocation; callback/hook and original provider call identities survive.
4. Exhaustion/cancellation during a callback retains its observed result and
   refuses later model work. Parent failure/disposal does not leave Engine tasks
   or callbacks waiting indefinitely.
5. Rejected tool names, schemas, arguments, model choices and foreign continuation
   identities cause no unauthorized effects. Invocation receipts, usage unknowns
   and retained result references appear in the run's existing evidence view.

Companion harness changes must be applied and pinned before building the adapter.
Focused local verification uses an explicit Cargo path override; that override
is not a release dependency or a claim that the companion has been published.
No live provider gate, default-backend switch or deployment is included here.
