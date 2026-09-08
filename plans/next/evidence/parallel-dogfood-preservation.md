# Preserved dogfood wave

Run `b93ee212-a20d-4e9c-9d13-fb6d77275393` was stopped after all lane
source work was saved. No wave implementation was merged into main.

Start with the coordinator handoff:

```sh
git show 3e26c0af28aa57bacae50a1e0e9132bea5b568f2:plans/parallel-dogfood/wind-down.md
```

The next planner must treat these as partial branches with explicit failing or
unverified gates, not an accepted integrated product. Applications remain at A0;
engine M1/M2 need repair and integration. Detailed private pane captures, bindings,
handoffs and inventories are retained at `target/dogfood-launch-20260908/closeout`.
The M1 `.cabal-m1` directory is a retained generated cache, not unsaved source.

## Source inventory

| Branch | Saved commit |
|---|---|
| `shoal/parallel-dogfood/owners/branches/ghc-api-integration` | `d87fa05dd56cc6446dc16ea0ccfb6ba9ba7d046a` |
| `shoal/parallel-dogfood/owners/branches/review` | `1258aa83c7753f7a3a809a095bdf38d850d4326b` |
| `shoal/parallel-dogfood/owners/applications-lead/a0-working-seam/tidepool-live-fixture/a0-fixture/branches/real-pty-fixture` | `7d142ab81cc49427f0b394d25b45c3aa4c26db4e` |
| `shoal/engine-implementation/m1-m2-frontier/branches/m2-failure-storage` | `e0fbe03b5f80f8419d7912d0dcd57a2dc88aa969` |
| `shoal/ghc-api-integration/integration-analysis/branches/pipeline-seam` | `27dc2893dae50a44d8db9ab78c7b752443e429e2` |
| `shoal/engine-implementation/m1-m2-frontier/branches/m1-ghc-handoff` | `63677698041d54366ab77dff50de730dec550843` |
| `shoal/ghc-api-integration/integration-analysis/branches/memo-tests` | `27dc2893dae50a44d8db9ab78c7b752443e429e2` |
| `shoal/parallel-dogfood/owners/branches/probe-fixtures` | `09c6439ab33a61fa94f4751923e70cb16a4e23f2` |
| `shoal/parallel-dogfood/owners/branches/coordinator` | `3e26c0af28aa57bacae50a1e0e9132bea5b568f2` |
| `shoal/parallel-dogfood/owners/branches/applications-lead` | `aa9a56cae0b9cc1f053e0b31f50b5a8f5e908183` |
| `shoal/parallel-dogfood/owners/applications-lead/a0-working-seam/tidepool-live-fixture/a0-working-seam/branches/review` | `fee203ffa8e34948221d305030b2839248992d8f` |
| `shoal/m2-failure-storage/post-scaffold-investigations/branches/root-failure` | `ff8509f8d40252aa479e6fe727522be2304c95b7` |
| `shoal/parallel-dogfood/owners/branches/engine-lead` | `85f5c48d94df4b387966fae4172c8a0638035cbd` |
| `shoal/parallel-dogfood/owners/branches/identity-inventory` | `2596a73e3c2fa22238b6ae6b22744948823a8bf1` |
| `shoal/parallel-dogfood/owners/applications-lead/a0-working-seam/codex-native-fixture/a0-working-seam/branches/fixture-design` | `0187df13c7f1d80f8542c425ba9e8dd53d365207` |
| `shoal/engine-m0/baseline/branches/cost-baseline` | `84cb4c70a8f426f1ac2e7e69552dce648d8001fb` |
| `shoal/engine-m0/baseline-review/branches/fixture-and-observer-review` | `119c3039f4098112c85c9a5df82f43fdf4e4c069` |
| `shoal/engine-m0/baseline/branches/semantic-baseline` | `e3793ad96df2c822696124a4704d0456ac3632a7` |
| `shoal/parallel-dogfood/owners/applications-lead/a0-working-seam/branches/codex-native-fixture` | `0187df13c7f1d80f8542c425ba9e8dd53d365207` |
| `shoal/parallel-dogfood/owners/applications-lead/a0-working-seam/branches/tidepool-live-fixture` | `fee203ffa8e34948221d305030b2839248992d8f` |
| `shoal/m2-failure-storage/post-scaffold-investigations/branches/external-storage` | `642f2d4fecd893de47662fc1b654ab26bad5022d` |
| `shoal/parallel-dogfood/applications` | `d562e74f2fc50e402d102aaecadc9463dc1372ef` |

The Codex branch belongs to `/home/inanna/dev/codex`, with its retained worktree
at `/tmp/tidepool-codex-applications`. All other branches belong to Tidepool.

## Planner feedback for the next run

- Keep one Sol coordinator and substantial mechanism leads; do not make every
  milestone a mandatory worker or every scaffold a planner approval gate.
- Give the planner a compact saved-source map and consequential open questions.
  Do not replay completed planning or import entire worker histories.
- Keep unresolved decisions in planner notifications; routine status belongs in
  the coordinator artifact. Remove superseded questions.
- Existing typed tasks, deliveries, updates and watches were sufficient. Improve
  compact progress examples instead of introducing more workflow abstractions.
- Warm shared builds before useful fan-out, retain build owners for validation,
  and distinguish build-cache reuse from conversation inheritance and provider
  prompt-cache reuse. Actual savings remain to be measured in the next run.
