# One spawn per BLOCK: feasibility checkpoint

**Lane:** `batch-turns`. Written BEFORE any implementation, against this
branch's tip (`2c6fa9cf`). Every claim cites `file:line`.

**The proposal.** A repl/harness block of N items costs N (often 2N) extract
spawns today, each paying a full GHC boot + stdlib `load'`. Compile the item
chain in ONE GHC session instead — item k's binder iface registered in the HPT
so item k+1's typecheck resolves it — emitting N fragments the block-runner
executes in sequence. Extends
`plans/one-spawn-turn-protocol.md` from one spawn per TURN to one spawn per
BLOCK.

**The checkpoint.** Three questions had to be answered before building:
(a) can a value-plane bind's iface be synthesized intra-spawn from the
typechecked turn module alone? (b) does mid-block compile failure keep the
run-until-first-error contract and per-item error attribution? (c) what is the
wire shape for N fragments out of one spawn?

**Verdict, in one line:** (a) **passes on the type plane by construction** —
but it was never the load-bearing question, and §2.3 names the one that is;
(b) **passes**, with one real requirement that lands in (c); (c) **fits the
manifest direction as a strict subset**, no rails fought. The lane proceeds to
a spike whose single job is to settle §2.3.

---

## 1. What a block costs today

`session_run` over N items already batches the two cheapest things and nothing
else:

- **Classification** is one spawn for the whole block —
  `run_block` calls `classify_block` once
  (`tidepool-repl/src/session.rs:778`), which is `--classify` →
  `classifyBlock` (`haskell/src/Tidepool/Binders.hs:286-295`): ONE `runGhc`,
  N parses. This is the in-tree precedent that N-items-one-session is a shape
  the extractor already speaks — but it is parse-only. No `load'`, no HPT, no
  typecheck.
- **Consecutive decl items** batch into one `define_scoped`
  (`session.rs:904-909`), which is still one spawn (`session/mod.rs:509-535`,
  the `TidepoolValidate` wrapper compile).

Everything else is per-item, and several item shapes cost more than one spawn:

| Item shape | Spawns | Sites |
|---|---|---|
| decl run (M items) | 1 validate + 1 type probe *per value decl* | `session/mod.rs:509`; "one extra extract compile per value decl" (`tidepool-repl/CLAUDE.md`) |
| pure bind (`let x = e`, `x <- pure e`) | 1 `define_scoped` + 1 `probe_pure_type`, or 1 + 1 failed + 1 `run_bind` | `session.rs:1305`, `session.rs:1377-1383`, `session.rs:1540` |
| effectful bind `x <- e` | 1 `--turn` | `session.rs:1711` → `turn.rs:435` |
| bare expression | 1 `--turn`, plus `query_inner_type` when it fires | `session.rs:2096`, `session.rs:2277` |

A five-item block of mixed shapes is routinely 8-12 spawns. At the measured
~5-8s spawn+boot floor that is the dominant cost of the repl test suite and of
every live multi-item turn.

**The win being chased is amortization of GHC boot + stdlib `load'`, not of
per-item typecheck.** State it that way because it decides the design: the
batch does not need to compile items *together*, it needs to compile them
*sequentially inside one live session* whose HPT already holds
`Tidepool.Prelude` / `Tidepool.Effects`.

---

## 2. (a) Intra-spawn synthesis of a value-plane bind's iface

### 2.1 Answer: yes, and nothing in the on-disk gen-module blocks it

The bind artifacts are already a **pure function of the compile**, with no
post-run Rust input. `mkBoundBinders` (`haskell/app/Main.hs:1036-1068`) takes
`[String] -> Word64 -> FilePath -> PipelineResult` and nothing else. Walking
what it needs:

1. **The type.** `prResultType` is `capturedBindingType "__result"`, read off
   `tcg_type_env` at TYPECHECK time (`GhcPipeline.hs:329-331`, `:755-760`),
   before any optimization — `GhcPipeline.hs:118-122` pins this as a
   must-not-move capture point. `stripMonadHead` (`GhcPipeline.hs:767`)
   recovers `T` from `Eff stack T`. **The value never enters this
   computation.**
