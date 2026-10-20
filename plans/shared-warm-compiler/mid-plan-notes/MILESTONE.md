# Milestone: shared-warm integration join

This note records the state of the shared-warm compiler campaign at the
integration join. It is written for a reviewer who did not participate in the
campaign. Supporting evidence lives outside Git under
`/srv/swarm/evidence/inanna/shared-warm-compiler-20261019/final-join/`.

## Branch

- Checkout: `/srv/swarm/checkouts/tidepool-join-final-20261019`
- Branch: `integration/compiler-query-warm-final`
- Tip: `a4aa30a0a2` (merge of the five-verb front door)

The join carries, in order: the handlers/moduleExports cut (`1df404b22f`), the
native product anchor (`630f6e5d35`), the d5 reopen fence (`961cc99a2a`), stale
reconciliation (`dbce7de523`), the reload parcel (`eadc18a6fe`), the warm
unified base (`34ae3b346c`), the restored source-transaction accessor
(`f87bfb59af`), the scoped target image (`a9e8ce5f25`), the second-cell
telemetry and bounded acceptance stack (`e34183b852`, `76b557305e`), the
five-verb documentation correction (`5767157d54`), and the front door
(`a4aa30a0a2`).

Two conflicts in the warm unified base were resolved per
`reviews/MERGE-PREFLIGHT.md`: `source_admission.rs` takes the d5 side, and
`activation_input_tests.rs` was migrated to the W1 `CellRecipe` /
`admit_planned_cell_for_execution` shape. The first join build then failed with
`E0599` because the d5 side had dropped
`RuntimeSourceCompilerTransaction::origin()`; `f87bfb59af` restores it.

## What the join delivers

- Five model-facing compiler-query helpers (`scopeView`, `rowFit`,
  `typeOfExpression`, `typeSearch`, `scopeBindings`) generated from the
  protocol schema and reachable through the resident workbench. `moduleExports`
  and its answer wire types are deleted rather than retained as an adapter.
- One compile view per actor with entry-pin and post-reply drift refusal for
  every query, including unpinned queries.
- Warm-cell publication: exact source selection, issued native custody,
  atomic successor adoption, and certified prepared installs that record
  off-checkout compilation.
- The reload parcel: unsealed preview installation refuses, spent admissions
  are not reusable, and cached renderers require fresh valid admission.
- A front-door fixture that executes through the toolchain path rather than a
  test-only constructor.

## Verification performed on the join tip

All commands ran through `swarm-build bash -lc 'cd <checkout> && JUST_TEMPDIR=/tmp just ...'`.
Counts are selected/executed/passed.

| Evidence | Selector or target | Result | Exit | Log |
|---|---|---|---|---|
| Configure | `scripts/buck2-configure.sh --tests`, then `--browser` | succeeded | 0 | `final-join/logs/01-configure.log`, `06-configure-browser.log` |
| Join build | affected native targets | BUILD SUCCEEDED, 16 local commands cold | 0 | `final-join/logs/02-join-build.log` |
| Schema check | `//bridge/protocol:snapshots_check` | analysis-time check, Pass 0/Fail 0 | 0 | `final-join/logs/03-snapshots-check.log` |
| Query dispatch | `resident_workbench::request_tests::compiler_query_verbs_decode_dispatch_and_resume` | 1/1/1 | 0 | `final-join/logs/04-actor-dispatch.log` |
| Persona successor | `actor_host::startup_persona_acceptance_tests::persona_with_shared_workspace_keeps_effects_and_later_cells_usable` | 1/1/1 | 0 | `final-join/logs/05-persona-successor.log` |
| Front-door fixture | `introspection_front_door_prepared_result_executes` | 1/1/1 | 0 | `final-join/logs/07-front-door-join-a4aa30a0a2.log` |

`snapshots_check` is an analysis-time consistency check, not a test run; its
zero counts are expected and are not evidence of executed behavior.

## Not verified at this tip

- Full `exomonad-host`, `exomonad-actor`, and `tidepool-runtime` unit-test
  suites were linked, not executed.
- The `tidepool-toolchain` suite was compiled; only the selectors listed above
  were executed.
- `A1c` (`published_introspection_example_returns_structured_declaration`) is
  still red at this source. See `OPEN-PARCELS.md`.
- No frozen release bundle was assembled or qualified. No end-to-end persona
  run of the packaged bundle was performed for this tip.
- Build timings here are cold-cache for this checkout; they are not a speedup
  measurement against trunk.

## Honest summary

The code at this tip is coherent: it builds, the five-verb surface is
consistent across protocol, generated bridge, Haskell helpers, prompts and
fixture, and the named executed cases pass. It is not a release milestone.
Several unmerged repair parcels and one known product defect remain; they are
listed in `OPEN-PARCELS.md` rather than silently folded in.
