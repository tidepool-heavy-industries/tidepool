# One spawn per BLOCK: the repl-planner finding

**Lane:** `batch-turns` → `batch-planner` child. Written against §3/§5/§8/§8.1 of
`plans/post-restart/batch-turns-feasibility.md` and both sibling receipts
(`batch-turns-rust-receipt.md`, `batch-turns-extract-receipt.md` — both
mechanism halves are BUILT, MERGED, and MEASURED at 67.9% wall-clock
reduction for a 5-item independent-bind batch). No file in `haskell/`
touched; §8's wire untouched; no fixture regeneration; no live model calls.

**Verdict, in one line: NOT hoistable within this lane's scope, as built.**
The compile step cannot be moved ahead of the run for `tidepool-repl`'s
`run_turn`-routed item shapes without either amending the frozen §8 wire or
adding new suspend-crossing state to `BlockCursor` beyond what this lane can
safely carry. This document is the required precise finding — per the lane's
own DONE CRITERIA, a stop-and-report here is a fully successful landing.

---

## 1. What this lane set out to do, and why it stops before writing code

Steps 2-6 of the lane brief (add a planner, wire `run_turn_batch`, pin
semantics with tests, measure spawn+wall-clock) all presuppose that
`tidepool-repl/src/session.rs`'s four `run_turn`-routed item handlers
(`run_bind` — `:1711`, `run_bind_discard` — `:1856`, `run_multi_bind` —
`:1894`, `run_bare_expr` — `:2096`) can have their COMPILE call swapped from
today's per-item `compile_session_turn` spawn to a batched
`run_turn_batch` spawn, with the RUN half (bootstrap/`add_fragment_session`/
`bind_funcid*`/`settle*`) left untouched and strictly sequential.

Reading those four functions against `run_turn`/`run_turn_batch`'s actual
built wire (`tidepool-runtime/src/session/turn.rs`) surfaces two blockers,
one structural-and-fixable-only-by-amending-§8, one fully disqualifying for
one of the four named shapes. Both are described precisely below, with the
narrower "maybe we can still batch 3 of 4 shapes" path also traced through
and rejected — not because it's impossible, but because what it would cost
(new suspend-crossing `BlockCursor` state, a synthesized-not-GHC-sourced
verdict, or both) is exactly what the boundary told this lane not to spend
its risk budget on.

## 2. `session.rs` does not use `run_turn`/`run_turn_batch` today — it uses a different, older wire

This is the load-bearing fact the rest of the finding follows from, and it
is worth stating first because the lane brief's own phrasing ("today these
functions compile-and-run in one call... batching needs the COMPILE hoisted
ahead of the run") reads as if `run_bind` et al. already speak `run_turn`'s
protocol and only need multiplexing. They do not.

- `run_bind`/`run_bind_discard`/`run_multi_bind`/`run_bare_expr`/
  `probe_pure_type`/`query_inner_type` all call
  `tidepool_runtime::session::compile_session_turn` (session.rs:53 import;
  call sites at :1384, :1736/:1877/:1918/:2129/:2153, :2293). That function
  (`turn.rs:1217-1339`) takes a **fully wrapped module source** — built
  Rust-side by `wrap_bind_source`/`wrap_bind_discard_source`/
  `wrap_multi_bind_source`/`wrap_bare_it_monadic`/`wrap_bare_it_pure`
  (session.rs:3145-3258) — and compiles it directly against `--target
  __result`, with an optional `SessionBind{names, gen}` for the bind
  metadata. There is no verdict, no template, no `{{TURN}}`/`{{TURN_STMT}}`
  splice anywhere in this path.
- `run_turn`/`run_turn_batch` (`turn.rs:435`, `:671`) are a **different**
  protocol: a `TurnRequest`/`BatchTurnItem` carries `turn_text` (the RAW
  user statement) plus a GHC-sourced `TurnClassification` verdict; the
  extract splices `turn_text` into a batch-wide `TurnTemplate` (`{{TURN}}`/
  `{{TURN_STMT}}`/`{{BINDERS}}`) selected by `TemplateSelector::for_verdict`.
  This is the mechanism §8/§8.1 froze and the `batch-rust`/`batch-extract`
  siblings built and measured — but **nothing in `tidepool-repl` calls it**.
  `grep -rn "session::run_turn\b\|TurnRequest" --include='*.rs' .` finds
  callers only in `tidepool-runtime`'s own tests and `tidepool-harness`
  (a different resident-turn engine, its own template construction) — never
  `tidepool-repl`. `session.rs`'s own test module
  (`turn_template_byte_identity_tests`, session.rs:4145-4224) exists
  precisely to PROVE the two protocols produce byte-identical output for a
  bind turn — a proof built for a migration that was never carried out, not
  evidence the migration already happened.

