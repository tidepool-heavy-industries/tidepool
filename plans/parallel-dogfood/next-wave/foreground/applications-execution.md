# Applications reconciliation execution proposal

Status: **proposal committed for the single planner checkpoint**. Source
reconciliation and target discovery may continue, but implementation descendants
remain held until the coordinator records planner release. This document is not
applications acceptance.

## Exact inputs and reconciliation state

The working continuation is
`1c6f8816320e987cd61b031a90718662674c890f`, whose parent is launch main
`f0c079fabd09c35ac7cbb50a3eba74d1a8ea4943`. The immutable R7 inputs are:

- applications handoff `8eda2640b5047786f5dcf2af8b7eae9760e5e767`
  and code head `cff1ce52723a3568dbca197b3aca2936f928c725`;
- coordinator preservation `0c1fb83f2285775cb16b212ce2e152bbe9a07374`,
  including the registry-test adaptation at
  `bfcc72afd09df8806e16d1454541b3cee5933414`;
- external Codex applications head
  `d84cda697a8dac2842bec09dbd7562a3fab4c926`; and
- selected external Codex launch baseline
  `80e36633f515b03e11189e8516be21065e73335e`.

The Tidepool R7 and launch lines share ancestor
`badb46615322b5f097eee342c4c5b5e88aecd407`. A direct merge is deliberately
rejected: its 168-file net includes unfinished engine, generated introspection
and temporary plans. A dry merge also conflicts in the shared pin, Haskell
introspection, execution-plan, backend node/interactive exports and runtime
inspection owners. Applications will instead adapt the production deltas named
below onto `1c6f881...`, recording the original head and ancestor in the
consolidation commit.

The native lines share ancestor
`fe15831c8a22c0d1b8d78d5ce55b7aa5fc3fa666`; neither is an ancestor of the
other. The R7 native line is linear and changes 31 files. A dry merge onto
`80e36633...` auto-merges `codex-rs/tui/src/host_dynamic_tools.rs` and reports
one content conflict in
`codex-rs/tui/src/host_dynamic_tools/input_control.rs`. This corrects the older
`7259e937` inventory hint: both files changed on both lines, but only
`input_control.rs` presently conflicts. The native continuation must still
review both semantic joins and retain command `Jobs` from `7259e937` /
`0cf2d417` together with R7 Bind, immutable `TurnInput`, operation reconciliation
and durable completion.

No reconciled pair exists yet. The precise work to create it is:

1. branch external Codex from `80e36633...`, merge the immutable linear
   `d84cda697...` line without rewriting it, resolve and test the single textual
   conflict plus both semantic joins;
2. on Tidepool `1c6f881...`, select the applications behavior from the R7
   baseline and later repairs rather than importing its combined tree;
3. join the resulting native commit to the Tidepool consumer without changing
   the coordinator-owned `flake.nix` / `flake.lock` pin; and
4. publish both exact heads after the first native and Tidepool owning checks.

## Behavior selected, adapted and omitted

The retained application boundary is one owner chain:

1. `tidepool-node/src/inbox.rs` owns the durable, immutable
   `InputOperationId` and tracked-row ordering.
2. `tidepool-agent/src/interactive.rs` owns `QueueReadyThread` and the
   backend-neutral bind/submit/query/withdraw/seal/acknowledge interface;
   `tidepool-agent/src/backend/codex/input_control.rs` owns only the private
   Codex relay representation.
3. `tidepool/src/actor_host.rs::{deliver_pending,run_delivery_pump}` owns exact
   actor/incarnation/native-generation binding and uncertain-delivery
   reconciliation. `InteractiveConnection` and `InteractiveDeployment` remain
   the host lifecycle owners.
4. Native `QueuedItemService`, `QueueStore`, queued-item state and completion
   outbox own accepted native execution and persisted terminal results.
5. `tidepool/src/actor_host/hosted_retirement.rs` and
   `retire_scoped_process`, `retire_interactive_application{,_guarded}`,
   `stop_retired_{tool_service,delivery}`, `retire_native_pane` and
   `retire_pane_artifact` join producer sealing, hosted work, HTTP drain,
   process custody and cleanup without converting uncertainty to success.
6. `tidepool-runtime::PersistentSession` remains the checkout/reuse owner.
   The R7 `ResidentSessionState` behavior is adapted at that public boundary,
   and `prepare_root_recovery` / `root_recovery_launch_mode` in
   `tidepool/src/actor_host.rs` consume it. Recovery replays safe source only;
   it never reconstructs live values, handles, effects or grants.

