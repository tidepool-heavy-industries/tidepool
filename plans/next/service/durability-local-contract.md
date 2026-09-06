# Strict durability owner contract and node integration

The shared directory persistence owner is `tidepool-atomic-write`:

- `sync_parent_directory(path) -> Result<(), WriteError>` persists the containing
  directory after the caller has synced the file. It creates no directories.
- `create_dir_all_durable(directory) -> Result<(), WriteError>` creates directories
  then syncs every lexical ancestor deepest first, including existing components.
  Retrying directory establishment after an earlier error therefore does not skip
  already-visible but unconfirmed entries. This is not permission to retry a write.

The caller owns the hierarchy against concurrent rename/removal. Existing symlink
entries, targets and target ancestry must already be durably established; the
helper does not resolve a separate external symlink target hierarchy. Unsupported
platform/filesystem directory sync fails rather than claiming durable success.

## Old and new semantics

Previously `write_durable` ignored parent-directory open and fsync errors after
rename. It now propagates both, preserving same-directory atomic replacement and
file fsync. A failure may follow visible replacement: it is not rollback evidence.
`write_best_effort` is unchanged and makes no sync promise.

JSONL path append previously synced only its file. `Data` now syncs file data and
required file metadata, then its parent directory; `All` syncs all file metadata
and data, then its parent directory. Both sync the directory on EVERY strict
append, avoiding racy first-creation classification. `None` remains unsynced.
Neither append nor atomic write creates parents implicitly. `write_line(File)`
can sync only its borrowed file; its owner must durably establish the path.
Errors may follow a visible partial or complete row, so no automatic uncertain
retry is permitted. One `write_all` does not guarantee one OS write syscall.

## Service-owned integration recipe

Merge the reviewed owner candidate into the notification candidate before testing.
In node inbox's existing `create_parent`, replace `std::fs::create_dir_all(parent)`
with `tidepool_atomic_write::create_dir_all_durable(parent)` and preserve the nested
error and kind, for example:

```rust
tidepool_atomic_write::create_dir_all_durable(parent)
    .map_err(|error| std::io::Error::new(error.source.kind(), error))?;
```

Keep both calls for rows and checkpoint/cursor paths, before declaring the inbox
open. They can be in separate newly created nested hierarchies; syncing one is
not evidence for the other. Do not silently fall back on unsupported sync.
After a post-publication append or checkpoint error, the notification owner must
retain uncertainty/poisoning and require its defined reopen/reconciliation path,
not roll memory back and blindly retry. Run notification failure/reopen tests
with the repaired owners, including first rows creation, checkpoint replacement,
and directory establishment failure on each separate fresh hierarchy. Owner
unit/fault tests and downstream compilation do not establish this integration.

## Evidence boundary and consumer audit

Process-scoped Linux LD_PRELOAD tests inject directory open/fsync EIO, assert the
injector was reached, assert errors and visible state, and check best-effort/None
remain unaffected. They prove error propagation, not persistence after power loss.
`create_dir_all_durable` must be wired into node before its intended production
consumer is complete; this branch deliberately does not modify inbox or host.

Worktree BindingTable comments assume a failed durable write means unchanged
storage while reverting memory. That assumption no longer holds after a surfaced
post-rename directory failure (file-fsync errors already complicate append owners).
No double-bind defect is established here: binding returns no lease on failure,
and failed settlement retains the active in-memory lease. Root should keep the
caller uncertainty boundary visible rather than broaden this into a storage redesign.
