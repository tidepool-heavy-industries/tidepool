# Demand-driven compiler Worlds

Accepted implementation contract, 2026-10-20. Baseline:
`3fe27b8d9f55d21077e6f2dc6ea4f7d455d5b274`, branch
`integration/compiler-query-warm-final`. This document supersedes contradictory
machinery in the earlier reset; the G2/G3 release gates in plan.md remain.

## Outcome and boundaries

Keep the existing World service, worker pool, compiler transactions, certifier,
runtime admission and publication. Haskell owns compiler products. Rust owns
runtime authority, exact native selections, custody and publication.

Planning consumes parser/recipe/operand facts, checking consumes exact interfaces
and source homes, and native lowering additionally consumes reached finalized
Core and certified native identities. Query-only demand must not acquire native
execution inventories. A generated checking target can still require frontend
work; this wave does not add an identical-query cache.

Reuse actual completed products through their existing authenticated owners.
No new generic dependency scheduler, global fingerprint registry, hard remote
residency lease, second source driver or legacy compiler fallback.

## Shared contracts

### Identities and compiler inputs (N)

Separate opaque World correlation, compiler-input identity, native-demand
identity and worker realization reference. Full World correlation remains on
currentness, reservations, lineage, source admission, replies and settlement.
Compiler-input equality never grants another World's custody or native authority.
Native reuse additionally requires the actual exact selected executable inputs.

The existing Rust admitted-row owner privately issues one compiler-primary
product, with InterfaceOnly versus Original/canonical variants. It carries exact
scoped bindings, producer/image/evidence facts, required owners and captured TYPE
inputs. Keep canonical certificates, package evidence and required Core metadata.
Replace both the exported full-context root and duplicate primary-association
map. Full runtime-context commitment becomes opaque bytes, not a traversable
compiler DAG link. Rust still calculates its existing full commitment initially;
measure that residual work rather than claim it disappeared.

The private issuer authenticates acquisition membership and the exact
Original-to-canonical edge and derives projection and commitment together.
Haskell checks wire/CID integrity, selected-row census, physical TYPE custody and
semantic inputs. It does not claim to independently authenticate the hidden
runtime graph. No loose commitment/projection constructor issues authority.

Cold and successor restoration reproduce both identities from the same retained
Rust owner. Reuse prepared dependencies only through the actual authenticated
predecessor. Observation scaffold names may use complete compiler-input identity;
execution scaffold names retain their plan hash, reservation and full correlation.
Delete World-specific native ancestry/selection decoding and duplicate maps.
Durable runtime-context codecs and native admission remain.

### Completed products (E)

Keep parser-only cell planning. Pair existing successful compiler completion and
staged certification in one private completed-product handoff. Retain actual
HMI/boot-aware graph and completed source owners; mutable attempts, interpreter
state and per-operation DependencyEvidence do not escape or merge across requests.

Extend CertifiedOriginalProducts with exact certified Name-to-SymbolIdentity
associations, restricted to its TargetCertificationContext. Retain the full
certified availability, including today's output for tomorrow's import.
Same Name/same identity reuses; same Name/different identity refuses; different
Names/same admitted identity is valid. Missing associations remain missing.
Native projection and recovery consume this same map, bound to the compiler
universe/NameCache and exact certified owner. Delete HPT/name-spelling recovery.

An opaque captured-canonical-metadata value is issued by the existing capture
operation before consumers require interface/certificate/package bytes.
Descriptor presence is not readiness. Capture reached prior owners on demand;
do not acquire unused old Core. Preserve actual finalized Core/canonical seal
validation and actual native selection.

Use one prepareCompilationOriginals worklist with optional prior completed
products. Fold useful parts of e192545d66; it is not a passing implementation.
Delete the parallel staged-source driver and cross-operation resolution merging.
Publish pending additions only after the entire operation succeeds.

Reuse unchanged HMI/details; retain GHC's required rehydration around changed
SOURCE SCC completion, exact boot/nonboot keys and affected closure. Name
associations are not frozen Id/details knots. Do not retain entire mutable
HscEnv values or accidentally capture attempts in lazy product fields.

### Optional residency and routing (D, C)

Realization references are issued accelerators, never authority. Replace
unqualified strings/sentinels at the owning boundaries with private typed values.
Ready reports RetainedAtAdmission or ActiveOnly(reason with usage/limit).
NeedsAcquisition reports only known absence/mismatch; an unexplained miss does
not prove eviction. Actual eviction has an owning event.

Keep 128 MiB as the existing configurable shared optional input/backing storage
allowance. Remove World participation in the mixed 4096 entry limit, including
subtraction from original-image slots. Original-image count limits retain their
original units. Charge actual shared owners once; account additional retained
captures/backing before publication. Product/World/module counts are diagnostic,
not heap estimates. Keep FIFO and existing RSS admission/rotation.
Oversize valid Worlds run active-only. Failed cleanup stays charged/quarantined.

