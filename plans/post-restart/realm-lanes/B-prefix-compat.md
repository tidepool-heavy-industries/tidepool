# Lane B — the prefix-compatibility check at park time (verdict §7 step 3)

Crate: `tidepool-codegen` only. Pure-Rust tier — no GHC slot, no
`TIDEPOOL_EXTRACT`.

This is **ENFORCED CONSTRAINT 1** of the two the GO verdict is conditional on.
The verdict is explicit that these are not caveats to note and move past:

> The conditions are not caveats to note and move past; each is a thing the
> landing must actively enforce, and each is cheap.

Constraint 2 (the two suspension paths must not mix) is already enforced and
tested from step 1 — the L7 assert on the run side, its sibling assert on
`resume_parked`, and `parked_resume_while_the_slot_is_occupied_panics`. This lane
closes the remaining one.

## WHY

`DispatchEffect` is POSITIONAL over an `HList`: `HCons<H, T>::dispatch` peels tag
0 to the head handler and recurses with `tag - 1`
(`tidepool-effect/src/dispatch.rs:275-301`). The suspend test is
`tag >= suspend_tag` (`jit_machine.rs`, in `drive_effect_loop`). Both are only
correct relative to ONE effect row whose handled effects occupy a contiguous low
prefix.

`realm-checklist.md` Item 5 works the break in both directions. Row A
(`FileIO=0, Proc=1, Ask=2`, threshold 2) against Row B
(`FileIO=0, Proc=1, Memory=2, Ask=3`, threshold 3):

- A's threshold applied to B's continuation: a `Memory` request is tag 2, `2 >= 2`
  is true, so the machine **wrongly suspends on a normal handled call**,
  surfacing it as an Ask to a caller that has no idea what to do with it.
- B's threshold applied to A's continuation: a real `Ask` is tag 2, `2 >= 3` is
  false, so the machine **tries to dispatch it to a handler** — either
  `UnhandledEffect` off the end of a shorter `HList`, or worse, silently running
  whatever handler happens to sit at position 2.

Either direction is a silent misclassification, not a crash you would catch.

The verdict's §5 resolves this from a wall to a constraint by checking what rows
the harness actually builds: every one has a handled prefix that is either
`base` or empty, an empty prefix is position-compatible with anything, and every
agent-family row shares `base` identically. So the property holds today — **but
holds by construction of row-building code written for other reasons, with
nothing checking it**:

> What the landing must add: an enforced check. Nothing today verifies
> prefix-compatibility, and the property holds by construction of row-building
> code that was written for other reasons. Parking a realm whose handled prefix
> disagrees position-by-position with the machine's installed handler stack, up
> to the shorter threshold, must be refused loudly at park time. That converts a
> silent-misroute class into a startup error.

## THE DESIGN, AND WHY IT IS SHAPED THIS WAY

The machine cannot introspect its own handler stack: `H` is a compile-time
monomorphized type parameter, not runtime data. So "compare against the machine's
installed handler stack" is implemented as **the machine's ESTABLISHED prefix** —
the first non-empty handled prefix any realm parks with becomes the machine's
record of what `H` is, and every later park is checked against it.

That is not a weakening. Every realm on a machine is driven through the same
single `H`, so realms agreeing with each other is exactly equivalent to realms
agreeing with `H`.

1. The parked run entries take the realm's handled prefix — the effect names for
   tags `[0, suspend_tag)`, in position order. Take it as `&[String]` /
   `Arc<[String]>` (the caller builds the decls row, so it has the names).
   `ContinuationFrame` carries it, alongside the `table` lane A put there.
2. The machine gains an established prefix, `Option<Arc<[String]>>`, set from the
   first non-empty prefix parked and thereafter **monotonic — never cleared on
   resume**. `H` is fixed for the machine's life, so a realm that resumed and
   completed does not release the constraint.
3. At park time, compare position-by-position up to the shorter of the two
   lengths:
   - equal at every position up to the shorter length → COMPATIBLE;
   - an EMPTY prefix is compatible with anything (this is the outer driver, which
     compiles `vec![runllmturn_decl()]` — threshold 0, handled prefix empty; §5
     calls it "interposed at threshold zero");
   - any disagreement at any position → REFUSE.
   - a strict EXTENSION (`[FileIO, Proc]` then `[FileIO, Proc, Memory]`) agrees
     up to the shorter length, so it is COMPATIBLE by the verdict's rule.
     Accept it, and say plainly in the doc comment what the residual is: if the
     extension goes beyond the machine's actual handler stack, that tag falls off
     the end of the `HList` and surfaces as `EffectError::UnhandledEffect` — a
     clean error, which is the whole point of the check, not a silent misroute.
     Do not pretend the check makes extensions fully safe; state the bound.