2. **The iface.** `mkThinSessionIface hsc sm [(occ, cty)]`
   (`haskell/src/Tidepool/Session.hs:224-236`) is built from `(OccName, Type)`
   pairs directly — no `mi_extra_decls`, no `ifIdUnfolding`
   (`Session.hs:16-22`). There is no body to carry, so there is nothing a
   run could have contributed.
3. **The value-plane key.** `stableVarId (sessionBinderName hsc sm occ)`
   (`Main.hs:1056`) hashes the *module-name string and the occ string only* —
   `Session.hs:243-245` says so in as many words ("The `Unique` is irrelevant
   to that hash"). So the key both planes agree on is derivable at compile
   time, before the value exists.
4. **What the Rust side actually contributes post-run** is the heap root slot,
   and that is resolved at *codegen* through the `ExternalEnv` override keyed
   on that same `stableVarId` (`Session.hs:8-11`). The type plane and the value
   plane are already decoupled by design; the disk file is a transport, not a
   dependency.

Mapped exactly, as the checkpoint asks — **what the on-disk gen-module carries
that an in-memory iface would not: nothing type-relevant.** Two mechanism notes
that constrain *how* to do it:

- **Keep the disk round-trip.** `injectSessionIface` reads the `.hi` by raw
  path and `typecheckIface`s it (`Session.hs:286-309`); the module doc notes
  the binder `Unique`s are *reallocated on read* because interface `Name`s are
  content-addressed by `(Module, OccName)` (`Session.hs:220-223`). The
  `mkUniqueGrimily` seeds (`Session.hs:254-257`) are scoped by their own
  comment to "the (transient) write session". A batch session is neither
  transient nor empty. Writing the thin iface and injecting it back costs
  microseconds against a 5-8s spawn, and it reuses the *exact* mechanism
  already proven in production. **Do not "optimize" the round-trip away.**
- **Mid-session injection is already proven.** `injectSessionScope` runs in
  `cpAfterLoad`, i.e. *after* `load'` has populated the HPT
  (`GhcPipeline.hs:654-661`). Injecting again between items is the same
  operation at a different point in the same session.

The decl plane needs nothing new either: a `Lib.G<g>` module is genuine source
the Rust side renders and writes (`session/mod.rs`, `write_module`), which
`depanal` summarises normally.

### 2.2 Where the sequencing seam already exists

The interleaved variant is the mechanism, verbatim. `OptimizeEveryModule`
runs `compileFront` → `compileBack` → `cpAfterModule` per module before the
next module starts (`GhcPipeline.hs:353-359`), precisely so that
`cpAfterModule`'s `mkIfaceTc` + `addToHpt` (`GhcPipeline.hs:716-723`) is
visible to a LATER module's typecheck. `plans/post-restart/ghcpipeline-seam-analysis.md`
§"Genuine seams" item 4/5 is the map. A batch item chain is the same shape one
level up: compile item k, register its Val iface, compile item k+1.

`cpSummaries` is already a plan field (`GhcPipeline.hs:153`), so supplying an
explicit item order is a variant concern, not a skeleton change. The batch is a
new `PipelineVariant` + an outer loop, which is what the boundary asks for
(extend via a variant, do not rework the skeleton).

### 2.3 THE actual open gate: session reuse across cycles

(a) as posed passes. The question it does not ask, and the one the win depends
on, is:

> **Can one `runGhc` session run N sequential `setTargets`/`depanal`/`load'`
> cycles with items 2..N skipping the stdlib compile?**

`runCompile` is one `runGhc`, one `depanal`, one `load'`
(`GhcPipeline.hs:188`, `:236`, `:256`). Nothing in-tree runs a second cycle.
If item 2's `load'` recompiles `Tidepool.Prelude`, the entire win evaporates
and the lane is a finding, not a feature. Three named risks:

1. **HPT reuse with no linkable.** The session variant registers deferred
   modules with `emptyHomeModInfoLinkable` (`GhcPipeline.hs:721`) under
   `noBackend`/`NoLink` (`GhcPipeline.hs:922-923`). Whether `load'`'s
   recompilation check reads "in HPT, no linkable" as up-to-date or as
   needing recompilation is the single make-or-break fact.
2. **Module-graph state.** `load'` leaves `hsc_mod_graph = depGraph` and
   `cpAfterLoad` restores `modGraphRaw` (`GhcPipeline.hs:652-653`). A second
   cycle must start from a clean graph, not an inherited one.
3. **EPS across cycles.** The `unpoison` discipline
   (`GhcPipeline.hs:216-235`, `:253-254`) is per-summary and would be
   re-applied per cycle — but a second `depanal` re-runs `enableCodeGenForTH`'s
   downgrade against an EPS that is now warm, not fresh. The comment at
   `:230-235` is explicit that a post-`load'` EPS flush does NOT work; the
   corresponding question for a second cycle is unanswered in-tree.

**This is the spike gate.** It is a Haskell-only question, answerable by a
standalone spike in the existing `spike-extract` mould (`haskell/CLAUDE.md`'s
component table), with no Rust side and no protocol. It is the next step and
the only next step; §3-§6 below are the design that spike unblocks or kills.

> **SETTLED — see §7.** The spike ran (`haskell/spike-batch/`,
> `plans/post-restart/batch-turns-spike-findings.md`). Risk 1's framing was
> wrong and the truth is blunter: `load'` never consults the live HPT for
> recompile avoidance at all — it clears it at the top of every call and reads
> its baseline only from a caller-supplied `ModIfaceCache`. Threading one
> flips cycles 2..N to `UpToDate`. Risks 2 and 3 did not materialize. The
> binder chain resolves across three cycles.

---

## 3. (b) Mid-block compile failure and error attribution

**Answer: the contract holds, because batching moves work EARLIER, never
later.**

Today, item k compiles at the moment it is about to run; a compile error
becomes a `TurnOutcome::Error`, `cursor.absorb` returns stop
(`session.rs:856-863`, `:946-953`), and `finish_block` assembles the response
from the items that ran (`session.rs:1005`).

Batched: items 1..N compile up front, the batch stops at the first item that
fails to compile (call it k), and the block-runner then runs fragments 1..k-1
in order before emitting item k's compile error and stopping. Two differences,
both unobservable:

1. Items k+1..N are not compiled either way — the batch stops at k exactly as
   the sequential runner would have. Same.
2. If some item j < k fails at RUNTIME, fragments j+1..k-1 are discarded
   unrun. Today they would never have been compiled. The block stops at j
   either way; the cost is wasted compile, not a semantic change.

There is no case where batching produces an error today's path would not, or
suppresses one it would.

**The one real requirement.** Per-item diagnostics are rebased against that
item's own wrapped source — `user_code_line_range` / `compile_fail`
(`session.rs:2952`, `:3012`) work off the *specific* wrapper text. Today's
channel-1 stdout is one flat array (`DiagJson.renderDiagsJson`,
`haskell/src/Tidepool/DiagJson.hs:102-104`) because one spawn means one item.
A batch spawn must carry diagnostics keyed by item index or attribution
breaks silently. That requirement lands in (c) and is the reason (c) needs a
document rather than a flag.

**The fallback is unconditional.** The per-item path stays the primary,
always-correct implementation. A block the planner cannot batch, a batch spawn
that fails in a way not attributable to an item, and any batch-infeasible item
shape all rerun per-item unchanged. The batch is a fast path that can always
be declined — which is what makes the "never a semantic change" boundary
enforceable rather than aspirational.

---

## 4. (c) The wire: N fragments out of one spawn

**Answer: on the `--turn` rail repeated N times, with per-item output
directories, and stdout as the first genuine caller of the manifest.**

Two existing multi-output rails, and why one is right and the other is not:

- `--targets a,b` (`Main.hs:754-778`) emits N targets out of **one** compile,
  merging into one `meta.cbor` with documented cross-target scalar rules
  (`Main.hs:516-534`). Batch turns are N **separate** compiles whose
  `DataConTable`s and `has_io` must NOT merge. **Wrong rail.**
- `--turn` (`Main.hs:844-929`) emits exactly one item's full output set:
  `result.cbor`, `meta.cbor`, `asks.json`, and the `TurnOut` sidecar
  (`--turn-out`, `Main.hs:925-928`), decoded by `decode_turn_out`
  (`turn.rs:752`). **Right rail — repeat it N times into N directories.**

### Proposed shape

```
tidepool-extract --turn-batch <plan.json> --batch-out <dir> [--include …]
```

`plan.json` carries one entry per item: the turn text (or its path), the
verdict (`kind` + binders, forwarded from the block's existing `classify_block`
so the extract never re-parses), the template kind, and the item's
session coordinates (`session-root`, `inject-val` list, `bind-gen`). Every
field already exists as a `--turn` flag; the plan is those flags, N times.

Emissions: item k writes into `<dir>/i<k>/` **exactly today's single-turn
output set, byte for byte**. That is the load-bearing property — `run_turn`'s
decode path (`turn.rs:435-590`) is reused per item with no new decoder, so a
batched item and a per-item item cannot diverge in what Rust reads.

### stdout

Per `plans/post-restart/extract-manifest-schema.md` §0, stdout becomes one
JSON document that *names* files while the large CBOR payloads stay files. The
batch mode is the first caller that genuinely **needs** it: per-item
diagnostics attribution has no home in the flat channel-1 array (§3).

```json
{ "version": 1,
  "diagnostics": [ … the failing item's diagnostics, verbatim … ],
  "items": [ {"index":0,"status":"ok","dir":"i0"},
             {"index":1,"status":"failed","dir":"i1","diagnostics":[…]} ] }
```

A strict superset: the flat top-level `diagnostics` stays and carries the
failing item's diagnostics, so `parse_diag_report`'s exact-match
`SUPPORTED_VERSION = 1` (`tidepool-runtime/src/diag.rs:48`) still parses it and
an un-upgraded reader still sees the real error. That is the manifest doc's own
no-flag-day migration story (§6) applied to one mode. **This does not fight the
schema direction — it is a scoped subset of it**, and it can be superseded by
the full manifest without a wire change to this lane.

`ExtractCmd` gains `turn_batch(path)` + `batch_out(dir)` (`tidepool-extract-cmd`
stays the one invocation builder), and Rust gains exactly one new spawn site:
`run_turn_batch`, a sibling of `run_turn` in `tidepool-runtime/src/session/turn.rs`.

---

## 5. Which items are batchable

A batch must be planned *before* the spawn, so an item whose route depends on a
previous item's compile OUTCOME cannot be in the same batch as its predecessor.
Auditing `run_eval` (`session.rs:1494-1558`):

| Verdict | Route | Statically plannable? |
|---|---|---|
| `Decl` | `run_def` → `define_scoped` | **yes** — already batched across runs (`session.rs:904`) |
| `Bind`, 0 binders | `run_bind_discard` | **yes** (`session.rs:1516`) |
| `Bind`, 1 binder, not a pure bind | `run_bind` (materialize) | **yes** — `pure_bind_to_decl` (`session.rs:3327`) is a pure syntactic test on the text |
| `Bind`, 1 binder, IS a pure bind | `try_pure_bind_as_decl`, falling back to `run_bind` | **NO** — the route is chosen from a compile FAILURE and from `iter_current()` session state (`session.rs:1324-1362`) |
| `Bind`, N binders | `run_multi_bind` | **yes** (`session.rs:1546`) |
| `Expr` | `run_bare_expr` | **yes** (`session.rs:1556`) |
| `Meta` | no compile | **yes** (trivially) |

So the planner takes **maximal runs of statically-plannable items**, splitting
at a pure-bind boundary; the ambiguous item is compiled as the batch's last
member (or on its own), Rust resolves its route, and the next batch continues.
This is exactly the fallback shape the lane brief describes, arrived at from
the route census rather than from the value-plane question — and it is not a
small residue: the repl suite's blocks are dominated by decl+expr chains and
effectful binds, all of which plan statically.

Speculation (compiling both routes of a pure bind in one batch, cheap
intra-spawn) is **rejected**: the two routes bump different generation chains
(`Lib.G<g>` vs `Val.G<g>`), so speculating forks every subsequent item.
Splitting the batch costs one extra spawn; forking costs 2^m.

---

## 6. What lands

Ordered, each gated on the previous:

1. ~~**Spike (§2.3).**~~ **DONE — §7.**
2. **Extract side.** A `batchVariant` over the interleaved tier + the
   `--turn-batch` mode; N item directories + the per-item stdout document.
   Carries the `ModIfaceCache` thread (§7.1) and is gated on the dep-guts memo
   probe (§7.3).
3. **Rust side.** `ExtractCmd` batch args, `run_turn_batch`, and a block
   planner in `run_block` that takes maximal statically-plannable runs and
   falls back per-item on any batch failure.
4. **Oracle.** The repl shard, green, with per-item error-attribution tests and
   before/after latency for the shard and for a multi-item live turn.

Non-negotiable throughout: run-until-first-error and per-item attribution are
preserved exactly (§3); the per-item path is never removed; the `runCompile`
skeleton's seams are extended, not reworked (§2.2).

---

## 7. Spike result, and the sizing question it reopens

Full evidence: `plans/post-restart/batch-turns-spike-findings.md`. Spike:
`haskell/spike-batch/Spike.hs` (`cabal test spike-batch`). Three sequential
cycles in one `runGhc`, with the production `mkThinSessionIface` /
`writeSessionIface` / `injectSessionScope` between them, instrumented with a
caller-supplied `Messager` recording GHC's own per-module recompile verdict.

### 7.1 The mechanism: RED as posed, GREEN under GHC's own documented API

**As production calls it, every cycle recompiles the whole stdlib closure.**
All 12 modules of `Tidepool.Prelude`'s home-package closure report
`NeedsRecompile(MustCompile)` on cycles 2 and 3, though those exact modules are
already in the session HPT.

Risk 1's framing ("does `load'` read *in HPT, no linkable* as up-to-date?")
was the wrong question. `load'` **clears the home-package table at the top of
every invocation** and reconstructs its recompile-avoidance baseline solely
from the caller-supplied `Maybe ModIfaceCache` — never from the live HPT.
`load' Nothing …` (every call site in this codebase, `GhcPipeline.hs:256`)
means `old_hpt = mempty` on every call, so every module looks brand new
regardless of session state.

The fix is GHC's own published mechanism for exactly this caller: `newIfaceCache`
created once and threaded as `load'`'s first argument. `Note [Caching
HomeModInfo]` (GHC `Make.hs`) is written for "API clients who call `load` …
[who] like to cache the HomeModInfo in memory between calls" — a batched
multi-`load'` caller, which is precisely this lane. Threading it flips cycles
2-3 to `UpToDate` across all 12 modules, `load'` 418ms → 1ms.

