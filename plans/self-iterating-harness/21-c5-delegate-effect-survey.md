# PRD 21 C5 (delegation surface) — mechanism survey

STATUS: DECIDED. Survey done before implementation, per the spec's STEP 1.

## The question

A branch-node window must compile a model-authored block against a row whose
`Member` constraints structurally exclude `Worktree` and raw `Subagent` — not
merely omit them from the prompt — while the delegation verb it DOES expose
must still dispatch to the real `Subagent`+`Worktree` machinery already
wired in Rust. No new wire effect, no new Rust registry row, no new
suspension/classification class.

## The two things that must both be true, and why they look contradictory

1. **The model's own compiled text must NOT be able to spell a well-typed
   `send (WorktreeCreate ...)` / `send (SubagentSpawnAsync ...)`.** This is a
   `Member`-constraint failure, not "not in scope" — `tidepool-mcp`'s
   vocab/row split already documents this exact distinction
   (`tidepool-mcp/src/eval_prep.rs:114-162`,
   `effects_module_source_with_vocab`): a GADT constructor can be **in
   scope** (vocab) without being **in the row** (`type M`), and a `Member`
   constraint at the call site is then unsolved.
2. **Something in the SAME compiled module must be able to dispatch real
   `Subagent` sends**, because delegation ultimately rides
   `agentSpawnAsyncRaw`/`agentAwaitRaw`
   (`tidepool-mcp/src/effect_defs.rs:1975,1985` — their bodies are literally
   `send (SubagentSpawnAsync ...)` / `send (SubagentAwait ...)`), and `send`
   only type-checks when the target effect is a `Member` of the row the
   surrounding code is compiled against.

Both requirements are about **the same generated module** (`Tidepool.Effects`
+ the turn module are compiled together; `type M` is ONE type for the whole
compile — `tidepool-mcp/src/lib.rs:220-244`). So "narrow for the model, wide
for the dispatch" cannot be two different `type M`s picked by two different
compiles of the same turn — it has to be two different **local types inside
one compile**.

## The mechanism: freer-simple `reinterpret`, applied to a value, not a row

`Eff` is freer-simple's free monad and its constructors
(`Control.Monad.Freer.Internal.Eff(..)`) are already an import of the
generated `Tidepool.Effects` (`tidepool-mcp/src/eval_prep.rs:216-225`,
comment: "for scopes that INTERPOSE on the computation they enclose"). PRD
19 lane L4 already proved, empirically, on the live JIT, that a function
which pattern-matches `Eff`'s `Val`/`E` constructors and rebuilds the tree
runs correctly as ordinary compiled code
(`plans/post-restart/worktree-lanes/L4-mechanism.md` — `pumpEff`/`withHandler`,
four evals against the real JIT). That is the "same family" the settled
design names. freer-simple ships the row-CHANGING member of that family as a
library combinator, not just a same-row interposition:

```haskell
-- freer-simple, Control/Monad/Freer.hs
reinterpret :: forall f g effs. (f ~> Eff (g ': effs)) -> Eff (f ': effs) ~> Eff (g ': effs)
```

Given a private, Rust-invisible GADT `Delegate`:

```haskell
data Delegate a where
  DelegateRequest :: DelegateBrief -> Delegate (Either DelegateError DelegateResult)

delegate :: Member Delegate effs => DelegateBrief -> Eff effs (Either DelegateError DelegateResult)
delegate = send . DelegateRequest

runDelegate :: forall effs. Eff (Delegate ': effs) ~> Eff (Subagent ': effs)
runDelegate = reinterpret handleDelegate
  where
    handleDelegate :: forall x. Delegate x -> Eff (Subagent ': effs) x
    handleDelegate (DelegateRequest brief) = do
      started <- send (SubagentSpawnAsync (delegateSpawnSpec brief) delegateSchema)
      ...
```

`runDelegate`'s own type never names `effs` concretely — it is an ordinary,
fully polymorphic stdlib function, needing **zero per-turn code generation**.
Two consequences fall out for free:

