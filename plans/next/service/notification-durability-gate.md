# Shared durability owner repair checkpoint

Notification inbox candidate 7ebb7c66 is not accepted as a durable pre-send fence.
An independent reviewer injected directory-fsync EIO with LD_PRELOAD and observed
three rejected directory syncs while the fence test passed. This is attributed
fault-injection evidence, not an executed power-loss/replay demonstration.
Evidence: reviewer worktree wt-956c0afb-9c86-468c-8b0e-88523761f27f,
`target/inbox-review-evidence/`.

Service directly inspected current owners: `tidepool-atomic-write::write_durable`
discards both parent-directory open and sync failures after rename;
`tidepool-repr::jsonl::append_new_line` synchronizes the file but not the parent
entry when first creating it. Caller-created ancestry and separately located
rows/checkpoint files need explicit ownership of durability too. Atomic rename
alone is not a durable pre-send fence.

Recommended bounded root-authorized repair: strengthen existing durable owner,
not an inbox-local writer; preserve the explicitly best-effort API. Fail on
strict durability errors, cover first-file/directory ancestry at the existing
creation owners, and test error propagation into the real inbox attempt path.
A reported failure after rename is not rollback; callers must retain uncertainty
and must not release a sendable token or retry mutation using stale state.
Review actual production callers affected by stricter errors and compile them.
Do not silently claim cross-platform directory-sync support.

Root receives this cross-owner decision through cumulative service progress.
Notification lead keeps its review obligation pending; no duplicate durability
implementation is authorized here. Execution-scope work remains independent.
Production notification admission remains unavailable before tracked publication
until a real correlated controller exists; no running inbox/host was migrated.