Risks 2 (module-graph state) and 3 (EPS across cycles) **did not materialize** —
neither scenario tripped them.

**And (a) holds under repetition.** `resolvedPriorBinder = Just True` on cycles
2 and 3, in *both* scenarios: item k+1's typecheck resolves item k's injected
binder, three deep, in one continuing session. §2.1's by-construction argument
is now also an observation.

### 7.2 The sizing claim, and why it is measured against the wrong denominator

The spike closes by scoping the win to ~5-20%: `load'` (145-566ms) over a
~2.6-2.7s per-cycle GHC cost, with the interleaved compile loop flat at
~2.0-2.2s per cycle in both scenarios.

**That ratio is real but it is not this lane's ratio.** Both scenarios ran
entirely inside ONE `runGhc`, so the spike never measured the per-spawn fixed
cost — which is the whole quantity batching exists to delete. The comparison
that decides the lane is *N items in one spawn* vs *N spawns*, and the terms
the spike's design put out of reach are:

- process fork/exec of the extract (through the nix wrapper);
- `getLibdir`, which shells out to `readProcess "ghc" ["--print-libdir"]`
  whenever `TIDEPOOL_GHC_LIBDIR` is unset (`GhcPipeline.hs:986-992`) — an
  entire additional GHC process per extract;
