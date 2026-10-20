# Q2/Q3 completion: one admitted operation and correlated items

Status: implemented, compiled and focused-test verified on the checkout below.

## Revision

- Checkout: `/srv/swarm/checkouts/tidepool-completion-q2-admitted-op-20261019`
- Branch: `completion/q2-admitted-op`
- Base: `099c91d97f57de9a47e735137cd8ac7870027c41`
- Implementation commit: `bba9588b69fc85b80a069f7b4a5a03c813fffda4`

## Scope delivered

Q2 (one admitted Haskell operation):

- `BoundCompilerRequest` now carries one `AdmittedCompilerOperation` sum
  (exact, held query, held cell plan, held cell preparation) instead of
  independently inspected operation/basis fields.
- `HeldWorldCellPreparation` is either a typed `HeldWorldCellRefused` or a
  `HeldWorldCellAdmission` owning its question, held binding, validated
  `CellSourcePlan` and matched slots. Admission constructs the plan once.
- `runWorldCellPrepareMode` borrows the admitted matched slots through
  `boundWorldCellSlots`; it no longer re-inspects the wire question.
- `normalizeCompilePurpose` runs once at the compile entry. It canonicalizes
  wrapper order and refuses duplicate settings through
  `CompilePurposeConflict` / `DuplicateCompilePurposeSetting`.
- The held-world recipe is no longer reparsed at consumption
  (`GhcPipeline.hs` old validation call removed). Admission records
  `world.cell.admission_parses`, and consumption still rechecks held-world
  identity, realization, executor and END.

Q3 (one correlated item record and closure entry):

- `WorldCellItemSource` / `PreparedWorldCellItem` combine the typed item,
  parsed source item, captures, capture signatures, issued thin interface and
  prepared artifact. `correlateWorldCellItems` validates exact coverage and
  uniqueness before the terminal item manifest is written.
- Consumers use the correlated collection; the old `zip3`, ordinal lookup and
  capture-signature `error` joins are gone.
- `WorldBindingRow` directly carries its checked `InterfaceRow`, so
  `worldBindingInterfaceArtifact` no longer has an unreachable `error` case and
  row decoding refuses non-checked-value evidence.
- `WorldSourceClosureEntry` couples artifact, canonical proof, decoded
  interface and package evidence. `readWorldSourceInputs` returns one keyed
  collection, and `captureHeldWorldSourceSelection` no longer joins parallel
  maps with `Map.!`.

No BUCK declaration change was required: the modified modules are already in
the existing extractor/test targets and the new case is part of the existing
`source_boot_product_reuse_test_bin` target. Nothing under `tidepool/toolchain`
policy or `exomonad/actor` changed.

## Commands and results

All build and test commands ran as `inanna` through `swarm-build`; Buck used
`--local-only -c remote.enabled=false` (no NativeLink evidence is claimed).

Configure:

```sh
swarm-build bash scripts/buck2-configure.sh --tests
```

Result: exit 0; wrote `.buckconfig.local` and retained toolchain generation
`generation.wLJxMfQV`. Log: `configure-tests-01.log`.

Compile the extractor and the source-boot test binary:

```sh
swarm-build bash -lc 'export PATH="$(/run/current-system/sw/bin/awk -F " = " '\''$1 == "action_path" {print $2}'\'' .buckconfig.local):/run/current-system/sw/bin"; buck2 build --local-only -c remote.enabled=false //bridge/haskell:tidepool_extract_bin //bridge/haskell:source_boot_product_reuse_test_bin'
```

Result: exit 0, `BUILD SUCCEEDED`, Build ID
`51caccd4-1f19-46e8-bc42-0b6eb45c7f73`. Log: `build-16.log`.

Focused tests (wrapper: `run-focused-test.sh` in the evidence directory):

```sh
swarm-build /tmp/q2-run-focused-test.sh --pattern "world cell admission"
swarm-build /tmp/q2-run-focused-test.sh --pattern "world requests require rooted boot hold"
swarm-build /tmp/q2-run-focused-test.sh --pattern "request input custody histories"
swarm-build /tmp/q2-run-focused-test.sh --pattern "request input admission boundaries"
```

| Case | Selected / executed | Result | Log |
| --- | --- | --- | --- |
| world cell admission parses one recipe | 1 / 1 | pass, exit 0 | `test-world-cell-admission-09.log` |
| world requests require rooted boot hold and fresh executor | 1 / 1 | pass, exit 0 | `test-world-request-04.log` |
| request input custody histories | 1 / 1 (QuickCheck 80 + 100 trials) | pass, exit 0 | `test-request-history-03.log` |
| request input admission boundaries | 1 / 1 | pass, exit 0 | `test-request-boundaries-01.log` |
| owned scope input reuse (same source state, earlier run) | 1 / 1 | pass, exit 0 | `test-owned-scope-03.log` |

`git diff --check` exited 0 before the commit.

## One-admission-parse evidence

`worldCellAdmissionParse` exercises the real production preparation path: it
captures a scope and Context parcel, acquires the held world through
`scopedEnsureWorld` with `CompleteExactInputs`, runs the world cell plan stage,
then binds a `WorldCellPrepare` request and validates it. It also checks that
the admitted plan bytes equal `worldCellPreparePlan` and that the admitted
matched slots equal the planned slots.

The focused run reports:

```text
world.cell.admission_parses count=1
```

and the same test passes with `All 1 tests passed`. This count is the only
admission parse for that preparation; the consumption path no longer calls the
recipe validator/parser again.

## Evidence

All logs and the test wrapper are under
`/srv/swarm/evidence/inanna/q2-admitted-op/`. The report is committed under
`plans/shared-warm-compiler/completion/q2/report.md`.

## Limits

- This pass covers Q2 and Q3 only; Q1 and Q4-Q10 remain separate.
- Acceptance is compile plus the focused cases above. The broad `just verify`
  gate and the full Tasty/corpus suites were not run as routine spot checks.
- Remote/NativeLink execution is not claimed; all builds and tests were local.