So "hoist the compile" is not "call `run_turn_batch` instead of N
`compile_session_turn` calls with the same inputs" — it is first "migrate
`run_bind`/`run_bind_discard`/`run_multi_bind`/`run_bare_expr` off
`compile_session_turn` and onto `run_turn`'s turn_text+template+verdict
protocol", and only then "batch the migrated calls". The migration itself is
where both blockers below live.

## 3. Blocker A: a batch's growing per-item IMPORTS have no home in a batch-wide template

`compile_session_turn`'s wrapped source begins with `begin_user_module(
preamble, imports, input)` (session.rs:3087-3092), where `imports` is
`self.turn_imports(turn_text)` / `self.session_imports()` (session.rs:534-560):
the current decl-plane `Lib.G<g>` module (if any) plus **every live value
binding's current `Tidepool.Session.Val.G<g>` module, as an explicit
unqualified `import` line**, plus a conditional `Tidepool.QQ` import when the
turn text splices a quasiquoter.

This import text is genuinely per-item within exactly the runs this lane
wants to batch: `current_val_modules()` (session.rs:527-529) reads LIVE
session state that changes as each binding item in the SAME candidate batch
runs — item k+1 must literally `import Tidepool.Session.Val.G<k>` to
reference item k's freshly bound name unqualified, which is the entire
point of chaining (`x <- foo; y <- bar x`) that makes batching worth having
at all.

**Confirmed, not assumed**: `--inject-val` (the flag `BatchTurnItem.
inject_modules`/`TurnRequest.inject_modules` already carries, correctly,
per item) only registers a module's thin iface into the GHC session's HPT/
finder (`Tidepool.Session.injectSessionIface`,
`haskell/src/Tidepool/Session.hs:278-305`, whose own doc says injection
exists "so an `import` of the module resolves purely from the serialized
interface" — i.e. it makes an import RESOLVABLE, it does not synthesize one).
No code path in `haskell/app/Main.hs`, `Tidepool/Session.hs`, or
`Tidepool/GhcPipeline.hs` builds or splices an `ImportDecl` — a targeted
grep for `ImportDecl`/`synthesizeImport`/`addImport`/`prependImport` across
those three files returns zero hits. A compiled module that does not
textually `import Tidepool.Session.Val.G<g>` cannot see that module's names,
qualified or unqualified, no matter what `--inject-val` list accompanies the
spawn. So the explicit import TEXT is load-bearing, not redundant — and it
has to live somewhere in what gets sent to the extract.

§8.1's ruling #2 (ratified, binding on both halves) fixes templates as
**batch-wide**: "supplied once as repeated top-level flags... not per item.
The per-item `"template"` field in `plan.json` is a SELECTOR... never a
path." The wire's only two substitution points are `{{BINDERS}}` and one of
`{{TURN}}`/`{{TURN_STMT}}` (`turn.rs:169-190`) — there is no `{{IMPORTS}}`
placeholder, and `{{TURN_STMT}}` places `turn_text` INSIDE the `do { }`
statement sequence of an already-built `__result = do { ... }` body, a
position where a module-level `import` declaration is not legal Haskell
syntax at all. A batch-wide template genuinely cannot express "item 2's
import list is item 1's import list plus one more line," and the two
substitution points genuinely cannot carry an import declaration through
`{{TURN_STMT}}`'s placement even if a caller tried.

**The one workaround considered and rejected.** Since `turn_text` is a
per-item `&str` with no runtime-enforced shape, a degenerate template
(literally just `{{TURN}}`, nothing else) would let a caller cram an
ENTIRE wrapped module — including its own per-item import block — into
`turn_text`, sidestepping the batch-wide constraint by never really using
the template mechanism. This is type-legal (nothing rejects it) and the
extract-side module-header rename (`renameModuleHeader`,
`batch-turns-extract-receipt.md` "Design notes") is generic over where the
header text came from, so it would probably not collide. It was rejected
for three reasons: (1) it contradicts `turn_text`'s documented meaning
("the raw turn text... `x <- e` / `let x = e` / a bare expression",
`turn.rs:561-563`) and the module's own stated invariant that "Rust never
classifies — every verdict is GHC-sourced" (`turn.rs:14-16`), turning
`run_turn_batch` into a bare CBOR-in/CBOR-out transport rather than the
protocol its own types describe; (2) it does not reduce or simplify any
Rust-side work — it requires reconstructing `wrap_bind_source`'s full
output per item anyway, just repackaged through a different call; and
(3) — decisively — it does not solve Blocker B below, so it cannot be the
whole answer regardless of its other merits.

## 4. Blocker B: `run_bare_expr`'s `it`/`__it_render` bind has no representation in the verdict-driven wire, and this one has no workaround

`run_bare_expr` (session.rs:2096-2239) is explicitly one of the four
in-scope shapes. Its classify verdict — GHC's own parser, from the block's
batch `classify_block` spawn — is `TurnKind::Expr` with `binders: []` (a
bare expression is definitionally not a bind). But `run_bare_expr` does not
compile it as an expression: it compiles it as a SYNTHETIC 2-name
materializing bind (`it`, `__it_render`) via `compile_session_turn`'s own
`SessionBind{names: ["it", "__it_render"], gen}` mechanism (session.rs:2118,
:2134-2138, :2158-2161) — orthogonal to, and inconsistent with, the
classify verdict. This is deliberate and load-bearing: it is how `it` gets
rooted onto the value plane and how the response's rendered value and type
come out of a SINGLE compile+run (see `run_bare_expr`'s own doc,
session.rs:2078-2095).

`run_turn`/`run_turn_batch`'s wire has no channel for this. `TemplateSelector::
for_verdict` (`turn.rs:148-155`) derives the template selector — and,
through it, whether the extract writes a `SessionBind` iface at all — STRICTLY
from `TurnClassification.kind`/`.binders`. `TurnResult::Expr` (`turn.rs:298-306`)
carries no `bound: Vec<BoundBinder>` field at all (only `TurnResult::Bind`
does, `turn.rs:285-297`) — an Expr-verdict turn-batch item structurally
cannot produce a bound binder, by the shape of the wire's own return type,
independent of any workaround.

Getting a bound `it` out of `run_turn_batch` for a bare expression would
require sending a **verdict that is not what GHC's classifier said** —
`TurnClassification{kind: Bind, binders: ["it", "__it_render"]}` for text
GHC parsed as `Expr` — which directly violates `turn.rs`'s own stated
invariant ("every verdict is GHC-sourced", `turn.rs:14-16`) and, via
`--turn-verdict`, tells the extract to SKIP its own re-parse and trust the
lie. Worse: `run_bare_expr` tries TWO turn-text shapes today, in order —
`wrap_bare_it_monadic` (`it <- __user`) first, falling back to
`wrap_bare_it_pure` (`let it = __user`) only if the monadic form fails to
typecheck (session.rs:2127-2171, the documented "monadic-first cascade" that
`plans/post-restart/bare-expr-retry-finding.md` independently confirmed is
close to cost-optimal and deliberately left alone). `run_turn`'s own
variant-retry mechanism ("several `TurnTemplate`s may share a `kind`... the
extract's own retry: try each in order," `turn.rs:186-190`) retries
TEMPLATE variants, which are batch-wide and fixed for the whole spawn — it
cannot retry two different SHAPES of a single item's own `turn_text` within
one plan entry, because a `BatchTurnItem` carries exactly one `turn_text`
string. Picking one shape ahead of time and sending only it would silently
drop the pure-expression fallback for every non-monadic bare expression in a
batch (`x + 1`, `v ^? key …`, both common) — a real semantic regression the
boundary explicitly forbids ("Run-until-first-error and per-item error
attribution must be OBSERVATIONALLY IDENTICAL to today... never a semantic
change").

This blocker is independent of Blocker A and independent of the "cram
everything into `turn_text`" workaround from §3 — it survives that
workaround unchanged, because the problem isn't where the import text lives,
it's that the wire has no way to say "this GHC-classified expression should
be compiled and bound as if it were a 2-name bind, trying two textual shapes
in order." Fixing it means amending §8 (a new verdict shape, or a
per-item variant-turn_text list) — explicitly out of this lane's scope
("Do NOT touch... §8's wire (both frozen and built)").

## 5. Why the narrower "batch everything except bare expressions, only when items don't chain" path was also rejected

Blocker A is solvable in a strictly narrower slice: restrict a batchable run
to items whose `turn_text` does not reference any name bound earlier in the
SAME run (a static `mentions_word` scan against each earlier item's
binder(s), ending the run at the first item that would need a same-run
import). Under that restriction every item's import list is identical —
exactly the FIXED, pre-run `session_imports()` — which fits a batch-wide
template. This is not hypothetical: it is exactly the "5 independent bind
items" shape `batch-turns-extract-receipt.md` measured its headline 67.9%
number against.

Two things stopped this lane from building even that slice:

1. **It still cannot include `run_bare_expr`** (Blocker B applies
   regardless of chaining), which means excluding the single most common
   terminal item shape in a block — `tidepool-repl/CLAUDE.md` itself:
   "end with a bare expression to populate `value`." A batching mechanism
   that can never batch a block's last item captures a narrow slice of real
   traffic.
2. **It still needs new suspend-crossing state in `BlockCursor`.**
   Compiling N items up front and running them sequentially means that if
   item k (mid-batch) suspends on an `ask`, items k+1..N of the batch are
   ALREADY COMPILED — sitting as decoded `TurnResult`s in the block-runner's
   local state — and must survive the suspend/resume round trip so
   `resume_block` can run them without recompiling. Today's `BlockCursor`
   (session.rs:354-385) carries exactly ONE pending item
   (`pending_item: Option<(usize, ItemKind)>`), a shape the module's own doc
   justifies precisely because "only a single item can suspend... the
   cursor has one pending-item slot rather than a stack" (session.rs:817-822).
   A batch's precompiled tail is new information that invariant doesn't
   account for — not a rework of the ask-suspend signaling itself, but new
   state that must be threaded through the exact machinery the boundary
   singles out ("Do NOT half-restructure `drive_block`/`BlockCursor`'s
   suspension machinery").

Given (1) caps the value of the slice below what the lane's own measured
number promised, and (2) is precisely the risk the boundary asked this lane
not to take on, building the narrow slice under this lane's time and
correctness budget was assessed as a worse outcome than reporting the
finding — especially given the stated stakes: "a failure [in the shard] is
the single most valuable signal this lane can produce, because it is the
only check that can catch a #313-class VarId collision," and this slice's
main NEW risk surface (a hand-rolled cross-item reference scan gating what's
"safe" to batch) is exactly the kind of subtle, hard-to-differentially-test
logic most likely to hide that class of bug.

## 6. What this means for the lane, concretely

- The mechanism `batch-rust`/`batch-extract` built (`run_turn_batch`, the
  `ModIfaceCache` thread, the incremental per-module dep-guts memo) is real,
  merged, and measured — 67.9% wall-clock reduction on a 5-item independent
  bind batch, `plans/post-restart/batch-turns-extract-receipt.md`. It is
  simply not yet reachable from `tidepool-repl`, because `tidepool-repl`
  compiles session turns through a different, older protocol
  (`compile_session_turn`) that the mechanism's wire (`run_turn`/
  `run_turn_batch`, keyed on GHC-sourced verdicts and batch-wide templates)
  was not built to receive without amendment.
- The nearest concrete next step, if this lane's parent wants to pursue it,
  is an amendment to §8 (not this lane's call to make) that gives a
  `plan.json` item either (a) a per-item import-lines field the template
  splices before `{{TURN}}`/`{{TURN_STMT}}`, or (b) turn_text meaning "the
  full per-item source, template reduced to a thin shell" as a DELIBERATE,
  DOCUMENTED wire shape rather than an ad hoc workaround — plus, separately,
  a verdict/binder shape (or a small enumerated set of named synthetic
  binds) that can express `run_bare_expr`'s `it`/`__it_render` materialize-
  with-retry behavior. Both are amendments to a frozen contract, correctly
  out of this lane's authority.
- No change was made to `tidepool-repl/src/session.rs`,
  `tidepool-runtime/src/session/turn.rs`, or any `haskell/` file. The
  per-item path is exactly as it was — nothing to fall back FROM, because
  nothing was switched.

## 7. Baseline numbers (current per-item cost, unchanged by this lane)

Measured via the existing `tidepool-repl/tests/batch_turns_spawn_census.rs`
(pre-existing infra, not authored by this lane), which reports real
`tidepool-extract` spawn counts per item shape through the actual
`session_run` entry point — the "before" side of the comparison this lane
was asked for. There is no "after" to compare it against, for the reasons
above.

```
=== batch-turns spawn census (tidepool-repl/tests/batch_turns_spawn_census.rs) ===
single decl:              4
3 consecutive decls:      6
pure bind:                4
effectful bind:           2
bare expression:          3
mixed 5-item block:       12
===================================================================
```

(`effectful bind` = `run_bind`'s shape; `bare expression` = `run_bare_expr`'s
— note it costs 3 spawns on its own steady state: classify + the monadic
attempt + the pure fallback, matching `bare-expr-retry-finding.md`'s
already-settled finding that this is close to cost-optimal per-spawn and is
deliberately left alone. `mixed 5-item block` — decl, pure bind, effectful
bind, then two bare expressions referencing prior bindings — costs 12
spawns today, the realistic shape this lane's batching was aimed at.)

Wall-clock was not additionally captured for this census (the test measures
spawn count only); `batch-turns-extract-receipt.md`'s own 5-independent-bind
measurement (3.09s batched vs 9.64s for 5 separate spawns, 67.9% reduction)
remains the standing wall-clock evidence for the mechanism itself, on the
one narrow shape (independent, non-chained binds) that doesn't hit either
blocker above — a shape `tidepool-repl` cannot currently reach without the
`compile_session_turn`→`run_turn` migration and the wire amendments §4-§5
describe.

## 8. VERIFY legs run

```
cargo check --workspace --all-targets                                   # clean
cargo clippy --workspace --all-targets                                  # clean
cargo fmt --all -- --check                                              # clean
scripts/battery.sh -p tidepool-repl -E 'binary(batch_turns_spawn_census)'
                                                                          # 1/1 pass, 120.8s –
                                                                          #   numbers in §7 above