- `runGhc` session init + `setSessionDynFlags` (unit-database load);
- the cold `load'` the spike *did* measure at 418-566ms.

The extractor already emits this partition: `TIDEPOOL_TIMING=1` gives one
`tidepool-timing phase=<name> ms=<int>` line per phase
(`haskell/src/Tidepool/Timing.hs:87`), and `startup` / `ghc_setup` / `ghc_load`
are exactly the amortizable terms while `typecheck` / `core` are the per-item
ones.

**MEASURED** (`plans/post-restart/batch-turns-baseline.md`):

| Shape | fixed (startup+ghc_setup+ghc_load) | total | fixed % |
|---|---|---|---|
| effectful bind | 4634 ms | 8995 ms | **51.5%** |
| bare expression | 3025 ms | 11336 ms | **26.7%** |
| small decl / probe compiles (avg) | — | — | **83.2%** |

Floor on the batch win at N=5 (fixed% × (N-1)/N): **~66.6% for decl-shaped
blocks, ~25% for effectful-bind / bare-expression blocks.** Both above the
spike's 5-20% line, confirming §7.2's objection: that line's denominator
excluded process boot entirely.

**The single most important number is a ratio the spike could not see.**
Cold-process `ghc_load` measures 2.7-7.5s here against the spike's
*within-session* `ghc_load` of 145-566ms — an order of magnitude. So the
dominant win is **never paying a cold process boot for items 2..N**, not
shrinking `load'` on an already-warm session. The `ModIfaceCache` fix (§7.1)
is necessary but is the smaller half.

