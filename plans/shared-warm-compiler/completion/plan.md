# Complete the compiler-native architecture

Status: implementation resumed after architectural review on 2026-10-20.
Controlling branch: `integration/compiler-query-warm-final`.
Reviewed root source: `e78e1152c1847f72b7442e02c2e05b5c48d90a73`.
Current implementation contract: [g2-completion.md](g2-completion.md).
It owns this wave's work split, constructor boundaries, sequencing and acceptance.
[ownership-completion.md](ownership-completion.md) and
[package-runtime-join.md](package-runtime-join.md) supply supporting contracts.
[contracts.md](contracts.md) remains the shared semantic contract except where
the current implementation contract explicitly refines it.
Workers may replace unfinished scaffolding; preserving its shape is not a goal.

## Direction

Haskell owns the live checked compiler environment. Rust owns published runtime
facts, grants, exact execution provenance, resources and publication. Warm work
carries compiler-owned facts; cold recovery reconstructs the same logical world.
The baseline already has persistent compiler processes. This wave removes repeated
reconstruction and competing execution routes, not a process-per-cell design.

There is one resident compiler route, one physical-output issuer, one native
planner and one publication owner. Reuse the existing parser, transactions/END,
certifier, evaluator, journals, and test infrastructure. Delete replaced routes.

## Current evidence and disposition

The reviewed checkpoint is clean; release qualification remains incomplete.

- Production startup exposed repeated 31–33 second World acquisitions. The
  retained packet has 5,204 reachable blocks; broad native proofs dominate its
  bytes. Both the mixed 4096 count and shared optional byte allowance prevent
  expected retention. Narrow compiler demand before tuning its limits.
- Engine experiment `e192545d66` advances SOURCE native preparation/checking but
  then loses the certified Name association needed for CacheEntry recovery.
  It is a reusable experiment, not a passing candidate.
- Lifecycle `1a497cdf8e` compiled joined consumers; four of eight focused controls
  passed. Three positive fixtures omit the producer-issued source selection;
  the delivery-loss oracle needs the actual terminal value before attribution.
- Public reload is blocked earlier by startup's canonical interface mismatch.
  Owner-specific diagnostics must establish its cause. Force/cold recovery and
  exact frozen-bundle M1–M3 remain unqualified.
- Mixed native binding ambiguity remains separately unresolved. Source acceptance
  of broader custody is not proof that this runtime failure is fixed.

Earlier leaf counts qualify only their exact code and boundary. Preserve the
historical failure ledger and useful test contracts; do not delete cases merely
because their old setup used a removed API.

## Ownership and integration

| Area | Current owner | Result |
| --- | --- | --- |
| Native producer | engine_completion | Common native output/provenance and active demand callback |
| Native selection | native_offer_scope_repair | Held initial/prefix composition and completion-bearing successors |
| Artifact storage | audit_native_selection | Exact package bindings, kinds and recovery |
| Codegen custody | review_world_lifetime_architecture | Exact HOME/package instance keys, leases and attachments |
| Runtime join | audit_reload_ownership | Closed recipe, atomic adoption and deletion of global package exports |
| Lifecycle qualification | audit_warm_lifecycle | Real production histories and joined G2 evidence |
| Transport completion | compiler_demand_transport | Single-shot refusal and joined exchange controls |
| Integration | root | Shared contracts, independent review, early joined tests, map and release evidence |

Reuse existing isolated warm checkouts. Children report proposal, blocker,
review-ready and settled events; parents continue the ready frontier. Each area
consolidates reviewed commits, records source/evidence and sends exact OIDs.
No native Codex goals. No unrelated task merely to keep an agent busy.

Root reviews production consumers and failure paths directly. Independent review
checks each joined area for bypasses, repeated work and coupled fixtures. Rebase
on received contracts; do not silently resolve an authority conflict. Publish
integration checkpoints labelled with their actual qualification state.

## Sequence and gates

1. Freeze the G2 owner/recipe/lease/successor signatures on this checkpoint.
2. Join compiler and runtime tranches incrementally from reviewed exact commits.
3. Execute startup through two package-using public-B cells early, then complete
   the production histories and remaining ordinary-workflow controls.
4. Freeze one matched bundle and deliver the next Persona candidate after G2.
5. Complete G3 and reconcile/push reviewed release/main checkpoints.

### G2: early Persona candidate

One exact frozen bundle must demonstrate:

- Actual hosted Haskell tool with ordinary bindings, declarations, explicit/empty
  imports, expressions, mixed cells, source-defined types and later reuse.
- Actual used/unused private inputs, crossed/retired refusals and compile-before-
  effects. Uncertain END cannot execute or publish.
- All five discovery verbs assert semantic fields and use the held compiler;
  read-only queries do not mutate public state and pinned mismatches refuse.
- Successors survive predecessor realization eviction and disposable-output
  removal while required immutable custody remains retained.
- Actual default reload success/refusal and caller-loss retention; stale source
  and cancellation preserve truthful public state and effect evidence.
- Selected Persona spec, tools and initial memory/context verified, followed by
  a bounded real DeepSeek multi-turn smoke on the same bundle.

Force may explicitly refuse at G2; this is not campaign completion. No known bug
in the advertised ordinary workflow is waved through as a legacy-path pass.
Launch fresh conversation state while preserving authored files/durable memory.

### G3: campaign completion

Finish force, callback/continuation capture, forks, cold recovery, worker loss,
exact historical bindings and cleanup uncertainty. Close every recorded failure
with repair or an obsolete-test disposition whose useful contract survives.
Run affected suites, compiler fixtures and bundle-owned M1-M3 plus Persona
histories. Record exact source, artifact, command, executed count and outcome.

Update `docs/compiler-work-map.md` against the shipped source and execution.
Measure packaged warm/cold costs and counts: hydration, physical admission,
dependency planning, products and retained memory. Structural savings and measured
speedup are separate claims. Final delivery includes reconciled pushed main.

## Resource and handoff rules

At reset: scratch 121 GiB free with 100 GiB floor; available RAM 103 GiB.
Use measured output growth and existing warm outputs, not simultaneous cold
expansion. Follow admitted pinned local-only commands and current server policy.
Preserve evidence/WIP; no broad GC. Stop only owned idle daemons deliberately.
If evidence requires a genuinely new architecture decision, retain the smallest
counterexample and alternatives for root; continue independent work, then hand
over cleanly instead of adding a fallback.
