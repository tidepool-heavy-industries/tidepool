# Sequential bound adapter handoff

The Tidepool facade captures each `LocalResidentInstallation` transport policy
and its Responses tool projection together when `PolicyInstalled` reaches the
host. `EmbeddedPolicySnapshot` retains the exact actor incarnation and endpoint;
raw custom input stays raw, while structured function input stays JSON. This
captures the manifest/client pair, not the Haskell handler body.

The full `HostActor` binding awaits these owning contracts:

1. The actor lifecycle owner provides a scoped synchronous admission lease for
   the exact incarnation, fenced against retirement. `HostedWorkSeal` closes
   admission and cannot serve as that lease. A terminal-state precheck alone
   races retirement.
2. The harness scheduler passes its qualified `OperationId` to
   `CancellationOwner::cancel`. Its opaque random `JobHandle` cannot be
   converted to `ToolInvocationContext` without a duplicate handle registry.
   Embedded dispatch must require the origin conversation/incarnation, issuing
   request and original provider call ID. Bare `CallId` compatibility may remain
   only in standalone test paths; embedded scheduling, replay and inherited
   claims must never use it as a fallback key.
3. The host composes the actor's existing input delivery pump with
   `Conversation::input`: Store commits the idempotent envelope before wake,
   and the pump includes that exact envelope in a request or retains an
   admitted, retryable receipt. Wake acknowledgment alone is not inclusion.
4. Policy reload needs an actor-owned revision notification or snapshot
   publication. `ResidentInteractivePolicy::dispatch_boxed` sends a
   `WorkbenchRequest`, then `ResidentKernelBehavior::execute_workbench` reads
   the current `compiled_tools` and `dispatch` at execution. A semantic body
   reload can preserve declarations while changing that handler. At request
   admission the actor must issue an opaque lease over its installed
   `ResidentWorkbenchTools` dispatch and declarations. Carry the lease in the
   internal request and use it at execution, while future requests receive a
   new version/lease. A model-supplied version string is never authority.

The real M1 gate uses the public `Conversation::attach`/`engine` path and a
deterministic model transport against a resident actor. It must cover a raw
notebook cell, a structured tool, retained result, exact cancellation,
envelope retry/inclusion, reload snapshot, reconnect without effect replay and
the Codex default path. No live launch follows the gate.
