# Engine: finish the production prepared-path cutover

Read [resume.md](resume.md), then the retained R7
`plans/parallel-dogfood/next-wave/engine-wind-down-handoff-r7.md` at
`0c1ff8d9995dddfc30756a2d26d5278749571826` with `git show`.
The [engine design](../../haskell-engine-stg.md) still owns semantics and acceptance.
This map updates the execution frontier; it does not relax that design.

## Start from delivered work

M0–M5 and a bounded M6 runtime seam are integrated in the retained coordinator
`0c1fb83f2285775cb16b212ce2e152bbe9a07374`. The last checked fixture baseline is
`6bef6363d95ef5c9bb4749cfd5304c89767d9cb7`. Request 79 provenance `fc8067bd8`
has the same code tree, not an extra missing implementation delta. No successful
operation/handler ABI consultation was produced before wind-down.

The prepared seam writes, caches, parses, links and identifies artifacts, and
executes the bounded M4 subset. It does not yet replace general resident `Eff`
execution. Do not restart frontend/schema/collector scaffolding merely because
older plans describe those initial stages. Reopen them for concrete gaps at the
production join.

Reconcile once onto launch main, retaining current commands, resource containment,
workspace forks and routing/prompts. Preserve original refs; a recorded net-delta
squash is suitable. Keep candidate compiler/artifacts separate from the frozen
runner. The coordinator owns shared fingerprints/generated artifacts and paired
source selection. The applications lane selects its useful delta separately;
do not force unfinished engine code into its main release.

## Remaining tree

1. **Shared ABI decision.** A fresh Astra consultation settles the typed native
   operation/handler contract using the existing code and representative resident
   effect programs. Supply a compact evidence packet; there is no previous answer
   to recover. The Sol lead integrates the decision and owns production dispatch.
2. **Native lowering and managed roots.** From the shared calling/layout/root
   contract, fork meaningful independent outcomes: general `Case`/`Enter` and
   recursive `Call`, operation lowering, multiple physical arguments/wide values,
   and nonempty managed-reference stack-map spill/reload. Respect actual shared
   code ownership; do not invent separate incompatible ABIs to parallelize.
3. **Production integration.** The lead and retained owners exercise the real
   resident `compile_and_run`/workbench consumer on the prepared path. Prove M2
   disposition/reuse and M5 moving-GC with generated live references, including
   cancellation. A bounded standalone native program is not this acceptance.
4. **Remaining failure fixtures.** A bounded worker can add deterministic resident
   compiler-process-loss and concurrent-generation-mutation cases alongside the
   real integration. Structured introspection response tests already exist and
   do not prove these failures end to end.
5. **Cutover, deletion and final checks.** Remove legacy execution/fallback only
   when the production path works; maintain the design's deletion ledger. Run
   applicable `fixtures-check` after extractor/serialization changes, final
   applications recovery/full-TUI checks on the changed consumer, and M7 against
   the M0 baseline after actual cutover.

This is a dependency graph, not a mandatory actor per numbered item. Fork from
useful completed shared reasoning; each Sol parent retains substantive integration.
Report commits, decisive checks and changed gates, using local Haskell routing
for routine evidence. The initial planner does not run the implementation loop.

## Completion boundary

The engine release requires general production behavior and the design's semantic,
memory and deletion gates. Keep x86_64 execution evidence distinct from unavailable
native aarch64 acceptance; no runner request is needed. Existing source-recovery
and applications guarantees must hold on the final consumer. No broad workspace
suites, unrelated server migration or reconstruction of old live actor handles.
External RSI owns old-run custody and host resource monitoring.
