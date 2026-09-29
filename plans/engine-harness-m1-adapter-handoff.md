# Sequential bound adapter handoff

The Tidepool facade captures each `LocalResidentInstallation` transport policy
and its Responses tool projection together when `PolicyInstalled` reaches the
host. `EmbeddedPolicyInstallation::request_snapshot` calls the actor endpoint's
opaque `snapshot_for_request` hook. The resulting `EmbeddedPolicySnapshot`
retains the exact actor incarnation, manifest and installed handler/source lease;
raw custom input stays raw, while structured function input stays JSON. An
endpoint without a supported snapshot returns an error.

The full `HostActor` binding awaits these owning contracts:

The external harness crate is not a Tidepool workspace dependency or pinned
source today. Composition needs a tracked package placement/pin before the
facade can implement `harness::embedding::HostActor` and compile against it.
The separate harness companion `87d59967` is based on operation-identity
candidate `2948eaa`; it narrows admission leases and gives cancellation owners
the scheduler's exact `OperationId` alongside the opaque job handle.

1. The actor lifecycle owner provides a scoped synchronous admission lease for
   the exact incarnation, fenced against retirement. `HostedWorkSeal` closes
   admission and cannot serve as that lease. A terminal-state precheck alone
   races retirement. The harness currently holds its `HostActor::admit` guard
   across dispatcher `.await` in `PinnedProvider::call_with_context` and
   `call_custom_with_context`, and across `wake().await` in
   `Conversation::input`. Narrow that guard to the synchronous Store binding
   or input transaction and drop it before awaiting. The existing actor
   endpoint must atomically accept/fence the actual tool message and own its
   execution after admission; a long-held guard can deadlock self-retirement.
2. The harness scheduler must pass its qualified `OperationId` to
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
4. The actor now issues an opaque handler/source lease at request snapshot and
   carries it beside the runtime `WorkbenchRequest`. The adapter must call
   `EmbeddedPolicyInstallation::request_snapshot` for each model request and
   retain its result with that request's published tool surface. The version
   string is evidence for the harness Store, never model-supplied authority.

The real M1 gate uses the public `Conversation::attach`/`engine` path and a
deterministic model transport against a resident actor. It must cover a raw
notebook cell, a structured tool, retained result, exact cancellation,
envelope retry/inclusion, reload snapshot, reconnect without effect replay and
the Codex default path. No live launch follows the gate.