4. **Refuse LOUDLY, and refuse CLEANLY.** Return a typed `JitError` (this is a
   caller/configuration error, not a machine invariant violation — a startup
   error, per §5's own words, so an `Err`, not a panic). The message must name
   both prefixes and the position they disagree at. The refusal must happen
   BEFORE anything is mutated: no id minted, no root registered, no frame
   inserted, established prefix unchanged. A refused park leaves the machine
   byte-for-byte as it was.

## TESTS — both directions, plus the refusal's cleanliness

New `tidepool-codegen/tests/realm_prefix_compat.rs`:

- **Accepted, empty ↔ non-empty, BOTH orders**: park an empty-prefix realm then a
  `base`-prefix realm; and in a fresh machine, `base` first then empty. Both
  park fine. (Both orders matter — the established-prefix rule is asymmetric in
  its bookkeeping even though the relation is symmetric.)
- **Accepted, identical prefixes**: two realms with the same `base`.
- **Accepted, strict extension**: `[FileIO, Proc]` then `[FileIO, Proc, Memory]`.
- **REFUSED, disagreeing**: `[FileIO, Proc]` then `[FileIO, Memory]`. Assert the
  error names the disagreeing position, AND assert the machine is untouched —
  `parked_count()` and `stowed_roots_count()` unchanged, `parked_ids()`
  unchanged, and a subsequent COMPATIBLE park still succeeds (proving the refusal
  did not corrupt the established prefix).
- **REFUSED in the other direction too**: `[FileIO, Memory]` first, then
  `[FileIO, Proc]`.
- **The established prefix survives a resume**: park a `base` realm, resume it to
  completion (registry now empty), THEN try to park a disagreeing prefix — still
  refused. This is the test that pins "monotonic, not cleared", which is the
  subtle half of the design.

Heap-touching tests: `set_gc_poison(true)` + `set_heap_verify(true)`, and assert
`stowed_roots_count() == parked_count()` at every quiescent point. Copy
`realm_multi_continuation.rs`'s and `realm_per_realm_fields.rs`'s setup idioms
rather than inventing new ones.

## VERIFY (receipts are per-binary PASS COUNTS, never exit codes)

```
cargo nextest run -p tidepool-codegen
cargo nextest run -p tidepool-codegen -E 'binary(realm_prefix_compat) or binary(realm_multi_continuation) or binary(realm_per_realm_fields) or binary(nested_child_gc_rooting) or binary(continuation_gc_root)'
cargo clippy -p tidepool-codegen --all-targets
cargo fmt --all -- --check
```

Report the "N tests run: N passed" line for each. The whole-crate run must be
green on EVERY commit, not only the last. Current baseline is 699 passed, 8
skipped — it may only grow.

## BOUNDARIES

- Zero pre-existing tests edited. If your change to the parked entries' signature
  forces a syntax-only adaptation in `realm_multi_continuation.rs`,
  `realm_cycle_scoped_drop.rs` or `realm_per_realm_fields.rs`, that is allowed —
  but it must be syntax ONLY. **Do not weaken, delete or renumber a single
  assertion.** `realm_multi_continuation.rs` is the falsifier suite; its green is
  the safety claim for this whole branch, and it is re-checked against a negative
  control after you land.
- Do NOT touch `tidepool-runtime/src/session/resident.rs` — the
  `pending`/`ChildSuspended` conversion is HELD on a cross-lane signal that has
  not fired.
- Do NOT touch `suspended_continuation`, `enter_nested_child`,
  `run_child_fragment`, or any L7 `is_none()` assert. The single-slot path stays
  byte-identical.
- Do NOT change what lane A landed (`bound_root` inline on `Completed`,
  `finalized_root`/`cancel_flag`/`table` on the frame). You are adding a field
  and a check next to them, not revisiting them.
- Do NOT introduce a `Box<dyn DispatchEffect<U>>` per-frame dispatch object. §5
  names it as the escape hatch IF a future row genuinely needs a different
  handled prefix; no row that exists needs it, and building it now is speculative
  machinery.
- Do NOT merge source-level capability rows.
- Do NOT write the per-machine RSS constants (~200/~125/~50 KB) into code or
  comments as facts — the verdict marks them UNRESOLVED.
- The `large_enum_variant` clippy warning on `ResponsePlan` (~L2848) is
  PRE-EXISTING at the shared base and out of scope. Leave it.
- Comments describe what IS — invariants, not the story of the change. No
  workstream/finding IDs in production code.
- If a step turns out to be blocked or wrong, finish every other step in full and
  say explicitly what you left out and why — do not silently narrow scope.

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
