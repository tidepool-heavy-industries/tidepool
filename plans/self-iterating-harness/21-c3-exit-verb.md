# The exit verb — a forked window's abnormal exit is data at its branch position

**Status:** landed (2026-08-17). Closes
[the C3 slice's §8 gap 3](21-c3-recursive-companion-slice.md) against
[PRD 21](21-recursive-companion-prd.md) locked decision 6.

---

## 1. The surface

```haskell
runLLMTurn       :: Text   -> M T                            -- UNCHANGED
runLLMTurnFork   :: Text   -> M (Either InvocationExit T)
runLLMTurnFanout :: [Text] -> M [Either InvocationExit T]
runLLMTurnBranch :: ContextRef -> Text
                             -> M (Either InvocationExit (T, ContextRef))
freezeContext    :: M ContextRef                             -- UNCHANGED
```

Every verb that OPENS A WINDOW AT A BRANCH POSITION answers an `Either`; the
two that do not (`runLLMTurn`, answered in context; `freezeContext`, not a
window at all) keep their bare answers. §1's two subsections below are why.

Changed IN PLACE — no `try`-prefixed sibling, no policy knob. One spelling per
verb, `Either` where failure is data, which is the codebase's own typed-failure
idiom (`run :: Text -> M (Either ExecError Proc)`,
`llm :: … -> M (Either LlmError Value)`, #335). Natural spellings:
`Right x <- runLLMTurnFork @T p`, `mapM liftEither =<< runLLMTurnFanout @T ps`,
`renderInvocationExit e` to display one.

`@T` still pins the CHILD's answer type — it is the first forall'd tyvar — so
call sites read exactly as before, and the site's recorded `asks.json` type
stays `T` (`[T]` for the fanout). That type is what the driver derives the
child's `Finalize T` row pin from, and the child's own finalize contract is
untouched: the `Either` is what the PARENT receives, not what the child
answers. Nothing in `Translate.hs` changed.

### Why `runLLMTurn` keeps its bare answer

The asymmetry is the point, and it is documented at the declaration
(`tidepool-mcp/src/effect_defs.rs`, `runllmturn_effect_def!`):

- a fork/fanout child is a BRANCH POSITION — its window is a separate node
  with siblings, and an exception there erases results those siblings already
  produced. Locked decision 6 requires the exit to fold as data at that
  position, and the caller folding it there IS the design, so the type hands
  it to them;
- `runLLMTurn @T` is answered IN CONTEXT by the same node, on the outer turn's
  own continuation. It has no siblings and no branch position. Its failure IS
  the outer turn's failure. Wrapping it would make every in-context call site
  unwrap an `Either` whose `Left` means "the turn you are in has already
  failed".

### The third verb: `runLLMTurnBranch`

`runLLMTurnBranch @T ref prompt` (the gap-1 verb, landed in parallel) opens a
child window at a branch position too, so it takes the same treatment:

```haskell
runLLMTurnBranch :: ContextRef -> Text -> M (Either InvocationExit (T, ContextRef))
freezeContext    :: M ContextRef                      -- UNCHANGED
```

The `Either` wraps the WHOLE pair rather than only the answer
(`(Either InvocationExit T, ContextRef)` is the other spellable shape): a
window that never finalized has no post-finalize prefix to freeze, so a
`ContextRef` beside a failure would be a capability with nothing behind it.

`freezeContext` is not a window — it resolves immediately, no model round, no
operator — so it has no exit to report and keeps its bare answer.

It reaches the exit through a DIFFERENT path than fork/fanout do:
`service_outer_branch` is sequential (`&mut self`, one suspend/resume
round-trip per call) and drives its child through
`drive_answerer_to_finalize`, the round loop it SHARES with the in-context
`service_runllm_hole`. So that loop now returns
`Result<Result<TurnOutcome, InvocationExit>, DriverError>` — the same
mechanism/window nesting — and the two callers differ in what they do with an
exit, which is correct because the difference is exactly whether the window
sits at a branch position:

- `service_outer_branch` folds it as `Left` at the branch;
- `service_runllm_hole` collapses it back into a hard failure, unchanged from
  before this plumbing existed.

The mechanism line holds identically: `resolve_context_ref` refusing an
unknown or stale ref is a capability that was never valid, not a window that
failed, and it still hard-fails — as does a branch child that finalizes a
closure.

## 2. `InvocationExit`

Generated into `Tidepool.Effects` alongside the `RunLLMTurn` GADT
(`runllmturn_effect_def!`'s `type_defs`), the way `ExecError`/`FsError` are.
Hand-written rather than produced by the `errors` block because `RunLLMTurn`
has no Rust handler projection to generate an enum for.

```haskell
data InvocationExit
  = ExitRoundsExhausted Text
  | ExitNotFinalized    Text
  | ExitCancelled       Text
  | ExitRuntimeFailure  Text
  deriving (Show, Eq)

renderInvocationExit :: InvocationExit -> Text
```

Exactly decision 6's four classes — round exhaustion, non-finalization,
cancellation, runtime failure — each carrying the runtime's own detail text.
`deriving Show` makes `liftEither` work on the result; a `ToJSON` instance
makes an exit journalable.

What each means:

| Constructor | The window… | Produced today by |
|---|---|---|
| `ExitRoundsExhausted` | burned its round budget without finalizing | `drive_fanout_child_inner` and `drive_answerer_to_finalize`, at `rounds >= hard_rounds` |
| `ExitNotFinalized` | ended on something that is not an answer (a nested `askUser`/`note`/`fork` the concurrent path does not service; or a branch child whose loop ended on a non-`finalize` outcome) | `drive_fanout_child_inner`'s non-`finalize` suspension arm, and `service_outer_branch`'s own `is_finalize` check |
| `ExitRuntimeFailure` | had its own provider call fail | `HarnessError::Engine(EngineError::Provider(_))` |
| `ExitCancelled` | was cancelled before it could answer | **nothing yet** — cancelling a live branch is lane C5 (draining a recursive scope under structured concurrency). The constructor exists because decision 6 enumerates it and an exhaustive matcher should not have to be rewritten when C5 lands. |

**The constructors are reachable by construction, not by luck.** A
fork/fanout/branch call site head-swaps to a `*Sited` sibling whose own
top-level type mentions `Either InvocationExit a` — and
`collectTransitiveDCons` seeds its closure from the types of reachable
top-level binders. So any program that HAS such a site carries `Left`,
`Right`, and all four `Exit*` constructors in its `DataConTable`, which is
what the Rust side needs to build the answer.

### One real constraint: an exit cannot be a session VALUE BIND

`InvocationExit` lives in the per-fragment generated `Tidepool.Effects`, so
the cross-row bind guard (`Main.mkBoundBinders` →
`Translate.typeMentionsEffectMonad`) refuses
`steps <- runLLMTurnFork @[Int] "…"` as a cross-turn session bind: a later
turn gets its own `Tidepool.Effects`, so a value naming one cannot mean
anything there. Project to a pure value AT the bind —
`steps <- either (\_ -> []) id <$> runLLMTurnFork @[Int] "…"` — which still
suspends at the fork and still materializes on resume. The same rule already
applied to `Schema`; nothing was carved out for this type.
Pinned by `tests/acceptance_value_bind.rs`.

## 3. The line that matters: child-attributable vs mechanism

Stated at `SelfHarnessDriver::service_outer_fanout` and
`drive_fanout_child_inner`, and it is the whole content of the change:

**A failure ATTRIBUTABLE TO ONE CHILD'S WINDOW becomes a typed exit** — its
rounds ran out, it ended on something that is not an answer, its own provider
call failed. It folds as `Left exit` at that child's branch position and its
siblings are untouched.

**A failure of the MECHANISM still hard-fails the turn** — fan cardinality
mismatch, `Either`/list assembly against the `DataConTable`, session
bookkeeping, the per-loop inference-call runaway cap, and a child that
finalized a CLOSURE. Reporting a broken mechanism as "the model failed" would
put a false receipt in front of the operator.

The closure case is the one worth reading twice, because it is
child-attributable in the shallow sense and still hard-fails: that window DID
answer, and it is this driver that cannot carry a closure across the fanout
join (v1 scope). The gap is ours, so it fails as ours. The criterion is
therefore not "which child" but: **the window ended without an answer → typed
exit; the driver could not carry an answer it has → mechanism.**

## 4. What the Rust plumbing does

`tidepool-harness/src/engine.rs`:

- `InvocationExit` — the Rust mirror of the ADT; `constructor()` is the one
  place the variant ↔ Haskell-constructor correspondence is spelled.
- `build_invocation_exit_value` / `build_child_answer_value` — build the
  `Left`/`Right`/`Exit*` `Value`s against the turn's own `DataConTable`,
  following `build_list_value`'s loud-failure discipline: a constructor the
  table does not carry is a HARD error, never a defaulted or omitted value.
  (Resuming with some other constructor would feed the parent's `case` a value
  of the wrong shape, which case-traps far from the cause.)
- `ForkSource` — a new field on `HoleRouting::Fork`. Two effects share that
  routing and they DISAGREE on the shape their parked continuation expects:
  `Tidepool.Fork`'s `fork`/`forkAll` still resume with a bare `T`/`[T]`,
  `runLLMTurnFork`/`runLLMTurnFanout` resume with the `Either`. `ty`/`fan`
  cannot tell them apart (both record the child's answer type identically),
  so the discriminator is carried rather than re-derived.

`tidepool-harness/src/selfharness/driver.rs` (the concurrent OUTER path, the
one PRD 21 needs):

- `drive_fanout_child` / `drive_fanout_child_inner` return
  `Result<Result<Value, InvocationExit>, DriverError>`. The nesting IS the
  contract: outer = mechanism, inner = this child's window. The node is
  retired either way — a child that exits without an answer still releases its
  realm and scope.
- `service_outer_fanout` no longer propagates a child's failure with `?`. It
  assembles per-child `Either` values in DECLARATION order (unchanged
  completion-order insensitivity), then builds the list (fanout) or takes the
  single value (fork). The `?` that remains is only ever reached by a
  mechanism failure.

`tidepool-harness/src/harness.rs` (the nested/general-Agent path):

- `wrap_fork_answer` puts one child answer into the shape the parked
  continuation expects, keyed on `ForkSource`. `answer_fork`/`answer_fanout`
  wrap a `runLLMTurn`-sourced answer in `Right`; a `Fork`-effect answer passes
  through untouched.
- Nothing on that path produces a `Left`. A child that fails there still
  hard-fails the fan — the escalation ladder in `drive_answerer_to_value`
  (auto corrective retry, then an operator popup) owns that policy, and
  turning its outcome into a typed exit is a separate decision. The `Either`
  is honest about what CAN arrive; this path simply has not been taught to
  attribute yet.

## 5. Acceptance

`tidepool-harness/tests/outer_fanout.rs` (the existing family bundle, one
fixture, one compile shape — no new extract compile):

`outer_fanout_round_exhausted_child_folds_as_data_without_erasing_siblings` —
nine-wide fanout, `FANOUT-4` (the MIDDLE, so position preservation is checked
on both sides) is given a reply with no ```haskell block, so every round is a
`NoBlock` re-prompt and its budget is spent without it ever running anything;
round caps lowered to 1/2 so that is four instant provider calls and zero
compiles. Asserts the outer turn COMPLETES, that `answers == [0,1,2,3,5,6,7,8]`
(all eight siblings arrived), and that position 4 of the per-branch `outcomes`
carries the rendered `ExitRoundsExhausted` while the other eight carry their
own answers at their own positions.

`fixtures/ConcurrentFanoutHarness.hs` keeps both projections in its state —
`answers` (the answers that arrived) and `outcomes` (one entry per BRANCH
POSITION) — so the bundle's other two tests read unchanged and the new one has
something position-shaped to assert on.

`KeyedProvider` now matches its needle across the WHOLE transcript rather than
the last message: the driver's own nudge and ultimatum turns carry no needle,
and each child is a fresh node whose transcript contains exactly one, so the
scan stays unambiguous.

`tidepool-harness/tests/companion_context_ref.rs` gets the branch-verb
counterpart on ITS existing fixture —
`branch_child_that_exhausts_its_rounds_folds_as_data_without_erasing_its_sibling`:
branch A is starved of rounds while branch B finalizes; the turn completes,
branch B's answer and ROOT's own trailing re-read both survive, branch A's
position carries the typed exit, and BOTH children still write a
`BranchInvocation` receipt (an exiting window is one that failed to ANSWER,
not one that was never opened). `ContextRefHarness.hs` keeps the same
`answers`/`outcomes` pair the fanout fixture does, and deliberately does NOT
bind `Right (a, _) <- …` — a refutable bind would turn a branch exit back into
an abort.
