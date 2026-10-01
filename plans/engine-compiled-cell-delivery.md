# Compiled-cell engine and embedded harness delivery

Approved 2026-09-30 after the independent architecture review. This owns the
next implementation wave and supersedes incompatible staging and migration
choices in earlier completion plans. Baseline: Tidepool `b954c4674e`, harness
`9986ca3cf8b1e4be9826cb7420de01e4371922c7`. Preserve all retained worktrees and
`test-source-boot/`. Pushes, live provider runs and default changes remain deferred.

## Accepted contracts

- Declarations separate inference segments. Preserve ordinary whole-do inference
  inside each executable segment and check every segment before effects.
- The toolchain returns one immutable `CellProgram` from one admitted compiler
  transaction, containing ordered prepared items, original identities, result
  interfaces, exact dependency requirements and presentation plans. Multiple GHC
  passes inside the transaction are permitted. Runtime must not reconstruct and
  compile items after earlier effects. Publication join compilation stays separate.
- One toolchain-owned artifact inventory represents original module products,
  value interfaces and lexical join interfaces. Use petgraph `StableDiGraph` with
  typed direct dependencies and shared rooted views. Persist artifact IDs, never
  graph indices. Keep lexical selection, interface dependencies and native group
  requirements distinct. No per-cell whole-graph or historical payload clones.
- Future Val interfaces authorize typechecking only. Actual native imports need
  exact completed binding leases. Original identities and hidden dependencies
  survive shadowing, captures, publication and durable hydration. Lost live values
  stay unavailable after restart; never replay source or effects to restore them.
- Configured compiler authority is distinct from the observed worker identity.
  Admission requires both deployment authority and the exact consumed producer.
- A kernel execution owns cursor, continuation, reply, cancellation, private writes
  and resources. Ractor owns scheduling and actor lifecycle. Keep typed stateful
  actor protocols non-reentrant, while independent notebook executions progress.
- One qualified original-operation boundary contains origin/incarnation, request
  and original provider call. Nested local invocations remain beneath that
  boundary. Dispatch, captures, acknowledgments, reconciliation and fork cleanup
  retain it unchanged. Internal route boundaries use a typed alternative.
- Invocation cancellation uses the existing native safepoints with an invocation
  flag, not the shared actor realm flag. External/compile owners report actual
  settlement. Preserve the existing cancellation/publication commit decision.
- Publication atomically joins the final declaration/binding delta into the latest
  public environment; stale joins restage without repeating effects. A returned
  capture independently retains its completed private prefix and survives parent
  failure. Child admission still revalidates current authority.
- Startup atomically journals admission plus exact intent; the actual actor stays
  in Boot with its original entry and sealed inventory. Confirm manifest ownership,
  then exact Store binding, then durable ApplicationBound, then release original
  initialization once. No tools/provider/authored work before the activation gate.
- ApplicationBound precedes release and means activation may have happened. A
  crashed unactivated intent can roll split owners directly to a fresh admitted
  incarnation through exact CAS and a linear journal-proven intent chain. Never
  reactivate old live authority or weaken the bootstrap inventory fence.
- New artifact and startup journal formats may reject old versions. The user
  explicitly permits breaking experimental compatibility: preserve old state with
  typed refusal, omit migration machinery, and never infer missing evidence.

## Owners and integration order

Root owns operation identity, harness/facade composition, recovery integration,
joined verification and delivery. Component owners scaffold their public contract
and notify dependents before consumer edits. Use isolated worktrees. Root reviews
and joins exact commits; no automatic application of historical patches.

1. Compiler owner: Haskell parser/segment checking and complete cell producer;
   coordinate the Rust immutable program boundary with artifact/runtime owners.
2. Artifact owner: petgraph inventory, retained interface kinds, configured
   producer admission and declaration context integration.
3. Runtime owner: consume complete cells, native completed-prefix authority,
   atomic publication, captures and new durable artifact graph.
4. Actor owner: keyed executions, prepared effect boundary, invocation cancellation,
   exact cleanup and deletion of whole-behavior serial fallbacks.
5. Startup owner: journal intent/activation contract, pending root and child
   initialization, confirmation/re-entry and exact startup release.
6. Performance owner: production resident measurement and immutable compiler reuse.
7. Buck owner: optimized matched host/worker profile, generated edges and packaging.
8. Fixture owner: required-worker gates, deterministic fixtures and evidence tools.

Use Luna for bounded leaves and Sol 6.1 for component ownership/review. Up to 16
workers may be used for meaningful independent work, not duplicated ownership.
Root works the integration lane while coordinating. Broad acceptance runs only on
joined candidates; compile every changed target and execute focused owner tests.

First vertical compiler gate: native bind -> declaration importing that value ->
expression yielding 42. Preserve original module/binding identities. A bad final
item must execute zero effects. Keep reviewed parser/reservation/publication
primitives, replacing unfinished per-item issuer machinery as needed.

### Immutable support compilation

