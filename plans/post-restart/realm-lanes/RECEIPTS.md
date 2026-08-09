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

## Step 2 — per-realm fields onto the frame (verdict §7 step 2)

`last_bound_root`, `suspended_finalized_root`, `cancel_flag` and the
`DataConTable` no longer live as machine-level singletons on the parked path.

| hazard | fix | receipt |
|---|---|---|
| A1 `last_bound_root` | returned INLINE via `ParkedOutcome::Completed { value, bound_root }`; `finish_suspendable` writes the machine field only under `ParkTarget::Slot` | red-then-green, below |
| A2 `suspended_finalized_root` | onto `ContinuationFrame::finalized_root`, taken by `take_parked_finalized_root` (frame stays parked and rooted) | `a2_finalized_root_is_per_frame_not_per_machine` |
| A3 `cancel_flag` | per-realm `HashMap<RealmId, Arc<AtomicBool>>`; `install_registries` becomes a wrapper so every non-parked entry is unchanged | `a3_cancel_is_realm_scoped_resuming_a_sibling_realm_is_unaffected` |
| A4 `DataConTable` | `Arc<DataConTable>` on the frame; `resume_parked` DROPS its `table` parameter, so resuming against a foreign row is unrepresentable | `a4_resume_parked_uses_the_frames_own_table` |

**The red run, quoted from the production commit** (`707dfdd3`) — run against the
machine-level API before any production code changed, reaching the
silent-wrong-value shape rather than a panic or a `None`:

```
assertion `left == right` failed: materialize_binder must bind realm A's value under realm A's name
  left: 222
 right: 111
```

A1's fix is stronger than the verdict's literal "move it onto the frame": a
completion leaves NO frame in the registry, so there is nowhere for a per-frame
slot to live. Returning the root inline removes the write→read window entirely
rather than narrowing it.

## Negative control, RE-PROVEN after the falsifier was edited

Lane A's mandated API changes (`ParkedOutcome::Completed` becoming a struct
variant, `resume_parked` losing its `table` parameter) forced syntax-only
adaptations in `realm_multi_continuation.rs` — the falsifier itself. A green
falsifier that has been edited proves nothing until it is shown to still fail, so
the control was re-run on the edited suite:

`cargo nextest run -p tidepool-codegen -E 'binary(realm_multi_continuation) or
binary(realm_per_realm_fields)'` under the control → **11 tests run: 5 passed, 6
failed**

| case | outcome under the control |
|---|---|
| F3 | dies on `resume_parked(ContinuationId(1))`, tag 221 |
| F4 | dies on `resume_parked(ContinuationId(3))`, tag 221 |
| A5-parked | dies on `resume_parked(ContinuationId(0))`, tag 221 |
| A2, A3, A4 | also die — lane A's new heap-touching tests are live too, not vacuous |
| F1, F2, A1 | stay green |

Identical continuation ids and tag to the pre-edit run: the adaptations left the
falsifier's teeth intact. Diff reviewed line by line — dropped `table` args,
`Completed(_)` → `Completed { .. }`, rustfmt reflows; every
`assert_rooting_receipt` count, every `expect_captured` value, both guard tests,
and the VSZ finding-gate assertion unchanged.

**A1 staying green under the control is a stated limitation, same class as
F1/F2.** A1 exercises the completion path, where no continuation is parked and
nothing is rooted, so it carries no memory-safety claim — it pins the
bound_root plumbing. F3 and F4 remain the cases carrying the safety claim.

## Step 3 — the prefix-compatibility check (verdict §7 step 3, ENFORCED CONSTRAINT 1)

`JitEffectMachine::established_prefix` + `check_prefix_compatible`, threaded
through `ContinuationFrame` / `ParkTarget::Registry` and the three public parked
entries. Refusal is `JitError::IncompatibleHandledPrefix { established, incoming,
position }` — naming both prefixes and the disagreeing position.

| claim | receipt |
|---|---|
| Whole crate | `cargo nextest run -p tidepool-codegen` → **708 tests run: 708 passed, 8 skipped** |
| Targeted 5-binary set | **27 tests run: 27 passed** |
| `realm_prefix_compat.rs` | **9 tests run: 9 passed** (7 original + 2 gap-closing) |
| fmt / clippy | clean, except the pre-existing `ResponsePlan` `large_enum_variant` (verified byte-identical at the shared base `cd0f4002`, out of scope) |

