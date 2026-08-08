# jit-chain-2 — cluster ledger and successor handoff

Successor to `13-jit-chain-handoff.md`. Every item on that document's
"Where to pick this up" list is now landed. Where this document and an
earlier one disagree, this one is later and was checked against the source.

## What landed

| cluster | scope | status |
|---|---|---|
| B | free-vars index + petgraph topo sort | verified (was already folded; evidence was missing, not the code) |
| ConTags | frozen-`tags` refresh, `by_qualified_name` hard-error guard | landed |
| NurseryExhausted | class audit + one shared retry helper | landed — **does not fix the reported intermittent, see below** |
| prune | `wrap_with_datacon_env` referenced-only, re-landed | landed with a populated-table gate |
| D | `runtime_apply`/`runtime_tail_apply` + `FunctionImports` | landed |
| E | incremental lambda registry, sorted-range `contains_address`, narrowed `seed_external_env` | landed |
| F | `declare_env` removal under a verified function-wide contract | landed |

## Four stated premises that were wrong

Each was corrected by evidence, not argument. Recording them because the
same premises are what a successor would otherwise inherit.

1. **"Cluster B's petgraph topo sort was never compiled."** It was folded,
   compiled, and green. The gap was *evidence*, not code: 170 tests
   (goldens + emit units + free-vars equivalence) confirm it.
2. **"The Park arm is a zero-retry `NurseryExhausted` gap."** It was already
   protected transitively — `alloc_stream_tail_thunk` backs onto
   `host_alloc_gc`, which retried. The asymmetry was outer-layer only.
   Consequence in the next section.
3. **"`stack_map.rs`'s linear scan is in `lookup`."** `lookup` is an O(1)
   HashMap get. The linear scan is `contains_address`, called by the frame
   walker on every frame of every stack walk — i.e. inside GC.
4. **"Sorted ranges + binary search is sufficient for `contains_address`."**
   Unsound for nested ranges: with `(0,100)` and `(10,20)` present, a search
   for `50` lands on the narrower interval and wrongly reports false. The
   landed version merges intervals on insert so the search is correct rather
   than lucky.

## The NurseryExhausted intermittent is still unexplained

**Do not re-audit retries.** Every allocation site on the
nested-child/stowed-continuation path was ALREADY retry-protected before this
lane touched it — the enumeration is in the commit message for the gc-retry
class commit, classified per site as protected / needed / structurally
unreachable-because-X.

Therefore `resident_session::nested_child_runs_while_parent_suspended_then_resumes`
failing as `Run(Jit(HeapBridge(NurseryExhausted)))` is **not** a missing-retry
bug, and this lane's work very likely does not fix it. Three green GHC runs
were observed and are explicitly NOT a clearance — repetition is the gate and
the rate sets the count.

Hypotheses NOT eliminated, for whoever picks it up: something live across a
retry that is not a registered root; heap-cap exhaustion where doubling
genuinely cannot satisfy the request; a distinct failure upstream of
materialization.

What the lane did contribute here: the class is now consolidated onto one
`heap_bridge::gc_retry` helper (five hand-rolled copies removed), it has a
test-only `gc_retry_fired_count()` observable proving a retry branch was
actually exercised rather than merely reached, and the one real asymmetry is
closed.

## The `declare_value_needs_stack_map` contract, settled

**FUNCTION-WIDE**, per Cranelift `Function`. Established from the pinned
`cranelift-frontend` 0.129.1 source, not inferred:

- `declare_value_needs_stack_map` (`frontend.rs:562`) inserts into a flat
  `func_ctx.stack_map_values` set — no block scoping.
- Cleared in the per-function `clear()` (`frontend.rs:82`).
- Consumed once at finalize (`frontend.rs:726-728`), passing the whole set to
  `safepoints.run` over the whole function.
- `LivenessAnalysis::run` (`frontend/safepoints.rs:455+`) is a genuine
  backward dataflow: reverse-post-order worklist pumped to a fixed point,
  re-enqueueing predecessors when a live-in set changes, then a second pass
  recording exact live sets at each safepoint.

So marking a Value anywhere in a function is sound, and `declare_env`'s
per-branch-point re-declaration was redundant. `declare_env` is gone.

