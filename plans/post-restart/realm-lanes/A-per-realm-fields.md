# Lane A — per-realm fields onto the frame (verdict §7 step 2)

Crate: `tidepool-codegen` only. Pure-Rust tier — `cargo nextest run -p
tidepool-codegen` needs no GHC slot and no `TIDEPOOL_EXTRACT`.

## WHY

The registry (§7 step 1) landed additively: `continuations:
HashMap<ContinuationId, ContinuationFrame>` now holds N parked continuations in
one machine, each a registered GC root, resumable in any order. But the
per-computation state those continuations depend on is still MACHINE-LEVEL
singletons written by whichever realm ran last.

`realm-checklist.md` Item 3 names the concrete bug this creates, and the verdict
(§1, §7 step 2) says the landing must not walk into it:

> Two realms park concurrently; realm A's bind-fragment completes and sets
> `last_bound_root = Some(slot_A)`; before the caller drains A, realm B's
> bind-fragment also completes and overwrites the slot with `Some(slot_B)`. The
> caller then calls `take_last_bound_root()` expecting A's value and silently
> receives B's — `materialize_binder` would bind the WRONG value under realm A's
> name, with no error raised anywhere (both are valid `RootSlot`s; nothing
> type-checks or panics on the mismatch).

Today that is unreachable (one computation per machine, `&mut self`
serialization). Step 1 made it reachable. This lane closes it.

## SCOPE — four fields, one per hazard

Read `tidepool-codegen/src/jit_machine.rs` first: the type block at ~L136-267
(`ContinuationId`/`RealmId`/`ParkKind`/`ContinuationFrame`/`ParkedOutcome`/
`ParkTarget`/`ParkedRaw`), `finish_suspendable` (~L1259-1374), the take-sites
(~L1431-1443), and the parked entries (~L2535-2727).

### A1. `last_bound_root` — return it inline, do not stash it

Written at `finish_suspendable`'s `Done` arm under `bind_forced`
(`jit_machine.rs:1312`), taken by `take_last_bound_root`.

A completion leaves NO frame in the registry (a frame exists only while parked),
so "move it onto the frame" has nowhere to land. The correct and strictly
stronger fix is to remove the stash on the parked path entirely: return the slot
in the outcome, so there is no window between write and read for a second realm
to occupy.

- `ParkedOutcome::Completed` grows the tenured root:
  `Completed { value: Value, bound_root: Option<RootSlot> }` — `Some` exactly
  when the park kind was `ParkKind::Binding { .. }`, `None` for `ParkKind::Plain`.
  Thread it through `ParkedRaw::Completed` the same way.
- `finish_suspendable` MUST NOT write `self.last_bound_root` when `park` is
  `ParkTarget::Registry { .. }`. The `ParkTarget::Slot` path keeps writing it,
  byte-for-byte as today — that is the pre-existing single-slot contract and its
  callers are a control group.
- Do NOT delete `take_last_bound_root` or the machine field. The slot path still
  uses them, and `resident.rs` (step 4) is HELD.

### A2. `suspended_finalized_root` — onto the frame

Written at `finish_suspendable`'s `Suspended` arm (`jit_machine.rs:1356`), taken
by `take_finalized_root`. A frame DOES exist here, so this one relocates
literally.

- `ContinuationFrame` gains `finalized_root: Option<RootSlot>`.
- On the registry park target, the tenured slot goes into the frame being parked,
  not into `self.suspended_finalized_root`.
- New accessor `pub fn take_parked_finalized_root(&mut self, id: ContinuationId)
  -> Option<RootSlot>` — takes from that frame, leaving the frame parked and
  rooted. A frame's `finalized_root` is dropped with the frame on resume; make
  sure resuming a frame whose finalized root was never taken does not leak the
  handle silently — take it into the local before `drop(frame)` and let it fall
  out of scope, with a comment saying the slot stays a registered persistent root
  for the machine's life regardless (same as `take_finalized_root`'s doc).
- `ParkTarget::Slot` behavior unchanged.

### A3. `cancel_flag` — per realm, not per machine

`cancel_flag: Arc<AtomicBool>` (`jit_machine.rs:306`) is one flag for the
machine's life, re-installed into `MachineState` at every run entry via
`install_registries` (`:741`). Per Item 4 this is MECHANICAL: change which `Arc`
gets cloned into that existing call, not the call sites.

Cancellation is realm-scoped, not park-scoped — a realm's ids change on every
re-suspension, so the flag cannot live only on the frame.

- Machine gains `realm_cancel_flags: HashMap<RealmId, Arc<AtomicBool>>`, lazily
  minted on first park-path entry for a realm.
- The parked run/resume entries install THAT realm's flag into `MachineState`
  instead of `self.cancel_flag`. Every other entry is unchanged.
- `ContinuationFrame` carries a clone (`cancel_flag: Arc<AtomicBool>`) so a
  resume installs the right flag without a second map lookup.
- New accessor `pub fn realm_cancel_handle(&mut self, realm: RealmId) ->
  CancelHandle`, minting the realm's flag if absent.
- A cancelled realm's flag must NOT be reset by an unrelated realm's run. Whether
  a realm's flag is cleared after a cancelled run is your call — pick one, state
  it in the doc comment, and test it.

### A4. `DataConTable` — onto the frame; resume stops taking one