Two side findings from the same measurement:

- **`TIDEPOOL_GHC_LIBDIR` is unset in the live repl server's launcher env**
  (`~/.claude.json` `mcpServers.tidepool-repl.env == {}`), so every spawn's
  `startup` phase forks a real `ghc --print-libdir` subprocess
  (`GhcPipeline.hs:986-992`). Under 1% of a full-stack turn, but pure
  amortizable waste and an orthogonal one-line fix.
- **The measured spawn census runs consistently +1 over §1's predicted table
  on all three decl-route shapes** (single decl 4 vs 3, three decls 6 vs 5,
  pure bind 4 vs 3; effectful bind matches at 2). Unexplained. Trace it before
  the batch planner's spawn budget is trusted — §1's table is a code-reading
  prediction and is now known to be wrong somewhere.

**Shard baseline** (`scripts/battery-shard.sh tidepool-repl`): cold
**2919s / 48m40s**, warm **3313s / 55m14s**, 195/195 passing both times. Warm
is *slower*, +13.5% — the compile memo barely fires on this suite because each
test compiles distinct per-test Haskell rather than a byte-identical repeat,
unlike the harness-binary case the root `CLAUDE.md` cites. Anyone quoting
that doc's 99s-cold/47s-warm figure for the repl shard would be quoting the
wrong workload.

