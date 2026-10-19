# Ordinary-cell dependency reuse and checked import-context experiment

Supervisor: [warm workers](README.md). Arm B is core ordinary-cell work under
the [current delivery sequence](../README.md#next-complete-workflow); it begins
with the first nonempty actor cell and does not wait for the complete pool,
retirement or recovery matrix. Arm C is a bounded subsequent experiment with
a semantic and performance go/no-go. Both use the exact realization, existing
retention owner and current-source baseline. Transport success alone establishes
neither kind of compiler reuse.

## Question and fixed language contract

Can a new metadata query or ordinary cell consume a complete retained exact import environment plus the genuinely new target, without regenerating/re-expanding all baseline imports and rebuilding the selected HPT, while preserving existing Haskell module semantics and original/native ownership?

Normal GHC generalization, language flags/defaulting/MR policy, fixities, aliases/hiding, qualified names, orphan/family/class visibility, exact source identities, typed cell capture/order and original display context remain the contract. The actor's available effects and pending-request locals remain exact to that request. A retained declaration or value remains bound to its original nominal source and implementation edges. No GHCi evaluator, implicit IO result convention, interface-ABI-only cutoff or free-name input filtering is an accepted shortcut.

This wave does not authorize injecting a saved raw `TcGblEnv` into later compilation. It is mutable: knot/used-name/keep refs, TH state/finalizers and plugin callbacks have lifetimes. `initTcWithGbl` creates local scratch but does not clone all global refs. A complete closed import-context projection must be issued by the existing exact owner and consumed through a fresh checking attempt. If that cannot be expressed without unsupported GHC mutation or language drift, stop the arm and retain the ordinary frontend.

## Owned paths and inputs/outputs

Primary ownership:

- `bridge/haskell/src/Tidepool/GhcPipeline.hs`: `residentCompileOne` initial selected-home construction, `CompilePlan`, `compileChecked`, native checking entry and per-module hooks.
- `ExactHydration.hs`: complete selected HPT/GRE/import/instance ownership, lazy hydration knots, `exactHomeInstancesFor` and original-preview context; `DeclarationJoin.hs` source-free interface construction/inspection.
- `app/Main.hs`: `runInspectionMode`, `runCellProgramMode`, generated scaffold source/recipe construction and `checkCellInstances`; `InspectionRunner.hs` and `Introspection.hs` query consumers.
- `Session.hs`, `SessionArtifacts.hs`, `CheckedPrefixImports.hs` and typed segment owners: thin value globals, generation identity, exact import/capture substitution. Change only where the prototype needs a real consumer.
- `tidepool/runtime/src/session/inspection.rs`, `turn.rs`, `view.rs`, existing exact export facade owner; `exomonad/actor/src/resident_workbench.rs` production source/query construction.

Inputs: complete issued exact root and realization, immutable source/template/flags and actual new target syntax, new explicit authored import delta and current purpose/effect/request context. Outputs: unchanged checked observations/diagnostics and, where executable, the same declared product/entry/capture/native contracts under fresh transaction settlement. Experimental measurements identify each reused/recomputed stage and any full-scope floor. Protocol additions require the parent contract owner; do not create an independent query framework or value representation.

## Three controlled arms

| Arm | Concrete implementation boundary | Claimed removable work / unproven remainder |
| --- | --- | --- |
| A: current warm module entry | Existing source/import scaffolds, selected initialHomes construction and ordinary CheckedEnvironment/prepared path, with the same realized bytes from waves 1–2 | Reference behavior and cost. Existing dependency/body reuse and CheckOnly leaf interface elision stay enabled. |
| B: persistent exact HPT, ordinary module entry | Borrow a complete selected persistent HPT/import graph, apply checked new selections, then use normal module checking and source imports | Remove full `initialHomes` reconstruction and any redundant baseline hydration where ownership proves it. Ordinary GHC import/GRE/orphan/instance processing can remain O(scope). Do not relabel this as complete frontend incrementality. |
| C: explicit complete checked-import context / maintained scope interface | Issue a closed import projection from the exact root; borrow its immutable name/type/import/instance facts with fresh target/scratch. Use the existing source-free facade/interface owner where it preserves semantics; consume through the native module frontend or a narrowly specified query entry | Seek elimination of baseline import rendering **and** baseline name/import-environment reconstruction. A single facade import may still expand all exports and traverse instance/orphan closure; count that work. No raw saved TcGblEnv engine or ambient “latest actor session.” |

Implement A/B instrumentation with the real ordinary-cell join first. B retains
eligible completed dependency answers as well as the selected HPT: HPT seeding
alone is insufficient if the pipeline still recompiles unchanged dependencies.
Use the existing world-owned prepared-module state and actual validity/recovery
owners, not a second cache or a full inventory copied into a fresh cache.

Fresh target, request memo, diagnostics, hooks, executor, cancellation and output
reservations remain per attempt. They do not require discarding compatible
immutable dependency answers. Same-world reuse and reuse in a published
successor are separate controls. Preserve source/role/instance/negative
dependencies, actual worker/NameCache/package/interpreter constraints, and
independent backing ownership across predecessor eviction. Do not retain an
ancestor world to keep its scratch alive. If a field cannot safely cross the
boundary, retain its typed reason for recomputation and measure that cost.

The initial empty-world cell implementation clears HPT and starts with no
completed candidates. It is a correctness bootstrap, not the accepted B route.
Failure to establish meaningful B reuse is a campaign design decision to bring
back with evidence; it cannot silently become a completed warm-cell migration.

For C, specify the immutable fields and fresh mutable borrow in a reviewed short design note before coding the entry. If a maintained facade is used, it exports precisely the visible names under the existing lexical membrane, preserves original Names/instances/provenance, and remains separate from retained native custody. It does not export every retained historical implementation or become another selection issuer. A target importing one facade still needs all semantic dependencies, including negative resolution and unused instance imports.

GHCi is a mechanism comparator only: InteractiveContext retains and incrementally extends reader state, but `runTcInteractive` rebuilds TypeEnv from its retained things and unions instance/orphan context. Its naming/defaulting/declaration/evaluation behavior differs. Do not add a fourth evaluator arm merely because the parsed APIs are available. Use pinned HLS's separately owned summary/parse/typecheck/interface projections to guide output boundaries; its stale-editor rules are not execution authority.

## Prototype sequence and semantic stop points

1. Retain A's exact baseline under the joined implementation revision. Count source bytes/import declarations, selected-home traversal, interface hydration, GRE expansion, instance/orphan work, frontend/finalization and products. Preserve timing nesting and actual package-source/profile identity.
2. Implement B only within the exact realization owner. Keep fresh finder/target hooks/knots and selected HPT closure, using delta transitions from the issuing owner rather than reconstructing from all stored versions. Join nonempty binding activation, actual Prepare certification and atomic successor publication now. Compare A/B values and counts for bind then later use with a real imported home module before moving on. Empty-world or query-only successes do not pass this step.
3. Prototype C first for repeated fixed-result pure name/signature queries and a fixed module's bounded export query in one exact scope. Charge full-scope export observations by their actual output rows/bytes rather than requiring constant total work. These already use CheckOnly; the target is elimination of repeated generated checking leaves/import assembly, not prepared codegen. Then add arbitrary TypeOf and availability controls, which still require new expression checking or exact-context solver work.
4. If those controls pass, exercise one new executable typed cell segment and one declaration segment through normal authored source/templates and product issuance. Preserve fresh target identity, produced thin interfaces, capture origins and generated-instance retry policy. Do not remove necessary authored body compilation to meet a count target.
5. Exercise delta histories: append, shadow, fork, changed alias/hiding, instance-only import, package-shadowing home owner, source/helper reload and old retained value. On each transition either derive a correct checked successor or explicitly recompute; do not skip required invalidation merely because no term identifier mentions the dependency.
6. Compare all three arms through the joined actor front door, including cold worker replacement and exact input acquisition. Record each semantic mismatch as a minimized counterexample before adjusting the prototype.

If C needs broad GHC internal patching, new module/unit identity semantics, disabled validation or a changed effect/value ABI, escalate with a concrete boundary proposal to the parent supervisor. Do not expand the experiment silently. Arm B is accepted with meaningful measured reuse and no semantic regression; C can end with a documented no-go. A negative B result requires revisiting the core performance path, rather than postponing that question until final qualification.

## Independent equivalence and refusal controls

Qualification's [baseline/oracle plan](../qualification/01-baselines-and-oracles.md) owns independent expected semantics and [joined histories](../qualification/02-joined-histories.md) owns the actor workflow. Compare observations/products under the same issued root and source, not unrelated regenerated fixtures. Use GHC cold compilation where it is the independent language oracle and explicit selected-root/edge expectations where the language oracle cannot establish runtime authority.

Required cases reuse maintained fixtures and fill specific missing controls:

- Imported aliases/hiding/explicit lists, duplicate occurrences in different namespaces, abstract exports/hidden constructors, fixities/operators, qualified original Names, source roots and implicit Prelude/defaulting/NoMR/rank-N generalization.
- Request-only `respond`/`sessionReply`/`sessionInput` locals and actor effect availability. Same symbol signature with a different instance/orphan closure changes availability correctly. Arbitrary TypeOf must not become lookup of a stored declaration signature.
- Both equal-interface Original winners, optional dormant alternative, required historical child and unrelated child group. Preserve optional-root success and genuine consumed dependency conflict refusal. Never deduplicate divergent original instances by equal Name alone.
- A -> B -> A with deferred home/package type/instance forcing after the switch; mutually recursive/hs-boot knots; constructor/layout changes at equal module spelling; exact old values kept while new source becomes visible.
- TH/quasiquotes, transitive code dependencies, dependent files, compiler plugins and original display instances. Unsupported/untracked profiles retain the existing recompute/refusal rule. Reusing saved metadata does not authorize replaying a splice or reusing it as a fresh compile result.
- Cancellation during context build/query/check, source failure, failed END, stale publication/reload generation, worker eviction/rotation/recovery and inaccessible input. Current purpose and output receipts remain tied to the fresh source operation; old Ready handles cannot bypass currentness.

Failed probes retain diagnostics and exact source mapping; infrastructure/missing input failures cannot be downgraded into authored rejection eligible for a singleton fallback. A homogeneous batch failing user typechecking retains its existing controlled singleton behavior. Query outputs remain complete, with no stale editor result presented as execution evidence.

Compile every changed Haskell/Rust library and direct test target before integration. Use maintained `//bridge/haskell:cell_splitter_test` for syntax/batch policy and `//bridge/haskell:source_boot_product_reuse_test` for real compiler/world controls after exact discovery; counted Rust package tests for runtime/actor callers. Translation/serialization changes require `just fixtures-check` and the relevant native corpus gate. Run through `swarm-build` and pinned setup with exact commands/counts/artifact identities retained. No broad verify as a routine spot check and no passing skip for missing compiler inputs.

## Measures and go/no-go

Hold result rows/bytes and the requested symbol/type size fixed for the singleton-query sweep. Measure whole-scope export/browse observations separately: their output can legitimately grow with scope, so output-proportional enumeration is not a hidden baseline rebuild. Use the same finite histories at small then increasing retained scopes (for example 8/32/128 visible bindings with repeated identical and overlapping distinct dependency roots). Include a changing-scope sequence, not only a cache-hit loop. Keep package closure/profile, current requests, CPU budget and process state comparable. Report first registration, first target, warm repetition, invalidation and cold reconstruction separately. The qualification supervisor chooses repetitions and statistical treatment from observed variance and resource cost.

Structural acceptance for the targeted C fixed-result pure-query hit requires no baseline source/import serialization, no baseline HPT reconstruction/hydration, no generated checking-leaf frontend and no hidden full GRE/instance expansion attributed to the “hit.” New query-specific checking/solver work is counted separately. For a new executable cell, identify exactly which baseline traversals disappear and which GHC module/import/instance floor remains; do not claim O(delta) from a shorter wire or single facade import.

Measure end-to-end actor wall/p50/p95, worker setup/load/typecheck and process CPU/allocation/GC, host offer/admit/seal/receipt work, bytes/imports/names/edges visited, distinct owner facts, retained backing bytes and typed graph generations, process-tree residency/swap/page-in and scratch products. Report costs moved to registration/publication and any larger retention. Historical matched worker service was a minority of whole-case wall and cannot substantiate a 10x worker-only end-to-end claim; current critical-path evidence and campaign host work removal must join the outcome.

**Go:** independent semantics/refusals/cleanup controls pass, the targeted baseline work is absent through the production consumer, and measured amortized latency/CPU improves without unacceptable retained-memory/scratch or maintenance cost under the agreed workload. The parent [qualification performance plan](../qualification/03-performance-and-release.md) owns the measurable useful-benefit criterion, variance treatment and memory/scratch/pressure limits. Lock those decisions after baseline calibration and before the candidate promotion sweep; counts alone do not pass a latency target.

**No-go:** language/nominal/instance behavior differs; mutable checked state crosses worlds; work is only hidden/moved to another full fold; cost is dominated by an unavoidable normal-module floor without material benefit; retention/complexity outweighs savings. Record the discriminator and remaining floor. Keep independently accepted lower waves and the ordinary frontend; do not call the whole campaign frontend-incremental on their behalf.

## Deletion and integration/review gate

On go, delete the superseded full import-context reconstruction/assembly branch for migrated consumers, unused raw helpers and duplicated policy; retain an independent oracle and explicit cold acquisition. Do not maintain two default GHC namespace owners or a permanent feature matrix of A/B/C. Update owning docs and source/template contracts together.

On no-go, delete unaccepted bypasses, experimental public/protocol surfaces, unused production flags and tests of hypothetical behavior. Retain the concise design result, measurements and real counterexamples under the campaign evidence owner. The baseline path stays current; no experimental snapshots remain required to run ordinary cells.

Independent review reads the exact joined source, GHC mutable/lazy owners, produced source/interface contracts, all moved call paths and newly impossible states. Fold the approved result or explicit no-go into the supervisor branch, then run the small joined actor history immediately. Final performance/release acceptance links [qualification](../qualification/03-performance-and-release.md); bundle freezing and deployment remain separate parent work.
