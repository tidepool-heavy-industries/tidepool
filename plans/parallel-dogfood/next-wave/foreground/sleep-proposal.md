# Resident sleep execution proposal

Status: sleep-lead readback begun at
`1c6f8816320e987cd61b031a90718662674c890f`. This is a proposal, not an
implemented contract. It incorporates the canonical interruption contract from
planner artifact `2b6799a7810a47485e965413e48335d23bef5cb1`, recorded at
coordinator source `56e6903eafbcb059f95aad06fcf21595c71b6659`. The narrow design
hold is released.

## Observed owners and interfaces

The existing duration vocabulary is `Tidepool.Duration.Duration`, constructed
with `milliseconds`, `seconds`, and `minutes`. Its public constructors accept
`Natural` and reject values beyond Haskell `Int` before an effect request is
formed. The private representation is `DurationMilliseconds Int |
DurationSeconds Int | DurationMinutes Int`. Rust already decodes that exact Core
shape in `tidepool-actor/src/request_effect.rs`; `RequestDuration::checked`
rejects negative values and checked-multiplies units into the request deadline
representation. Sleep should reuse this vocabulary, not add `Minutes` or a
second duration type.

Effect contracts originate in `tidepool-protocol/src/effects/`. Generation
projects an authored declaration into
`tidepool-mcp/src/generated/` and an actor-side suspension decoder into
`tidepool-actor/src/generated/`. `tidepool-protocol/src/effects/mod.rs` owns the
generation rosters. `tidepool/src/actor_host.rs::shoal_effect_declarations`
selects the effects compiled into the resident Shoal surface.
`tidepool-actor/src/resident_workbench.rs::ResidentRequest` decodes the selected
request and `ResidentActorBoundary` carries its continuation.
`ResidentActor::resolve_effect` in `tidepool-actor/src/resident_actor.rs`
serially services active actor effects and resumes the captured `ResidentHole`.
The actor's one admitted turn therefore already supplies handler sequentiality;
sleep must not create a callback actor or allow a second handler to mutate actor
state while the first sleeps.

Effect-row selection is also authority-neutral runtime policy.
`tidepool-actor/src/role.rs::ActorEffectKey` and every role's effect-key list,
plus `ActorEffectKeyWire` conversion in `tidepool-actor/src/start.rs`, own
explicit child rows. Adding `Sleep` only to the global declaration roster would
leave selected/default actor rows inconsistent.

The observed model-visible path is:

1. The native Codex TUI invokes the actor-scoped `haskell` dynamic tool.
2. `tidepool/src/host_dynamic_tools.rs::call` validates protocol, namespace,
   bound thread, and argument kind, then awaits
   `ResidentToolEndpoint::dispatch_boxed`.
3. `ResidentInteractivePolicy::dispatch_boxed` in
   `tidepool-actor/src/resident_interactive.rs` parses GHCi input and calls
   `ResidentToolClient::dispatch_workbench`.
4. That client holds its dispatch mutex, sends
   `KernelMessage::Workbench`, and awaits its reply.
5. The actor workbench captures each effect boundary; the proposed sleep branch
   waits natively, resumes the same `ResidentHole` once with `()`, and lets the
   ordinary stabilization loop execute any Haskell suffix without another model
   turn.
6. The workbench response returns through the oneshot and HTTP handler to the
   native tool call. Codex then owns presentation of that one eventual tool
   result and any subsequent provider continuation.

No timeout is visible in steps 2–5. That observation does **not** establish
fifteen-minute transport survival or interruption semantics: the HTTP peer may
disconnect or cancel its request, and the current
`ResidentToolEndpoint`/`KernelMessage::Workbench` path carries no public
per-invocation cancellation handle. Dropping an outer HTTP response waiter is
not, by itself, evidence that the actor evaluation or its timer was cancelled.
`InteractiveAgentBackend::present_update` is correlated active input, not a
sleep-cancellation API. The headless Codex turn deadline and
`BackendCanceller` are a different backend seam and must not be assumed to
govern the long-lived TUI.

## Proposed typed minimum seam

Add a non-dispatched, actor-serviced `Sleep` effect with the model-facing
signature:

```haskell
sleep :: Member Sleep effects => Duration -> Eff effects ()
```