C extends the existing daemon transaction route, not scheduling policy:
prefer the eligible idle worker from the issued reference; otherwise immediately
use existing workload/capacity routing and bounded queueing. Preparation still
cannot consume the foreground reserve. Accepted transactions never reroute.
Select the hint before lazy BEGIN. Existing active scopes keep their worker.
Report actual slot and worker instance without confusing queued acceptance with
the worker instance eventually executing after rotation. Reuse the existing
worker boot nonce. Direct work has no cross-process residency promise.
Stale affinity is an ordinary miss; restore the same immutable World without
replaying accepted/uncertain operations or authored effects.

END restores attempt-local mutable state before releasing the attempt hold.
Retained products remain optional; no client-side permanent lease is implied.

## Work ownership and integration

All implementation checkouts begin from the contract checkpoint containing this
file. Keep old WIP/commits intact; port only reviewed necessary deltas. Each owner
reads local contributor rules, compiles production and affected test consumers,
runs focused nonzero controls, and sends root exact base/tip and evidence.

| Owner | Primary paths | Required evidence |
| --- | --- | --- |
| N / audit_native_selection | toolchain compiler_world/compiler source projection; Haskell World primary reader/wire; paired request types | crossed-row refusal, no native graph frontier, cold/successor identity parity |
| E / engine_completion | CompilerProducts, CertifiedProducts, HomeProducts, PreparedRecovery; GhcPipeline demand/product sections | CacheEntry SOURCE history through actual next execution, Name collision/refusal and native-demand isolation |
| D / audit_compiler_dataflow_cost | ExactScope retention, GhcPipeline cache/backing/END sections | production-size retained admission, active-only, shared backing, growth and cleanup histories |
| C / audit_command_retirement | extract-cmd daemon/endpoint routing and actual worker-location transport | preferred idle/fallback/rotation, accepted-work nonreplay, resource grant preservation |
| L / audit_reload_ownership | source/input lifecycle fixtures and receipt repairs | producer-issued fixture setup, public startup diagnosis, truthful terminal metadata |
| Root | shared contracts, map, joined review/qualification and critical diagnosis | early joined production history, source-qualified measurements, release gates |

N owns the World acquisition request/result wire; C owns daemon BEGIN/worker
location metadata; D proposes retention variants to N before changing the wire.
E and D modify separate GhcPipeline sections in isolated checkouts and coordinate
shared type changes before implementation. Integrate E's product shape before
rebasing D's final consumer changes. Merge coherent reviewed parcels promptly;
do not wait for every stream before compiling joined consumers.

Independent review covers the actual production consumer, refusal/cleanup paths,
remaining alternate APIs and needless work. Source acceptance is not execution.
Escalate new identity/authority decisions; do not replace them with permissive
fallbacks. Root resolves conflicts and keeps the remaining frontier progressing.

## Acceptance and measurement

First joined test uses the real declared packaged compiler and production World
front door, not hand-built authority or a replacement test framework:

1. Production-size acquire -> END -> query/plan -> END -> prepare. Establish the
   same worker instance and no repeated Complete acquisition, baseline FD export,
   rematerialization or unchanged dependency regeneration.
2. Interleave compatible/divergent Worlds; passive native changes retain compiler
   products but changed consumed choices prevent stale executable reuse.
3. Crossed projection/owner, wrong canonical pair, missing TYPE/package input,
   stale realization and absent native permission refuse at their real owners.
4. SOURCE/family/instance histories, shadowing and private Names; cancellation
   and later type errors publish no partial products.
5. Budget eviction, zero allowance, worker replacement, shared-successor release
   order and cleanup uncertainty; restoration never executes historical effects.
6. Warm/cold semantic equivalence of types, exact dependencies and execution.

Measure existing timing phases for Rust projection/full commitment, bytes
transferred, Haskell decode, source preparation, frontend/Core/native work,
residency, END and cleanup. Keep timings inclusive/exclusive explicit. Count
work for regression assertions; do not use flaky wall-clock thresholds.
Report retained encoded bytes, actual source backing, product counts and RSS
separately. Confirm the real narrowed startup fits the unchanged default.

Existing mixed-native ambiguity, public startup and reload failures remain
release gates. Fix the known source fixture by carrying its producer-issued
selection into the receiving World. Public startup's canonical mismatch needs
its actual owner-specific diagnostic before attributing it to path relocation.
Do not normalize paths speculatively. Source-only candidate repairs require
joined compilation/execution. G3 force/cold recovery remains campaign work.

Break affected private wire versions together; reject old packets explicitly.
Final acceptance uses one exact frozen bundle, relevant fixtures/M1-M3 and a
fresh Persona run. Update docs/compiler-work-map.md against resulting source.

## Research anchors

Pinned HLS revision: 5905e8b9674eb5b2cf761faafabb55a0e976a538.
RuleTypes.hs 186-217 separates interface/details from Core identity.
Rules.hs 880-945 assembles actual dependency environments; 1104-1112 projects
consumer-specific fingerprints; 1192-1204 gates Core production.
Smaller fingerprints alone do not eliminate upstream production.
GHC 9.12.2 revision 383be28ffdddf65b57b7b111bfc89808b4229ebc,
GHC/Driver/Make.hs 2641-2710 documents required SOURCE rehydration.
These patterns inform the boundary; no HLS dependency or IDE stale-result policy
is introduced into exact runtime admission.