Cold-start investigation found that the package supplies immutable library
source but no compiler products for it. The existing GHC candidate admission
already skips lowering and product emission for accepted modules. Extend that
owner rather than introduce another compiler cache. This parcel is required
before claiming the packaged cold-start target; its speedup remains unmeasured.

- The artifact owner exports a closed `Tidepool.Prelude` support cohort through
  the existing compile front door after dependency and target certification.
  Keep original product bytes, owners, skinny interfaces and package witnesses.
  Runtime-generated Effects dependencies and live values are excluded.
- For the first version, compile and consume at identical canonical immutable
  deployment roots. Nix supplies the final source root and product catalog;
  Buck declares the resources it consumes. Do not relocate authored identities,
  copy an ordinary cache tree into a package, or invoke Buck at runtime. The
  initial producer action is batched; per-module files alone do not establish
  independent per-module build actions or cache granularity.
- The configured catalog binds source, producer, paths and file content. A
  malformed, missing or mismatched configured catalog fails with a typed error.
  Current GHC source selection, package interfaces and closed dependency checks
  still decide which valid candidates can be reused. Shadowing invalidates the
  affected candidate and its importers; ordinary authored-cache rules stay intact.
- The facade represents frozen runtime libraries as captured sources or a pinned
  deployment source root. Authored actors remain captured. Reuse deployment
  sources directly, avoiding a second copy in every run. Resume refuses changes
  of mode, source, producer, catalog or canonical deployment path. Older frozen
  workspace formats may be refused while retaining their bytes.
- Begin with ordinary startup and empty exact contexts. Subsequent private
  contexts inherit only admitted originals through their existing artifact graph;
  never inject the entire packaged instance environment into every context.

Sequence: artifact API and strict exporter; facade source-root composition;
declarative package wiring; joined production verification. Independent owners
may prepare these components concurrently against the same frozen API. Root
reviews their join and the package closure before running the cold gate.

Acceptance requires fresh-process reuse with empty user caches and zero fresh
lowering or product emission for the packaged cohort, followed by real native
evaluation and display. Exercise instances/families, source shadowing, changed
source/package/producer, missing closure and product tampering. Confirm later
exact contexts retain original owners and refuse lost live values. The installed
package must run without Buck or build scratch paths. Retain optimized timing
separately from the existing debug investigation.

## Acceptance

- M1 real Engine/Store/browser: raw Haskell and installed typed tools; retained
  output; input inclusion; request-pinned reload; interrupt/continue; compaction,
  late output, reconnect and lost acknowledgments without replay. Repeated
  provider call IDs across requests cannot release/reconcile another operation.
- Record and repair the current browser native failure with bounded typed evidence;
  it occurs before Sleep and is not an established timer/cancellation defect.
- Separate-process nonempty recovery, crash at every startup transition, repeated
  split-owner failures, uncertain writes, stale completions and lost release ack.
  No premature readiness or replay. Unsupported old records remain untouched.
- M2: A parks/B publishes/A resumes preserving B; two parked plus a progressing
  third/control; completion-order shadowing; invalid joins publish nothing; two
  capture children reply before parent completion and survive later failure;
  cancellation, partial launch, revocation, drain and last-reader reclamation.
- Compiler/native: all segment semantics, exact original/hidden/qualified/instance/
  family facts, future-interface native refusal, producer/source/package/boot
  invalidation, fresh CAFs, demand omission, rollback and final-owner reclamation.
- Optimized production composition with actual daemon/PID/epoch evidence:
  at least 50 varied warm simple cells including display, p95 <= 1 second;
  five cold packaged reference-workspace starts, each <= 10 seconds;
  active cancellable-effect acknowledgment p95 <= 250 ms, cleanup separately.
- B0/B100 x N1/N10/N100 plus eight-actor workload: record queueing, graph visits,
  closure selection, copies, decodes, hash/write bytes, retained memory and release.
  Zero whole-graph copies per cell, no per-item compiler round trips after effects,
  no repeated payload publication for unchanged durable artifacts.
- Complete structural corpus and embedded producers, matched Rust/Haskell/browser
  checks, Buck cache/invalidation and packaged startup. Missing required workers
  must fail acceptance, not silently pass. Capture exact source/pair/binary hashes,
  commands, executed counts, exits, logs and cleanup.
- Fresh-context Sol 6.1 adversarial reviews of exact accepted M1, engine and M2
  revisions. Finish selected commits, evidence, verified bundles and live-trial
  packet. No milestone closes on compile-only or neighboring-test evidence.

## Server use

Use existing `tidepool-completion-build.slice` (88 GiB high, 104 GiB maximum,
2 GiB swap) and actual bind-mounted Buck outputs. Preserve shared daemons.
Start production compiler measurement with two workers and explicit 10240 MiB
rotation ceiling, measuring peaks and replacement overlap before expansion.
Admit expensive work through the existing slice; bound build parallelism and
measure aggregate memory. Remote execution remains disabled. Do not build Codex
with Buck or put runtime compilation behind Buck.