**A placement gap was found on review and fixed before merge.** As first
submitted, the check ran inside `finish_suspendable`'s `Suspended` arm — so it
fired only when a turn SUSPENDED, and a parked-path run that COMPLETED was never
checked at all. That inverts the check's value: a turn completing without
suspending is precisely a turn whose every effect was *handled*, i.e. dispatched
positionally through the machine's single `H`, which is exactly the misroute
surface §5 describes. The check was covering the realms whose unhandled tags went
up to the caller and never reached a handler, and missing the ones whose effects
actually went through the handler stack.

The gap was inherited from this TL's lane spec, which took the verdict's phrase
"at park time" literally instead of reasoning about where the hazard lands. Not a
dev error — the implementation was faithful to what it was given.

Fixed by moving both the check and the establishing write into
`enter_parked_path`, called at the TOP of `run_fragment_suspendable_parked` and
`resume_parked`, before the machine is driven. A refusal now means **nothing
ran**, not merely nothing parked. Establishment also had to move: `H` is fixed
for the machine's life whether or not anything suspends, so establishing only on
suspension left a realm that ran-and-completed with a non-empty prefix never
recording what `H` is — after which an incompatible realm would park
successfully because nothing was established. The two tests that close it are
`refused_disagreeing_completing_run_never_executes` and
`establishment_on_completion_then_refuses_disagreeing`.

## Negative control, RE-PROVEN a third time (after lane B's edits)

Lane B added a `&[]` argument at each existing parked call site — the falsifier
edited again, so its green is again unearned until shown killable. Under the
control:

`-E 'binary(realm_multi_continuation) or binary(realm_per_realm_fields) or
binary(realm_prefix_compat)'` → **5 passed, 11 failed**, with F3
(`ContinuationId(1)`), F4 (`ContinuationId(3)`) and A5-parked
(`ContinuationId(0)`) dying on tag 221 — **the same three ids and the same tag as
both prior runs.** Diff reviewed first: pure `&[]` additions, zero assertions
touched.

## Both enforced constraints, with a demonstrating refusal each

| constraint | enforcement | demonstrating test |
|---|---|---|
| 1 — position-compatible handled prefixes | `enter_parked_path` at entry, typed refusal, machine untouched | `refused_disagreeing_a_then_b`, `refused_disagreeing_b_then_a`, `refused_disagreeing_completing_run_never_executes` |
| 2 — the two suspension paths must not mix | L7 assert on the run entries + its sibling on `resume_parked` | `parked_resume_while_the_slot_is_occupied_panics` |

## Step 6 — DEFERRED, and why

One-line reason, as the spec requires: **no caller can supply a `RealmId` until
step 4's `ResidentSession` conversion lands, so a realm-keyed `current` would be
unreachable machinery — a parameter with exactly one possible value.**

The longer form, so a later reader can check the judgment rather than take it:

`BindingTable::current` is a flat `HashMap<BindingName, SessionVarId>` with no
realm concept, and it lives as one field on `PersistentSession`
(`persistent.rs:251`). Realm-scoping it means threading a `RealmId` through
`BindingEntry` and filtering `resolve`/`iter_current`. Nothing above
`BindingTable` knows what realm it is in until step 4 converts `ResidentSession`,
so every call site — the REPL's five `resolve`/`iter_current` uses among them —
would pass one constant realm. That is speculative machinery plus churn across
the repl, with no behavior change and nothing able to exercise the new path.

Deferring is cheap because of what step 6 is NOT. `SessionVarId`s are minted
fresh per (re)bind, so ids stay collision-free and **a realm cannot corrupt
another's binding**; what survives is that a `:bindings`-style view could report
the wrong owner for a name two realms both bound. The verdict scores it exactly
that way — "Lowest priority; it is a display bug, not a safety one" (§7 step 6).
Step 5's pinning test covers the half that does carry safety, and it is landed.

So this belongs WITH step 4, not before it: the conversion that gives callers a
realm to name is the same conversion that makes a realm-filtered `resolve`
reachable. Recorded here rather than dropped, so whoever picks up step 4 inherits
it as part of that work.
