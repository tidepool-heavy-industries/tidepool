# Milestone: shared-warm integration join

This note records the state of the shared-warm compiler campaign at the
integration join. It is written for a reviewer who did not participate in the
campaign. Evidence lives outside Git under
`/srv/swarm/evidence/inanna/shared-warm-compiler-20261019/final-join/`.

## Branch

- Checkout: `/srv/swarm/checkouts/tidepool-join-final-20261019`
- Branch: `integration/compiler-query-warm-final`
- **Code tip: `92fa518105`** (Batch A merge)
- Documentation-only commits follow it: `75476fb97b` (these notes) and
  `e5e86803e5` (guide and plan anchor corrections). Treat `92fa518105` as the
  OID for any code or test claim.

The join contains, in first-parent order: `1df404b22f` (handlers/moduleExports
cut), merge `630f6e5d35` (native product anchor), merge `961cc99a2a` (d5 reopen
fence), merge `dbce7de523` (stale reconciliation), merge `eadc18a6fe` (reload),
merge `34ae3b346c` (warm unified base), merge `a9e8ce5f25` (scoped target
image), `f908e58302`, `f13005b0b7`, `f87bfb59af` (restored source-transaction
accessor), `5767157d54` (five-verb correction), `e34183b852` then reverted by
`1b37c01049` (unproven install telemetry), `76b557305e` (bounded acceptance
stack), `a4aa30a0a2` (front-door merge), `92fa518105` (Batch A merge),
`75476fb97b` and `e5e86803e5` (documentation).

The warm unified base had two conflicts, resolved per
`reviews/MERGE-PREFLIGHT.md`: `source_admission.rs` takes the d5 side, and
`activation_input_tests.rs` was migrated to the W1 `CellRecipe` /
`admit_planned_cell_for_execution` shape. The first join build then failed
`E0599` because the d5 side dropped
`RuntimeSourceCompilerTransaction::origin()`; `f87bfb59af` restores it. The
Batch A merge conflicted in `declaration_context.rs`; the join kept its newer
checked-issuance block, which already performs recover/issue/retain with an
extra `is_full` guard and a negative coverage control.

## What the join delivers

- Five model-facing query verbs (`currentScopeView`, `rowFit`,
  `typeOfExpression`, `typeSearch`, `scopeBindings`) generated from the
  protocol schema and reachable through the resident workbench. `moduleExports`
  and its answer wire types are deleted rather than retained as an adapter.
- One compile view per actor with entry-pin and post-reply drift refusal for
  every query, including unpinned queries.
- Warm-cell publication: exact source selection, issued native custody, and
  atomic successor adoption.
- The original reload parcel (`eadc18a6fe`): unsealed preview installation
  refuses, spent admissions are not reusable, cached renderers require fresh
  admission. Its follow-up branch is blocked; see `OPEN-PARCELS.md`.
- A prepared front-door fixture compiled through the toolchain path.

There is no install-provenance telemetry: the campaign's
`compiled_off_checkout` constant was reverted because certified targets carry
no compile provenance.

## Verification

Logs are in `final-join/logs/`. Except where noted, the runner's exit status was
not retained in the log; counts come from the libtest summary printed there.

| Evidence | OID | Selector or target | Result |
|---|---|---|---|
| `01-configure.log` | pre-join | `scripts/buck2-configure.sh` | log records `Wrote .buckconfig.local`; the outer invocation, `--tests`/`--browser` flags and exit status are not retained |
| `02-join-build.log` | join | affected native targets | first attempt `BUILD FAILED` (`E0599`, 1670 commands, 99% cache); retry `BUILD SUCCEEDED`, 16 local commands, 0% cache |
| `03-snapshots-check.log` | pre-front-door | `//bridge/protocol:snapshots_check` | analysis-time check, Pass 0/Fail 0; not a test run and not evidence of executed behavior |
| `04-actor-dispatch.log` | pre-front-door | `resident_workbench::request_tests::compiler_query_verbs_decode_dispatch_and_resume` | 1 selected / 1 executed / 1 passed |
| `05-persona-successor.log` | pre-front-door | `actor_host::startup_persona_acceptance_tests::persona_with_shared_workspace_keeps_effects_and_later_cells_usable` | 1 selected / 1 executed / 1 passed |
| `07-front-door-join-a4aa30a0a2.log` | `a4aa30a0a2` | `introspection_front_door_prepared_result_executes` | 1 selected / 1 executed / 1 passed; executes `result = pure 43`, **not** `frontDoor` |
| `08-second-cell-case1-post-revert.log` | `1b37c01049` | `actor_host::call_timing_tests::second_cell_install_compiles_off_checkout` | 1 selected / 1 executed / 0 passed / 1 failed, exit 1 (expected after the telemetry revert) |
| `09-batch-a-11-on-join-92fa518105.log` | `92fa518105` | eleven Batch A selectors | 11 selected / 11 executed / 11 passed, 0 failed, 30 ignored |
| `10-dispatch-75476fb97b.log` | `75476fb97b` | query dispatch selector | 1 selected / 1 executed / 1 passed |
| `11-persona-75476fb97b.log` | `75476fb97b` | persona successor selector | 1 selected / 1 executed / 1 passed |
| `12-toolchain-full-75476fb97b.log` | `75476fb97b` | full `tidepool-toolchain` unit suite | 1116 selected / 1086 executed / 1054 passed / 32 failed / 30 ignored, exit 1 |

The full toolchain run improves on the recorded trunk baseline
(1029 passed / 44 failed / 30 ignored): +25 passed, -12 failed, same ignored.
It is not green. `final-join/TOOLCHAIN-SWEEP.md` classifies the 32 failures as
2 stale, 12 product-suspect and 18 undetermined.

## Not verified at this tip

- Full `exomonad-host`, `exomonad-actor` and `tidepool-runtime` unit suites were
  not executed at the join; host and actor test binaries were linked as a side
  effect of the named selectors. No runtime unit-test link is retained.
- `A1c` (`published_introspection_example_returns_structured_declaration`) was
  not run at this tip. Its last retained run is red at `36827d80a7` (0 passed /
  1 failed, exit 1) and also red at handlers `5eb2b3dbed`.
- The 44-name trunk-red ledger was not rerun on the join as a ledger; the full
  toolchain run supersedes it with 32 failures at this OID.
- No frozen release bundle was assembled or qualified, and no packaged
  end-to-end persona run was performed.
- The front-door fixture was not rerun after `a4aa30a0a2`.
- Build timings are not a speedup measurement; the first join attempt was 99%
  cached, not cold.

## Honest summary

The code at `92fa518105` builds, the five-verb surface is consistent across
protocol, generated bridge, Haskell helpers, prompts and fixture, and the named
selectors pass. The toolchain suite is 32 red, the second-cell off-checkout
case is red by design after removing a false fact, A1c is red as last observed,
and five repair parcels are blocked in review. This is a coherent integration
state, not a release.
