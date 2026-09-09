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
- Haskell handles known continuations. `awaitAnyProgress` composes independently
  advancing streams with existing all-of joins. `followAttentionSources` retains
  per-source state, advances cursors, suppresses identical/reordered questions and
  preserves unresolved questions on closure. Its sink chooses local policy.
- Evidence is not Attention. `Candidate` and optional `WorkProgress` carry source,
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

The tooling-only baseline is `546fc000899c0325397f319fc2df16a08aafbec1`, with
working source/build snapshot forks and isolated startup resources. Its engine
sources match the previous main. The next runner must include this RSI change.

Focused integration: 56 tests passed across tidepool, actor, runtime, toolchain,
handlers and harness. This covers the guide/catalog, retained repair flow, grouped
readiness, source authority, multiline parsing/sequencing and diagnostic consumers.
Changed Rust packages pass formatting and `git diff --check`. Whole-workspace
formatting reports an existing module-order difference in the unchanged codegen
test suite; no unrelated engine formatting is included.

Authored recipes verified 57 assertions across focused runs: workbench 11,
collaboration 20, existing routing plus actual multiline reply 20, independent
sources 6. The latter prove silent-sibling progress, duplicate/reorder suppression,
source identity, retained questions after closure and independent resolution.
Setup admits test actors individually so observation tests do not assume parallel
activation order. Recipe values are inspected separately from binding receipts.
These are model-free resident checks, not a claim about live model behavior.

Record the final package identity with the immutable runner. Recipe runs must use
a copied executable: rebuilding its Cargo path while a check is running invalidates
the exact executable identity used at recipe restarts.

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