- **`Delegate` is never "in the row."** It is not a `RowArgs`/`EffectDecl`
  entry, never touches `tidepool-mcp/src/effect_defs.rs`, never gets a
  Rust-side registry slot. It exists purely as a Haskell type between the
  model's compile and `runDelegate`'s own definition — exactly the settled
  design's "no new wire effect."
- **`handleDelegate`'s body needs only `Member Subagent (Subagent ': effs)`**,
  which holds structurally (head of the list) — no `Member Worktree`
  anywhere. Delegation never needs to `send` a `Worktree` GADT constructor
  at all: `Tidepool.Agent.Spawn`'s own `spawnSpec`/`spawnSpecIn` helpers
  (`tidepool-mcp/src/effect_defs.rs:1954-1959`) are **pure data
  construction** (`SpawnSpec (SpawnNewWorktree wspec) lbl task`) — worktree
  creation/binding happens *inside the Rust spawn saga* once `SubagentSpawn`/
  `SubagentBegin` is dispatched (`tidepool-agent/CLAUDE.md`'s
  `SpawnSubstrate` — it owns the `WorktreeManager` + `BindingTable`
  directly). Haskell only needs `WorktreeSpec`'s **type**, not the
  `Worktree` **effect** — `WorktreeSpec { specSource = SourceCurrentRepository,
  specLabel = ..., specDirtyPolicy = RequireClean }` is plain data
  (`tidepool-protocol/src/effects/worktree.rs:203-303`).

That last point is the resolution to what looked like the hard blocker: I
do NOT need `Worktree` in the ROW at all. I need its **type definitions**
(`WorktreeSpec`, `WorktreeSource`, `DirtyPolicy`, and transitively
`WorktreeError` off `SpawnError`'s own variants) nameable — i.e. in
**vocabulary**, not row. `tidepool-mcp` already has exactly this split
mechanism, already exercised by `EngineConfig::turn_target` for a DIFFERENT
purpose (`tidepool-harness/src/engine.rs:1317-1339`,
`vocab_with_runllmturn` — widens vocab with `RunLLMTurn` when it is not
already in the row, a documented no-op otherwise). The precedent is:
row-CLOSED helpers (the `worktreeCreate`-family, `helpers_row_polymorphic:
false` per `tidepool-protocol/src/effects/worktree.rs:108`) are simply
**not emitted** when an effect is vocab-only
(`tidepool-mcp/src/eval_prep.rs:142-156`, `emits_helpers_for`) — which is
fine, because `runDelegate` never calls them. Type declarations, by
contrast, emit for every VOCAB entry unconditionally
(`tidepool-mcp/src/eval_prep.rs:250-267` — the `type_defs`/GADT-constructor
loop iterates `effects` = the vocab-filtered list, not `row_effects`).

## What this means for the row a branch-node window compiles against

- **ROW** (`type M`, `Member`-satisfiable): `Subagent` prepended to today's
  answerer row — `'[Subagent, AskUser, Fork, ReadState, Finalize T]`. A
  NEW decls-list function in `tidepool-harness` (analogous to
  `answerer_decls()`, `tidepool-harness/src/selfharness/driver.rs:527`), not
  a change to that shared function (dev-tree and every other harness must
  stay byte-identical). `Subagent` is reused verbatim
  (`tidepool_mcp::subagent_decl()`) — no new registry row.
- **VOCAB** (nameable, not `Member`-satisfiable): ROW ∪ `Worktree` — a new
  `vocab_with_worktree` next to `vocab_with_runllmturn`
  (`tidepool-harness/src/engine.rs`), applied inside
  `EngineConfig::turn_target`'s existing `Finalize`-pinned branch
  (`tidepool-harness/src/engine.rs:1490-1509`) exactly where
  `vocab_with_runllmturn` already runs — guarded the same way (no-op unless
  `Subagent` is already in `self.decls`), so dev-tree's compiles are
  byte-identical to before.
