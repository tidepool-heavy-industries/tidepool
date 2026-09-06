# Strict durability implementation boundary

Existing write_durable is now strict about directory failures. Two shared owning
functions support the production callers:

- `sync_parent_directory(path) -> Result<(), WriteError>`: caller synced file;
  persist containing directory entry, no parent creation.
- `create_dir_all_durable(directory) -> Result<(), WriteError>`: mkdir-p followed
  by deepest-first directory/ancestor sync, including existing components so retry
  after partially visible creation does not skip persistence. Existing symlink
  target ancestry must be durable and hierarchy owned against concurrent mutation.

write_best_effort stays unchanged. JSONL append will sync the file according to
Data/All and then call sync_parent_directory on EVERY strict append, avoiding
racy existence classification and retry after a failed first publication. None
retains no sync. write_line(File) cannot own the missing path/ancestry; its caller
must establish that once. Parent creation remains explicit and no writer retries
on uncertain post-publication failure.

Node integration, owned by service: replace both current create_parent calls'
std::fs::create_dir_all with create_dir_all_durable for rows and checkpoint paths.
Then test node poisoning/reopen around strict append/checkpoint errors, including
separate fresh hierarchies. This branch does not edit node inbox or host.

Atomic owner is implemented/compiled at scaffold; JSONL consumer and fault tests
remain intentionally incomplete. Test specialist owns separate integration tests
and C injection fixture; this lead owns production atomic/repr edits. Final fresh
review follows exact combined candidate, not this scaffold.
