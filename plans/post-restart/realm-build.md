# Spec: realm-build TL

Lands the realm machine per the GO verdict in
[`realm-verdict.md`](realm-verdict.md) — continuation registry, per-frame
realm ownership, cycle-scoped lifetime — under the verdict's TWO ENFORCED
CONSTRAINTS. The verdict's §7 recommended landing shape IS the work plan;
this spec adds sequencing, boundaries, and the one cross-lane hold.
Started 2026-08-08 (Inanna: start now) in parallel with the extract wave.

## ANTI-PATTERNS

- DO NOT treat the verdict's two conditions as notes: (1) the
  prefix-compatibility check at park time must be BUILT and refuse loudly;
  (2) no machine may hold both a slot continuation and a parked one — the
  landing removes the slot path from any machine that parks.
- DO NOT walk into Item 3's named bug: `last_bound_root` /
  `suspended_finalized_root` MUST move onto `ContinuationFrame` in the
  same step that makes two realms concurrently bindable — a machine-level
  Option means realm B silently overwrites realm A's bind and
  `materialize_binder` binds the wrong value with no panic.
- DO NOT touch `resident.rs` step 4 (the ResidentSession `pending`→map
  conversion + ChildSuspended deletion) until root announces the extract
  wave's boot-site work has landed — the ONE cross-lane hold
  (`resident.rs:208` is a Track-1 site). Everything else proceeds now.
- DO NOT merge source-level capability rows at any step
  (`fork_child_decls` enforces the compile-time boundary; it is
  independent of machine unification).
- DO NOT build toward an immortal machine — cycle-scoped is the decided
  shape; a realm outliving its cycle is a §8 NO-GO trigger, escalate.
- DO NOT carry the per-machine RSS constants (~200/~125/~50 KB) forward
  as machine properties — the verdict marks them UNRESOLVED. The
  256 MiB/machine VSZ figure and the monotonic direction are the only
  numbers with cross-lane agreement.
- Operational, copy VERBATIM into every dev spec:
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

## READ FIRST

- `plans/post-restart/realm-verdict.md` — §7 is the plan; §1 the scored
  checklist; §3 the boundary condition; §5 the constraint mechanics; §8
  the NO-GO triggers.
- The preserved prototype: branch `root.realm-spike.proto` (HEAD
  `55a9df5c`) — the registry, parked run/resume entries, and the
  falsifier suite `realm_multi_continuation.rs`. START FROM IT (cherry-pick
  or re-derive with it open); it is rebased onto the current tree and
  green. Its notes: `git show
  root.realm-spike.proto:plans/post-restart/spike-notes/realm-prototype.md`.
- `spike-notes/realm-checklist.md`, `realm-lifetime.md`,
  `realm-leak-comparison.md` (on the main branch).
- `tidepool-codegen/src/jit_machine.rs`, `machine_state.rs`, `gc.rs:902`
  (`extend_stowed_roots` in root assembly).

## STEPS (verdict §7, each independently green)

1. Registry at the machine layer, ADDITIVELY (the prototype's shape):
   `ContinuationFrame { cell, realm, suspend_tag, kind }`, invariant
   `stowed_roots_count() == parked_count()` at quiescent points. Existing
   tests are the control group — edit none. The falsifier suite comes
   along and must pass, INCLUDING re-proving the negative control kills
   (delete the rooting call → F3/F4/A5 die on tag 221) at least once on
   this branch.
2. Per-realm fields onto the frame: `last_bound_root`,
   `suspended_finalized_root`, `cancel_flag`, `suspend_tag` +
   `DataConTable`. Add a test that REACHES the Item-3 overwrite with
   machine-level fields (red) and passes with frame-level (green).
3. Prefix-compatibility check at park time — refuse loudly; test both
   directions (empty prefix compatible with anything; disagreeing prefix
   refused).
4. HELD: ResidentSession conversion (verdict §3 blast-radius list) —
   after root's go-signal only.
5. Item-2 pinning test: two scopes, colliding local names, neither's
   `ExternalEnv` contains the other's `SessionVarId`.
6. Realm-scoped display names (`current`/`resolve`) — lowest priority,
   display bug only.

## VERIFY

- Every machine-touching commit: `cargo nextest run -p tidepool-codegen`
  green; heap-touching work under `TIDEPOOL_GC_POISON` +
  `TIDEPOOL_HEAP_VERIFY`.
- The falsifier's negative control re-proven on this branch (green suite
  with a killable falsifier, not just green).
- Receipts per-binary counts, never exit codes.

## DONE CRITERIA

- Steps 1–3 + 5 landed with both constraints ENFORCED in code (a test
  demonstrates each refusal), falsifier suite green with negative control
  re-proven, zero pre-existing tests edited.
- Step 4 landed only after the coordination go-signal, with the §3
  blast-radius sites (repl server.rs:516, harness.rs run_child/
  ChildSuspended arms, resident_session.rs tests) swept and the harness
  acceptance binaries green.
- Step 6 landed or explicitly deferred with a one-line reason.