§5 of the verdict: "the caller must decode the tag against *that frame's* row.
The frame carries its `DataConTable` and effect names." Today `resume_parked`
takes `table: &DataConTable` from the caller, so nothing stops a caller from
resuming realm A's continuation against realm B's table.

- `ContinuationFrame` gains `table: Arc<DataConTable>` (`DataConTable` is
  `Clone`; `tidepool-repr/src/datacon_table.rs:77`).
- The parked run entries take `Arc<DataConTable>` (or take `&DataConTable` and
  `Arc::new(table.clone())` at park — your call, but say which and why in the
  doc comment; the clone is once per park, not per collection).
- `resume_parked` DROPS its `table` parameter and uses `frame.table`. This is the
  enforcement: it becomes impossible to resume a frame against a foreign row.
- Effect NAMES / handled prefix are NOT this lane — lane B (step 3) adds them.
  Do not add a `handled_prefix` field; you would collide.

## THE RED-THEN-GREEN REQUIREMENT — this is the deliverable, not a formality

The spec calls for "a test that REACHES the Item-3 overwrite with machine-level
fields (red) and passes with frame-level (green)". Do it in that order and
capture the receipt:

1. FIRST, before changing any production code, add
   `tidepool-codegen/tests/realm_per_realm_fields.rs` with a test that reaches the
   overwrite against TODAY's machine-level API:
   - run a parked BIND fragment in realm A to `Completed` (do not drain),
   - run a parked BIND fragment in realm B to `Completed`,
   - `take_last_bound_root()` and assert the bridged value is A's.
   It must FAIL, and it must fail by binding the WRONG VALUE (B's), not by
   panicking or returning `None` — if it fails some other way you have not
   reached Item 3's bug, you have reached a different one. Adjust until the
   failure is the silent-wrong-value one.
2. Run it. Save the failure output verbatim.
3. THEN make the production change and rewrite the assertion against the new
   inline `bound_root`, asserting both realms' slots are distinct and each
   bridges to its own realm's value.
4. The commit message for the production change MUST quote the red run's failure
   line. That quoted line is the receipt that the test reaches the bug rather
   than merely passing.

Also add, in the same file:
- A2: two realms park on a closure-valued `finalize`; each
  `take_parked_finalized_root(id)` yields its OWN slot; taking one leaves the
  other frame parked and rooted (`parked_count()`/`stowed_roots_count()`
  unchanged).
- A3: two realms parked; cancel realm A's handle; resume realm B → completes
  normally; resume realm A → observes cancellation. Assert realm B's run was NOT
  cancelled — that is the whole point of the field moving.
- A4: park in realm A, resume it without the caller supplying a table at all
  (the signature no longer accepts one) and the resumed turn behaves as before.

Every heap-touching test in this file: `set_gc_poison(true)` +
`set_heap_verify(true)`, and assert `stowed_roots_count() == parked_count()` at
every quiescent point, exactly as `realm_multi_continuation.rs` does. Copy that
file's setup idioms rather than inventing new ones.

## VERIFY (receipts are per-binary PASS COUNTS, never exit codes)

```
cargo nextest run -p tidepool-codegen
cargo nextest run -p tidepool-codegen -E 'binary(realm_multi_continuation) or binary(realm_per_realm_fields) or binary(nested_child_gc_rooting) or binary(continuation_gc_root)'
cargo clippy -p tidepool-codegen --all-targets
cargo fmt --all -- --check
```

Report the "N tests run: N passed" line for each. `cargo nextest run -p
tidepool-codegen` must be green on EVERY commit you make, not only the last.

## BOUNDARIES

- Zero pre-existing tests edited. The ~650 pre-existing codegen tests are the
  control group; if one changes, you changed behavior on the single-slot path and
  that is out of scope.
- Do NOT touch `tidepool-runtime/src/session/resident.rs` — the `pending` /
  `ChildSuspended` conversion is HELD on a cross-lane signal.
- Do NOT touch `suspended_continuation`, `enter_nested_child`,
  `run_child_fragment`, or any L7 `is_none()` assert. The single-slot path stays
  byte-identical.
- Do NOT add a `handled_prefix` / effect-name field (lane B owns it).
- Do NOT merge source-level capability rows.
- Do NOT carry the per-machine RSS constants (~200/~125/~50 KB) forward as facts
  anywhere in code or comments — the verdict marks them UNRESOLVED. The 256
  MiB/machine VSZ figure and the monotonic direction are the only numbers with
  cross-lane agreement.
- Comments describe what IS — invariants, not the story of the change. The story
  goes in the commit message.

## OPERATIONAL (verbatim)

- Every GHC-heavy run goes through
  `/home/inanna/dev/tidepool/scripts/ghc-slots.sh run -- <cmd>`
  (absolute path). NEVER `exclusive` mode.
- `export XDG_CACHE_HOME="$PWD/.cache"` before any tidepool-harness
  test shard (persistent per-worktree, not mktemp).
- Spawns pass an explicit `model: sonnet` (or `opus` for sub-TLs);
  never fable.
- Never path-unscoped `pkill -f`; scope kills to PID or full worktree
  path.
- Commit with `--no-verify`. Never `git add -A`. Repo-root `tmp/` is
  protected. Grep/Read over LSP; no per-worktree rust-analyzer.
