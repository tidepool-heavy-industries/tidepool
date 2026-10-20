# Complete the compiler-native architecture

Status: implementation resumed after architectural review on 2026-10-20.
Controlling branch: `integration/compiler-query-warm-final`.
Reviewed root source: `e8ad8a0f50f9350bb80d68ec924ca9846c779e54`.
This plan and [implementation reset](implementation-reset.md) supersede previous
execution ordering and progress claims. [contracts.md](contracts.md) remains the
shared semantic contract except where this reset explicitly narrows machinery.
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

| Source | Established | Unqualified / disposition |
| --- | --- | --- |
| Root `e8ad8a0f50` | Reviewed source/item identity repair and carried checked-import selection | Joined release is not qualified; root was 24 commits ahead of public branch at reset |
| Engine `2f4f4cdf28` | Worker/toolchain compilation; actual Original sealing 1/1 | Mixed history loses G32 at phase-specific import checking; selection fixture expects an unissued operand interface |
| Actual input `593d9f1da9` | Two executed, one owner-refusal pass, confirmed cleanup | Positive fails NativeRootOutsideView for 354aaa09; exact Rust artifact/callsite not yet established |
| Native `cf54086244` | Three of four focused controls pass | Replace partial root partition with exact/new typed planning; fourth expectation needs stage-specific refusal review |
| Lifecycle `6f3e46993b` | Focused owner histories and consumer compilation | Actual Reload bypasses retirement owner; default/force public behavior unqualified |
| Retirement child `a1e88c78c4` | Earlier target compilation | Three WIP files incomplete/uncompiled; preserve before replacing |
| Fence repair `65c930b2fa` | Source-reviewed same-fence typed errors | Compile, execute, then receive; do not treat as already integrated |

Earlier leaf counts remain evidence only for their exact code and boundary. The
32-case historical ledger needs dispositions, not blanket deletion or stale pass
claims. Synthetic graph tests are component controls, not actual GHC histories.

## Ownership and integration

| Area | Current owner | Result |
| --- | --- | --- |
| Compiler World | engine_completion | Actual HMI and lexical graph at consuming stage; mixed cells and retry identity |
| Rust output assembly | world_native_assembly | Final normalized physical issuer retained through prefix and runtime adoption |
| Native planning | audit_native_selection | Exact historical versus new groups, coordinated with assembly owner on shared files |
| Reload/recovery | audit_reload_ownership | Single retained attempt, public default first, then force and recovery |
| Discovery/verification | audit_query_surface | Actual all-five query and notebook histories, exact qualification commands and gaps |
| Integration and unresolved refusal | root | Shared contracts, NativeRootOutsideView diagnosis, critical review, joined checkpoints |

Reuse existing isolated warm checkouts. Children report proposal, blocker,
review-ready and settled events; parents continue the ready frontier. Each area
consolidates reviewed commits, records source/evidence and sends exact OIDs.
No native Codex goals. No unrelated task merely to keep an agent busy.

Root reviews production consumers and failure paths directly. Independent review
checks each joined area for bypasses, repeated work and coupled fixtures. Rebase
on received contracts; do not silently resolve an authority conflict. Publish
integration checkpoints labelled with their actual qualification state.

## Sequence and gates

1. Preserve source/WIP and publish the reviewed checkpoint and reset plan.
2. Repair Haskell stage environments and Rust physical issuance in parallel;
   execute an actual mixed/private-input history immediately on their join.
3. Complete native planning and public default reload; integrate reviewed pieces
   promptly. Root localizes the remaining native refusal before changing policy.
4. Deliver an early Persona candidate after G2. Continue force/recovery and final
   qualification as mandatory campaign work.
5. Reconcile current public release/main, qualify the integrated result and push.

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
