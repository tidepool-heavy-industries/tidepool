# realm-build — receipts

Per-binary PASS COUNTS, never exit codes. Every heap-touching run under
`TIDEPOOL_GC_POISON` + `TIDEPOOL_HEAP_VERIFY` (the falsifier sets both
in-process via `set_gc_poison`/`set_heap_verify`).

## Step 1 — registry landed additively (verdict §7 step 1)

Prototype commits `9d6987ba`, `5cc3b2f2`, `630c4c73` cherry-picked from
`root.realm-spike.proto` onto `root.realm-build`. **Zero conflicts** — the only
intervening code change on this branch since the shared base `cd0f4002` was
`1b5708a8`'s lifetime-lane instrumentation, which does not touch the parked path.

| claim | receipt |
|---|---|
| Whole crate green, existing tests an intact control group | `cargo nextest run -p tidepool-codegen` → **694 tests run: 694 passed (2 slow), 8 skipped**. Same count as proto HEAD; zero pre-existing tests edited. |

## Step 1 — the negative control, RE-PROVEN on this branch

The falsifier's PASSes only mean something if the falsifier can fail. Control
patch (2 lines, NOT committed — production code carries no test-only escape
hatch): comment out `self.machine_state.register_stowed_root(slot)` in
`JitEffectMachine::park_continuation`, and early-return from
`assert_rooting_receipt` so the rooting-count assertion cannot mask the memory
failure.

`cargo nextest run -p tidepool-codegen -E 'binary(realm_multi_continuation)'`
→ **7 tests run: 4 passed, 3 failed**

| case | outcome under the control |
|---|---|
| F3 `f3_gc_and_heap_doubling_between_parks` | dies on `resume_parked(ContinuationId(1))` — `force_ptr: unexpected heap tag 221` |
| F4 `f4_eight_parks_gc_between_each_shuffled_resume` | dies on `resume_parked(ContinuationId(3))` — tag 221 |
| A5 `parked_bottom_answer_leaves_the_frame_parked_and_rooted` | dies on `resume_parked(ContinuationId(0))` — tag 221 |
| F1, F2 | stay green |

221 is `0xDD`, the byte `TIDEPOOL_GC_POISON` fills from-space with — a
deterministic read-a-freed-object, not a flake.

F1/F2 staying green is the falsifier's stated limitation, not an oversight:
neither forces a collection between its parks, so they test the registry's
ordering and bookkeeping rather than memory safety. **F3 and F4 carry the safety
claim.** The control was verified once before on the proto branch post-jit-chain-2
and once here — the re-run matters because a GC-path change that made parked
continuations reachable by some other route would leave the suite green and
vacuous.

Patch reverted; working tree clean.

## Step 5 — the Item-2 VarId pinning test (verdict §7 step 5)

Two tests, both new files, zero pre-existing tests edited:
`tidepool-codegen/tests/binding_table_realm_isolation.rs` (hand-wired table, two
colliding-name scopes, isolation asserted in BOTH directions) and
`tidepool-runtime/tests/realm_varid_pinning.rs` (two real `run_bind` turns
binding the same display name as independent scopes, then a referencing turn
through `run`).

| claim | receipt |
|---|---|
| Unit-level isolation | `-E 'binary(binding_table_realm_isolation)'` → **1 test run: 1 passed** |
| Real-path isolation | `--ignore-default-filter -p tidepool-runtime -E 'binary(realm_varid_pinning)'` → **1 test run: 1 passed** |
| Codegen crate still green | `cargo nextest run -p tidepool-codegen` → **695 tests run: 695 passed, 8 skipped** (694 + the new unit test) |

**The falsification check is what makes these tests worth having.** Reverting
`seed_external_env` to its pre-D9 unconditional `self.live.values()` sweep kills
BOTH, and kills them by finding the foreign scope's `SessionVarId` in the env —
not by some incidental failure:

- C1: `A's env must NOT contain B's x — cross-scope isolation`
- C2: `scope A's x is NOT referenced by this fragment — its SessionVarId must be
  ABSENT from the env (the cross-realm isolation property)`

Patch reverted, not committed. This is the whole point of the lane: D9's own
tests would stay green under a change that reintroduced the leak, because D9 was
motivated by compile-time cost, not realm isolation.

**Integration change on fold.** As submitted, `seed_external_env_for` duplicated
the two-line `free_vars` + `seed_external_env` computation that `run`/`run_bind`
inlined, so the test asserted on a *reconstruction* of the env rather than the
env itself — a later change to the `free_vars` side could drift and leave the
test passing vacuously. Both call sites now go through the accessor, so it IS
the seeding path and the asserted env is the one a fragment really compiles
against.

## Steps 2, 3 — pending (lanes A, B)