Its sole generated request should carry `Duration`, auto-import
`Tidepool.Duration (Duration, milliseconds, seconds, minutes)`, and return
unit. Decode the private duration representation at the actor boundary and
convert it with one shared checked function before scheduling. That function
must reject negative Core values and multiplication/representation overflow;
zero resumes immediately. The runtime timer uses Tokio's monotonic clock and
may complete late but not early.

Likely owned edits after release:

- `tidepool-protocol/src/effects/sleep.rs` and
  `tidepool-protocol/src/effects/mod.rs`: schema, public helper, imports, and
  actor-generation membership.
- Generated `tidepool-mcp/src/generated/{sleep,mod}.rs` and
  `tidepool-actor/src/generated/{sleep,mod}.rs`.
- `tidepool/src/actor_host.rs::shoal_effect_declarations`: resident compilation
  roster.
- `tidepool-actor/src/request_effect.rs` (or a narrow new `sleep.rs`): the one
  reusable `Duration` decoder/converter. The request-deadline decoder becomes a
  consumer of this shared conversion rather than a duplicate.
- `tidepool-actor/src/resident_workbench.rs`: `ResidentRequest::Sleep`,
  `ResidentActorBoundary::Sleep`, decoding, and operation naming.
- `tidepool-actor/src/resident_actor.rs::resolve_effect`: timer/lifetime
  integration and exact-once resume.
- `tidepool-actor/src/{role,start}.rs`: `ActorEffectKey::Sleep`, wire mapping,
  and intended default role membership.
- `prompts/shoal/api-guide.md` plus its documentation tests: exact signature
  and `sleep (minutes 15)` example. The canonical running `.shoal` remains
  frozen for this swarm.

Do not add a timer registry, command job, process reservation, wake message,
user timer handle, durable schedule, or Haskell polling loop.

## Walkthroughs and settled interruption design

Normal case: `sleep (minutes 15) >> observableSuffix` suspends at one Sleep
boundary. The actor yields its machine checkout while Tokio owns the delay;
siblings continue. On expiry the owning actor reacquires its resident machine,
resumes the exact hole once, runs `observableSuffix` without inference, and
returns one final workbench result through the existing dynamic-tool call.
A sleep in an actor record handler uses the same branch; because the actor
remains inside its admitted handler turn, later mailbox inputs remain queued.

Awkward cases:

- `milliseconds 0` resumes without registering a meaningful timer.
- Forged negative Core input or unit conversion overflow is rejected before
  scheduling; the suffix does not run.
- Retirement/evaluation cancellation drops or explicitly cancels the timer
  under the existing actor invocation lifetime; expiry racing cancellation has
  one linearization point, so only resume or cancellation wins.
- Host death loses the live continuation and timer; no restart durability is
  claimed.
- Operator interruption must promptly restore TUI usability and permanently
  suppress the suffix. Current source does not prove how native TUI interrupt
  becomes cancellation of the already-admitted HTTP/actor workbench call.

The planner selected one logical pending invocation. Loss of an HTTP response
waiter or other observation/transport loss neither cancels nor completes it and
never permits replay. The existing `WorkbenchExecutionId`, completed-execution
journal, and exact-input retry check remain its reconciliation identity.

Every message actually delivered to the sleeping LLM through the normal human,
actor-steering, or notification path cancels the whole suspended evaluation
before inference sees that message. There is no prose classifier. Collector or
mailbox data not delivered to the LLM, and requests merely queued behind the
active request, do not interrupt. Haskell actor handlers stay sequential.

The native delivery owner must queue the message, join an exact-evaluation
cancellation/terminal result, settle the original tool invocation, and only then
present/activate the queued message. Cancellation aborts the continuation; it
must not resume sleep with unit. Completed effects and independently owned
command jobs keep their existing semantics, while arbitrary locals from the
unfinished input unit are not reconstructed across inference.

This requires a narrow extension to the existing owners:

- `ResidentToolEndpoint`/`ResidentInteractivePolicy` expose an
  exact-`WorkbenchExecutionId` cancellation operation whose typed result is
  terminal evidence or explicit uncertainty, rather than interpreting dropped
  futures as cancellation. Exact-ID reconciliation returns retained evidence
  and never starts a new execution.
- `ResidentToolClient` and the actor kernel pass the cancellation signal to the
  already-admitted workbench execution by a path that bypasses its occupied
  dispatch mutex and actor mailbox turn, without admitting a conflicting actor
  turn. The existing actor/workbench state and completed-execution journal own
  its terminal result; a disconnected observer must not overwrite known
  terminal evidence, and no second general evaluation registry is added.