scripts/battery.sh -p tidepool-runtime -E 'binary(cross_mode_targeted)'  # 10/10 pass, 21.7s –
                                                                          #   the normal/session
                                                                          #   paths are untouched
scripts/battery-shard.sh tidepool-repl \
  XDG_CACHE_HOME="$PWD/.cache"                                          # THE ORACLE
```

**Oracle result: 198/198 passed, 0 failed, 0 skipped, 2269.1s (37m49s).**
The suite has grown from the `batch-turns-baseline.md` snapshot's 195/195
(3 more tests — plausibly the sibling lanes' own new test files, e.g.
`batch_turns_spawn_census.rs`, landing on this branch since that baseline
was recorded) but is fully green at its current count. Since this lane made
no code change, this run reconfirms the standing per-item path is intact —
it is not evidence FOR or AGAINST a batching mechanism, because none was
wired in.

## 9. Done-criteria check

- Assessed feasibility BEFORE writing implementation code, per step 1 of the
  lane brief — this document IS that assessment, and it decided against
  building. ✅
- No `tidepool-repl/src/session.rs`, `tidepool-runtime/src/session/turn.rs`,
  or `haskell/` file touched. ✅
- Per-item path is exactly as it was (nothing changed, so nothing to
  regress). ✅
- Baseline spawn-count census captured (§7) — no "after" number, per the
  finding. ✅
- `scripts/battery-shard.sh tidepool-repl`: **198/198 passed, 37m49s** —
  the lane brief's 195/195 bar, met at the suite's current size. ✅
- This receipt is the only doc this lane owns; `batch-turns-feasibility.md`
  and both sibling receipts are unmodified.
- `submit_branch` to follow, carrying this finding as the lane's landing.