### 7.3 The compile loop is probably not irreducible either — and that is the next probe

The spike treats the ~2.0-2.2s interleaved compile loop as flat and
unavoidable, citing `sessionVariant`'s own rationale
(`GhcPipeline.hs:702-715`): `load'`-provisioned ifaces carry no -O2
unfoldings, so home-library calls must resolve against freshly recompiled
bodies or `resolveExternals` bakes `ErrorSentinel` poison. That rationale is
correct **per spawn**. It does not obviously survive contact with a batch:

Cycle 1's loop already produces full -O2 `ModGuts` for all 12 stdlib modules,
in memory. Cycle 2 recompiles those same modules, from the same source, under
the same flags, in the same session — and then `runCompile` merges
`concatMap mg_binds depGuts ++ mg_binds targetGuts` (`GhcPipeline.hs:449`).
**Memoizing cycle 1's dep guts and reusing them for cycles 2..N** would cut the
per-item marginal cost down to the turn module's own compile, which is small.
If that holds, the dominant per-cycle cost is amortizable too, and the batch
win is not bounded by `load'`'s share at all.

The soundness argument, as far as reading goes: the target's Core references
dep bindings through EXTERNAL names, and `Translate.stableVarId` keys those on
`(module, occ)` strings — so a memoized dep binding and a freshly recompiled
one carry the same key by construction. A dep module's INTERNAL top-level
floats are externalized with their unique baked into the OccName
(`externalizeInternalTops`, `GhcPipeline.hs:947`), and those uniques *would*
differ between cycle 1's and a hypothetical cycle 2's compile — but using only
the memoized copy keeps that set internally consistent, and the target cannot
reference a dep's internal float anyway (`GhcPipeline.hs:942`: internal names
are not referenceable across `ModGuts`).

**Treat that as a hypothesis, not a conclusion.** It is exactly the shape of
claim that reads sound and fails as a `case`-trap at run, and this codebase has
the scar tissue to prove it (#313, the poison-`ErrorSentinel` commentary the
rationale above comes from). It gets its own probe, on the differential oracle,
before any batch mode relies on it — and the batch mode must be correct, if
slower, without it.

### 7.4 Revised gate order

1. ~~**Measure the real phase table.**~~ **DONE — §7.2.** The lane is worth
   building: floor of ~25% on the worst-measured shape, ~66% on decl-shaped
   blocks, before the memo.
