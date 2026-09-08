# Hosted calls, completion and context forks

Status: planned. Implements slice A6 using the session binding from A1 and the
retained process/hosted-work obligations from A5.

## Result

A hosted Haskell call, its native persisted result and any child fork waiting on
that result have distinct, correlated states. Completion processing does not
depend on terminal rendering speed. Callback failure remains visible and cannot
silently lose a child launch or replay a side effect.

The person keeps the full native TUI if hosted coordination becomes unavailable.
Native conversation and tools continue to work; failed hosted capabilities are
disabled with an honest explanation.

## Owners to extend

| Source | Responsibility |
|---|---|
| Codex `tui/src/app_server_session.rs` | Lifetime of the primary native session and its non-rendering event consumers |
| Codex `tui/src/host_dynamic_tools/` | Bound hosted-call transport and ordered completion work |
| Codex `core/src/tools/handlers/dynamic.rs` | Existing native invocation/history boundary before hosted dispatch |
| Codex `codex-rollout` completed-call boundary implementation | Canonical validation of real results and completed native batches |
| Codex `app-server/src/request_processors/thread_fork_boundary.rs` | Exact `afterCallId` history boundary |
| Tidepool `host_dynamic_tools.rs` and its extracted backend adapter | Accepted call ownership, callback validation and hosted service lifetime |
| Tidepool `actor_host.rs`, `hosted_retirement.rs`, actor fork owner | Pending child admission, exact actor policy and retirement |
| Tidepool runtime session/workbench owners | Haskell binding tip, accepted evaluation and cleanup |

The current Codex fork already has an ordered background completion queue with
60-second attempts and bounded retries. Keep that responsiveness fix. This slice
clarifies its lifecycle owner and closes failure/accounting gaps; it does not
return callbacks to the terminal render loop.

## Native session component

Create a small native-session coordination object inside the existing hosted-tool
modules, retained by `AppServerSession`. It owns the bound application/generation,
accepted call correlations, pending completed-call boundaries, bounded completion
queue and coordination state. It must not depend on chat widget rendering,
terminal size or whether the person is viewing the primary conversation.

Subscribe through the existing native event fan-out. The UI and completion owner
receive their own observations; neither consumes the other's receiver. Reuse
`CompletedCallBoundary` as the single algorithm for finding a valid boundary.
Do not implement a second parser that merely looks for a matching call ID or
assumes all results have arrived when a turn ends.

Keep this in the same full-TUI process. No separate execution daemon is necessary.
Core changes, if needed to expose a committed-input/result event, should be a
small hook at the existing append owner. Do not put HTTP callback policy or a new
workflow subsystem into `codex-core`.

## Call and fork state machine

1. Before evaluation, native dispatch records the real invocation through its
   existing history owner and reserves bounded completion capacity. If the host
   bridge is disabled or capacity is exhausted, reject before executing Haskell.
   Do not accept a side-effecting call that cannot later be accounted for.
2. The host validates protocol, exact launch/application/generation, primary
   thread and tool identity. Runtime actor context supplies authority. An
   arbitrary `actorId` in a request body cannot select another actor's workbench.
3. The host installs an accepted-work record in its existing retained service
   before running the Haskell call. Associate the native invocation and context
   call IDs with that work. A duplicate invocation cannot evaluate Haskell again;
   it observes the retained operation or receives an explicit duplicate outcome.
   Do not build a separate durable Haskell execution journal.
4. Haskell evaluation uses the existing resident machine and source/binding-tip
   owner. A context `unfold` records pending child work at the existing fork gate;
   it does not immediately launch from invocation-only history.
5. Returning the hosted result means evaluation returned a result to the native
   client. It does not mean native history has persisted that result. Lost result
   delivery is uncertain; neither side may automatically rerun the call.
6. The native history owner persists the actual result and observes completion of
   the relevant native call batch. Only then does the session component enqueue
   its correlated completion callback.
7. The host callback validates the exact accepted invocation and result boundary,
   marks the existing fork gate ready once, and retains the resulting admission
   work before acknowledging. The acknowledgment means boundary processing was
   accepted; it need not wait for children to compile, authenticate or bind.
8. Child launch uses the original parent conversation and
   `--destination-local --after-call` with the canonical enclosing context-call
   boundary. It inherits the final resident source/binding tip and the frozen
   shared tool/prompt selection. Launch failure is an ordinary retained child
   failure, not a reason to repeat parent evaluation or change the boundary.

Keep native invocation identity, enclosing code-mode/context call identity and
hosted actor identity distinct. Mixed native/hosted batches, nested Haskell work
and multiple completed calls must use the same boundary logic as native fork.
Never substitute `throughCallId`, an incomplete last line, a generated textual
summary or the invocation timestamp for the actual completed result.

