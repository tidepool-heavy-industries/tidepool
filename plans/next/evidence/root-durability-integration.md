# Root strict-owner integration and required consumers

Accepted reviewed owner candidate 2d6bbc7f29d50bf8a85b6e0137523a759dda05c8.
Integrated and directly tested root revision 4088cd9463e7aff1938a3f20c58c449391ad5d5a.
Root inspected owner diff and full typed delivery/review evidence. Direct checks in
inherited repository Nix shell, all expected passing:

- `cargo test -p tidepool-atomic-write --lib`: 5 executed/passed.
- `cargo test -p tidepool-atomic-write --test strict_directory strict_directory_faults_are_reported_after_visible_publication -- --exact --nocapture`:
  1 parent test passed, exercising 5 injected subprocess cases (1 helper filtered).
- `cargo test -p tidepool-repr --lib jsonl::tests`: 6 passed, 175 filtered.
- `cargo test -p tidepool-repr --test strict_jsonl_directory strict_jsonl_append_syncs_new_and_existing_directory_entries -- --exact --nocapture`:
  1 parent passed, exercising 6 subprocess cases (1 helper filtered).
- `cargo check -p tidepool --lib`: CompileOnly passed, not downstream behavioral proof.
- `cargo fmt -p tidepool-atomic-write -p tidepool-repr --check`, diff check: passed.

Logs: /tmp/root-durability-evidence/{atomic-unit,atomic-fault,jsonl-unit,jsonl-fault,downstream-check}.log.
Fault-test binary SHA256:
- atomic: 4d961d4917f31fc5969a627aa525ba88039ea6321463d5e82dd396aaa21d148c
- JSONL: d514a2c4b05c6412332b5dce10550447cee621431fc4e8bce57854adf7554902
These establish propagated syscall failures and visible uncertain publication,
not physical power-crash behavior. Best-effort semantics remain unchanged.

## Service integration gate

Service must incorporate the exact owner baseline (merge/cherry-pick preserving
staged custody/notifications), replace node inbox create_parent implementation with
shared create_dir_all_durable for BOTH rows/checkpoint hierarchies, and rerun strict
fault/poison/reopen tests including separate fresh nested paths. This is existing
owner wiring, not permission to add an inbox writer. Publication of this root
baseline is not yet acknowledged service/child incorporation. No live inbox/host
migration occurred. Root will not repeat a known-failing amendment as a queued
assignment fallback.

## Separately authorized consumer follow-up

Retained durability lead now owns bounded fixes for EventJournal ambiguous-append
cursor reuse, BindingTable stale rollback after uncertain publication, and LogWriter
new-path durability. Node/host remains service-owned. Require owning public-path
fault tests, retain-first uncertainty, no unsafe further writes before authoritative
reconciliation/reopen, exact binding generations and independent review. No double-
bind exploit was established by the initial finding. This does not reopen accepted
strict writer mechanics or authorize a broad storage redesign.
