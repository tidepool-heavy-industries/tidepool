# Shared durability owner repair checkpoint

## Disposition

The defect below is repaired in root strict-owner baseline4088cd94 and reviewed
node adaptation58db0e1a, integrated into service17d37211. Service directly reran
the 20-case hit-checked syscall fault test at17d37211: one parent test passed,
44 excluded, nextest71c7c965-4cbf-420f-810f-1c7382bdb39f; daemon teardown observed.
Evidence: service target/service-retirement-evidence/strict-inbox-integrated.log.
This accepts the node-local fence under its one-open-owner, controlled-hierarchy
filesystem-sync contract, not physical power-loss behavior or old-host downgrade.
See notification-contract.md for final child review and evidence.

Native delivery remains unavailable pending matching human native pin. Separately,
strict inbox open revealed a host socket-directory pre-deployment cleanup gap;
service owns its guard/retention repair. Root BindingTable/EventJournal/LogWriter
uncertainty repairs remain independent. No running inbox/host was migrated.

## Original evidence and decision (superseded by disposition above)

Notification inbox candidate 7ebb7c66 is not accepted as a durable pre-send fence.
An independent reviewer injected directory-fsync EIO with LD_PRELOAD and observed
three rejected directory syncs while the fence test passed. This is attributed
fault-injection evidence, not an executed power-loss/replay demonstration.
Evidence: reviewer worktree wt-956c0afb-9c86-468c-8b0e-88523761f27f,
`target/inbox-review-evidence/`.

Service directly inspected the pre-repair owners: `tidepool-atomic-write::write_durable`
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
At that checkpoint the notification lead kept its review obligation pending; no duplicate durability
implementation is authorized here. Execution-scope work remains independent.
Production notification admission remains unavailable before tracked publication
until a real correlated controller exists; no running inbox/host was migrated.