- **`Delegate` itself is in neither list.** `Tidepool.Agent.Delegate`
  (`haskell/lib/`) is an ordinary auto-imported-when-Subagent-present stdlib
  module (Form/Spawn precedent — `extra_imports_for!`,
  `tidepool-mcp/CLAUDE.md`'s "If an effect's helpers need a companion
  Haskell import" section), gated on `Subagent`'s presence exactly like
  `Tidepool.Agent.Spawn` already is.

## The remaining piece: the model's OWN block must be the thing `runDelegate` wraps

`reinterpret`'s row-narrowing only protects code that is textually its
ARGUMENT. `harness.rs::run_block` embeds the model's raw fenced block
(`block`) directly into whichever template compiles
(`Decl`/`Bind`/`BindDiscard`/`Expr` — `tidepool-harness/src/harness.rs:1613-1642`,
via `engine::expr_turn_template`/`engine::session_bind_template`). If
`block`'s text shared `type M` (containing `Subagent`) UNWRAPPED, the model
could spell `send (SubagentSpawnAsync ...)` directly — `Member Subagent`
would solve, defeating the whole point.

So the harness, not the model, must inject the wrap: `block` becomes
`"Tidepool.Agent.Delegate.runDelegate $ do\n" <> block` before it reaches
`expr_turn_template`/`session_bind_template`, gated on the SAME
"is this a delegating row" flag (`EngineConfig` needs one new `bool` field,
computed once at construction from `decls.iter().any(Subagent)`). This is
**semantically a no-op for a block that never calls `delegate`** —
`reinterpret`'s `Val`-case and its pass-through of every other effect mean a
plain `finalize @T (...)` compiles exactly as it does today, just inside an
extra (invisible) `runDelegate $ do`. Verified by inspection against
freer-simple's `replaceRelay` (the primitive `reinterpret` is built from):
non-matching effects are re-sent unchanged, matching effects (`Finalize`,
`AskUser`, ...) are untouched since they are not `Delegate`. Only the
`Decl` template (pure, non-monadic top-level bindings — a session `let`)
is left unwrapped, since it is never `Eff`-typed at all.

This wrap is the one genuinely new piece of Rust: a text-prefix, gated by a
boolean already known at `EngineConfig` construction, threaded through
`run_block`'s existing template-building call sites. It adds **no new
suspension class** (nothing about `AskWith`/`FinalizeWith`/hole
classification changes — `Subagent`'s dispatch is the pre-existing
`SubagentHandler`, reached exactly as `Tidepool.Agent.Spawn` already reaches
it) and **no new registry row** (`subagent_decl()`/`worktree_decl()` both
already exist and are reused verbatim).

## Amendment (post-implementation): `Worktree` rides in the ROW, not vocab

The vocab/row split (above) turned out to be the wrong tool for `Worktree`
specifically — discovered empirically, not by further reasoning. `Subagent`'s
own auto-import (`extra_imports_for!(Subagent)`,
`tidepool-mcp/src/effect_defs.rs`) unconditionally pulls in
`Tidepool.Agent.Spawn`, which imports `Tidepool.Worktree
(renderWorktreeError)` — and GHC must fully typecheck `Tidepool.Worktree.hs`
as a WHOLE MODULE to import anything from it, including its own **`M`-typed**
bindings (`worktreeBranch :: WorktreeHandle -> M BranchName`, `worktreeHead ::
WorktreeHandle -> M GitOid`). Those two bindings need `Worktree` GENUINELY in
`type M`'s row — a vocabulary-only `Worktree` makes their own row-CLOSED
generated helpers (`worktreeBranchOf`/`worktreeHeadOf`) unemitted, so
`Tidepool.Worktree.hs` itself fails to compile, and the cascade
(`haskell/CLAUDE.md`'s Known Limits) reports it as "`Tidepool.Agent.Spawn`
... which is not loaded" everywhere downstream.

**Fix:** `Worktree` rides in the ROW for real — `selfharness::driver::
answerer_decls_with_delegate` is `[subagent_decl(), worktree_decl(),
askuser_decl(), fork_decl(), readstate_decl(), finalize_decl()]`, and
`Tidepool.Agent.Delegate.runDelegate` uses `reinterpret2` (not `reinterpret`)
to re-add BOTH `Subagent` and `Worktree` freshly on the OUTPUT side:

```haskell
runDelegate :: forall effs a. Eff (Delegate ': effs) a -> Eff (Subagent ': Worktree ': effs) a
runDelegate = reinterpret2 handleDelegate
```

Unnameability survives intact: `reinterpret2`'s signature adds `Subagent`
and `Worktree` to the OUTPUT only — they are never members of `effs`, the
tail the model's own block (`runDelegate`'s ARGUMENT) is checked against.
The handler's body never actually `send`s a `Worktree` constructor (worktree
creation/binding happens inside the Rust spawn saga once `SubagentSpawnAsync`
dispatches — `WorktreeSpec` is consumed as plain data, not sent as an
effect), so no `Member Worktree` obligation is ever discharged; `reinterpret2`
doesn't require one.

