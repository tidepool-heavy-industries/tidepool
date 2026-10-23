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

## Closeout sequence

The shared owners and their consumers are now joined. Finish their execution
evidence before opening another refactoring wave. Root owns diagnosis and design
decisions; P consolidates exact candidates; V pins and verifies the warm checkout.
Reuse the existing component owners for bounded corrections.

1. Compile the joined production worker, toolchain/runtime and affected test
   consumers. Include the reviewed genuine publication fixtures and compiler-map
   refresh. A compile refusal is not evidence that a runtime design failed.
   Repair the first failing action and retain its complete diagnostics.
2. Execute the small joined histories: mixed source stages, staged first native
   demands and display, and publication followed by reuse of a checked value.
   Pair them with the existing native-entry substitution, World request,
   checked-value closure and carried-TYPE controls. Do not rerun an unchanged
   failure without a changed cause or a discriminating new observation.
3. Execute source classification and publication through actual issuers, then
   Context9 recovery and linked-code properties. The publication tests use the
   existing frozen-entry owner; do not add an authority factory to make them
   pass. The equal-canonical/different-native historical fixture has an unproven
   producer premise: exposed unfoldings may make its interfaces differ. Keep
   that premise fatal, and distinguish a failed fixture construction from an
   exercised product refusal. See [verification](verification.md) for the full
   retained invariants and [TEST-GATES](../deepseek-g2-closeout/TEST-GATES.md)
   for owning selections.
4. Run the required G2 notebook, actor, ordinary reload and refusal histories,
   affected cross-language/corpus controls, and deletion audit. A passing small
   history permits broader qualification; it does not replace these gates.
5. Build and freeze one matched bundle. Verify the actual Persona spec, tools
   and rendered memory before a fresh DeepSeek conversation. Preserve authored
   files and memory. Broader force/fork/recovery matrices, full M1–M3 and packaged
   performance measurements remain G3 work under the existing acceptance split.

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

## Current evidence and decisions

At joined checkpoint `72c34141e3`, linked-code consumers, independent source
scope, source-import protocol 4 and Context9 are consolidated. The first joined
build stopped on a missing `renderDependencyQualifier` import; no histories
executed there. `b9ef749f1c` repairs that import and the two reported formatting
differences. These changes require the next admitted compile and execution.

Earlier executed histories cleared the acquired-payload panic, then exposed two
distinct boundary mistakes. A checked native entry was incorrectly required to
have a complete display-instance environment; it now consumes its existing exact
target proof. The World TYPE reader rejected checked-value dependencies that its
producer legitimately admits; it now preserves those roles through the existing
checked-value decoder. Independent source reviews accepted both repairs, but
their joined runtime effects remain unqualified. Display completeness, source
imports and visible value authority retain their own checks.

Publication review also caught broad compiler reachability selecting an unrelated
source grant. Publication now follows the independently admitted source-root
closure; native-only borrowing cannot recreate source permission. Actual paired
producer/publication and Context9 restore controls remain required. Prior
Context8 success does not qualify Context9.

Replan the architecture only when a concrete remaining history shows that the
accepted owner cannot express a required fact, callers must reconstruct authority,
or preserving one invariant necessarily breaks another. A newly reached failure
is not automatically a new regression; trace its producer and consumer first.
For routine integration errors and invalid fixture premises, repair that boundary
and rerun the discriminating check without broadening the design.

The live exact-source, command, count, cleanup and pending-work ledger is
`/srv/swarm/evidence/inanna/g2-sol-tranche-20261022/validation/V-HANDOFF.md`.
No matched frozen bundle, fresh Persona or G2/G3 acceptance is claimed.
