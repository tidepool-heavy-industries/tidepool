# Complete the compiler-native architecture

Current continuation (2026-10-21):
[DeepSeek G2 closeout: B with C investigation](deepseek-g2-closeout/README.md).
Its [state](deepseek-g2-closeout/STATE.md),
[contracts](deepseek-g2-closeout/CONTRACTS.md) and worker plans govern the active
wave. G2 is not accepted. Reviewed implementation base is
`0ea5b8e2a9168752c05bcba9cd927635d9004b5a` on
`g2-closeout/deepseek-20261021`; `integration/compiler-query-warm-final` remains
the broader consolidation destination. Reconcile current refs before promotion.

The [G2 contract](g2-completion.md) owns acceptance and semantic boundaries.
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

The current three repair fronts are exact package occurrence linkage, selected
interface provenance versus retained-copy location, and compiler-progress evidence
for native no-cell publication. Defining-Name correction is a reviewed unmerged
prerequisite. Required cold/eviction and joined bundle evidence remain incomplete.

Common `NativeInstantiationPlan` installation and deletion of `code_exports`
are already implemented. Preserve them; do not restart their migration. The
active work pack owns the next changes; do not restart previously integrated
closed-support, HOME census, affine initialization or logging work.

## Ownership and integration

The [current worker tree](deepseek-g2-closeout/README.md#worker-tree) assigns a
package producer/consumer subtree, independent witness and publication owners,
four quality leaves, history work, two producer-demand investigators, resource
inspection, review and qualification. Its supervisor maintains actual owner,
branch, dependency and evidence state; a proposed role is not a running worker.

Reuse existing isolated warm checkouts. Children report proposal, blocker,
review-ready and settled events; parents continue the ready frontier. Each area
consolidates reviewed commits, records source/evidence and sends exact OIDs.
No native Codex goals. No unrelated task merely to keep an agent busy.

Root reviews production consumers and failure paths directly. Independent review
checks each joined area for bypasses, repeated work and coupled fixtures. Rebase
on received contracts; do not silently resolve an authority conflict. Publish
integration checkpoints labelled with their actual qualification state.

## Sequence and gates

1. Merge the defining-Name prerequisite, freeze the package-linkage contract and
   dispatch the independent repairs/cleanup. Preserve D's dirty history work.
2. Join reviewed tranches and compile production plus test consumers early.
   Run real imported HOME, package, native publication and interface-copy controls.
3. Run unchanged Discovery/notebook and required finite/cold/eviction, cutoff,
   private-input, reload/caller-loss and uncertain-END controls on the joined tip.
4. Freeze and qualify one matched bundle, reconcile the map, publish the candidate
   and deliver a fresh Persona conversation. C investigation does not block B.
5. Complete G3 and reconcile/push reviewed release/main checkpoints under their
   integration policy; do not claim main containment from local replication.

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
