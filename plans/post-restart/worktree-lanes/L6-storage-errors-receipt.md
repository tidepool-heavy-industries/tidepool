# L6 receipt — storage errors (genuine I/O panics → typed `StorageFailure`)

## What landed

Every genuine filesystem I/O panic in `registry.rs`, `binding.rs`, `journal.rs`,
and `create.rs` now returns `WorktreeError::StorageFailure { path, detail }`,
naming the specific file or directory the failing operation targeted. Each
module got a small private `storage_failure(path, detail: impl Display) ->
WorktreeError` helper (duplicated per module rather than added to the frozen
`error.rs`, since it's a trivial constructor and this lane was told to flag
rather than change frozen files). `BindingTable::persist` and
`EventJournal`'s I/O sites changed from `.expect`/`panic!` to `Result`, exactly
as anticipated; `EventJournal::end_cursor` needed no change (see below).

`cargo nextest run -p tidepool-worktree` → **45 tests run: 45 passed, 0
skipped** (the 35 pre-existing tests, unedited, plus 10 new in
`tests/storage_errors.rs`). `cargo check --workspace`, `cargo clippy -p
tidepool-worktree --all-targets`, `cargo fmt --all -- --check` all clean.

## Site inventory, by module

### `registry.rs` (16 panic sites found)

| Site | Decision | Reason |
|---|---|---|
| `now_ms()` — clock before epoch | **Leave** | Machine broken in a way no `Result` helps with; matches `error.rs`'s own stated exception. |
| `write_atomic`: `path.parent().expect(...)` | **Leave** | Not I/O — a structural invariant of `record_path`, which always joins onto `RECORDS_DIR`. Unreachable by construction. |
| `write_atomic`: `NamedTempFile::new_in`, `write_all`, `sync_all`, `persist` | **Convert** | Genuine I/O against the record file/dir. `write_atomic` now returns `Result`; its one caller (`put`) propagates. |
| `write_atomic`: best-effort directory `fsync` | **Leave (already non-panicking)** | Already an `if let Ok(..)`, deliberately best-effort. |
| `open`: `create_dir_all(root)`, `.canonicalize()`, `create_dir_all(records_dir)` | **Convert** | `create_dir_all`/`canonicalize` are explicitly in the CONVERT boundary; a bad registry root is exactly the caller-actionable case the variant exists for. |
| `put`: `serde_json::to_vec_pretty(receipt).expect(...)` | **Leave** | Serializing our own type; unreachable by construction. |
| `get`/`list`: `fs::read`, `fs::read_dir`, dir-entry iteration | **Convert** | Explicitly in the CONVERT boundary. |
| `get`/`list`: `serde_json::from_slice(...).unwrap_or_else(panic!)` on record bytes | **Convert** — see "Deserialization decision" below | |

### `binding.rs` (10 panic sites found)

| Site | Decision | Reason |
|---|---|---|
| `open`: `create_dir_all(root)`, `read_dir`, dir-entry iteration, `fs::read` | **Convert** | Same reasoning as registry. |
| `open`: `serde_json::from_slice(...)` on a binding record | **Convert** — see below | |
| `persist`: `serde_json::to_vec_pretty(&rows).expect(...)` | **Leave** | Serializing our own type. |
| `persist`: `path.parent().expect(...)` | **Leave** | Structural — `path_for` always joins onto `self.root`. |
| `persist`: `NamedTempFile::new_in`, `write_all`, `sync_all`, `persist` | **Convert** | Genuine I/O. `persist` signature changed `() → Result<(), WorktreeError>`, exactly as anticipated in the spec; `bind`/`settle` already returned `Result` so propagation was a one-line `?` each, no further signature changes needed. |

### `journal.rs` (9 panic sites found)

| Site | Decision | Reason |
|---|---|---|
| `now_ms()` — clock before epoch | **Leave** | Same as registry's. |
| `open`: `create_dir_all(parent)`, `OpenOptions::open` (create), `File::open` (read) | **Convert** | Genuine I/O. |
| `open`: `line.expect(...)` from `reader.lines()` | **Split** — see below | |
| `open`: JSON-parse failure on a line | **Unchanged (already non-panicking)** | This was already the torn-final-row skip-and-log path; not a panic, not touched beyond leaving it as the reference behaviour. |
| `append`: `serde_json::to_string(&entry).expect(...)` | **Leave** | Serializing our own type. |
| `append`: `OpenOptions::open` (append), `writeln!`, `sync_all` | **Convert** | Genuine I/O; `append` already returned `Result`, so this was a local change only. |
| `end_cursor()` | **No change needed** | It reads only the in-memory `entries` Vec (loaded once at `open`); it does no I/O and never panicked. The spec's mention of it as a signature-change candidate doesn't apply to the code as it stands — flagging this explicitly per the "say so" instruction rather than silently doing nothing. |

