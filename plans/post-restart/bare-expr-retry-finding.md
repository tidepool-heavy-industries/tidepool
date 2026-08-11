# The bare-expression wasted spawn: mechanism, numbers, and why no fix lands

**Lane:** `bare-expr-waste`, a child of `batch-turns`. Scope: nail down and,
if safely fixable, eliminate the wasted extract spawn the baseline
(`plans/post-restart/batch-turns-baseline.md`) found on a bare-expression
turn. Branch tip at measurement time: `7bfbb258` (the baseline lane's BEFORE
measurement).

**Bottom line: the mechanism is corrected below with direct evidence, the
pure-vs-monadic split is measured, and no fix is landed.** Every fix
candidate that could be built inside `tidepool-repl` alone either doesn't
work (parse can't see monadic-ness) or is a net loss under this pipeline's
actual cost structure (there is no cheap type-only compile — a probe that
*succeeds* pays the same GHC cost as the real compile it's checking for).
The invariant this lane's boundary protects — a monadic bare expression's
effect must always actually run — is never at risk, because no fix reaches
the point of being worth landing.

---

## 1. Mechanism, corrected: it is `run_bare_expr`'s monadic-first retry, not `query_inner_type`

The baseline's own §1 table (line 51 of `batch-turns-feasibility.md`)
attributes the bare-expression "+1" to `query_inner_type`
(`session.rs:2277`). **That attribution is wrong for the path a real
`session_run` block takes.**

**Structural proof `query_inner_type` cannot fire here.** `query_inner_type`
has exactly one call site: `run_plain_eval` (`session.rs:1619`).
`run_plain_eval` is reached only from `run_eval`'s `None` arm
(`session.rs:1507`) — i.e. only when the block's *batch* `classify_block`
spawn (`session.rs:778`, one spawn for the whole block, not per item) itself
failed to produce a verdict. Every item in a normal, successful
`session_run` block (the steady state this census measures) gets a real
verdict from that one batch spawn, so `run_eval` always takes the
`Some(verdict)` branch into `run_bare_expr` (`session.rs:1556`) for a
confirmed bare expression. `run_plain_eval`/`query_inner_type` is a
degradation path for when the extractor's `--classify` support itself is
unavailable — not part of the steady-state bare-expression turn.

**Direct evidence: the wasted spawn's own GHC diagnostic names the exact
do-block `run_bare_expr` emits.** Driving `1 + (1 :: Int)` as a bare
expression through the real `session_run` entry point and capturing the
extractor's forwarded stderr (`compile_session_turn`'s
`eprintln!("[tidepool-extract stderr]…")`, `tidepool-runtime/src/session/turn.rs`)
shows:

```
/tmp/.tmpVnLhY3/Expr.hs:35:8: error: [GHC-83865]
    * Couldn't match expected type `Eff
                                      [Console, KV, Fs, Http, Exec, Lsp, Llm, Git, Time, Ask,
                                       RunLLMTurn, Fork]
                                      a'
                  with actual type `Int'
    * In a stmt of a 'do' block: it <- __user
      In the expression:
        do it <- __user
           pure (it, toWire it)
      In an equation for `__result':
          __result
            = do it <- __user
                 pure (it, toWire it)
```

This is `wrap_bare_it_monadic`'s literal generated source
(`session.rs:3224-3237`: `"__result = do {\n it <- __user ; pure (it, toWire
it)\n }\n"`), variable names (`__user`) and all — not `wrap_probe_source`'s
shape (`session.rs:3304-3319`: `__t <- __probe`, a different binding name, a
different do-block layout, one statement per line rather than
semicolon-braced). `query_inner_type` was never in the running for this
spawn; the diagnostic is signed by `run_bare_expr`'s own monadic-first
attempt.

**So the mechanism is exactly what the lane's context said going in:**
`run_bare_expr` (`session.rs:2096`) always compiles `wrap_bare_it_monadic`
FIRST (`:2124`); on ANY compile error (`Err(_monadic_err)`, `:2143` — the
error itself is discarded, not inspected) it falls back to
`wrap_bare_it_pure` (`:2144`). For a PURE bare expression this pays one
wholly wasted spawn before the real one, every time. For a MONADIC bare
expression the first attempt succeeds outright — no waste.

---

## 2. Measured: pure vs monadic, spawns and ms, today

