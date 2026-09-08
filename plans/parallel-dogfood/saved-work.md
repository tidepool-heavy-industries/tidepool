# Saved work for lane owners

These commits are preserved partial artifacts from the stopped wave, not claims
about current integrated behavior. Git objects were checked locally before this
restart brief. Read only your lane's documents with `git show COMMIT:PATH`.
Review actual diffs and integrate into the new source before further forks.

| Owner | Saved commit | Read next / remaining work |
|---|---|---|
| Applications | `aa9a56cae0b9cc1f053e0b31f50b5a8f5e908183` | `plans/parallel-dogfood/applications-wind-down.md`; includes partial A0 at `fee203ffa8e34948221d305030b2839248992d8f`, needs combined review and process/deployment seam |
| Engine lead | `85f5c48d94df4b387966fae4172c8a0638035cbd` | `plans/parallel-dogfood/engine-wind-down-handoff.md`; reviewed M0 at `1258aa83c7753f7a3a809a095bdf38d850d4326b`, M1/M2 separate |
| M1 | `63677698041d54366ab77dff50de730dec550843` | `plans/parallel-dogfood/m1-ghc-handoff.md`; compile corrected worker probe, exercise direct/resident entry, finish typed-site elaboration before preparation |
| M2 | `e0fbe03b5f80f8419d7912d0dcd57a2dc88aa969` | `plans/parallel-dogfood/m2-failure-storage-handoff.md`; scaffold with unmerged children below |
| M2 root/failure | `ff8509f8d40252aa479e6fe727522be2304c95b7` | Known allocation case reports `MissingStackMap` rather than `HeapOverflow`; establish actual safepoint PC convention, repair before acceptance |
| M2 external storage | `642f2d4fecd893de47662fc1b654ab26bad5022d` | focused evidence exists, shared-file conflicts with root/failure require one integration owner |

Native A0 fixture is in the **Codex** repository at
`d562e74f2fc50e402d102aaecadc9463dc1372ef`, retained at
`/tmp/tidepool-codex-applications`. It was tested against older `d760c5c`.
Current Tidepool pins `06d99357becc4d870f5b5141ba7626daf68e819a` with workspace
admission; preserve that mechanism when adapting A0. Use an isolated native
checkout and explicit cross-repository evidence. Never use a Codex hash as a
Tidepool worktree seed or alter the original Codex working checkout.

Detailed historical reports and shutdown observations remain outside the prompt
package in `target/dogfood-launch-20260908/closeout/`. The external supervisor owns
that inventory; descendants do not need raw pane captures. Prior reported checks
are historical evidence. Verify affected boundaries at the resulting integration.
