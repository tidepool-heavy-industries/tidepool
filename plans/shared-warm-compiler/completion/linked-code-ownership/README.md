# G2: carry one linked-code owner

Accepted implementation plan, 2026-10-22. Base `be716054b0`; integration branch
`refactor/g2-linked-code-ownership-20261022`. Root owns integration and technical
review. This replaces the previous blocker-by-blocker repair strategy, not the
existing G2 acceptance contract. G2 and G3 remain unqualified.

The user selected linked-code ownership plus bounded, understood state cleanup.
Fresh runs are required; backward compatibility for old internal compiler
snapshots is explicitly unwanted. Preserve authored files, memory and evidence.

## Contracts and owners

- [Code identity and linkage](code.md): B (`review_private_progress`).
- Codegen image/export bridge: root; bounded caller tests: `linked_codegen_tests`.
- Canonical encoder/export contracts: `linked_graph_review`, coordinated with B.
- [Physical stage and prefix](phases.md): P (`review_package_selection`).
- [Source admission](source.md): S (`g2_package_demand_binding`).
- [Runtime and persistence](consumers.md): A (`g2_actor_custody`) and D
  (`g2_demand_closure`) respectively.
- Live acquired-prefix collection and issued-output consumers: C
  (`review_support_text`), coordinated with B/P/D.
- [Verification](verification.md): T (`review_g2_completeness`) and V
  (`g2_validation`); root reviews independent oracles and integrated consumers.

Each worker uses an isolated source checkout/branch from this contract commit,
records exact base/candidate, compiles affected consumers and executes its bounded
controls. Coordinate concrete output/resource conflicts, not every command.
Current scratch headroom only supports bounded warm work; V maintains the warm
integration lane. No cold expansion without measured admission headroom.

## Sequence

1. B publishes concrete shared types and a producer-to-consumer vertical slice.
   Root adversarially reviews canonical identity and authorization separation.
   P and S work independently on their already-known ownership boundaries.
2. A migrates runtime image preparation/installation to B's exact owner. D
   migrates context persistence using that same representation. They may prepare
   caller inventories first, but must not invent independent graph schemas.
3. T supplies independent properties and the earliest actual staged World/runtime
   history. Compile each joined tranche, then run that history before actor replay.
4. Delete obsolete routes, qualify the joined candidate, build/freeze one matched
   bundle and launch fresh Persona only after its required gates.

Intermediate commits may be incomplete migrations; no release may retain a
selectable old path, compatibility adapter, or a fourth parallel native graph.
Root keeps accepted parcels consolidated and pushes coherent checkpoints.

## Included cleanup and exclusions

Carry existing issued owners instead of reconstructing selection/view pairs.
Replace expanded completion snapshots with component references; reuse the
existing store, HAMTs and transactions. Add typed local refusals and normal
filterable tracing. Authenticated content comparisons may use issuer-owned
identities, but never replace exact acquisition, source or generation checks.

Do not redesign the compiler service, scheduler, generic persistence machinery,
publication fences, parcel transport or runtime staleness observations. No new
global interner, installed-state graph in World, generic test framework or retry
fallback. Dormant availability is not executable demand.

## Baseline evidence and uncertainty

At base: three focused controls pass; the fourth fails because its test compares
raw custody namespace with actual compiler projection. Text fails on a retained
hidden-owner guard; the exact owner/guard is not exposed. Ack fails at completion
join; its bounded graph comparison is inconclusive, not proof of equality.
Keep those facts separate from source-confirmed architectural defects: package
identity includes unrelated issuance peers, HOME reuse omits closed children,
and source dependency adjacency is reused as admission membership.

Evidence: `/srv/swarm/evidence/inanna/g2-sol-tranche-20261022/validation/`.
The diagnostic-only `d21d74b242` is preserved, not integrated or behavior-qualified.

## Progress

- Contract recorded; implementation active in isolated branches. P owns the
  coherent phase/source/consumer join; root owns the graph/codegen join.
- Snapshot preflight integrated at `2e6ff0abd2`: compilation and three counted
  controls pass. This is current-format-7 evidence, not Context8 qualification.
- Graph, codegen, phase, source and staged-test checkpoints exist; joined
  migrations and execution remain pending. Two independent graph findings
  (distinct PACKAGE groups, private helper exports) require repair before acceptance.
- Joined compilation/execution, frozen bundle, Persona: pending.