**Row shapes, before/after, verified against the real compiler**
(`tidepool-harness/tests/delegate_type_pinning.rs`, 5/5 green):

- Model's own block (`runDelegate`'s argument): `Eff '[Delegate, AskUser,
  Fork, ReadState, Finalize T] a` — `Member Worktree`/`Member Subagent` both
  UNSOLVED here (`raw_worktree_and_subagent_are_unnameable`).
- The whole turn module's `type M`: `Eff '[Subagent, Worktree, AskUser, Fork,
  ReadState, Finalize T] a` — genuinely carries both, needed for
  `Tidepool.Agent.Spawn`/`Tidepool.Worktree.hs` to compile at all and for
  `runDelegate`'s own definition to type-check
  (`worktree_verb_compiles_when_the_row_actually_carries_worktree` is the
  control proving the row is what refuses the narrow side, not a broken
  reference).

## Second amendment (post-implementation): the runtime gap `reinterpret2` hits

The TYPE-LEVEL mechanism above is fully verified — `delegate_type_pinning.rs`
(5/5 green) proves unnameability and that `delegate`-using code compiles.
Wiring the FULL runtime positive path (`delegate_positive_path.rs`) surfaced
a genuine, reproducible RUNTIME gap, isolated by direct comparison on the
identical row and driver wiring:

- A `send (SubagentSpawnAsync ...)` written DIRECTLY in a branch-node
  window's own turn text dispatches correctly — classified as
  `HoleRouting::Subagent`, serviced by the driver's `SubagentHandler`, the
  typed response crosses back. Green:
  `direct_subagent_send_dispatches_within_the_answerer_row`. This required
  one piece of NEW (but purely additive) plumbing this lane found missing:
  the nested-answerer's own turn-driving loop
  (`SelfHarnessDriver::drive_answerer_to_finalize` /
  `drain_note_holes`) had never served `HoleRouting::Subagent` before —
  only the AUTHORED OUTER loop's session had (`service_outer_subagent`,
  reached via a structurally different, lower-level session-resume path).
  `Harness` already retained the raw suspended request
  (`PendingHole.raw_request`, for `take_finalized_value`) but exposed no
  accessor for it; `pending_hole_with_request` (new, `pub(crate)`, mirrors
  the existing `pending_hole_full`) and `resume_with_value` (new,
  `pub(crate)`, a thin wrapper reusing the existing PRIVATE `resume_parent` —
  same as `answer_dialog`/`answer_note` already do, just without their
  `Ask`/`AskUser`/`ReadState`-only routing restriction) are the two additions;
  `drain_note_holes` gained one new match arm calling both, reusing
  `service_outer_subagent`'s existing dispatch unchanged. No new suspension
  CLASS (`HoleRouting::Subagent` already existed), no new registry, no new
  wire effect — reuse, confirmed by 1/1 green.
- The SAME send, performed from inside `Tidepool.Agent.Delegate.runDelegate`'s
  `reinterpret`/`reinterpret2` handler (i.e. via calling `delegate`), reaches
  the driver as an UNCLASSIFIED suspension —
  `HoleRouting::Ask { payload: Null }`, meaning `classify_hole`'s `con_name`
  lookup does not resolve `SubagentSpawnAsync`'s own constructor name for
  this specific compiled shape. Reproduced identically under PLAIN
  `reinterpret` (targeting only `Subagent`) and under `reinterpret2`
  (targeting `Subagent`+`Worktree`) — ruling out `reinterpret2`'s extra
  `Weakens`/`replaceRelayN` machinery as the specific culprit and pointing at
  `reinterpret`'s shared `replaceRelay` foundation (and, transitively,
  `Data.OpenUnion`'s `decomp`/`weaken`) more generally. `#[ignore]`d as
  `root_coalgebra_window_delegates_and_finalizes_on_the_result`, with the
  isolating comparison recorded in its doc comment.