2. ~~**Probe the dep-guts memo.**~~ **DONE — §7.6. GREEN.**
3. Then steps 2-4 of §6, with the `ModIfaceCache` thread (§7.1) and the
   per-module guts memo (§7.6) included from the start, and §8's wire contract
   as the seam between the two build halves.

### 7.5 The bare-expression retry: not a competing win — a worked example of this lane

Settled: `plans/post-restart/bare-expr-retry-finding.md`.

The mechanism correction held. It is `run_bare_expr`'s monadic-first cascade
(`session.rs:2124`, error discarded unexamined at `:2143`), not
`query_inner_type` — proven two ways: the wasted spawn's own GHC diagnostic
reproduces `wrap_bare_it_monadic`'s literal generated source (`__user`,
semicolon-braced do-block) rather than `wrap_probe_source`'s different shape,
and `query_inner_type`'s single call site sits on `run_plain_eval`, reachable
only when the block's batch classify itself failed — never on a steady-state
turn. Measured: pure bare expr 3 spawns / ~11.6s, monadic 2 spawns / ~5.75s.

**My own framing in the previous revision — that this was "worth more than
the lane" — was wrong, and the finding's structural argument is the reason.**
A compile *failure* short-circuits in typecheck and pays fixed cost only
(`startup` 26ms + `ghc_setup` 38ms + `ghc_load` 2035ms, **no** `typecheck` /
`core` line at all). A compile *success* pays the full pipeline, and `core` —
almost entirely `core2core` — is the largest phase in every successful compile
in the census (2566-5079ms). So:

- today's monadic-first ordering is close to **cost-optimal** for a
  one-process-per-attempt world: zero waste when the guess is right, and the
  *cheapest available* failure when it is wrong;
- reordering to pure-first does not create a cheap probe, because
  `let it = <expr>` always typechecks — it just moves the expensive miss onto
  the monadic shape (~2.5× regression there), needing an implausible ~80%
  pure-dominant mix merely to break even. Correctly rejected.
- the one-wrapper-serves-both option (overlapping instances) was rejected as
  **unproven rather than unsafe**, with the failure mode named (ambiguous
  instance resolution on open principal types, not wrong-branch selection).

The decisive part for this lane: **the wasted spawn is ~100% fixed cost —
precisely the term batching amortizes.** This is not an adjacent inefficiency
competing with the batch design; it is a worked example of what the batch
design is for, and it needs no separate fix. Left unfixed deliberately.