### `create.rs` (1 panic site found)

| Site | Decision | Reason |
|---|---|---|
| `create`: `fs::create_dir_all(&self.worktree_root).unwrap_or_else(panic!)` | **Convert** | Explicitly in the CONVERT boundary; `create` already returns `Result`, so this was a one-line change. |

Total: 36 sites inventoried (spec estimated "roughly 38" — close enough that
I'm confident the sweep was exhaustive; verified with a `grep -n 'panic!\|
\.expect(\|unwrap_or_else(|e| panic'` pass across all four files before and
after, and again post-edit to confirm only the deliberately-left sites
remain).

## Deserialization decision

**Registry and binding records: convert to `StorageFailure`, not a panic, and
not silently skipped either.**

Reasoning: `write_atomic`/`persist` write records via temp-file + `fsync` +
rename — a crash mid-write cannot land a torn file at the target path; the
path holds either the old content or the new content, never a partial one.
That's structurally different from the journal (see below), so a corrupt
record here is NOT the expected, recoverable byproduct of an ordinary crash —
it signals something else went wrong (bit rot, a hand edit, a filesystem
fault, a foreign process writing where it shouldn't). That's exactly the class
of runtime-storage fault `StorageFailure` exists for, not the
unreachable-by-construction class that stays a panic.

I also rejected silently skipping a corrupt record (treating it like `Ok(None)`
in `get`, or dropping it from `list`): `registry.rs`'s own docs draw a careful,
deliberate line between `WorktreeNotRegistered` (typo) and `WorktreeLost`
(data loss) specifically so one failure mode is never allowed to masquerade as
the other. A corrupt record is a third, distinct thing — collapsing it into
"never registered" would hide it behind that same silent-collapse pattern the
existing docs already call out as wrong. A loud, typed, caller-actionable
error is the correct fit, and it's the direct instance of the general
principle stated in `error.rs`: loud means an error the caller must handle,
not a crash it cannot.

`list()` fails on the first corrupt record it hits, same as it always did
under the panic (that panic aborted the whole call — now `Err` does, with the
same "first bad record halts the rest" granularity). This is an error-shape
change only, not a behaviour change: WHEN `list()` gives up is unchanged: HOW
it reports is what became typed. Redesigning `list()` to skip bad records and
return the rest would be a behaviour change beyond error shape, and out of
scope here.

**Checked against L3's torn-final-row behaviour, and it does NOT transfer**:
the journal's skip-and-log is specifically justified by append-in-place
writes, where a crash mid-`writeln!` really can leave a torn trailing line
while every earlier line stays intact and valid — an expected, routine
failure mode for that write pattern, not corruption. Registry/binding writes
never touch the target file in place at all; the write happens entirely in a
temp file that's atomically swapped in, so there is no routine "torn write"
failure mode to design a tolerant path for. Same reasoning, different
mechanism, different conclusion — which is what "check it against L3's
existing behaviour" asked for.

**One real bug found and fixed along the way, in the journal itself**: the
module doc already claimed "the one recoverable-by-design failure — a torn
final row — is handled explicitly below and never reaches a panic", but the
actual code only granted that treatment to a line that decoded as valid UTF-8
and then failed *JSON* parsing. A line torn mid-multi-byte-UTF-8-character (at
least as plausible from a crash mid-`writeln!` as a torn JSON body) hit
`line.expect(...)` on `reader.lines()` and panicked the whole `open()` —
exactly the failure the doc comment claims doesn't happen. Fixed by matching
on `io::ErrorKind::InvalidData` (the specific error `read_line` raises for
invalid UTF-8) and giving it the identical skip-and-log treatment as the JSON
branch below it; any OTHER line-read error (permission denied, a genuine read
fault) is not explained by a torn write and now propagates as a real
`StorageFailure`. This makes the doc comment's claim true rather than
aspirational, without changing the deliberately-designed tolerance for a
crash-mid-write journal. Flagging this for root/L3, since it's a correctness
fix to already-landed code, not new design.

## Signature changes

- `registry.rs::write_atomic`: `fn(&Path, &[u8])` → `fn(&Path, &[u8]) ->
  Result<(), WorktreeError>`. Private, one caller (`put`), which already
  returned `Result`.
- `binding.rs::BindingTable::persist`: `fn(&self, &WorktreeId)` → `fn(&self,
  &WorktreeId) -> Result<(), WorktreeError>`, as the spec anticipated by name.
  Private, two callers (`bind`, `settle`), both already returning `Result` —
  propagation was `self.persist(worktree)?;` in each, no further ripple.
- No other function's signature needed to change: `WorktreeRegistry::open/put/
  get/list`, `BindingTable::open/bind/settle`, `EventJournal::open/append`,
  and `WorktreeManager::create` already returned `Result<_, WorktreeError>`
  before this lane, so converting their internal panics to `?` was entirely
  internal.

## Tests (`tests/storage_errors.rs`, 10 new)

Two induction techniques, per the spec:

1. **A regular file where a directory is expected** (`create_dir_all` then
   fails with "not a directory") — deterministic, identical result whether
   run as root or not, so these are unconditional assertions:
   - `registry_open_reports_typed_failure_when_a_file_blocks_the_root`
   - `binding_open_reports_typed_failure_when_a_file_blocks_the_root`
   - `journal_open_reports_typed_failure_when_a_file_blocks_the_directory`
   - `create_reports_typed_failure_when_a_file_blocks_the_worktree_root`
     (also asserts `manager.list()` is empty afterward — the failure happens
     before any registry row is written)
2. **A read-only directory** (`chmod 0o555`) so a write inside it gets
   `EACCES` — root ignores permission bits, so these two check their own
   postcondition and print a `SKIPPED: ...` message plus a reason (not a
   silent pass) if the write unexpectedly succeeded, rather than assuming the
   environment enforces permissions:
   - `registry_put_reports_typed_failure_when_the_records_dir_is_read_only`
   - `binding_bind_reports_typed_failure_when_the_root_is_read_only`

   Confirmed this repo's test environment is NOT root (`id -u` → `1000`) and
   both permission-based tests actually exercised the failure path, not the
   skip branch — verified in a solo run of `--test storage_errors` before
   folding into the full suite.

Plus direct proof of the deserialization decision, no permission tricks
needed — write a valid record, then overwrite its bytes with garbage and
assert the typed error names that exact file:
   - `registry_get_reports_typed_failure_for_a_corrupt_record`
   - `registry_list_reports_typed_failure_for_a_corrupt_record`
   - `binding_open_reports_typed_failure_for_a_corrupt_record`

And one exercising `EventJournal::append`'s converted panics without needing
permission bits — remove the journal's directory out from under an already-open
handle (a stand-in for "the volume went away"), then assert `append` returns
`StorageFailure` naming the journal file rather than aborting:
   - `journal_append_reports_typed_failure_when_its_directory_is_gone`

Every test asserts both that the error is `StorageFailure` and that `path`
names the specific file/directory that actually failed (never a parent, never
the root) — e.g. `registry_get_...` asserts the corrupt-record path itself,
not the registry root; `registry_put_...` asserts the `records/` directory
(where the temp-file creation actually failed), not the registry root either.

`find_file_containing(root, needle)` is a small test-only helper that walks
the registry/binding root looking for a filename containing a worktree id,
used instead of hardcoding the private on-disk layout (`RECORDS_DIR`, the
`{id}.json` naming) into the test file.

## For root / the surface lane

- No `WorktreeError` variants changed — `StorageFailure` already existed;
  this lane only put it to use. `error.rs` was not touched.
- The journal doc-comment/behaviour mismatch described above (torn-UTF-8 line
  used to panic despite the doc's claim otherwise) is now fixed as part of
  bringing its I/O panics in line with `StorageFailure`. Worth a quick look
  from whoever reviews L3's original landing, since it's a real gap in
  already-shipped code, not something introduced by dirtying new ground.
- Nothing outside `registry.rs`/`binding.rs`/`journal.rs`/`create.rs` was
  touched. `monitor.rs`'s classification, `snapshot.rs`'s temp-index sequence,
  and `git.rs`'s `GitCli` scrubbing are all untouched — none of them had a
  storage-I/O panic in the CONVERT boundary.
- All 35 pre-existing tests pass byte-for-byte unedited; only new tests were
  added. Confirms this was an error-shape change, not a behaviour change.