- The sleep resolver races monotonic expiry with that signal at one owning
  linearization point and records whether expiry or cancellation won. An expiry
  winner may run suffix effects, which must be reported honestly. Once
  cancellation is acknowledged, no later suffix effect may run.
- `tidepool/src/actor_host.rs` tracked-message and request-update delivery paths
  join cancellation/settlement before calling the native presentation owner
  when the target has a sleeping evaluation. Notification admission alone is
  not delivery and therefore does not cancel.
- The native Codex transport preserves and settles the original correlated tool
  call before inference handles the queued message. Shared backend/host edits
  remain with the applications native integrator unless the coordinator
  transfers exact hunks.

If exact terminal proof is unavailable, the host reports uncertainty: it may
neither claim cancellation/quiescence nor admit another workbench evaluation.
There is no program-across-inference fallback.

## Recursive execution frontier after planner release

The lead first lands the schema, shared duration conversion, generated files,
effect-row wiring, boundary enum, and a compiling zero/short-delay actor path.
This is the shared fork point. The lead retains production lifetime/cancellation
wiring, handler integration, final guidance, and fold checks.

Then two substantial children can proceed independently:

1. **Timer and lifetime evidence:** controlled-time tests for fifteen minutes,
   zero/negative/overflow, not-before completion, expiry/cancellation race,
   retirement cleanup, sibling progress, and actor-handler sequentiality. It may
   add an isolated clock/test seam but not a second timer owner.
2. **Native wait acceptance:** coordinated with the applications owner, add the
   actual scripted-provider TUI fixture proving the complete dynamic-tool path,
   one provider-visible completion, one suffix, normal interruption, cancelled
   suffix suppression, and post-interrupt reuse. Shared
   `host_dynamic_tools`/native Codex edits require coordinator reservation.

After folding both, the lead verifies the public example in a resident block and
an actor handler. The coordinator owns the matched sleep/applications rerun and
manual fifteen-minute smoke scheduling.

## Targeted verification map

Before implementation, enumerate exact test names; do not run a workspace suite.
Expected boundaries are:

- `cargo run -p tidepool-protocol --bin tidepool-protocol-gen` followed by
  `just test-target tidepool-protocol generated_files_are_current
  'test(=generated_files_match)'` (use the actual enumerated name), protocol
  validation/generation tests, `cargo fmt` for changed Rust, and
  `just fixtures-check` because the extracted effect/serialization corpus
  changes.
- Focused `tidepool-actor` unit/integration tests for checked duration decoding,
  zero/short completion, controlled fifteen-minute advancement, race,
  retirement, sibling progress, handler serialization, exact-ID retry, and
  refusal of conflicting evaluation while terminal outcome is uncertain.
  Compile each changed actor test target before execution.
- Focused `tidepool` actor-host tests for the public resident Haskell example,
  generated effect roster/profile selection, and the exact dynamic-tool
  completion path.
- The applications-owned scripted native TUI target for long wait,
  interruption, suffix suppression, completion count, and subsequent use.
  Distinguish a delivered human/steering/notification message (must cancel)
  from collector/mailbox-only data and a queued-not-activated request (must not);
  also cover transport disconnect without replay, cancellation uncertainty
  without conflicting admission, expiry winning the race, and exact terminal
  settlement before the queued message reaches inference. Controlled time does
  not replace one real fifteen-minute mock-provider smoke.
- Prompt catalog and
  `shared_api_guide_example_handles_success_and_unavailable` when the shipped
  guide changes.
- Appropriate formatting and `git diff --check` on the resulting candidate.

## Challenged assumptions and settled constraints

The initial plan's `sleep (Minutes 15)` spelling conflicts with the current
lowercase smart-constructor API; propose `sleep (minutes 15)`. A Tokio
`sleep().await` inside `resolve_effect` appears sufficient for normal
serialization and machine release, but it is not sufficient evidence for
operator interruption or HTTP/native transport lifetime. Message/steering
semantics are not inferred from the PRD.

The planner resolved the semantic question above, but implementation must still
prove the exact native event and settlement join on the selected Codex source.
The binding negative rule is: without exact terminal proof, neither claim
cancellation/quiescence nor admit a conflicting evaluation.