The safety argument is an exhaustive 14-site table (every `ScopedEnv`
`insert`/`insert_scoped`, in the removal commit's message). The load-bearing
insight: **pass-through sites introduce no new Cranelift Value id**, so they
need no mark — the induction bottoms out in producer sites, which all mark
directly. `restore`/`restore_scope` are inert for the same reason.

## A GC-coverage gap this lane found and closed

The GC gate had **zero** tests that EXECUTE a `Join`/`Jump` under a forced
collection. `gc_audit::test_stack_map_join_safepoints` compiles a Join and
asserts stack maps are non-empty without ever running the program.

`tests/stackmap_join_param_gc_coverage.rs` closes it, mutation-proven (remove
`join.rs:163`'s mark → `read_tag` returns 221/0xDD where `TAG_LIT` expected).
It is explicitly NOT a regression guard for the `declare_env` removal —
`join.rs:163` is untouched by that diff. The gap predates it.

## Zero-tests-exit-0 struck three times

The known trap (a mis-filtered nextest runs zero tests and exits 0) hit three
times in this lane, including once via a NEW mechanism:

- A wrong crate: `-p tidepool-repl -E 'binary(resident_session)'` — that
  binary lives in tidepool-**runtime**. Caught only by gating on tests-RUN.
- **Punctuation bleeding out of spec prose.** A spec wrote
  `--run-ignored all . Do NOT take a GHC slot`; the sentence-ending period was
  copied into the shell command, nextest took it as a positional filter,
  matched no test name, and filtered to zero. **Write commands on their own
  line in specs.**

The differential gate itself is sound. `--ignore-default-filter` is NOT needed
for it — tidepool-codegen is not in `default-filter`'s exclusion list. What it
does need is `-p tidepool-codegen`, `-E 'binary(haskell_suite_differential)'`,
`--run-ignored all`, and `TIDEPOOL_EXPENSIVE_TESTS=1`; add `--no-capture` to
see the counters at all.

## Cross-lane: XDG_CACHE_HOME, confirmed effective by observation

`export XDG_CACHE_HOME="$PWD/.cache"` before any harness test run. Verified
empirically, not just from `paths.rs`: running the harness acceptance binaries
under `--ignore-default-filter` with it set created
`.cache/tidepool/selfharness/` inside the worktree and left the real
`~/.cache/tidepool/selfharness` untouched.

Scope: the plain quick tier is SAFE without it — tidepool-harness's
integration binaries are excluded wholesale by `kind(test)`, and the only one
that runs is `provider_behavior` (pure-Rust, no Harness use). The hazard is
specifically `--ignore-default-filter` at anything harness-touching. `.cache/`
is gitignored (an untracked one blocks spawning worktree children).

## Gates, on the fully composed tree

| gate | result |
|---|---|
| quick tier (`--no-fail-fast`) | 1814 run, **1814 passed**, 9 skipped |
| GC gate, `GC_POISON=1` + `HEAP_VERIFY=1`, 14 binaries | 49 run, **49 passed** |
| differential (`--run-ignored all`, `TIDEPOOL_EXPENSIVE_TESTS=1`) | 1 run, 1 passed |
| harness + fork acceptance (`XDG_CACHE_HOME` set) | 7 run, 7 passed, 4 binaries |
| cross-turn binding gate (repl + runtime, GHC-heavy) | 45 run, **45 passed** |
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --workspace` | 2 warnings, both pre-existing |

Differential counters, **field-identical to the 2026-08-08 baseline**:

```
tested=349, compared=312, closure_skip=34, mismatch=0,
both_error=0, jit_only_error=0, eval_jit_diverge=3, skipped=1
eval_jit_diverge_names: ["xs'_u8286623314361937461", "thunk_blackhole",
                         "xs_u8286623314361937397"]
```

The two clippy warnings are `ResponsePlan`'s `large_enum_variant` in
tidepool-codegen (confirmed present on an unmodified baseline via stash) and
one in tidepool-harness, which this lane does not own.

## Open, and owned elsewhere

- **`selfharness_compaction`** — deterministic garbage con_tag. Untouched here.
- **`works_from_json_float`** — never green. `eitherDecode "3.5" :: Either
  Text Float` reaches `decodeFloat_Int#`, which has no dedicated split in
  `splitUnaryMultiReturnPrimOp`. Disproves the old M5 comment claiming the
  2-result fallback was unreachable from exported stdlib (corrected in
  `stdlib_regressions_02.rs`). Fix is haskell/-side; queued to phase-b.
- **`qq_fmt_brace_inside_hole_non_string_expr_still_works`** — never green.
  `Tidepool.QQ.HsMeta.Translate.toExp` lacks let-in. haskell/-side; phase-b.
- **Cluster B's performance claim** is still unevidenced. Its correctness is
  gated; no before/after `emit_ms` was ever measured. Same for D/E/F: every
  claim in this lane is a correctness claim. **No latency number in this lane
  has been measured.**

## What the next person should not redo

- The `declare_value_needs_stack_map` contract. Settled from primary source.
- The retry audit on the nested-child path. Settled by enumeration.
- Whether `EmitSession.tree` can drift from an index built against it. It
  cannot: `tree` is a `&'a CoreExpr` immutable for the struct's lifetime and
  `emit/*.rs` has no `replace_subtree` call. Settled by immutability.