New test: `tidepool-repl/tests/bare_expr_retry_census.rs` (a fresh file,
alongside — not editing — the baseline lane's
`batch_turns_spawn_census.rs`, which this reuses the pattern of). Three
tests, driven through the real `session_run` entry point
(`build_full_server`, full effect stack), each resetting
`tidepool_extract_cmd::reset_extract_spawn_count()` and reading
`extract_spawn_count()` around one 1-item block:

| Shape | Item | Spawns | Total ms (`TIDEPOOL_TIMING=1`, one clean run) |
|---|---|---|---|
| **Pure** bare expr | `1 + (1 :: Int)` | **3** (classify + doomed monadic-first + real pure) | **~11.6s** (classify ~0.9s + wasted ~2.1s + real pure ~8.5s) |
| **Monadic** bare expr | `run "echo bare-monadic" >>= liftEither` | **2** (classify + real monadic, no retry) | **~5.75s** (classify ~0.9s + real monadic ~4.86s) |

(`monadic_bare_expr_costs_one_turn_compile` / `pure_bare_expr_costs_two_turn_compiles`,
`scripts/battery.sh -p tidepool-repl -E 'binary(bare_expr_retry_census)'`,
`TIDEPOOL_TIMING=1 … --no-capture` for the phase breakdown.)

This reproduces the baseline's spawn count (bare expression = 3) and sizes
the wasted spawn concretely for both shapes in one apples-to-apples run (same
box load, same test binary, back-to-back). Absolute ms varies with
concurrent GHC load on this shared box (the baseline saw ~3.8s for the same
wasted-spawn shape; this run saw ~2.1s) — **the spawn counts are the
load-independent, decisive numbers; the phase breakdown below is what
explains *why* the ms differ and why no fix pays off.**

### The decisive structural fact: a compile FAILURE is cheap; a compile SUCCESS is not

The wasted (monadic-first, doomed) spawn's phase log for the pure case:

```
tidepool-timing phase=startup   ms=26
tidepool-timing phase=ghc_setup ms=38
tidepool-timing phase=ghc_load  ms=2035
# — no typecheck / core / translate line: GHC-83865 aborts the compile
#   DURING typecheck, before that phase's own timer completes —
Compilation failed.
```

fixed cost only (~2.1s), zero typecheck/core cost — the type mismatch is
caught immediately on the `it <- __user` statement, before GHC ever reaches
Core generation.

The real (successful) compile that follows it:

```
tidepool-timing phase=startup     ms=27
tidepool-timing phase=ghc_setup   ms=40
tidepool-timing phase=ghc_load    ms=2832
tidepool-timing phase=typecheck   ms=424
tidepool-timing phase=core        ms=5079   # core2core_ms=4805 of this
tidepool-timing phase=translate   ms=115
tidepool-timing phase=cbor_encode ms=1
```

