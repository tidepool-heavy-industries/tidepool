# Applications source inventory

Inventory: 2026-09-10. Two read-only Sol inventories and the committed R7 handoffs
replace the older missing-implementation checklist. Historical executed evidence
below has not been rerun on today's main or the eventual final engine source.

## Exact retained sources

| Input | Commit / evidence |
|---|---|
| Combined coordinator | `0c1fb83f2285775cb16b212ce2e152bbe9a07374` |
| Applications handoff | `8eda2640b5047786f5dcf2af8b7eae9760e5e767` |
| Applications code before handoff | `cff1ce52723a3568dbca197b3aca2936f928c725` |
| Registry-test incorporation in coordinator | `bfcc72afd09df8806e16d1454541b3cee5933414` |
| Last combined fixture baseline | `6bef6363d95ef5c9bb4749cfd5304c89767d9cb7` |
| Matched native candidate | `d84cda697a8dac2842bec09dbd7562a3fab4c926` |
| Engine handoff in combined source | `0c1ff8d9995dddfc30756a2d26d5278749571826` |

Read with `git show <commit>:plans/parallel-dogfood/next-wave/<file>`:
`coordinator-wind-down-handoff-r7.md` at the coordinator and
`applications-wind-down-handoff-r7.md` at the applications handoff.
The coordinator corrects the applications handoff's stale assertion that the
registry test was not incorporated. `a8-matched-pin-evidence-r7.md` contains
command detail; its trailing pending-payload statement is superseded by the later
acceptance commit `dc7d354149988c87a6a926784a99c0d43e6df9c6` and final handoffs.

The native branch is `shoal/typed-continuation-r7/native-delivery-completion`,
with retained worktree `/tmp/tidepool-codex-applications-r7-native` and matching
remote ref. Its tested package was
`/nix/store/y820j4v0ldaim6hdrqn3fa41i6icwm39-codex-rs-0.0.0-dev+d84cda6`.
Retain source refs even if that store output is later collected.

## Completed work and its evidence boundary

| Area | Retained implementation / recorded evidence |
|---|---|
| Binding, admission, host delivery | Exact launch/generation, immutable input, no-overtaking reconciliation; preflight and lost-ack/order/restart checks passed |
| Completion and retirement | Accepted-work/custody domains, correlated hosted completion and fork release; native-seal/hosted-drain/degraded-cleanup checks passed |
| Recovery | Typed resident state, real actor-host recovery, ordered source-only replay, registry races; four focused cases independently rerun |
| Matched applications integration | Actual socket/PTY/TUI, challenged Bind, exactly one provider delivery, foreign-thread rejection, Presented/Compacted fence, process/provider/host cleanup passed |
| Combined fixtures | 217/217 at `6bef6363d`; this does not establish final prepared-engine cutover |

A0–A7 are implemented in the retained candidate; A8 passed on the then-selected
pair. Do not commission those mechanisms from scratch. Final-source acceptance
remains open, including the engine recovery join and reconciliation with new main.
Native execution evidence is x86_64 only; no aarch64 runner exists. No broad
workspace suite was run or is required by this plan.

## Main versus candidate

Inventory main `997a60a6a` already carries the later command-job/resource foundation,
workspace/process fixes and curated routing/prompts; its native pin is
`7259e93777a0c3a323ce8ad6911b836eb1b73d37`. The applications candidate is not on
main. Aggregate swarm-slice placement is separate RSI work still being finalized;
the next launch record must select its accepted successor, not freeze these hashes.

The coordinator source also contains unfinished engine work. It is the complete
preservation source, not a commit to merge wholesale into main. Reconcile the
applications delta and its required runtime interfaces explicitly. Preserve the
engine continuation separately. If an applications check requires the final
engine candidate, record and test that join there; do not silently import an
unfinished engine into main to close applications.

## Native integration scope

The R7 native tip is linear and supersedes the older R6 admission/completion
candidates. Do not replay those separately. Its common ancestor with tooling
`7259e937` is `fe15831c8`; the inventory found actual conflicts in
`codex-rs/tui/src/host_dynamic_tools.rs` and
`codex-rs/tui/src/host_dynamic_tools/input_control.rs`. Compose command `Jobs`
with the candidate's binding/control routes; neither owner replaces the other.
A fresh review should concentrate on these resolutions and their lifecycle edges.
The preserved recovery WIP `9d68c0db` remains provenance, not an extra accepted
patch series to import blindly.

## Actual remaining assignment

1. Consolidate the existing native and Tidepool implementation on launch main,
   retaining newer resource/command owners and resolving concrete conflicts.
2. Compile affected runtime/actor/application targets and rerun the four recovery
   cases plus decisive matched full-TUI input/completion/cleanup checks.
3. Repair only observed defects or uncovered contract gaps. Recheck the same
   recovery/full-TUI boundary when the engine delivers its final changed consumer.
4. Integrate the accepted pair and retire temporary plans. Give the subsequent
   shared-server design an owner/coupling map, not speculative migration code.

Historical degraded cleanup reports are evidence, not proof of current live
processes. External RSI subsequently stopped the previous run at operator request,
keeping the last dead panes and source. Recovery records/scrollback are retained
under `target/dogfood-retirement-20260910`; termination alone does not certify every
old hosted-work/resource obligation. Keep that reconciliation outside the new
implementation tree unless it exposes a reproducible product defect.
