# Coordination RSI: make the real integration loop fluent

Status: proposed next pass, based on the r6 historical audits and three rounds of
feedback from its Astra planner, Sol coordinator and Sol engine lead. The operator
has invited actor capability and DSL changes. This is a design, not an assertion
that the proposed surface is already available.

Main is the working platform. Disk/custody fixes and executable skills are on main
through `ee946b181`. Product applications/engine work stays on continuation
branches. Build the next runner/package from main and rebase those continuations
on it; do not hot-change a running swarm or use its unfinished engine as the harness.

## The concrete case that determines scope

Make this actual engineering loop excellent:

1. A Sol owner unfolds a useful frontier and installs its routing once.
2. Child evidence and final replies reach a local typed actor in order.
3. A selected candidate goes directly to an already-chosen, available reviewer.
4. The verdict returns to the same actor. A declared repair/review edge can run;
   a new engineering decision goes to the owning Sol.
5. Typed results from independent components join at the integration owner.
6. The owner receives the relevant source/decision packet, integrates and checks
   it, then explicitly releases work it no longer needs.

This is the acceptance case for each DSL addition. A simple observer still needs
only a short expression. The more capable case should eliminate the current
route + extra mailbox + callback collector registry, repeated watch polling,
whole-state dumps and hand-operated collector retirement.

[Usage sketches](actor-loop-usage.md) describe the desired Haskell.
[Audit evidence](evidence/r6-coordination-retrospective.md) records why these
changes are needed. Keep API, examples and verification in one implementation.

## The actor surface

Keep ordinary Haskell: a small result-indexed message type, explicit state, a
handler using normal effects, and fixed typed sources. Keep `Actor.stateful`,
`withSources`, `startActor`, `call` and `cast` recognizable.

Generalize actor definitions to carry a typed effect row using the existing
`KnownEffects`/`Subset` vocabulary. The immediate row needs `Replies`, `Actor`
and `Notifications`; project aliases can supply it without repeated stack plumbing.
Reuse the existing effect witness owner and Rust attenuation checks. Do not add a
Rust review-manager role or a new named profile for every combination.

An effectful actor executes under its own principal. It owns requests it creates.
Effect membership, a captured closure or a parent response handle does not transfer
request/resource authority. Extend existing authorization only for a demonstrated
operation in this loop; do not turn an actor into an impersonated parent.

Add a typed self mailbox that can be captured and passed to a forwarder. It names
the actor whose address was obtained, even when used in another actor. It carries
no fabricated terminal-result cell or authority. Existing actor refs should remain
convenient call/cast targets.

Add owned forwarding over existing sources and actors. A handler can submit a
request, forward its terminal result to its own mailbox, and continue processing
other events. The forwarder has fixed sources, an inspectable handle and a finite
lifetime. New attempts get new forwarders; do not add mutable source attachments.

Automatic continuations request an existing worker or use a deliberately selected
context where authorized. Inherited-context `unfold` stays at an actual model/tool
boundary; a background actor cannot invent a provider transcript fork point.

## State and observations

The local actor retains full selected results, including `ResponseResult` execution
and worktree evidence. A separate settlement watch adds no authority. Normalize
child updates into the receiving project's protocol; heterogeneous result/progress
types must compose without a universal worker schema or Text encoding.

A lightweight `Work progress result` bundle is useful where it removes repeated
tuple/type plumbing. Distinguish an exact request from its reusable worker. A new
review request is not a new agent launch. Keep applicative `unfold` available and
do not make every admission batch its own integration actor.

The normal brief shows outstanding candidates with exact refs, actionable
questions, join readiness and terminal/failure state. It scales with outstanding
engineering, not the length of history. Counts alone are insufficient, and rendering
all historical checks/gates under a function called “summary” is not compactness.

Retain distinct attached publications in order, with the existing request identity
and progress cursor. Preserve changed evidence at the same commit and question
remove/reopen cycles. Across independent sources, preserve actual mailbox acceptance
order; do not claim a global causal order. Initial attachment captures retained
current state, not unavailable history from before attachment.

Replace full before/after payloads in notices with actual changes and references
to retained values. Receipts refer to those changes rather than copying cumulative
state again. This remains authored actor state, not another durable event store.
Keep existing producer progress semantics so a later subscriber can still obtain
the current unresolved state.

Latest does not mean superseding. Explicit authored messages can record which
artifacts were incorporated or superseded and at which source. That changes the
outstanding-work view without erasing history. Detail queries select one result,
source, issue or event interval.

Forward only what the receiving level needs. The coordinator should not receive a
recursive copy of every descendant's observations. A typed integration packet can
travel directly between Haskell actors; native steering carries the actionable
delta or join identity. Batch without dropping intermediate changes. No normal
model-managed cursor, rearm, acknowledgment or token-preview ritual is needed.