Candidate changes in the inbox, request-update, exact completion/deferred-release,
retirement and typed recovery commits are selected where current main does not
already satisfy them. Main's newer command-resource admission, foreground
`Cmd` jobs, process/workspace custody, actor routing, prompting and session
implementation win on conflict. The engine execution schema, prepared STG/JIT,
introspection generation, combined fixture fingerprint and historical plan stack
are omitted. R7 `flake.nix` / `flake.lock` changes are evidence only; the
coordinator owns the final matched pin.

The newly settled sleep contract is a shared consumer constraint, not permission
to weaken applications: a delivered human/actor message queues, obtains the
exact evaluation's cancellation or terminal outcome, settles the original tool
invocation, and only then resumes inference. Transport loss proves none of
cancellation, completion or replay permission. Baseline `80e36633...` has no
inspected explicit whole-call deadline on this host-tools path. Applications
therefore owns any edits in the shared Codex backend and
`tidepool/src/actor_host.rs`; the sleep owner supplies its typed wait/cancel
requirements and acceptance fixture. Those edits must preserve exact input
identity, custody and no-redispatch fences, and must prove the complete outer
path plus the real fifteen-minute wait.

This contract is canonically incorporated from
`2b6799a7810a47485e965413e48335d23bef5cb1` at coordinator resulting source
`56e6903eafbcb059f95aad06fcf21595c71b6659`. Its cancel-and-settle join has the
following concrete ownership:

- the sleep lead owns the resident evaluation cancellation primitive and typed
  sleep outcome at the existing `ResidentToolEndpoint` /
  `ResidentInteractiveClient` boundary in
  `tidepool-actor/src/resident_tools.rs` and its
  `resident_actor.rs` / `resident_workbench.rs` consumer; it must identify the
  exact `ToolInvocationContext` / `WorkbenchCallKey`, not a socket or waiter;
- the applications lead owns the shared host transition in
  `tidepool/src/actor_host.rs`: queue delivered steering, request cancellation
  of that exact invocation, retain an explicit terminal-or-uncertain result,
  settle the original native tool call, and only then release the queued
  message to inference;
- the native integrator owns the matching Codex TUI control and durable outcome
  in `codex-rs/tui/src/host_dynamic_tools.rs`,
  `host_dynamic_tools/input_control.rs` and their existing protocol/state
  consumers, composed with command `Jobs`; dropping the HTTP future, response
  waiter or socket is never terminal proof; and
- the sleep resident evaluation owner linearizes expiry against cancellation;
  applications coordinates and tests ordering across the native/host boundary,
  while an acknowledged cancellation forbids later suffix execution and an
  unconfirmed outcome keeps conflicting evaluation admission fenced.

Before applications edits the dependent cancellation path, sleep publishes a
typed exact-identity cancel operation returning terminal-or-uncertain evidence
and the fixture expectations. The cancellation signal must bypass an occupied
dispatch mutex or actor mailbox turn without admitting another workbench
evaluation.

The join tests must prove: exact invocation A can be cancelled without affecting
B; a delivered human message and a delivered actor notification both queue
behind A's terminal outcome; cancellation settles A before the next inference
begins; expiry-versus-cancel produces one honest terminal result; transport loss
leaves A uncertain and rejects conflicting evaluation; acknowledged cancellation
prevents the suffix while already completed effects survive; and a subsequent
interaction succeeds after confirmed cancellation. The matched scripted-provider
fixture proves this through the real TUI, and the final pair also runs one
uncancelled real fifteen-minute wait.

## Consumer walkthroughs

Normal applications flow: an actor binds one live native TUI generation, records
one immutable input row, `deliver_pending` submits it once, and a lost or delayed
reply is resolved by querying that same operation. Native persistence supplies
correlated presentation and completion. Only the exact persisted completion
releases the original call and its exact-context fork; retirement then seals the
producer and accounts for hosted work, HTTP service, process and workspace
custody.

Awkward flow: native consumes the input but its acknowledgement is lost while
the generation changes and compaction advances. The host must not allocate a new
operation, resend the payload or bind a fallback executor. It queries the original
identity; stale or foreign producers are rejected, `Presented` / `Compacted`
remain monotone fences, and unknown custody remains reported through retirement.
If the host or native process is lost, recovery reports lost live state and only
an explicit new execution may replay safe source.

