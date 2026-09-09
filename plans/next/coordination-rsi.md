# Coordination RSI for the next dogfood run

Main is the working Shoal platform. The interactive-applications and prepared-STG
implementations remain task branches, updated from main between runs. Build the
harness from the exact recorded main revision, not a combined product candidate.
Use committed partial handoffs to resume work; preserve remaining acceptance gates.

[Persistent coordination actors](coordination-actors.md) is the implementation
checklist for replacing progress rearming with typed sources and Haskell handlers.

## Selected behavior

- Initial Astra plans the recursive engineering graph, then checks the first Sol
  execution understanding once. It idles without routine progress subscriptions.
- Sol owners implement shared contracts, allocate substantial independent trees,
  review and integrate real consumers. Hard questions use fresh selected Astra
  contexts with compact evidence, returning directly to the requesting Sol.
- Haskell handles known continuations. `Project.Routing.followWork` retains
  evidence, per-source questions and terminal receipts in one local wave actor.
  Its sink selects actionable deltas; normal closure is quiet and preserves
  unresolved questions. Typed casts compose subtrees without model relay turns.
  Retire incorporated wave collectors after handing off remaining obligations.
- Evidence is not Attention. `Candidate` and `WorkProgress` carry source,
  checks and known gates; questions require a recipient's action. Compact
  projections retain access to the original evidence. No universal message schema.
- Agent messages optimize recoverable information per token. Examples model refs,
  expressions and terse deltas; human readability is secondary between agents.
  Preserve consequential uncertainty, identity, authority, custody and acceptance.
- Retained review/repair requests carry typed inputs without duplicating whole
  packets in guidance. `designQuestion` derives evidence from existing values;
  `updateDecision` retains ordinary admission/presentation/incorporation receipts.
- Component leads explicitly use Medium. Bound-source errors recommend `boundHead`.
  Indented GHCi continuations stay together before a subsequent reply; diagnostics
  no longer infer root cause solely from a generated-wrapper location.
- Each frontier names the parent's retained engineering and concrete integration
  consumer. Minimum usable wiring arrives early. `workingAndAbnormal` and
  `actorSummary` provide the default compact RSI roster from existing snapshots;
  full observations remain available and filtering authorizes no cleanup.

Read the shipped package's `.shoal/plans/coordination.md` for executable examples.
The shared prompt prefix stays uniform; no Rust role taxonomy or task scheduler
was added. Prompt/module changes activate only at the next swarm boundary.

## Acceptance

Current usage fixes and focused acceptance are recorded in
[the routing review](evidence/routing-usage-review.md). This replaces the previous
attention-only package: local collectors retain full terminal receipts and failed
notification attempts, typed subtree handoffs preserve actual Delivery values,
and an authorized review can start without a model relay turn. Amended questions
produce an update without falsely resolving the same question.

The underlying actor implementation's earlier boundary acceptance remains in
[coordination-actors.md](coordination-actors.md#integration-evidence). Its recorded
64-assertion run describes that earlier package, not the current usage revision.
Neither set of model-free checks claims live efficiency or provider cache reuse.

Use the copied runner for acceptance; a rebuild can replace a Cargo executable
while later recipe sessions still need its exact path. Prompt/module changes
activate only at the next authorized swarm boundary.

## Remaining mechanism work

These are explicit follow-ups, not claims made by this package:

- Stale watch notices: polling needs an acknowledgment tied to the existing
  publication watermark and durable inbox. A local dedupe cache cannot safely
  retract an already published event. Coordinate with the applications owner.
- Presentation receipts: remove redundant model polling through the existing
  request/event machinery. Preserve uncertain delivery; do not silently retry or
  infer incorporation from presentation.
- The live wave encountered the existing 32-active-descendant admission fence.
  It must be addressed before claiming an uncapped next run; prose is not a grant.
- Provider first-call cache misses and opt-in tracing for external native forks
  remain owned by the native completion/cache work. Package recipes prove resident
  behavior, not provider cache reuse or measured token savings.
- Cleanup custody and retry decisions remain in the owning lifecycle mechanisms.
  Do not add parallel prompt-driven state registries or speculative failure enums.

The next run should expose whether repeated questions, routine cursor turns and
planner activations decrease while integrated engineering progresses. External
RSI observes compact evidence; development owners retain product acceptance.

## Saved work while the current run winds down

The coherent partial coordinator checkpoint is
`6411304153def9e41d7dfc187e0538821923d695`. Newer lane handoffs are
applications `326255bc32f589f9352c181d3245812f9e630862` and engine
`1aabf25cb9393817c0c11d34595b92c96d669b8b`; they were not yet incorporated
by that coordinator checkpoint. Read `applications-checkpoint.md` and
`engine-lane-checkpoint.md` under `plans/parallel-dogfood/next-wave/` at those
respective commits. The committed external Codex bridge candidate is
`72f7b60008452a1f63c5cb92f42c731dc67fbb49`, distinct from the live runner's
native `06d99357becc4d870f5b5141ba7626daf68e819a`.

These are resume inputs, not main integration candidates or full acceptance.
Owners of resident safety, process supervision and the native bridge reached
committed partial stopping points. Full native failure-path acceptance, M1
process/corpus and M2 wrapper-complete reclamation remain open. Later committed
handoffs supersede these pins; preserve retained custody under their actual receipts.
Do not restart the whole product effort or overwrite the current live package.
