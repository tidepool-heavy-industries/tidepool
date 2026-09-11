# Historical resume record: sleep and applications

This file preserves the source selection and settled decisions used to resume the
resident-sleep and interactive-applications campaign. It is no longer an active
resume instruction. Both implementations are accepted on unified Tidepool main,
paired with native Codex `d0e5fd48e0`. The
[main integration record](../../interactive-applications/main-integration.md) owns
the matched source, evidence and limitations.

Do not recreate the sleep or applications leads from these refs. A later campaign
must choose a new scope and launch from then-current unified main. Git preserves
the earlier actors' source and handoffs; it does not recreate live TUI or Haskell
handles, authority, or checked build state.

## Preserved source provenance

The campaign began from R7 coordinator
`0c1fb83f2285775cb16b212ce2e152bbe9a07374`, containing applications handoff
`8eda2640b5047786f5dcf2af8b7eae9760e5e767`, engine handoff
`0c1ff8d9995dddfc30756a2d26d5278749571826`, and fixture baseline
`6bef6363d95ef5c9bb4749cfd5304c89767d9cb7`. Its original native applications
candidate was `d84cda697a8dac2842bec09dbd7562a3fab4c926`.

Those hashes explain provenance only. Applications and sleep were reconciled with
main rather than preserving every historical patch. Engine M0–M5 and the bounded
M6 seam remain preserved on their own refs; general M6/M7 work was excluded from
the applications and sleep integration. Structured engine introspection remains
part of that preserved engine work, not an unfinished obligation in this release.

There are no external TPLR consumers. The campaign used a coordinated format
cutover and required matched extractor/runtime checks against Shoal usage. The
only available native platform was x86_64; historical evidence must not be widened
into an aarch64 acceptance claim.

## Reusable operating guidance

For a future recursive Sol campaign, start from one explicit main and package
selection. Give each substantial owner a real integration responsibility and fork
related work after shared contracts are concrete. Related children should inherit
the useful completed reasoning prefix and current bound source; choose fresh
context for unrelated mechanisms or independent review. Keep effort stable across
related Sol forks unless the new scope gives a reason to change it.

Use Haskell collectors for mechanical progress and terminal routing. Send model
turns only useful checkpoints, changed decisions, failures, or final outcomes.
Retain uncertain receipts and exact source evidence. External supervision can own
build, cache, disk, usage, and harness observation while product owners run the
decisive product checks. Do not hot-change a running package or treat a completed
build snapshot as authority for different source.