Shared sleep flow: an ordinary fifteen-minute sleep tool call keeps its original
invocation pending while other actors progress. Timer expiry resumes the Haskell
suffix with no intervening inference, then returns one final tool result. On
operator interrupt, the exact evaluation is cancelled, the original invocation
settles promptly, its Haskell suffix never runs, and the next interaction remains
usable. Socket/transport loss makes the observer's knowledge uncertain; it does
not erase a known retained terminal result, cancel the invocation, or authorize
a second execution. Exact-ID reconciliation returns existing evidence only.

## Released frontier after the checkpoint

The first usable shared seam is the reconciled native tool/control source. One
native integrator owns the external repository and retains dispatch,
completion and custody joins. After committing the combined `Jobs` plus
input-control seam, it may fork independent focused work for:

- input-route Bind, stale generation, immutable payload, lost acknowledgement,
  compaction and transport-loss behavior; and
- command-job completion, cancellation, timeout absence, failure and cleanup.

The applications lead concurrently retains Tidepool selection and host recovery
wiring. From the reconciled Tidepool seam, a recovery owner may adapt and check
the registry race and source-only replay while a matched-consumer owner adapts
the existing `tidepool/tests/interactive_applications.rs` scripted-provider
fixture to the exact pair. No child may edit the native/Tidepool wire contract,
shared pin or combined host join without returning it to its owning integrator.

The lead integrates coherent native and recovery slices as they finish, owns the
shared sleep/application outer transport join, runs the final matched TUI and
retirement matrix, and records omissions and exact evidence. A fresh independent
review then examines the actual conflict resolutions, admission/completion
ordering, transport-loss semantics and Jobs cleanup on the committed pair; the
same reviewer is retained for repairs.

## Targeted verification

Before children, the native integrator must compile/enumerate the affected
`codex-tui`, queue/state/thread-store, app-server and
`hosted_interactive` targets and run one owning input-control/Jobs join test.
The Tidepool lead must compile `tidepool-runtime`, `tidepool-actor` and
`tidepool` after its first join.

Resulting-source behavior checks are:

- the registry state/race and source-only declaration recovery tests;
- `abnormal_root_reuses_only_a_queue_ready_conversation`;
- `forest_operator_survives_model_root_recovery`;
- exact lost-acknowledgement, stale-generation, conflicting-payload,
  Presented/Compacted and no-overtaking cases;
- delayed/lost/duplicate completion, persistence failure, cancellation with
  accepted work, producer seal, lost waiter and degraded cleanup cases;
- the matched `pinned_full_tui_binds_and_accepts_exactly_one_owned_input`
  socket/PTY/scripted-provider consumer and its preflight failures; and
- a short interrupted actual-TUI sleep proving original-invocation settlement
  before message inference, Haskell suffix suppression and subsequent use; and
- a separate uncancelled real fifteen-minute actual-TUI wait proving one Haskell
  suffix, one final tool result and no intermediate provider request.

Use focused `just test-lib` / `just test-target` or the native repository's named
target enumeration; record selected and executed counts. Reuse unchanged
exact-source R7 evidence, but never substitute it for a changed join or final
consumer. Run formatting for changed languages and `git diff --check`. Run
`just fixtures-check` only if extractor/serialization source is actually changed;
no broad workspace suite or aarch64 execution is claimed.

## Challenged assumptions and planner question

- `d84cda697...` passed matched R7 A8 once, but neither its package nor historical
  greens establish behavior after the `80e36633...` Jobs join.
- A clean textual merge of `host_dynamic_tools.rs` does not prove semantic
  coexistence; both native shared files require review.
- The combined R7 tree is preservation, not an applications implementation
  source. Its convenient direct merge would silently import unfinished engine.
- Main's newer owners are not automatically behavioral supersets. Every omitted
  R7 application delta needs either an equivalent production path plus check or
  an explicit adaptation record.
- Process exit, socket loss, acknowledgement loss and elapsed time are not
  completion or cancellation. There is no accepted implicit whole-call deadline.
- Final engine revalidation is not an applications-only ship gate unless a
  concrete recovered interface proves inseparable.

Planner question: approve this selective two-repository reconciliation and
applications ownership of shared backend/host edits, with the coordinator
retaining the pin and sleep supplying the typed wait/cancel contract; or identify
a concrete application guarantee or sleep outer-path seam that requires a
different owner before the reconciled pair and implementation frontier proceed.
