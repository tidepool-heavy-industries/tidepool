# Persistent Haskell coordination actors

Implement on main. Product engine/applications candidates stay on their task
branches. The next dogfood harness is built from main; task branches incorporate
that baseline between runs. Do not change the live swarm's frozen package.

## Authored contract

Actors run small typed Haskell handlers under existing actor supervision. Sources
and direct authored sends feed one arbitrary, result-indexed mailbox protocol.
Ordinary Haskell maps source events into that protocol. Each invocation handles
one event and returns its result and next state. Runtime scheduling may batch;
authored handlers never have to batch. Closures capture immutable Haskell values;
shared mutable state requires effects and runtime authority.

Sources are fixed at creation. Creation atomically captures retained current
values and follows subsequent publications, without a capture/subscribe gap.
After attachment, preserve every publication in order, including source closure.
No requirement to replay historical publications preceding attachment. Start with
request progress, settlement and actor lifecycle, not a generic world-event bus.
Source completion does not terminate its receiving actor.

Each actor executes sequentially; a suspended effect does not block unrelated
actors. A successful event commits next state and its reply/acknowledgment as one
settlement. Admission is not handling. Use existing retained request/reply cells
for asynchronous replies, not a second result registry.

Handler failure pauses the whole actor. Preserve the last committed state, failed
input, available effect evidence and later queue; continue accepting messages.
Automatically send one ordinary steering message to its supervisor, independently
of the failed handler. Never automatically replay uncertain effects.

`sendMessage` sends normal Codex steering through the owning TUI: active input is
incorporated at its ordinary next boundary; idle input wakes the same conversation.
No delivery-mode enum, generation killing or separate operator interface.

`drainActor` detaches sources and closes admission atomically, then returns while
the actor finishes admitted messages. The existing exit handle retains its final
state; callers can explicitly use `awaitExit`. A failure during drain pauses with
remaining work retained and notifies the supervisor, which stays free to repair
it. An already-paused actor rejects draining before closing admission. Existing
supervision owns resource retirement.

`replaceActor oldHandle newActorDefn` requires matching state and mailbox types.
At a quiescent/paused boundary, atomically move last committed state, unprocessed
queue and original source connections/positions to a new exact actor identity.
Do not rerun initialization or recapture source snapshots. Preserve the failed
event as evidence, but do not replay it. Old handles become stale, never aliases.
Failed replacement preserves old custody. To change sources, drain/stop and create
a fresh actor. Whole-Shoal crash recovery remains Git checkpoints, not replay of
arbitrary Haskell heaps.

## Replacement transaction

Stage the new entry against borrowed committed state in an isolated realm. Accept
only its kernel bootstrap, checkpoint and receiver installation; no initializer,
source attachment or user effects run during staging. A staging failure releases
the candidate and leaves the old actor's state, input queue and subscriptions intact.

Start the staged successor under the original supervision/lifetime owner. Keep
its mailbox execution closed until cutover. Transfer child supervision and resource
custody through their existing owners; historical creator identity must not stand
in for the current custodian. Starting an ordinary child and stopping its parent
would destroy the very resources replacement must preserve.

Fixed source connections share a destination owned by their connection guard.
While holding that destination, fence the old mailbox and redirect subsequent
publications to the staged successor. The old fence collects all previously
admitted inputs. Install that backlog ahead of the successor's post-fence events,
then enable its handler. Source connections never detach or recapture current
values. This internal source destination is not a forwarding alias for old public
actor handles.

Commit transfers the source guard, queued inputs and resource custody. Preserve
the failed input as evidence and skip its execution. Transfer an existing drain
intent as well, so repair can finish the already-admitted queue. Retain the exact
successor in the old actor's terminal observation for a lost caller reply; do not
turn reply loss into permission to repeat replacement.

## Implementation and usage together

- [x] Extend existing actor/mailbox execution with explicit state custody,
  successful settlement, paused handler failure and supervisor notification.
- [x] Attach fixed typed sources atomically at their owning registries. Preserve
  each published live value for active recipients before replacing latest state.
  Existing observations can still return latest state. No second scheduler or
  unbounded history for sources without subscribers.
- [x] Implement ordinary `sendMessage` through the durable inbox and native
  active-input owner, preserving exact correlation and uncertain delivery.
- [x] Implement drain/stop and atomic typed replacement with queue/source transfer,
  explicit failed-input retention and honest failure cleanup.
- [x] Replace `Project.Work` progress-route chains with persistent actor handlers;
  retain per-source identity, unresolved questions and completion independently.
  Lossless transport does not prevent explicit application-level deduplication.
- [x] Curate single-child, two-lane and failed-handler/v2 examples. Keep compact
  Task/Candidate/review/decision vocabulary; avoid universal delta records,
  worker-stage taxonomies and mandatory receipt polling.
- [x] Rewrite existing prompts/examples: Sol owns ordinary engineering and
  integration; initial Astra plans/reviews once, hard consultations use fresh
  compact evidence. Actor messages use minimum recoverable deltas. Keep watching
  through final handoff, and distinguish available from incorporated candidates.
- [ ] Validate and build an immutable main runner with its exact curated package.

## Acceptance

Compile authored examples and mismatched state/protocol rejection cases. Exercise
actual retained closures and state, ordered A/B delivery while busy, concurrent
source creation/publication/completion, and independent suspended actors. Exercise
A success, B failure after an effect, C queued; replacement skips B without losing
C or replaying A. Check drain/send and replacement races, stale handles, failed
replacement custody, source termination and owner shutdown.

The decisive dogfood regression publishes an initial partial checkpoint, then
later final checkpoints from two lanes after the coordinator has consumed the
initial update. Both later events reach the handler without model rearming. The
handler selects meaningful wakeups, and the coordinator integrates exact heads.

Test normal active/idle TUI steering, exact correlation, failed admission and
uncertain delivery without retries or overtaking. Retain finite one-shot watches
for finite obligations; do not keep the old progress-rearm loop as a second
recommended orchestration model. Run focused owning checks during development,
compile all changed consumers, then validate the full curated package at the
integration boundary. Extractor/serialization changes require fixtures-check.

## Integration evidence

The curated package identity is
`2c96b731e77fabc9e7ee2a1a6aa464a7444442dd9c8b88a5a727afa7b29ac26b`.
The prepared runner at `target/dogfood-routing-2533b57a/` compiles that exact
package; its selection manifest records executable and runtime dependency hashes.
It is not a launched or fully accepted swarm. Full package execution remains
pending. The shared API guide success/unavailable check passes.

Focused resident checks prove failure-after-effect replacement without replay,
retained owned children, fixed source transfer, lifecycle capture and subsequent
publication, and state/protocol mismatch rejection. The independent-source and
two-lane recipes have each passed six assertions, including actual two-head Git
integration. The final combined package run covers the strengthened exact lane
association assertion. All 17 local actor tests pass, including successor backlog
ordering with inherited drain. The fixture boundary passes all 217 semantic tests;
regeneration changes only its source fingerprint.

Replacement failure review: staging rejects before source handoff; a failed
mailbox fence restores workspace ownership through the existing binding owner.
Custody commit precedes child supervision transfer. A lost caller reply does not
cancel cutover: the old retained exit names the exact successor before the reply
is sent, and old handles never forward. Uncertain failures retain custody rather
than authorizing automatic retry. The resident owned-child check passes after the
ordering repair at `2533b57a`; source handoff's five owning tests also pass.