## Acknowledgment, replay and failure

Completion callbacks carry the bound launch, application, thread and call
correlation. Repeating an identical callback is idempotent at the existing host
gate. An unknown call, wrong generation with no retained accepted call, ambiguous
boundary or changed payload is a protocol error. An accepted call from an older
generation in the same application remains explicitly queryable; it is not
silently rebound to a new call.

Keep one ordered bounded completion worker, initially using the existing capacity
and retry constants. Its network waits cannot block input, status, interruption
or rendering. A dropped event or worker restart must reconcile from the canonical
history and retained call set. Do not infer success from an empty in-memory queue.

During a temporary connection loss, hold the accepted work and bounded completion
records. On reconnection to the same still-live host incarnation and native
application, resend only completion acknowledgments for already persisted results;
do not repeat evaluation. If the outcome cannot be established, keep it pending or
disable coordination when the existing bounded retry policy is exhausted.

After exhausted retries, an accounting conflict or unrecoverable completion
overflow, mark hosted coordination disabled for that actor incarnation. Stop new
hosted call admission, publish the native coordination-state event and show a
plain TUI notice. The host's health view must observe this even if its HTTP
listener still answers requests. Do not silently reenable a fatally disabled
actor when a later ping succeeds; recovery follows
[05-recovery.md](05-recovery.md).

Turn completion without the required persisted result does not complete a fork.
Interruption, tool error, host retirement or a reattachment that establishes an
abandoned invocation causes the existing fork owner to abort unpublished child
work explicitly. A returned Haskell error may itself be a real persisted result;
whether child work remains valid is still decided by the actor/fork owner, not by
string matching an error message.

On host process death, old pending fork gates and resident values are not assumed
reconstructible. Keep surviving native history for inspection. A new host cannot
release old child work merely because it finds a completed call in that history.
If a child launch may already have started, its separately reserved deployment
and process scope remain obligations under the launch contract.

## Retirement interaction

Quiescing rejects new hosted calls while continuing completion/status for already
accepted work. Its entry point atomically seals the exact actor and retains the
shutdown operation, as in the existing hosted retirement implementation.

Do not deadlock by waiting for all HTTP handlers before allowing the callback that
finishes them. Do not wait indefinitely for the native application after exact
scope termination. The accepted-work owner can account for a cancelled/aborted
call once its interpreter and external resources have actually stopped, while
recording that no native result persistence was confirmed. This discharges the
work obligation, not a completed-call fork boundary.

The final custody decision requires both process completion and accepted hosted
work completion. A successful callback does not prove process cleanup. A killed
native process does not prove a host-side Haskell evaluation has stopped.

## Model and operator surface

Preserve the familiar fenced Haskell tools, including the fork's built-in run
Haskell surface. Keep raw protocol state out of the model-facing tool schema.
Failures should say which requested operation could not be completed and whether
its outcome is uncertain; do not require the model to manipulate connection IDs.
Keep frozen tool definitions stable; do not hot-swap their schemas to communicate
temporary connection health. Native UI status and honest tool failures carry that
information.

Add concise native coordination status to the TUI and existing Shoal status:
available, reconnecting, quiescing or unavailable. Detailed diagnostics can include
accepted-call counts, oldest completion age, bounded queue occupancy and exact
correlation IDs. Use existing tracing. Do not create a second full-context or
Haskell-source payload capture; reuse private opt-in native request tracing when
the actual provider prefix needs inspection.

## Acceptance

- [ ] Native rendering stalls and an unviewed primary conversation do not stall completion processing.
- [ ] Slow completion callbacks preserve normal typing, native tools and host input routing.
- [ ] Duplicate hosted invocation cannot execute its Haskell side effects twice.
- [ ] Lost hosted result response does not cause automatic reevaluation or premature child launch.
- [ ] Child admission waits for the real persisted result and complete relevant native batch.
- [ ] Duplicate completion acknowledgment releases each pending child gate at most once.
- [ ] Native mixed-tool batches and nested/context call IDs select the correct fork boundary.
- [ ] A native turn finishing without the required result aborts or retains pending forks honestly.
- [ ] Queue saturation rejects before new hosted evaluation; accepted calls remain accounted for.
- [ ] Completion retry exhaustion disables hosted coordination visibly while native TUI interaction remains usable.
- [ ] Reconnection to the same host reconciles persisted results without rerunning Haskell.
- [ ] A new host incarnation cannot acknowledge old calls as authority to restore resident work or launch children.
- [ ] Retirement can finish an aborted accepted call without synthesizing a native persisted result.

Use a fixture with a real resident Haskell fork and a controllable native provider.
Verify concrete child history and final binding visibility, not just a callback
count or a mocked command string.