`core` (dominated by `core2core`, the `-O2`/exposed-unfoldings optimization
pass this session's preamble always requests) is the single largest phase in
every SUCCESSFUL compile in this census — 2566-5079ms across the samples —
and it is **never reached by a compile that fails in typecheck.** This
codebase has no "compile-only-far-enough-to-learn-the-type" mode short of
that: `compile_session_turn` runs the full pipeline through CBOR encoding
for every successful compile, whether the caller wants to *use* the result
or merely *inspect its type* (this is exactly `query_inner_type`'s own cost
model too, when it does fire on its own path — a full compile, discarded
except for one type string). **There is no cheap type-only probe in this
architecture.** That fact is what rules out the leading candidate below.

---

## 3. Candidate fixes, evaluated

### Candidate 2 (classify-verdict-driven) — ruled out structurally, not just by measurement

`classify_block` is a parse-only spawn (`--classify`): it distinguishes
decl/bind/expr *shape*, which is syntactic. Monadic-ness (does this
expression's type unify with `Eff <stack> a`) is a **typecheck** property —
a bare expression like `run "echo hi"` and a hypothetical pure look-alike are
syntactically identical; nothing short of running the typechecker can tell
them apart. The classify verdict already reaches `run_bare_expr` (that's how
it got there instead of `run_plain_eval`) and it carries no type
information; there is nothing more to extract from it. Confirmed by reading
`TurnClassification`'s fields (`kind`, `binders` — no type field) in
`tidepool-runtime/src/session/turn.rs`. Ruled out — not a fixable gap, a
category error (asking a parser a typechecker's question).

### Candidate 1 (type-driven: pure-probe first, recompile monadic if the captured type says so) — technically invariant-safe, economically a bad trade

**Invariant safety, proven:** `compile_session_turn` is compile-only — it
writes CBOR to a tempdir and returns; it never touches the JIT machine.
Execution happens later and separately, via `add_fragment_session` +
`bind_funcid_render`, which `run_bare_expr` calls exactly once, on whichever
compiled `turn` it decides to keep. A candidate-1 implementation that
compiles `wrap_bare_it_pure` first, inspects `it`'s captured
`type_display` (which — since `let { it = __user }` makes `it` alias
`__user`'s type unchanged — would literally read `Eff [Console, KV, …] T`
for a monadic RHS, a reliably distinguishing prefix in this closed-effect-stack
system) and, when it says `Eff …`, **discards that compiled turn without
ever merging or executing it** and recompiles with `wrap_bare_it_monadic`
instead, never risks running an unexecuted action: the pure-probe's `it` is
never rendered or bound unless the type check confirms it's genuinely pure.
So this candidate does not trade away the never-skip-an-effect invariant.

**But it loses on cost, and not narrowly.** §2's structural fact is the
reason: `wrap_bare_it_pure`'s `let { it = __user }` **always typechecks**,
for both pure and monadic RHS — that's precisely why it's the existing
fallback. A probe built on it therefore never gets a cheap failure; it pays
the full successful-compile cost (~8.5s in this run, core2core included)
every single time, whether or not the caller ends up using the result.
Reordering to pure-first means:

- **Pure bare expr:** now 1 useful compile instead of 2 (drops the ~2.1s
  wasted fail) → **~9.4s**, a ~18% win.
- **Monadic bare expr:** now pays the pure-probe's full ~8.5s (discarded)
  PLUS the real monadic compile's ~4.86s → **~14.3s**, up from today's
  ~5.75s — a **~2.5x regression**.

The reordering doesn't create a cheap probe; it just moves which shape gets
the expensive miss. Working the breakeven from these numbers: candidate 1 is
a net win only if pure bare expressions are the overwhelming majority of the
shape — solving `p × 2.1s = (1-p) × 8.5s` gives **p ≈ 80%** pure share
needed just to break even, before counting any margin. I have no evidence
this codebase's bare-expression mix is that lopsided (the one signal
available — the baseline's own "mixed 5-item block" test, designed by the
sibling lane as a *realistic* workload — used two bare-expression items and
picked both pure: a decl reference and a `.stdout` field projection; that's
suggestive, not a measured distribution). Even granting pure dominance, the
asymmetry of outcomes (an assured ~2.1s win vs. a possible ~8.5s loss per
wrong guess) is the wrong shape of bet to make silently inside a hot path
without real usage telemetry. **Rejected: not a confident win, and wrong
under a plausible/likely mix.**

A sharper way to see why: today's monadic-first ordering is actually close
to *optimal* for a one-process-per-attempt world, given this pipeline's cost
profile (cheap typecheck-failure short-circuit, expensive successful
core2core pass) — it pays zero waste when the first guess is right
(monadic), and the cheapest available failure mode when the first guess is
wrong (pure). No reordering beats that without either a real cheap
type-only probe (a `GhcPipeline.hs` change — out of this lane's boundary) or
eliminating the guess entirely (candidate 2, structurally impossible here).

### Candidate 3 (one wrapper that works for both shapes) — architecturally possible, not provably safe in scope

A single compile that's correct for both a pure and a monadic RHS is
reachable in principle via return-type-polymorphism: define (purely as
Rust-templated source text prepended to the wrap — no `GhcPipeline.hs`
change needed) a class like `AutoBare a r | a -> r` with an instance for
`Eff <stack> a` (peel and run) and an `OVERLAPPABLE` instance for bare `a`
(treat as already-pure), then compile one `it <- autoBare __user` do-block
that resolves to whichever instance fits. This is the standard
overlapping-instance "polymorphic return type" trick, and it would, if it
worked, need exactly one compile for every shape — strictly better than
either of the other candidates, with no probe/discard tax.

I am not landing it, and would not without dedicated follow-up: overlapping
instance resolution is a genuinely fragile GHC feature outside a small set of
well-trodden shapes. The failure mode isn't "runs the wrong branch" (GHC's
own instance resolution is what decides, at compile time, before any
execution — so it can't silently pick "pure" for a genuinely monadic value
any more than today's cascade can) but "fails to resolve at all, or resolves
ambiguously" on expression shapes this census didn't exercise — a bare
numeric literal with no further constraint, `mempty`, a locally polymorphic
helper, anything whose principal type is still open when instance selection
has to happen, interaction with the `NoMonomorphismRestriction` pragma
`wrap_pure_ref_source`'s sibling path already needs for exactly this class of
problem. Proving it safe means exercising that edge-case space, which is
real, open-ended work — out of a single-wasted-spawn lane's scope. **Rejected
as unproven, not as unsafe** — a legitimate candidate for a dedicated,
better-scoped follow-up, not something to ship on a hunch.

### Candidate 4 (no fix outside a batched world) — the honest answer, and it's sharper than originally framed

No fix built from what's observable/changeable inside `tidepool-repl` alone
clears the bar. The real fix for this waste is structural: eliminate the
need to *pay full compile price to find out which guess was right*, which
is exactly what a warm, multi-turn GHC session (the `batch-turns` lane's
actual mechanism) changes. Per the sibling spike's own numbers
(`batch-turns-spike-findings.md`), a subsequent compile inside an
already-warm session pays `load'` at 145-566ms, not this lane's ~2000-2800ms
`ghc_load` cold-process cost — so the SAME "try monadic, retry pure on
failure" cascade that costs ~2.1-3.8s of wasted, wholly-fixed-shaped cost per
pure bare expression *today* would cost a small fraction of that inside a
batch, because the fixed cost the retry re-pays (`startup` + `ghc_setup` +
`ghc_load`) is exactly the part batching amortizes away. **This finding
strengthens the batch lane's case rather than sitting beside it as an
unrelated waste**: this specific tax is close to the textbook example of
what batching is for.

---

## 4. Commands run / receipts

```bash
# reproduce the baseline census (unmodified, reused as-is)
unset TIDEPOOL_EXTRACT
scripts/battery.sh -p tidepool-repl -E 'binary(batch_turns_spawn_census)' --no-capture
# → bare expression: 3 (matches baseline)

# this lane's own pure-vs-monadic census + invariant guard (new file)
scripts/battery.sh -p tidepool-repl -E 'binary(bare_expr_retry_census)' --no-capture
# → pure_bare_expr_costs_two_turn_compiles: 3 spawns, ok
# → monadic_bare_expr_costs_one_turn_compile: 2 spawns, ok
# → monadic_bare_expr_effect_actually_runs: ok (kvSet as a bare expression,
#   no bind, is visible on a later kvGet — the effect really ran)

# phase breakdown for both shapes, same run
TIDEPOOL_TIMING=1 scripts/battery.sh -p tidepool-repl -E 'binary(bare_expr_retry_census)' --no-capture

# format/lint/type-check (no production code changed — test-only addition)
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets
```

No production code changed (`tidepool-repl/src/session.rs` untouched;
`wrap_bare_it_monadic`/`wrap_bare_it_pure`/`run_bare_expr` are exactly as
they were before this lane). Per the spec, `scripts/battery-shard.sh
tidepool-repl` is therefore skipped — nothing it would gate changed.

## 5. Done-criteria checklist

- [x] Mechanism established with direct evidence (the wasted spawn's own GHC
      diagnostic matches `wrap_bare_it_monadic`'s literal source), and the
      baseline's `query_inner_type` attribution corrected with a structural
      proof (single call site, unreachable on the steady-state path).
- [x] Measured spawns + ms for a pure AND a monadic bare expression, before.
      No fix landed, so there is no "after" — see §3 for why.
- [x] Each candidate fix assessed against the never-skip-an-effect
      invariant (candidates 1 and 3 are invariant-safe by construction;
      candidate 2 doesn't reach the invariant question at all, it's ruled
      out earlier; candidate 1 is additionally rejected on cost grounds,
      candidate 3 on unproven-safety grounds).
- [x] No fix landed; this document is the honest answer, with the sharper
      finding that today's ordering is close to cost-optimal for a
      one-process-per-attempt world, and that the win lives in batching, not
      here.