**One structural fact from that census that re-aims §7.3.** The eval preamble
is *imports*, not inlined source (`patched_preamble` rewrites import lines;
`begin_user_module` = preamble + imports + the user's text). So the
2900+ bindings the baseline attributed to "the preamble" live in DEPENDENCY
modules — `Tidepool.Effects`, `Prelude`, `Library`, `Orchestrate` — while the
target module stays small. That is the memo hypothesis's exact target: the
dominant `core2core` cost sits in modules that are *identical across items of
a block*. It also sharpens the known condition — the stdlib closure is stable
within a block, but `Lib.G<g>` changes whenever a decl item lands, so the memo
must be per-module and invalidated for the modules that actually changed.

### 7.6 The dep-guts memo: GREEN, with the headline calibrated

Settled: `plans/post-restart/batch-turns-gutsmemo-findings.md`. Scenario C
memoizes cycle 1's post-`core2core`, post-`externalizeInternalTops` dependency
`ModGuts` and reuses them unchanged for later cycles, computing the fresh-deps
and memoized-deps merges **in the same run and cycle** off the same `HscEnv`
and diffing the real `translateModuleClosed`.

- `cmUnresolved` and `cmPoisoned`: **empty and identical on both paths, every
  cycle.** This is the instrument that mattered — a poisoned entry is the
  silent failure the `sessionVariant` rationale predicted, and it did not
  appear.
- Node counts: **exact match** (2283, then 481/481).
- Compile loop on memo-reusing cycles: **3114 ms → 9 ms, 3215 ms → 4 ms.**

So §7.3's by-construction argument holds under measurement: external
references key on `(module, occ)`, which `externalizeInternalTops` /
`stableVarId` make invariant to *which* compile produced the binding, and a
memoized module's internal floats stay self-consistent because they all come
from one compile.

**Calibrating the headline, because the probe's own targets undercut it.**
Those single-digit millisecond figures are the *spike's* targets — one-line
expressions. The residual per-item cost in production is `load'` (~1 ms) plus
**the real target module's own typecheck + `core2core`**, and nobody has
measured that under the memo. The baseline's `typecheck` 424ms / `core` 5079ms
for a real bare expression describes the *dependency* side of that split, not
the target's own delta. **The correct claim is that the memo removes the
dep-recompile share — which the baseline's phase table shows is the dominant
part of `core` — not that per-item cost goes to zero.** That residual is the
first thing the build wave must measure.

**Two named gaps, neither closed:**

1. **The incremental-population case was argued, not measured.** The probe's
   dep closure is fixed across cycles. A decl item introduces a *new*
   `Lib.G<g>` home module mid-batch, which a frozen-after-cycle-1 memo would
   either miss (an unresolved external) or have to recompile every cycle
   (forfeiting the win for exactly the items that add modules). The fix is a
   generalization, not a different mechanism — memoize **per module**,
   populated **incrementally** on that module's first compile in the batch —
   and the soundness argument is unchanged by it. But it is extrapolation.
   Build it with a probe, not on faith.
2. **This is a translate-level check, not an execution-level one.** It cannot
   rule out a **VarId collision of the #313 class** between a memoized
   internal float and a later cycle's fresh target compile — which by
   construction would show *clean* unresolved/poisoned sets on both paths and
   surface only as two source bindings sharing one JIT heap slot at run. The
   end-to-end oracle (JIT-compile and *run* both paths, differential) is
   mandatory before production traffic, and the repl shard is that oracle.

---

## 8. The wire contract

Fixed here so the two build halves can proceed in parallel without
negotiating. Extends `plans/one-spawn-turn-protocol.md` from one spawn per
TURN to one spawn per BLOCK.

```
tidepool-extract --turn-batch <plan.json> --batch-out <dir> [--include <dir>]…
```

**`plan.json`** — `{"version":1,"items":[…]}`, one entry per item in execution
order. Each item carries exactly the fields its equivalent `--turn` spawn
takes today, so the batch introduces no new per-item semantics:

```json
{ "index": 0,
  "turn_text": "…",                  // verbatim, as --turn's input file
  "verdict": {"kind":"bind","binders":["x"]},   // from the block's existing classify
  "template": "bind",                 // TemplateSelector wire name
  "session_root": "/…",
  "inject_vals": ["Tidepool.Session.Val.G3"],
  "bind_gen": 4 }
```

**Output** — item `k` writes `<dir>/i<k>/` containing **exactly today's
single-turn output set, byte for byte**: `result.cbor`, `meta.cbor`,
`asks.json`, and the `TurnOut` sidecar. This is load-bearing: `run_turn`'s
existing decode path (`turn.rs:435-590`) is reused per item with no new
decoder, so a batched item and a per-item item cannot diverge in what Rust
reads.

**stdout** — one JSON document, a strict superset of today's report:

```json
{ "version": 1,
  "diagnostics": [ … the failing item's diagnostics, verbatim … ],
  "items": [ {"index":0,"status":"ok","dir":"i0"},
             {"index":1,"status":"failed","dir":"i1","diagnostics":[…]} ] }
```

The flat top-level `diagnostics` stays and carries the failing item's
diagnostics, so `parse_diag_report`'s exact-match `SUPPORTED_VERSION = 1`
(`tidepool-runtime/src/diag.rs:48`) parses it unchanged and an un-upgraded
reader still sees the real error. Compilation **stops at the first item that
fails**; items before it still have complete output directories, items after
it are absent. That is what preserves run-until-first-error (§3).

**Rust side** — `ExtractCmd` gains `turn_batch(path)` + `batch_out(dir)`
(`tidepool-extract-cmd` stays the one invocation builder); exactly one new
spawn site, `run_turn_batch`, a sibling of `run_turn`.

**Invariants no implementation may weaken:** the per-item path stays the
primary, always-correct implementation and is never removed; any block the
planner cannot batch, and any batch failure not attributable to a specific
item, reruns per-item unchanged; run-until-first-error and per-item
attribution are observationally identical to today (§3).