## Completion, retirement and failure

Keep the integration actor alive through its useful repair/integration cycle.
Only intrinsically finite forwarding work auto-finishes in this pass. This avoids
making a live integration query race an automatic exit. The parent's finish helper
drains its collector and returns retained final state; later inspection uses that
value. Do not add a general live-or-exit query protocol without a real consumer.

Finite forwarding must account for accepted work before exit. Merely returning
from a custom receive loop is not proof that queued casts/calls were drained.
Preserve the normal actor's failure/replacement behavior and fixed source ordering.

Typed mailbox admission transfers responsibility for delivering that message. It
does not prove handling or product acceptance. Native presentation likewise does
not transfer worktree custody. Avoid a new LLM acknowledgment for every typed
handoff. Retain failed/uncertain sends, report an actionable exception to an owner
and never automatically resend them.

The parent chooses when workers are no longer useful. Compose a short scoped
release over the existing cleanup entry point, whose admission/revision/provider
checks must fence concurrent new work. Do not teach Haskell callers check-then-stop.
Retain pending/uncertain members and useful specialists; no automatic WIP commits,
acceptance inference or cancellation of unfinished work.

Late provider/resource anomalies remain with the worker's existing supervisor,
even after its request forwarder finishes. Do not keep every observer alive just
in case or duplicate custody in a project ledger. Nested Haskell actor failures
must reach a responsible live owner when the immediate supervisor has no native TUI.

Effects are not rolled back when a handler fails before state commit. Before
shipping handler-owned requests, prove that the exact admitted operation and its
typed result/receipt remain recoverable if the returned handle never reaches state.
Use the existing request/operation owner and retained managed values. Add a narrow
typed recovery operation there only if needed. Labels are not idempotency keys:
reserving the same label currently creates a new request. Replacement must not
replay an uncertain request or send.

## Implementation sequence

- [ ] Add typed effect-row selection and a typed self mailbox, using the existing
  effect/authority owners. Compile an actor that submits a real typed review request.
- [ ] Implement owned forwarding and replace the existing review-continuation
  workaround with one ordinary stateful actor plus finite source forwarding.
  Preserve exact request/result identities through failure before state commit.
- [ ] Rewrite project collection around ordered changes, useful current views and
  direct typed joins. Make common request/progress handles consistent; remove
  duplicate settlement watches from this path.
- [ ] Add one parent-facing finish/release composition over existing drain and
  cleanup owners. Keep integration actors through repairs, auto-finish finite
  forwarders, and return retained evidence/uncertainty explicitly.
- [ ] Rewrite existing prompts and skills around this actual loop. Keep a short
  collection recipe and a focused actor-composition recipe. Fix qualified exports,
  supplied signatures and opaque-handle displays that caused the observed errors;
  remove contradictory watch examples and expansive default summaries.
- [ ] Execute the actual examples through focused owning checks, review the combined
  main diff and select a frozen runner/package for the next authorized wave.
  Rebase both product continuations on that main baseline.

Each step must shorten or repair the concrete loop above. Broader reactive graphs,
new world-event sources, global scheduling, arbitrary effect transactions and a
general lifecycle-query framework are not prerequisites.

## Decisive checks

Run the shipped fenced examples with model-free workers: two useful child results,
ordered partial evidence, a typed join, an already-selected review, a selected repair
and repeated review, an integration packet, then scoped finish. There should be no
model turn merely to rearm, unwrap a cumulative snapshot, forward known data or
close a finite forwarder.

Cover the meaningful boundaries:

- Heterogeneous message/result types and ordinary applicative unfolding.
- Actor versus parent request ownership; unauthorized captured handles remain
  unauthorized. A background inherited fork cannot silently become a fresh one.
- A/B while busy, changed same-commit evidence, question remove/reopen, and
  closure/settlement ordering. Full distinct evidence remains recoverable.
- Several outstanding candidates survive later publications. Claimed and actual
  submitted source remain distinct. A project source check can stop automatic
  review without baking universal hash-equality policy into Rust.
- Request admitted, handler fails before state commit, v2 replacement: inspect the
  original result without issuing it twice.
- Forwarding failure, a queued input at finite completion, failed handler during
  drain, retained exit reads and stale replacement handles.
- A nested machine-only supervisor still reports a consequential failure to a
  responsible live owner, without root status spam.
- A repair races release, provider state is stale, or custody is uncertain:
  the existing owner retains resources and reports why. A replied worker is not
  automatically retired.
- A long handled history does not grow the normal brief or repeat in notifications.
  Selected full evidence remains accessible.

Compile changed targets and use focused actor/source/request/cleanup/workbench and
prompt/example checks. Run `fixtures-check` if extractor translation or serialization
changes; do not run full workspace suites. Record executed versus compiled-only
coverage. Live efficiency/cache claims require evidence from the next run.
