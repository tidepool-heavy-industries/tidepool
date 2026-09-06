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

## Consumer repair accepted and integrated

Reviewed exact candidate 8a73dffa0f8dc99c122bf0020c28ed4520c3f253;
root merge and directly tested source: 9d64a066afa95ff5eaeec0842e0e9763e76c1bcf.
Root examined production diffs, binding public-path assertions and the independent
reviewer's retained review.md. Accepted EventJournal/monitor mutation fencing,
BindingTable whole-owner mutation AND authorization fencing, exclusive reopen
reconciliation, and LogWriter durable creation/uncertain-sequence fencing.
No serialized format change. Uncertain binding state sacrifices availability;
loaded Active rows remain blocking without fabricating a releasable generation.

Root executed, in the existing Nix environment:
- cargo test -p tidepool-worktree --test journal_uncertainty owning_paths_poison_and_reopen_without_reusing_sequences -- --exact --nocapture: passed (6 subprocess cases).
- cargo test -p tidepool-worktree --test binding_uncertainty binding_public_paths_fence_uncertain_custody_until_reopen -- --exact --nocapture: passed (6 cases).
- cargo test -p tidepool-worktree --test storage_errors binding_failed_ -- --nocapture: 2 passed, no skips.
- cargo test -p tidepool-harness --lib log::writer::fault_tests::public_writer_faults_retain_publication_and_fence_sequence -- --exact --nocapture: passed (4 cases).
- cargo test -p tidepool-harness --lib log::tests: 9 passed.
- cargo check -p tidepool --bin tidepool-selfharness: CompileOnly passed.
- cargo fmt -p tidepool-worktree -p tidepool-harness --check and git diff --check: passed.
Logs and exact fault-binary hashes: /tmp/root-consumer-evidence/.
No physical crash test or live host replacement was performed.

Service incorporation requirement: merge 9d64a066 (or its evidence descendant),
preserving staged service changes. Confirm resulting revision and focused custody/
inbox checks; table uncertainty now revokes current()/active_for_agent() authority.
This does not replace the service-owned separate rows/checkpoint-parent fault,
poison/reopen and process/HTTP/effect-drain gates. Root publication is not evidence
that the busy service lead has received or incorporated it. Known-broken active
amendment transport was not retried or disguised as a queued fallback.
