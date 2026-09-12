# Engine-lead merge ledger

## Baseline

- Fork point: `badb4661`.
- Engine source: `origin/shoal/typed-continuation/execution/branches/engine-lead` at `0c1ff8d9`.
- M1 source: `origin/shoal/parallel-dogfood-engine/first-implementation-wave/branches/m1-ghc-handoff` at `cbfdea15`.
- Main at merge start: `origin/main` at `4db208c6`.
- Integrated pre-ledger revision: `wip/engine-lead-tranche-1` at `92b48c55b`.

This ledger is generated from `git diff --unified=0 badb4661..0c1ff8d9`.

## Summary

| Disposition | Hunks | Files | Reason |
| --- | ---: | ---: | --- |
| Present unchanged | 338 | 96 | Branch-only content is byte-identical to the integrated tree. |
| Present with local reconciliation | 4 | 3 | Compiler-required or requested local adjustment retained the branch change. |
| Retained decision plans | 6 | 6 | The six named decision records remain. |
| Present through reconciliation | 163 | 21 | Branch changes were carried alongside main. |
| Main-selected | 235 | 26 | Main was authoritative or already carried the application work. |
| Dropped campaign plans | 51 | 51 | Historical campaign/process records. |

Total: **797 hunks across 203 files**.

The three locally reconciled branch-only files are
`tidepool-codegen/tests/proptest_jit_dispatch.rs` (the exhaustive
`MachineUnavailable` classifier),
`tidepool-protocol/src/effects/introspection.rs` (`core_module: None`), and
`tidepool-runtime/src/session/prepared.rs` (the requested vocabulary comment).

The retained decision plans are:

- `plans/parallel-dogfood/engine-m0-baseline.md`
- `plans/parallel-dogfood/m1-ghc-handoff.md`
- `plans/parallel-dogfood/next-wave/engine-abi-root-contract.md`
- `plans/parallel-dogfood/next-wave/m3-wire-contract-r7.md`
- `plans/parallel-dogfood/next-wave/engine-m2-final-r7.md`
- `plans/parallel-dogfood/next-wave/engine-m5-final-r7.md`

The 51 dropped plan hunks are one ledger group: campaign proposals, releases,
handoffs, rebase records, integration manifests, lane checkpoints, reviews,
and the committed patch artifact under `plans/parallel-dogfood/`. They describe
a campaign main already marked historical. `execution-contract.md` is restored
exactly from main.

## Byte-identical main selections

These 23 reconciliation files are byte-identical to main after integration;
their branch hunks are accounted for by main's independently landed/current
version:

- `flake.lock`
- `flake.nix`
- `haskell/src/Tidepool/Introspection.hs`
- `haskell/test/suite_cbor/.source-fingerprint`
- `tidepool-actor/src/lib.rs`
- `tidepool-actor/src/lineage.rs`
- `tidepool-actor/src/request.rs`
- `tidepool-actor/src/request/updates.rs`
- `tidepool-agent/src/backend/codex/input_control.rs`
- `tidepool-agent/src/backend/codex/node.rs`
- `tidepool-agent/src/interactive.rs`
- `tidepool-agent/src/lib.rs`
- `tidepool-extract-cmd/src/daemon.rs`
- `tidepool-node/src/inbox.rs`
- `tidepool-node/src/inbox/strict_faults.rs`
- `tidepool-node/src/inbox/tests.rs`
- `tidepool-runtime/tests/session_decl_recovery.rs`
- `tidepool/src/actor_host.rs`
- `tidepool/src/actor_host/hosted_retirement.rs`
- `tidepool/src/actor_host/hosted_retirement/tests.rs`
- `tidepool/src/actor_host/scoped_custody/tests.rs`
- `tidepool/src/shoal.rs`
- `tidepool/tests/interactive_applications.rs`

## Escalation files

| File | Current delta from main | Ledger disposition |
| --- | --- | --- |
| `tidepool/src/actor_host.rs` | untouched | Main retained. |
| `tidepool-actor/src/resident_workbench.rs` | `671+/0-` | Branch content retained; `27538093` adds two compiler-required lines. |
| `tidepool-runtime/src/session/inspection.rs` | `450+/4-` | Branch content retained; `27538093` adds `StructuredInfo`, `StructuredType`, `NameQuery`, `ScopeProvenance`, `NameScope`, and reformats `Browse` (`+173/-2` within that commit). |
| `tidepool-actor/src/resident_actor.rs` | `11+/0-` | Branch content retained; no main content removed. |
| `tidepool/src/host_dynamic_tools.rs` | main version | The duplicate branch assignment was reverted because it moved `challenged_binding` twice and prevented compilation. |
| `haskell/src/Tidepool/GhcPipeline.hs` | `86+/19-` | Branch content retained; the only escalation file with main content removed. |

## Red tests

Not green. `just suite tidepool-codegen` reported two red subnormal-float
property tests:

- `proptest_host_arrays::double_decode_show`
- `proptest_host_arrays::smoke_doubles`

Reproduce with `just suite tidepool-codegen`. Main-baseline status was not
checked. No test assertion or implementation was changed to address these
failures.

## Deviations and limits

- Commit `27538093` is titled `chore(merge): prune historical campaign records`
  but also contains substantive reconciliation in
  `tidepool-runtime/src/session/inspection.rs` and two lines in
  `tidepool-actor/src/resident_workbench.rs`. Those changes are retained for
  review; the commit subject is not a complete description of its contents.
- The plan arithmetic in the steering brief did not match the measured tree:
  57 campaign plans were imported. Retaining the six named records drops 51,
  not 56.
- The workspace compile-only command was interrupted before a terminal result
  at user direction. This ledger does not claim a completed workspace compile.
