# Complete the compiler-native architecture

Status: implementation resumed after architectural review on 2026-10-20.
Controlling branch: `integration/compiler-query-warm-final`.
Consolidated source checkpoint: `6bd1275d4a45f78bb11dbdf17e926af4a75948c4`.
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

The common installer and later compiler/lifecycle controls are consolidated.
G2 and release qualification remain incomplete; component passes apply only to
their recorded source. The current map and exact receipts live in
[the compiler map](../../../docs/compiler-work-map.md) and
[the verification ledger](tests/verification-ledger.md).

The remaining production repairs are ordered exact target selection, nominal
RetainedCore identity without executable-demand resolution, prior-stage produced
Source support, captured TYPE hydration in Haskell, and actual source ownership
through Ordinary installation. These are distinct boundaries; fixing an earlier
failure does not qualify downstream behavior.

Common `NativeInstantiationPlan` installation and deletion of `code_exports`
are already implemented. Preserve them; do not restart their migration. The
accepted Plan B in [g2-completion.md](g2-completion.md) owns the remaining work.

## Ownership and integration

| Owner | Current work | Completion evidence |
| --- | --- | --- |
| original_reference_boundary | Exact target slots and entry occurrence; remove logical rematching | Imported HOME, exact historical selection, stage/permission refusals |
| common_native_output | Nominal RetainedCore hashing and sharing | Matched Haskell encoding, identity sensitivity, unused/ungranted imports |
| stage_input_design | Existing source-support row carries actual prior produced completion | Actual multiple stages, first later use, initial-empty refusal |
| engine_completion | Captured TYPE import basis, hydration and exact Name references | Source-less imported module, lexical constraints, no TYPE-to-native promotion |
| compiler_demand_transport | Ordinary closed source transaction, checked current-source obligation | Actual effect sites, callback custody, reused-instance provenance |
| audit_native_selection | Independent exact-selection review and real-producer alias history | Same issuance under distinct full bindings; ordered slots stay distinct |
| closed_recipe_pairing | Joined controls, catalog and frozen-bundle qualification | Counted production histories and exact artifact receipts |
| compiler_map_concision | Current map and evidence ledger | Source claims distinguished from executed acceptance |
| root | Critical boundary review, conflict resolution, consolidation and launch | Reviewed joined revision; selected Persona and real DeepSeek smoke |

Reuse existing isolated warm checkouts. Children report proposal, blocker,
review-ready and settled events; parents continue the ready frontier. Each area
consolidates reviewed commits, records source/evidence and sends exact OIDs.
No native Codex goals. No unrelated task merely to keep an agent busy.

Root reviews production consumers and failure paths directly. Independent review
checks each joined area for bypasses, repeated work and coupled fixtures. Rebase
on received contracts; do not silently resolve an authority conflict. Publish
integration checkpoints labelled with their actual qualification state.

## Sequence and gates

1. Complete the accepted target, nominal, stage, TYPE and source-owner repairs.
2. Join reviewed commits and compile production plus cfg(test) consumers early.
3. First execute genuine imported HOME installation and mixed World history,
   then finite source → installer → public B and remaining ordinary controls.
   Uncertain-END qualification may run independently on the common checkpoint.
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

Use current measured scratch, available memory and build.slice consumption;
preserve the server's 100 GiB scratch floor including projected growth.
Use measured output growth and existing warm outputs, not simultaneous cold
expansion. Follow admitted pinned local-only commands and current server policy.
Preserve evidence/WIP; no broad GC. Stop only owned idle daemons deliberately.
If evidence requires a genuinely new architecture decision, retain the smallest
counterexample and alternatives for root; continue independent work, then hand
over cleanly instead of adding a fallback.
