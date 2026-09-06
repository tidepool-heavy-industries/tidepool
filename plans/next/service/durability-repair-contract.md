# Root-authorized strict durability repair

Observed defect: service notification reviewer injected directory-fsync EIO and
observed ignored errors; root directly inspected write_durable and confirmed both
parent directory open and sync_all errors are discarded. This establishes error
propagation failure, not a power-crash replay. Notification acceptance is blocked.

Root authorizes a bounded cross-owner repair. A root-owned durability lead owns
`tidepool-atomic-write` strict write/path persistence and `tidepool-repr/src/jsonl.rs`
plus focused tests. Service/notification lead retains all node inbox and host edits.
Root owns manifests and final integration. No duplicate inbox writer or fsync policy.

Contracts:
- Preserve write_best_effort semantics and existing atomic same-directory rename.
- Strict write_durable must report directory open/sync failures. A post-rename error
  means publication may be visible but durability unconfirmed; do not imply rollback
  or permit automatic uncertain retries at consumers.
- Preserve existing no-implicit-parent-creation contract of write/append entry points.
  Design the smallest existing-owner directory-creation/persistence helper needed by
  node's current create_parent consumer. Ensure newly created ancestry and separate
  rows/checkpoint directories are durably established before success is claimed.
  Existing file-handle append cannot prove a path it does not own; name that boundary.
- JSONL SyncPolicy::All must include required new-file directory-entry persistence;
  keep None best-effort and explicitly resolve/document Data semantics without hidden
  policy duplication. Never claim one write_all is guaranteed one OS write syscall.
- Put reusable directory persistence at one owner. If repr needs atomic-write as a
  dependency, root approves that narrow dependency (no runtime/state dependency back).
- Error-injection tests must establish directory-open/fsync propagation, first-file
  creation, nested directory creation and post-publication uncertainty. Keep injector
  process-scoped and test-only; no global production environment fault switch.
- Fresh reviewer requests repairs from retained implementer; compile changed consumers.
  Node's in-flight notification candidate must be tested with repaired owners by
  service before acceptance. Source approval or owner unit checks do not close it.

Root has not authorized live inbox migration, live-host replacement, broad storage
redesign or changes in serialized formats as an incidental durability fix. Platform
or filesystem unsupported durability must fail truthfully, not silently claim strict
success. Escalate concrete compatibility problems with evidence and scoped alternatives.
