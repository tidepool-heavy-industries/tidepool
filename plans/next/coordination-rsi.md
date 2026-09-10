# Coordination RSI: make the real integration loop fluent

Status: implementation complete; release preparation in progress. The complete
model-free package run passed 114 assertions. Focused checks cover request recovery,
review/repair/integration, compact history, scoped release and nested supervision.
Next-run readiness still requires committing/selecting main and reconciling the
preserved product continuations below.

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

One generic record declares the actor shape, interpreted into definition, client
and self views. Authors do not maintain a second mailbox GADT. A definition is the
same record: its single `State s` field contains the initial value; `Call input
NoReply` and `Call input (Reply output)` fields contain handlers; `Event event`
fields contain `on source handler`. Event types determine handler input; values
select the particular live source. All routes share one mailbox and lifetime.

Handlers use ordinary `get`, `gets`, `put` and `modify'` within `Eff`. Each
successful handler commits its reply and resulting state together. External
effects are not rolled back on failure. Clients expose typed endpoint values;
state and event fields confer no remote capabilities. `Self` exposes narrow
send-only return addresses, never synchronous self-RPC. Runtime-supplied sender
identity is distinct from original source provenance and conveys no extra grant.

Generalize actor definitions to carry a typed effect row using the existing
`KnownEffects`/`Subset` vocabulary. The immediate row needs `Replies`, `Actor`
and `Notifications`; project aliases can supply it without repeated stack plumbing.
Reuse the existing effect witness owner and Rust attenuation checks. Do not add a
Rust review-manager role or a new named profile for every combination.

An effectful actor executes under its own principal. It owns requests it creates.
Effect membership, a captured closure or a parent response handle does not transfer
request/resource authority. Extend existing authorization only for a demonstrated
operation in this loop; do not turn an actor into an impersonated parent.

Add a typed self view that can supply a narrow endpoint to a forwarder. It names
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

- [x] Derive the record definition/client/self views, single-state handler effect,
  typed calls and fixed event bindings over the existing actor owner.
- [x] Add typed effect-row selection and a typed self view, using the existing
  effect/authority owners. Compile an actor that submits a real typed review request.
- [x] Implement owned forwarding and replace the existing review-continuation
  workaround with one ordinary stateful actor plus finite source forwarding.
  Preserve exact request/result identities through failure before state commit.
- [x] Rewrite project collection around ordered changes, useful current views and
  direct typed joins. Make common request/progress handles consistent; remove
  duplicate settlement watches from this path.
- [x] Add one parent-facing finish/release composition over existing drain and
  cleanup owners. Keep integration actors through repairs, auto-finish finite
  forwarders, and return retained evidence/uncertainty explicitly.
- [x] Rewrite existing prompts and skills around this actual loop. Keep the short
  coordination skill and add `shoal-define-actors` for live record definitions,
  stateful handlers, typed joins and known continuations. Ship useful actor
  definitions in the project modules. Fix qualified exports,
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

## Current acceptance checkpoint

Implementation remains in progress on the main RSI checkout. The record actor
resident tests pass for typed calls/state/self/sender/drain and for compile-time
rejection of zero or multiple state fields. The latter required an explicit
launch constraint: an unused type family alone did not force the custom error.

The regenerated extractor corpus matched all 695 existing files byte-for-byte.
The final canonical regeneration changed only its source fingerprint and all 217
semantic tests passed again. No broader engine refactor was introduced.

The request-recovery check now proves that failure after admission followed by
replacement preserves the original handle and receives its review result without
resubmission. The declared repair edge also ran through actual repair and review
commits, finite-forwarder completion, and owner integration. The full declared-repair
recipe passed 13 assertions, including blocked and
source-mismatch outcomes. The finite-forwarding failure recipe passed three
assertions: stale endpoints are not retargeted and failed exits remain inspectable.
`requestWithProgressInto` transfers typed handles to the actor's mailbox before
admission. The receiving handler creates its collector with the current self
endpoint, rather than capturing a predecessor incarnation before replacement.

The current three resident record tests passed, including lifecycle input origin,
invalid state shapes and a nested handler failure reaching the interactive owner
once while its machine-only manager remained usable.

`shoal-define-actors` passes the standard skill format validator. SkillChecks ran
the actual fenced examples and passed all nine assertions, including scoped release
retaining pending work and returning its cleanup receipt. The five selected
prompt-catalog/shared-guide checks and nine generated-bridge checks passed.
The complete ten-recipe package passed 114 assertions against a frozen runner
copy, including the two-lane final-source integration. The subsequent nine-assertion
skill check adds pending-work retention and the executable scoped-release example.
Independent cases use separate recipe repositories; a swarm restart intentionally
preserves existing Git branches.
Do not treat partial passing routing assertions as a completed recipe suite.