PRD 21 C5's settled design names `reinterpret`'s FAMILY as proven on this JIT
("the same free-monad-interposition family `Tidepool.Event.withHandler` is
built from"), and that proof is real — but it covers ONLY the SAME-ROW
interposition member of the family (`pumped`/`withHandler`, walking `Eff`'s
`Val`/`E` constructors directly, verified end-to-end in
`plans/post-restart/worktree-lanes/L4-mechanism.md`). The ROW-CHANGING
member (`reinterpret`/`reinterpret2`, needed here specifically because
unnameability requires a FRESH effect on the output side rather than a
same-row `Member` constraint — see the first amendment above) has never been
exercised on this JIT before this lane, and this lane's testing is the first
evidence that it does not currently execute correctly when the
reinterpreted handler's own body performs a send that must reach a real
machine suspension (as opposed to completing purely, or passing an
unmatched effect through unchanged — both of which DO work, per
`plain_finalize_still_compiles_under_the_wrap`'s runtime behavior and this
same file's own passing tests).

Diagnosing the exact JIT-side root cause (most likely somewhere in how
`decomp`/`weaken`'s `Union`-tag manipulation compiles, or in how a
suspension's `DataConTable` entry is threaded through `replaceRelay`'s
`E u q` reconstruction) is `tidepool-codegen` work — outside this lane's
boundary, and genuinely a STOP-report condition per this lane's own
instructions ("STOP AND REPORT if every honest route requires NEW
Rust-side... machinery beyond reusing what exists"): the ONLY routes found
were (a) fix the JIT's support for freer-simple's row-changing combinators,
or (b) hand-roll a replacement re-interpretation primitive from scratch,
which would itself be new, unproven machinery of exactly the kind this
lane's mandate asks NOT to invent. Reported rather than worked around.

## Verdict

Every piece reuses existing, already-tested machinery: `reinterpret2`/`Eff`
interposition (same freer-simple family `withHandler` is built from),
`subagent_decl()`/`worktree_decl()` (unchanged, reused verbatim — no new
registry row), the existing per-turn `RowArgs`/`turn_target` materialization
path (unchanged in shape, just fed a different decls list + a text-prefix on
`block`), and — for the Subagent-within-the-answerer runtime wiring —
`HoleRouting::Subagent`'s pre-existing classification and
`service_outer_subagent`'s pre-existing dispatch, newly reached from the
nested-answerer's own driving loop via two small `pub(crate)` `Harness`
accessors. No STOP condition is hit on ARCHITECTURE: nothing here requires a
new suspension/classification CLASS, a new wire effect, or a new Rust
registry row.

Two genuine surprises, both caught by the compiler or the test suite, not
missed: `Worktree` needing the ROW rather than the vocabulary (because of
`Tidepool.Worktree.hs`'s own `M`-typed bindings — see the first amendment),
and — a real STOP condition, at the RUNTIME rather than the architecture
level — `reinterpret`/`reinterpret2`'s row-changing family not currently
executing correctly on this JIT when the reinterpreted handler's own body
performs a send reaching a real suspension (see the second amendment).
The compile-level unnameability proof is complete and green regardless;
the full runtime positive path is blocked on that JIT-level gap and is
reported, not worked around.

## Third amendment: the JIT-side mechanism (found, and fixed, by a
## separate `tidepool-codegen` lane)

The second amendment's gap turned out to be a genuine `tidepool-codegen`
execution bug, not an architecture question — diagnosed and fixed by the
`jit-reinterpret-rowchange` lane. Recorded here (not archived) because it
answers the exact question this amendment leaves open: WHERE the row-changing
`reinterpret` family actually breaks.

**Minimal repro, isolated from the Subagent/Worktree saga entirely.** A
two-line private effect (`Ping`), `reinterpret`ed onto the real `AskUser`
machinery (`send (NoteWith "ping")` inside the handler body) reproduces the
IDENTICAL misclassification (`HoleRouting::Ask { payload: Null }` instead of
`HoleRouting::Note { text: "ping" }`) with no Subagent, Worktree, or
MockBackend saga involved —
`tidepool-harness/tests/reinterpret_rowchange_repro.rs` +
`tidepool-harness/tests/fixtures/ReinterpretRepro.hs`. The isolating control
(the identical `send (NoteWith "ping")` written directly, no `reinterpret`)
classifies correctly on the same row.

**The divergence, pinned with `TIDEPOOL_DUMP_CLOSED` and
`RUST_LOG=tidepool::effects=debug` against the real extract/JIT:**

- `TIDEPOOL_DUMP_CLOSED=runPing`'s dump of the closed Core shows GHC fully
  specializing `reinterpret`/`replaceRelay`/`decomp`/`weaken`/`handlePing`
  into `runPing`'s own body — nothing here is left generic. The interesting
  shape is `decomp`'s own pattern, `case bx_aAJf of { 0## -> <matched>;
  __DEFAULT -> <weaken-and-passthrough> }`, where `bx_aAJf` is `Union`'s
  `{-# UNPACK #-} !Word` tag field, extracted via `Union @t1 bx a1 -> ...`
  (an existentially-typed Con pattern — the ONE shape genuinely new to this
  JIT: every previously-proven `Eff`/`Union` interposition,
  `Tidepool.Event.pumpEff`/`withHandler` included, walks `Eff`'s `Val`/`E`
  constructors directly and never destructures `Union`'s own fields via a
  compiled Core `case`).
- `RUST_LOG=tidepool::effects=debug` on the SAME repro shows the runtime
  actually dispatching: `effect_tag=0
  request=Con(DataConId(...), [Con(DataConId(...), [Lit(LitInt(5))])])` — the
  request is unambiguously `PingReq 5` (the ORIGINAL, un-transformed `Ping`
  payload), not `NoteWith "ping"`. This proves `decomp`'s `0## -> <matched>`
  arm never fired: the runtime took `__DEFAULT` (treat `PingReq`'s injection
  as "not the effect I'm looking for", weaken it through unchanged) even
  though the tag genuinely was `0` (Ping is the head of its own row, exactly
  like AskUser is the head of the row it's weakened back into — the same
  numeric coincidence is what let the misrouted request still read as
  `effect_tag=0` and get treated as a plausible-looking, if wrong,
  suspension instead of erroring outright).
- **Root cause: `bx_aAJf` reaches `decomp`'s literal-pattern match BOXED, not
  as the unboxed `Word#` the match assumes.** `ping`'s own `send . PingReq`
  is defined in a separate module (`ReinterpretRepro.hs`) and is NOT inlined
  at its call site in the turn module (the same un-inlined-generic-code
  phenomenon `Tidepool.Fork`'s module doc already names for a different
  function, confirmed again here via `TIDEPOOL_DUMP_CLOSED`) — so the `Word`
  tag `ping`'s own `Member Ping effs => ...`/`inj` produces is a genuine
  heap-allocated `W#` Con, not a literal. `tidepool-codegen`'s
  `CompiledEffectMachine::parse_result` (`tidepool-codegen/src/
  effect_machine.rs`) already had a fallback for exactly this "boxed W#"
  ambiguity when READING a suspended request's Union tag generically from
  Rust — but the COMPILED HASKELL CASE EXPRESSION for a literal alternative
  (`emit_lit_dispatch`, `tidepool-codegen/src/emit/case.rs`) had no
  equivalent tolerance: it unconditionally read `LIT_VALUE_OFFSET` (16) off
  whatever heap pointer the scrutinee happened to be. Fed a boxed `W#` Con
  instead of a bare `Lit`, offset 16 lands on the Con's `num_fields` header
  field, not a real Word value — a stray small integer that (almost) never
  equals the literal `0` the alt compares against, so `__DEFAULT` fires
  unconditionally. `emit_data_dispatch` (the sibling DataAlt dispatcher, a
  few lines above in the same file) already had the MIRROR-IMAGE tolerance
  ("Runtime Lit-tolerance": a bare `Lit` reaching a `case` that expects a
  boxed wrapper Con) — `emit_lit_dispatch` was simply missing its own
  direction of the same fix.

**The fix** (`tidepool-codegen/src/emit/case.rs`,
`tidepool-codegen/src/emit/primop.rs`): `emit_lit_dispatch`'s `HeapPtr`
branch now runs the scrutinee through `unwrap_boxing_chain` (widened from
`fn` to `pub(crate)`) — the SAME arity-guarded boxed-wrapper-Con-unwrap loop
`unbox_addr`/`unbox_bytearray` already use for primop operands — before
reading `LIT_VALUE_OFFSET`. A genuine bare `Lit` is unaffected (the chain is
zero-length for it, byte-identical to the old code path); a boxed `W#`/`I#`/
etc. now unwraps correctly, and a malformed multi-field wrapper still traps
loud (`BoxingArity`) rather than reading garbage. No representation change,
no new suspension class, no poison workaround — this is the SAME
"reconcile the ideal-unboxed-vs-actually-boxed literal representation"
mechanism already proven in three other places in this file
(`emit_data_dispatch`'s bare-Lit tolerance, `unbox_addr`, `unbox_bytearray`),
applied to the one case-dispatch shape that had never needed it before this
lane exercised `decomp`.

**Other row-changing freer-simple combinators — exercised or structurally
immune, per combinator (not fixed speculatively, since none of the others
are reached by the current stdlib surface):**

- `reinterpret`/`reinterpret2`/`reinterpretN` (built on `replaceRelay`/
  `replaceRelayN`) — the fixed combinator; `Tidepool.Agent.Delegate.
  runDelegate` (`reinterpret2`) and the minimal repro (`reinterpret`) both
  exercise the SAME `decomp`/`weaken` mechanism, now fixed identically for
  both (the gap was never specific to `reinterpret2`'s extra `Weakens`
  machinery — the second amendment already ruled that out, and this fix
  confirms why: the bug is in `decomp`'s own literal match, shared by every
  arity of `replaceRelay*`).
- `subsume` (`interpret send`) and `interpret`/`interpretWith` — also built
  on `decomp` (via `handleRelay`/`interposeWith`'s own `case decomp u' of`),
  so they inherit the SAME fix. Structurally immune to a DIFFERENT bug
  (never generic/un-inlined the way `ping`'s cross-module `send` was) is not
  claimed — the fix is at the mechanism `decomp` itself, so anything built on
  it benefits regardless of call-site specialization. Not separately
  exercised by this stdlib (no stdlib code currently calls `subsume`
  directly), so not separately tested here.
- `raise` (`Control.Monad.Freer.Internal.raise`) — built on `weaken` ALONE,
  never `decomp` (`loop (E u q) = E (weaken u) . tsingleton $ qComp q loop` —
  unconditional, no branch). Not exercised by this stdlib surface (nothing
  calls `raise`); if it ever is, it inherits the SAME fix trivially, since
  `weaken` never reads a literal tag at all (it only reads the Con's WORD
  field to increment it — a plain arithmetic read, not a literal-pattern
  match, so it was never subject to this specific bug either way).
- `translate` — reencodes via `qComp`/direct dispatch, not `decomp`/`weaken`
  (checked against the freer-simple source; it pattern-matches on the
  effect's OWN request type via ordinary `case`, not `Union`'s internals).
  Not exercised by this stdlib surface.

Gates: `tidepool-harness/tests/reinterpret_rowchange_repro.rs` (the minimal
repro + isolating control, both green) and
`root_coalgebra_window_delegates_and_finalizes_on_the_result`
(`delegate_positive_path.rs`, un-ignored, green — the full real-world
Subagent/Worktree/MockBackend path this amendment originally reported as
blocked).
