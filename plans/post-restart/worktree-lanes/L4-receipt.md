# L4 receipt — authored surface + the `withHandler` interpreter

STATUS: IN PROGRESS. The mechanism decision and the scaffold are landed; the
handler modules and the acceptance harness are in flight in two forked lanes.
Sections marked TODO are filled at convergence.

## 1. The mechanism decision

Full write-up, verification record, and rejected alternatives:
[`L4-mechanism.md`](L4-mechanism.md). Summary:

**Decided: `withHandler` is a scoped INTERPOSITION over the body's freer-simple
structure.** `Eff` is freer-simple's free monad, so the scope walks the
computation it encloses and runs a drain before every effect that computation
performs. The author's handler closure is applied by ordinary Haskell
application, inside the resident's own continuation. No closure crosses to
Rust, none is rooted by Rust, no second continuation is created per invocation,
and **`tidepool-codegen` is untouched by this lane** — so no root notification
for a codegen change was needed, because there is no codegen change.

Both scaffolding findings were verified independently rather than taken on
faith, and both hold:

1. `run_fragment_suspendable_parked` takes a `FuncId` and there is no public
   closure-apply on the parked path — confirmed by enumerating every public run
   entry on `JitEffectMachine`. Additionally, and not previously noted: the
   fragment entries are **nullary**, so even a hand-compiled dispatcher `FuncId`
   could not be handed an observation as an argument. That kills the
   dispatcher-table alternative on a hard fact rather than on preference.
2. No production consumer of the parked path exists — a repo-wide grep finds
   hits in exactly six files, all `tidepool-codegen/tests/realm_*.rs`. This
   lane's acceptance harness is the seventh caller and the first outside
   `tidepool-codegen`.

Rejected: growing `tidepool-codegen` with a closure-application entry point
(pre-authorized, and genuinely not needed — it would add a lifetime-long GC
rooting obligation on the hardest surface in the system to buy nothing);
a top-level dispatcher `FuncId` reading from a closure table (nullary fragments,
plus the same rooting obligation); driver-resumes-the-resident-into-the-handler
(would require every effect's answer type to become a sum carrying a possible
handler invocation — a token threaded through the whole row, which is exactly
the DSL-shrinking move the design stance forbids).

### Verification record

Four probes against the live JIT, run BEFORE anything was built on the answer:

| Probe | Result |
|---|---|
| Walk over a 2-effect body | tick fired exactly twice, body returned `7` — the existential GADT match over `Eff` and the type-aligned `Arrs` re-queue both compile and run |
| Nested scopes, effectful inner tick | `B B A B B B A B` — outer scope interleaves around the inner scope's own effects, i.e. exactly "separate handlers interleave only at suspension points" |
| Failing handler | eval aborted with the handler's error; the statement after `withHandler` never ran |
| Handler containing a real `ask` | genuine suspend, genuine resume, handler continued past its own suspension, body completed |

The fourth is the load-bearing one: "the handler may itself suspend" was the
semantic most likely to force a runtime change, and it costs nothing here
precisely because there is no separate handler continuation to reconcile.

## 2. What landed

Commit `0630b6bd` — scaffold:

- `worktree_effect_def!` and `event_effect_def!` (`tidepool-mcp/src/effect_defs.rs`)
- their decl projections (`tidepool-mcp/src/effect_decls.rs`)
- PRD 19 wire types (`tidepool-bridge-effects/src/lib.rs`)
- `Control.Monad.Freer.Internal` in the generated module's imports
  (`tidepool-mcp/src/eval_prep.rs`)

TODO: handler modules, acceptance harness, per-binary test counts.

## 3. Test name proving each `withHandler` semantic

Every semantic below is a ONE-FAILURE-MODE GATE, so per root's standing receipt
rule each is its own named test with its own pass line — an aggregate count is
not sufficient evidence. The reason is concrete and worth restating where the
evidence lives: a rename, an `#[ignore]`, a `cfg`, or an env-gated early return
leaves an aggregate green while the guard never executed. "N/N passed" cannot
distinguish "the guard ran and held" from "the guard silently stopped
existing"; only a named pass line can.

Pass lines are produced with
`cargo nextest run -E 'test(=name_a) + test(=name_b) + ...'` and reported here
verbatim, together with the BASE COMMIT the run happened on — base proves the
tree, name proves execution, and neither establishes anything alone.

All seven PASS. Instrument: `cargo nextest run -p tidepool-handlers
--ignore-default-filter -E 'test(=a) + test(=b) + …'` via
`/home/inanna/dev/tidepool/scripts/ghc-slots.sh detach --`; nextest run id
`f50c9eda-d005-4479-82d8-81f645d80385`; binary
`tidepool-handlers::repo_event_with_handler`. **Base commit `2a5e592d`.**
Timings and verdicts below are nextest's own lines, not a wall clock.

| Semantic | Test | Status |
|---|---|---|
| A subscription never replays rows older than itself | `no_replay_of_events_observed_before_registration` | PASS 9.290s |
| Events broadcast to all registered handlers | `one_commit_broadcasts_to_both_registered_handlers` | PASS 9.215s |
| One handler at a time; later matches queue in observation order | `queued_observations_invoke_in_order_across_a_handler_suspension` | PASS 9.314s |
| Lexical drain, then unregister, at body end | `body_end_drains_before_it_unregisters` | PASS 9.478s |
| Handler failure fails the enclosing scope | `handler_failure_fails_the_enclosing_scope` | PASS 9.213s |
| Bounded-queue overflow fails loudly | `bounded_queue_overflow_fails_loudly_rather_than_dropping_a_commit` | PASS 9.290s |
| Rooting receipt `stowed_roots_count() == parked_count()` | `rooting_receipt_holds_across_an_interleaved_park_and_resume` | PASS 16.633s |

Every one drives real GHC → extract → JIT → the parked path, with every
repository transition produced by `ScriptedWriter` against a real `TestRepo`.
No mock of git, no LLM, anywhere.

**COMPLETED vs SELECTED — checked, not assumed.** There is a live inherited red
in `tidepool-runtime` (`mock_stack_matches_production`), and under nextest's
DEFAULT fail-fast a run reaching that crate stops early while still emitting
real pass lines for everything it did reach. Such a receipt looks exactly like
a complete run; the only tell is completed-vs-total.

This run was NOT truncated, and its own output proves it: nextest printed
`Starting 7 tests`, numbered them `(1/7)`…`(7/7)`, and summarised
`7 tests run: 7 passed, 124 skipped`. **Started = run = selected = 7.** The
general check is started-vs-run — a fail-fast truncation makes run strictly
less than started, and it is visible in output one already has. The handler
lane's gates pass the same check: `Starting 19 tests` → `19 tests run: 19
passed`.

A targeted `-E` filter additionally cannot reach `tidepool-runtime` at all, so
these gates were never exposed to the hazard. That makes the batched-`-E`
technique robust under it: ONE invocation gives one slot (envelope rule), N
named pass lines (named-gate rule), and an explicit N to compare completed
against (this advisory) — three independent reasons, only one of which was
known when the technique was adopted.

**Instruments, per the name-the-instrument rule.** The rooting receipt uses the
machine's own in-code counters `stowed_roots_count()` / `parked_count()`,
asserted equal at every quiescent point plus `!is_suspended()`; gate 7 also
asserts EXACT counts, because `1 == 1` and `0 == 0` are different facts and an
equality that only ever sees zero proves nothing. Id non-reuse uses
`Session::ids_seen`, an in-code record of every id `ParkedOutcome::Suspended`
returned (3 before dedup, 3 after). Registry units:
`cargo nextest run -p tidepool-handlers --ignore-default-filter --lib -E
'test(event::tests)'` → 16 passed. No external process observation anywhere.

**Wrong-reason guards, built in rather than assumed.** A no-replay gate passes
both when replay is correctly suppressed AND when nothing was ever published;
a broadcast gate passes both when both handlers fire AND when the assertion is
too weak to notice one missing. Both are ruled out in-gate: a `Recording`
decorator around the source asserts the emitted log equals
`[gap, during_first, during_second]`, so the runtime demonstrably observed all
three, and broadcast asserts the exact sorted vector `["A:<sha>", "B:<sha>"]`
rather than a contains-check, so a dropped OR doubled handler fails.

**The no-replay gate tests the CURRENT rule, not the superseded one.** It no
longer commits-before-registering and asserts absence — under root's
cycle-shape update that would test the wrong rule, since gap-window movement
MUST be delivered. It uses two SEQUENTIAL `withHandler` scopes (the
re-registration path) and pins both directions at once: no replay of rows older
than a subscription, and no loss of movement that happened while none existed.

The rooting receipt gets BOTH treatments deliberately: it is asserted at every
quiescent point inside every acceptance test (cheap, and catches drift wherever
it happens), AND it has one dedicated named test whose whole purpose is that
equality under a deliberate park/resume interleaving. The per-test assertions
alone are exactly the buried-assertion shape this rule cannot certify.

Base commit for the runs: TODO.

### `UnwiredWorktreeRow` — confirmed test-only, checked rather than asserted

The acceptance harness's row must contain the Worktree effect, because the
frozen `RepoEvent` helpers are written against `WorktreeId` / `WorktreeHandle` /
`renderGitOid`. Rather than create `handlers/worktree.rs` (a sibling lane's
file), the harness declares a local `UnwiredWorktreeRow` that PANICS if a
worktree verb is ever dispatched — none is. A panicking stub that escaped into
a production row would be the workaround-indistinguishable-from-an-invariant
hazard in its most literal form, so both conditions were verified:

- **Genuinely test-only.** It exists solely in
  `tidepool-handlers/tests/repo_event_with_handler.rs` — an integration TEST
  TARGET, so it is not compiled into the library at all. That is a stronger
  guarantee than `#[cfg(test)]`, which still lives inside the lib's source.
- **Its panic reads as scaffolding, not as an invariant.** The message is
  "a Worktree verb was dispatched: this harness owns the RepoEvent effect only,
  and its worktree handles are plain data" — it names the harness and states
  the scope, so a reader who hits it learns it is a test-row limitation rather
  than a runtime rule they have violated.

## 3a. Open obligations this lane hands forward

Two items outlive this receipt. Both are carried at their CODE SITES as well as
here, because an obligation that lives only in a receipt is discharged only by
someone who happens to re-read the receipt.

**1. ROUTED, not done — the `(<|>)` relocation.** boot-vocab never folded to
this branch: `emits_helpers_for` and `effects_module_source_with_vocab` are both
ABSENT here (checked). So the conditional cannot be written against the function
it must call, and the collision is left **OPEN and DOCUMENTED** rather than
half-fixed.

The mis-scoped `eval_prep.rs` term was **DELETED** rather than shipped. Its
licence (`generated_module_compiles_without_its_own_hiding`) is green, and
shipping it would have landed a term that LOOKS like the fix and is not — the
workaround-indistinguishable-from-an-invariant hazard, in a shared file, where
the next reader would conclude the collision was handled and be wrong. An
honest absence beats a misleading presence. `eval_prep.rs` is byte-identical to
its pre-change state apart from the `Control.Monad.Freer.Internal` import, which
the pump genuinely needs.

What the successor inherits, complete: the verified patch
(`verified/L4-hiding-term-retarget.patch`), its gate file
(`verified/prd19_vocab_predicate.rs`), the GHC gate script
(`scripts/prd19-alternative-gates.sh`), the full reasoning (§3b), and a
fast-tier gate `the_alternative_collision_is_open_and_the_generated_module_is_unchanged`
that FAILS when the routed fix lands, so whoever lands it is told to retire it.

*(superseded framing:)* Blocked on
extract-wave folding boot-vocab to the shared base. The term is already
VERIFIED (four gates at `d6fce023`, patch and gate file in `verified/`), so
what remains is landing, not deciding. It must CALL boot-vocab's
`emits_helpers_for(eff, row_effects)` — never a restatement, never a local
copy, whatever the spelling. Carried at the call site in
`tidepool-mcp/src/eval_prep.rs`, where whoever performs the move will meet it.

**2. DISCHARGED — the `MonitorObservations` workarounds are removed.**
`wt-seam` landed both fixes (`reconcile` now returns
`Result<Vec<Observed<RepositoryEvent>>, WorktreeError>`; `monitor.rs` contains
zero `panic!`), verified by reading the merged code rather than the report.
All four sites are gone — `grep -rn 'WORKAROUND(wt-seam)'` returns **zero** —
along with the `registered` field, the membership check, the `next_event_id`
counter, and the id-minting. An unregistered id is now a typed monitor failure
mapped like every other one, which is what the authored surface always
declared, and the `EventId` reaching Haskell is the JOURNALLED one, so
`Observed.eventId` correlates with its journal row.

Nothing was kept as belt-and-braces. The delete-when conditions made the
removal mechanical rather than archaeological: each named exactly what to drop
and what to keep (`register` itself is real wiring, not part of the
workaround), and each stated "no test pins this field's behaviour; nothing goes
with it" — which removed a blind search for tests that would otherwise have had
to come out too.

**The seven `withHandler` gates were RE-RUN against the changed adapter, and
the first re-run was a FALSE GREEN.**

That first attempt reported 23/23 PASS with started == run and no truncation —
every structural check we have said clean. It was vacuous: the harness
early-returned and PASSED when `TIDEPOOL_EXTRACT` was unset, and it was unset.
A skip spelled as a pass is structurally IDENTICAL to a pass — it runs by name,
emits a real PASS line, and counts toward started-vs-run — so no amount of
layering on "did the gate run by name" can see it.

**The only thing that exposed it was DURATION**: 0.006–0.011s per gate against
a 9–16s baseline, for work that drives real GHC → extract → JIT → a temp git
repository. That baseline existed only because the event lane had REPORTED ITS
TIMINGS under the name-the-instrument rule. Duration-versus-work-claimed is now
doctrine (`GATES.md`), and the corollary is: report your durations, because the
next person's baseline is your reported timing.

Fixed at the source rather than worked around: all seven sites now call
`require_ghc()`, which FAILS LOUDLY naming `TIDEPOOL_EXTRACT`. The root
`CLAUDE.md` already required exactly that, so those sites were NONCONFORMING
rather than a competing convention — the expensive kind of defect, because
everyone assumes the rule is being followed.

Re-run properly, with `TIDEPOOL_EXTRACT` and the with-packages GHC set
explicitly, `--no-fail-fast`, **started 7 == run 7**:

```
PASS [3.655s] (1/7) body_end_drains_before_it_unregisters
PASS [3.917s] (2/7) bounded_queue_overflow_fails_loudly_rather_than_dropping_a_commit
PASS [3.599s] (3/7) handler_failure_fails_the_enclosing_scope
PASS [3.742s] (4/7) no_replay_of_events_observed_before_registration
PASS [3.558s] (5/7) one_commit_broadcasts_to_both_registered_handlers
PASS [3.964s] (6/7) queued_observations_invoke_in_order_across_a_handler_suspension
PASS [6.257s] (7/7) rooting_receipt_holds_across_an_interleaved_park_and_resume
Summary [28.698s] 7 tests run: 7 passed, 143 skipped
```

Durations are plausible for the work claimed (faster than the 9–16s originals
on a warm extract cache; ~500x the vacuous 6ms). A prior green carries evidence
about the code that produced it and nothing else, which is why this re-run
happened at all. A
prior green carries evidence about the code that produced it and nothing else;
treating it as still valid after changing the thing under test is the
receipt-that-looks-like-a-run hazard one level up. The change looked mechanical
and was re-verified anyway.

*(Original text of this obligation, for the record:)*
**Remove the two `MonitorObservations` workarounds once `wt-seam` lands.**
They exist only because the freshly-landed L3 monitor panics on an unregistered
worktree id (`monitor.rs:273`) instead of returning the typed `EventSourceLost`
the surface declares, and because `reconcile` returns no `EventId`, so
`Observed.eventId` cannot be correlated with its journal row. Both are being
fixed in `tidepool-worktree` by the `wt-seam` lane; the id fix wires up
`Observed<T>`, which already exists in `monitor.rs`, is exported, and is
entirely unused.

Both sites are marked with the greppable token `WORKAROUND(wt-seam)` carrying
the defect, the condition that makes the workaround dead, and an explicit note
that it is another crate's defect rather than an invariant of this adapter —
`grep -rn 'WORKAROUND(wt-seam)'` finds them all.

Why the marking rather than a receipt line: **a workaround that outlives its
defect is indistinguishable from a real invariant to the next reader, and will
be defended as one.** Someone finds the registered-id check, assumes it guards
something live, and builds on it. Belt-and-braces is not a safe default here;
it is how a dead guard becomes load-bearing.

## 3b. The predicate story, end to end — for whoever touches this next

Written as reasoning rather than conclusion, because the next person to touch
the `(<|>)` conditional will be reasoning from a relay exactly as two of us
did, and **every failure mode in this story is silent** — no build error, no
failing test, just a wrong preamble.

**1. The collision.** `Tidepool.Prelude` re-exports `Control.Applicative`'s
`(<|>)`. The frozen `Tidepool.Event` requires `Tidepool.Effects` to export its
own `Event`-merge `(<|>)`. Both unqualified is an ambiguous occurrence, which
GHC resolves at NAME RESOLUTION — before typechecking — so the operators'
different types (`Event a -> Event a -> Event a` vs
`Alternative f => f a -> f a -> f a`) can never disambiguate it. It fires on
PRD 19's own worked example.

**2. Why it is bigger than it first looked.** `Tidepool.Effects` has NO export
list, so it re-exports its own generated `(<|>)`, and the eval preamble imports
both it and `Tidepool.Prelude` BY DEFAULT. So the collision is in the DEFAULT
EVAL VOCABULARY — an author need not `import Tidepool.Event` at all. The first
reproduction imported it explicitly and therefore understated the radius.

**3. Why the DSL was not changed.** The available in-lane fixes were renaming
the operator or giving `Event` an `Applicative`/`Alternative` instance.
`Tidepool.Event` is frozen; the PRD says the operator "need not fake a general
`Applicative`"; and faking one would be dishonest in the type system, since
`Event` has no sensible `pure`. So the runtime grew and the DSL did not — the
design stance applied to its own question.

**4. `vocab_effects` was the WRONG predicate, and the reversal is the lesson.**
After boot-vocab split the generator into
`effects_module_source_with_vocab(row_effects, vocab_effects, row)`, "the row
contains RepoEvent" became two predicates. The first answer — key on
`vocab_effects`, since `(<|>)` is a helper and helpers follow the vocabulary —
was reasoned from the function's SIGNATURE, relayed. It was endorsed and
recorded, and it was wrong.

Reading the committed CODE shows helper emission is ROW-GATED:
`if !(in_row || eff.helpers_row_polymorphic) { continue; }`. A row-CLOSED
helper (`foo :: A -> M B`) only typechecks when its effect is in the row, so a
vocabulary-only effect's helpers are emitted only if it declares itself
row-polymorphic. `(<|>)` is a row-closed RepoEvent helper. **So the predicate
is the helper-emission condition itself**, which for RepoEvent as declared
reduces to the row.

The PRINCIPLE was right throughout — *the hiding must track the operator's
PRESENCE, not the program's capability*. The ANSWER was wrong because presence
turned out to be row-gated. **A relay can be accurate about a function's shape
and silent about the thing that decides the answer.** Read the committed
function; do not read about it.

Had `vocab_effects` shipped, a vocabulary-only RepoEvent would have hidden the
Prelude's `(<|>)` while emitting no replacement — costing `Alternative` for
nothing, silently.

**5. Only ONE mismatched pair exists.** `vocab_effects` must be a SUPERSET of
`row_effects`, enforced by an assert, so "row-with / vocab-without" panics and
cannot be constructed. The single constructible mismatch is vocab-only, where
the correct behaviour is NOT to hide. That is the discriminating gate; a gate
built from the old single-list entry point passes under EITHER predicate and is
therefore no evidence at all.

**6. Why the fix had to move AUTHOR-SIDE.** The conditional first landed on the
Prelude import inside the generated `Tidepool.Effects`. That governs name
resolution within that module's body and NOTHING ELSE — an author's module
issues its own `import Tidepool.Prelude`. The fix was correct-looking and
mis-scoped, and it went to `eval_import_lines` (`preamble.rs:61`) and the
Orchestrate module (`:252`) instead.

**The tell was in hand and unread for hours.** The typecheck probe compiled
only because `hiding ((<|>))` was written BY HAND, in the probe — author-side.
That the workaround had to go author-side WAS the evidence that the fix
belonged author-side. It was recorded as a caveat and stepped past.
**A workaround you write is evidence about where the real fix belongs.**

**7. Single-source discipline — derive, don't declare.** The conditional CALLS
boot-vocab's `emits_helpers_for(eff, row_effects)`. Never a restatement, never
a local copy, never a "keep in sync" comment. Two copies agree today and
diverge the moment RepoEvent becomes `helpers_row_polymorphic` — which is
exactly what the vocabulary-without-row story exists to enable — and the
divergence produces a wrong preamble rather than a build error. One source of
truth makes the disagreeing case unconstructible rather than a responsibility
to discharge; the parking contract imposes the same discipline on handled
prefixes for the same reason.

Two lanes converged INDEPENDENTLY on extracting that shared predicate — this
one from "the hiding must fire exactly when the loop emits", boot-vocab from
"a shared predicate makes the mistake unavailable". Reaching one structure from
two directions is the evidence the structure is right.

`emits_helpers_for` is private/`pub(crate)`; both files are `tidepool-mcp`, so
it reaches the call site without widening. Do not widen it.

## 4. Shared files touched, for the fold's conflict log

| File | How |
|---|---|
| `tidepool-mcp/src/effect_defs.rs` | ADDITIVE — two new `*_effect_def!` macros appended before the `#[cfg(test)]` module. No existing macro touched. Agent-wave may also be appending effect definitions here; per the conflict experiment this was not pre-partitioned. |
| `tidepool-mcp/src/effect_decls.rs` | ADDITIVE — two `*_effect_def!(effect_decl_projection)` lines appended at end. |
| `tidepool-bridge-effects/src/lib.rs` | ADDITIVE — a PRD 19 section appended before `bridged_records_module()`. The existing six records and that function are untouched. |
| `tidepool-mcp/src/eval_prep.rs` | **The one edit INSIDE an existing function** (`effects_module_source_at`): one added import line, `Control.Monad.Freer.Internal (Eff(..), qApp, tsingleton)`. `Eff` is the same type already re-exported by `Control.Monad.Freer`, so this adds constructors and the queue operations and shadows nothing. Flagged here because it is the only non-append in the lane. |
| `tidepool-handlers/src/handlers/mod.rs` | ADDITIVE — two `pub mod` + two `pub use` lines, from the two forked lanes. |

Deliberately NOT touched: `tidepool-codegen/**` (the mechanism needs no change
there, and `resident.rs` pending/`ChildSuspended` is a hard hold),
`tidepool-harness` observability/error files, `tidepool-worktree/**`,
`harness-dogfooding/dev-tree/Harness.hs`. The Worktree and RepoEvent effects
were deliberately NOT added to `build_base_stack`/`base_effects!`/`handler_for!`
— they are out of the default server row until the dogfood lands, so no
positional union-tag slot was claimed.

## 5. HOLD lines, and one I came close to

**Came close: the `(<|>)` collision — held the DSL, escalated, applied nothing.**

`Tidepool.Prelude` re-exports `Control.Applicative`'s `(<|>)`
(`haskell/lib/Tidepool/Prelude.hs:118`, `:317`) and every eval auto-imports
`Tidepool.Prelude hiding (error)` (`tidepool-mcp/src/eval_prep.rs:126`). The
frozen `Tidepool.Event` requires `Tidepool.Effects` to export its own
`Event`-merge `(<|>)`. So an author writing the PRD's own example
(`fmap Left e1 <|> fmap Right e2`) hits an ambiguity.

VERIFIED, not inferred — a negative control importing both unhidden:

```
Amb.hs:8:23: error: [GHC-87543]
    Ambiguous occurrence '<|>'.
    It could refer to
       either 'Tidepool.Event.<|>' (originally defined in 'Tidepool.Effects'),
           or 'Tidepool.Prelude.<|>' (originally defined in 'GHC.Internal.Base').
```

GHC cannot disambiguate by type: an ambiguous occurrence is resolved at NAME
RESOLUTION, before typechecking, so the operators' differing types
(`Event a -> Event a -> Event a` vs `Alternative f => f a -> f a -> f a`) never
get a chance to help.

Every fix available in-lane was a DSL edit — rename the operator, or fake an
`Applicative`/`Alternative` for `Event`. I took neither. `Tidepool.Event` is
frozen, the PRD says the operator "need not fake a general `Applicative`
instance", and faking one would also be dishonest: `Event` has no sensible
`pure`.

ESCALATED to root (via worktree-wave), and **APPROVED**: `effects_module_source_at`
now emits `hiding (error, (<|>))` when the row contains RepoEvent, and is
byte-identical to before otherwise. What the decision rests on: an ambiguity
error on the PRD's own example is the worst possible prompt, which is what
"the API is the prompt" exists to prevent; conditionality confines the blast
radius to rows that do not exist yet; and both in-lane alternatives were
rightly rejected, since renaming shrinks the DSL and a fake `Alternative` with
no honest `pure` is a lie in the type system.

Gates, each named and run separately (`scripts/prd19-alternative-gates.sh`,
run wrapped):

| Gate | Name | Status |
|---|---|---|
| RED baseline — the collision is real without the fix, the diagnostic is specifically the ambiguity, AND it is located in the probe | `alternative_collision_is_real_without_the_fix` | PASS |
| GREEN — the PRD's example compiles UNQUALIFIED with the fix | `prd_example_compiles_unqualified_with_the_fix` | **RED — correctly so, see below** |
| NO-REGRESSION — a non-RepoEvent row still resolves `Alternative`'s `(<|>)` | `alternative_still_resolves_in_a_non_repoevent_row` | PASS |
| Licences the `eval_prep.rs` deletion — the generated module compiles WITHOUT its own hiding | `generated_module_compiles_without_its_own_hiding` | PASS |

**The green gate is CORRECTLY RED and stays red until the relocation lands.**
That is the honest state, not debt. A red gate pinning an unlanded fix is
EVIDENCE: it is the thing that fails loudly if the relocation lands wrongly,
and suppressing or `#[ignore]`-ing it to get a clean board would destroy the
exact signal it exists to carry — which is the precise mechanism the named-gate
rule was written against. Its failure message is self-describing thanks to the
artifact assertion: "the compiled module DID carry the fix … and GHC still
objected", so it reports a mis-scoped fix rather than a stale artifact.

**Gate 1 carries a location guard, added after it was found capable of passing
for the wrong reason.** Requiring only "an ambiguity mentioning `(<|>)`" would
also have gone green if the GENERATED module failed to compile without the
hiding — a different fact entirely, and a live possibility since the module
declares `infixl 3 <|>` beside the imported operator. The gate now requires the
diagnostic to sit in the PROBE. It still passes, so the red baseline is
genuine. This hazard surfaced only because the deletion licence forced the
compiles-WITHOUT question to be asked at all.

The RED baseline is load-bearing and is why the script reconstructs the
pre-fix source rather than just compiling the fixed one: a green compile is
equally consistent with "the fix works" and "the collision was never
reachable". Gate 1 removes that ambiguity by asserting the *specific*
ambiguous-occurrence diagnostic, not merely that something failed.

**Which list the predicate keys on — decided deliberately, because getting it
wrong is silent.** Extract-wave's boot-vocab lane splits the generator into
`effects_module_source_with_vocab(row_effects, vocab_effects, row)`, at which
point "the row contains RepoEvent" stops being one predicate and becomes two.

~~The conditional must key on `vocab_effects`.~~ **SUPERSEDED — that answer was
wrong.** It is kept struck through rather than deleted so anyone reconstructing
the argument meets the refutation instead of re-deriving a wrong answer that
briefly carried an endorsement. (Same treatment in worktree-wave's `GATES.md`,
reversed at `ec48f685`.)

**The conditional must key on the HELPER-EMISSION CONDITION:**

```
in_row(RepoEvent) || RepoEvent.helpers_row_polymorphic
```

which, for RepoEvent as declared today (not row-polymorphic), reduces to
`row_effects`.

Why, from the committed code rather than from a description of it
(`d6fce023`, `eval_prep.rs:273`): helper emission is ROW-GATED —
`if !(in_row || eff.helpers_row_polymorphic) { continue; }`. A row-CLOSED
helper (`foo :: A -> M B`, the ordinary shape) only typechecks when its effect
is actually in the row, so a vocabulary-only effect's helpers are emitted only
when it declares itself row-polymorphic. `(<|>)` is a RepoEvent HELPER, so
`Event`'s `(<|>)` exists exactly when RepoEvent's helpers are emitted — not
merely when RepoEvent is nameable.

**The principle survived; the answer did not.** "The hiding must track the
operator's PRESENCE, not the program's capability" is correct and is what
exposed the error. The failure was reasoning from a RELAYED SIGNATURE rather
than from code: a relay can be accurate about a function's shape while silent
about the thing that decides the answer. Had the `vocab_effects` answer
shipped, a vocabulary-only RepoEvent would have hidden the Prelude's `(<|>)`
while emitting no replacement — exactly the "costs `Alternative` for nothing"
failure catalogued below, self-inflicted.

**Only ONE mismatched pair is constructible.** `vocab_effects` must be a
SUPERSET of `row_effects`, enforced by a loud assert (`eval_prep.rs:177`), so
"row-with / vocab-without" cannot be built — it panics. An earlier draft of
this receipt listed it as a live hazard; that overstated the hazard space.

| Pair | Correct behaviour | Wrong under the vocab predicate? |
|---|---|---|
| RepoEvent in row AND vocab | HIDE — `(<|>)` is emitted | no |
| RepoEvent vocab-only, not row-polymorphic | do NOT hide — no `(<|>)` emitted | **yes — this is the discriminating gate** |
| RepoEvent absent | do not hide | no |
| row-with / vocab-without | unconstructible (panics) | n/a |

**Implementation rule: derive, don't declare.** The condition is written as the
SAME expression that gates helper emission, never a restatement and never a
"keep in sync" comment. A restatement is a second source of truth that diverges
the moment RepoEvent becomes row-polymorphic — which is precisely what the
vocabulary-without-row story wants. This is the discipline the parking contract
imposes on handled prefixes, for the same reason: one source of truth makes the
disagreeing case unconstructible rather than a responsibility to discharge.

**Ownership topology, as ratified.** Sharing the condition means touching
boot-vocab's helper-emission loop, which is a larger claim than "add a
conditional hiding term" and was flagged upward as one rather than folded into
the word "retarget". Resolution: **boot-vocab extracts the condition in its own
lane, pre-fold; this lane lands ONLY the hiding term, keyed on whatever
function boot-vocab exposes.** That makes the cross-lane edit vanish rather
than merely making it reviewable. The condition is never restated locally —
not even if boot-vocab's spelling is inconvenient — because the single-source
property is the entire point and survives only by calling their expression.

If boot-vocab were to decline single-sourcing, the ratified default is that a
"keep in sync" comment is not an acceptable substitute here: a comment is
precisely the mechanism that fails silently when the two copies diverge, since
the divergence produces a wrong preamble rather than a build error.

### Verification, done BEFORE landing

Root split verification from landing so the reversal would not ship unverified.
The four gates ran in a DISPOSABLE `git worktree` at `d6fce023` (extract-wave's
boot-vocab ref), which was then removed — nothing from another wave's branch
entered this lane's diff.

Hazards handled in that scratch tree, because a disposable worktree is still a
worktree: its `.config/nextest.toml` was CHECKED at `max-threads = 1` (not
assumed); everything was invoked through the absolute
`/home/inanna/dev/tidepool/scripts/ghc-slots.sh`, never the throwaway's own
copy, since the slot registry is a box-wide arbiter and N copies at N commits
are N arbiters; and all runs used `detach`, so a queue wait could not kill them.

| Gate | What it establishes |
|---|---|
| `mismatched_vocab_only_repoevent_does_not_hide_the_prelude_alternative` | THE DISCRIMINATING ONE — fails under the superseded `vocab_effects` predicate |
| `matched_repoevent_row_and_vocab_hides_the_prelude_alternative` | passes under EITHER predicate; labelled in-source as **not** evidence for the choice |
| `vocab_only_repoevent_still_emits_its_gadt` | wrong-reason guard: without it the discriminating gate would also pass if the vocabulary split were broken outright |
| `row_effect_absent_from_the_vocabulary_panics` | pins why only ONE mismatched pair exists, so the hazard space stays documented rather than remembered |

The gates use synthetic `EffectDecl`s rather than this lane's real
declarations, so they need only boot-vocab's code and test the GENERATOR's
behaviour, which is the thing in question.

Status: the edit currently lives in `effects_module_source_at` (committed in
`f37f0d17`, before the restructure was announced), keying on the single list
that function has. The intent is recorded at the call site so the retarget is
unambiguous. **Owed at retarget:** move the conditional into
`effects_module_source_with_vocab`, key it on `vocab_effects`, and add the two
mismatched-pair gates. Until those exist, the conditionality evidence below
covers only the matched pair and is honestly weaker than it will be.

**The failure mode this owed item has, stated because it is the dangerous
kind.** Where the edit currently sits it is correct-but-non-discriminating: the
old entry point passes the same list twice, so it cannot misbehave today AND
cannot fail loudly tomorrow. Moved mechanically by someone who does not read
the call-site comment, it would simply end up silently keyed on the wrong
thing. There is no build error and no failing test waiting to catch that — only
a wrong preamble in the mismatched pairs. So this item is carried in three
places on purpose: the call-site comment (reaches the person doing the move),
this receipt, and the `[READY]` note at submit (reaches the fold, rather than
arriving after it). Independently recorded by worktree-wave in
`plans/post-restart/worktree-lanes/GATES.md` (`f24a9974`), which makes it
durable outside this worktree — but it remains this lane's to complete or to
hand over explicitly, not something the external record discharges.

Conditionality is additionally pinned in the fast tier by
`repoevent_row_hides_the_prelude_alternative` and
`non_repoevent_row_leaves_the_prelude_import_untouched` — the second is the
one that catches the hiding becoming unconditional and silently taking
`Alternative` away from every existing eval.

**The cost of that proposal, stated because a recommendation without its cost is
not a real recommendation.** In a RepoEvent row authors lose the unqualified
`Alternative` `(<|>)` for `Maybe`/lists, and cannot recover it with
`import Control.Applicative ((<|>))` — that re-collides with `Event`'s. So the
honest framing is not "hide a Prelude name nobody in this row wants"; it is
"a RepoEvent row gets exactly ONE unqualified `<|>`, and this picks `Event`'s".
That is defensible (a resident orchestrating worktrees writes the Event merge
far more often than a `Maybe` fallback, and the `Maybe` case has
`fromMaybe`/`maybe`/pattern-matching as alternatives where the Event merge has
none) but it is a trade, not a free win.

**The typecheck probe passes only because it says `hiding ((<|>))` by hand.
That is a LANE-LOCAL WORKAROUND standing in for a decision that has not been
made — it is not evidence the collision is resolved.** Nothing today is broken
by it, because RepoEvent is deliberately not in the base server row; it breaks
when the dogfood puts it there.

### The first fix was MIS-SCOPED, and the authored-surface gate caught it

The conditional first landed in `effects_module_source_at` — i.e. on the
Prelude import INSIDE the generated `Tidepool.Effects`. That governs name
resolution within that module's own body and NOTHING ELSE. An author's module
performs its own `import Tidepool.Prelude`, so the fix did not touch the scope
it was approved to fix. GHC said so precisely, placing both offending imports
in the probe:

```
Ambiguous occurrence '<|>'
   either 'Tidepool.Event.<|>'    imported from 'Tidepool.Event'   at AltGate.hs:4
       or 'Tidepool.Prelude.<|>'  imported from 'Tidepool.Prelude' at AltGate.hs:5
```

**Why the gate caught it and a cheaper gate would not have.** Gate 2 compiles
PRD 19's own example the way an AUTHOR writes it. A gate asserting on the
generator's output string would have gone GREEN — the generator emitted exactly
what it was asked to emit; the ask was wrong. General rule, now doctrine:
**prefer authored-surface gates to generator-output gates wherever both are
available**, because a generator faithfully serving a wrong ask produces a
green gate.

**The evidence was in hand and the inference was not drawn.** The positive
typecheck probe compiled only because `hiding ((<|>))` was written BY HAND, in
the probe — author-side. That the workaround had to go author-side WAS the
evidence that the fix belonged author-side. It was recorded as a caveat and
moved past. Noticing that a workaround you wrote is evidence about where the
real fix belongs is the habit this lane is meant to carry forward.

**Blast radius is larger than the reproduction suggests.** `Tidepool.Effects`
has no export list, so it re-exports its own generated `(<|>)`, and the eval
preamble imports both `Tidepool.Prelude` and `Tidepool.Effects` by default.
Once RepoEvent is in the row the collision therefore arises in the DEFAULT EVAL
VOCABULARY — an author need not `import Tidepool.Event` at all. The gate below
must reproduce that default-vocabulary case, not the explicit-import one.

**Approved resolution** (root, via worktree-wave): move the conditional to the
AUTHOR-FACING imports — `eval_import_lines` (`preamble.rs:61`) and the
Orchestrate module (`preamble.rs:252`) — keyed on the same derived predicate,
and DELETE the `eval_prep.rs` term, which is not what fixes the author problem
and would otherwise sit there looking like the fix. `emits_helpers_for` reaches
the new site at `pub(crate)` without widening, since both files are
`tidepool-mcp`: the visibility constraint accepted earlier turns out to have
been right for a call site nobody had identified yet.

The deletion is gated, not assumed: a NAMED gate must prove the generated
module compiles WITHOUT its own hiding first. Compiles-WITH is not
compiles-WITHOUT, and the module declares `infixl 3 <|>` and defines the
operator alongside the imported one — whether GHC calls that ambiguous is a
fact to establish by compiling, not to predict.

**Not crossed:** `workspaceOf`, any `Workspace` type, and any coupling to an
agent handle remain absent. `harness-dogfooding/dev-tree/Harness.hs` is
unedited. The dev-tree dogfood compile (PRD acceptance 9) was not attempted.
`readOnlyOf` was never built (it left the public surface in root's rewrite).

**Not crossed:** `workspaceOf`, any `Workspace` type, and any coupling to an
agent handle remain absent. `harness-dogfooding/dev-tree/Harness.hs` is
unedited. The dev-tree dogfood compile (PRD acceptance 9) was not attempted.

**Naming divergence, deliberate and worth a reviewer's eye.**
`WorktreeReceipt`'s id field is `treeId`, not the PRD snippet's `worktreeId`.
The PRD's own public surface also pins `worktreeId :: WorktreeHandle ->
WorktreeId` as a standalone function, and a record selector plus a top-level
function of the same name is an ambiguous occurrence at the export. The
signature the PRD pins won; the illustrative field name yielded. Access is
`r.treeId`, per the record-dot rule.

## 6. Corrections absorbed from root mid-flight

- **Poke semantics — my spec's version was stale and is now reversed.** I was
  told `dev-tree/Harness.hs` diverged because it "treats a poke as if delivery
  were guaranteed where the PRD has since made pokes fire-and-forget". That is
  backwards. PRD 18's revision makes `pokeAgent` a DURABLE PER-AGENT QUEUE: a
  poke is accepted, stays queued until deliverable, is never silently discarded,
  and delivery to an idle agent starts or queues a follow-up turn. So a
  `headChanged` handler that pokes and returns is CORRECT, and needs no
  error-handling choreography for unsteerable agents. Harness.hs's shape is
  closer to right than my spec said. It does still diverge, differently:
  `sendMessage` and `followupTask` no longer exist as separate operations (both
  are `pokeAgent`), so its `pokeAgent = sendMessage` is wrong for that reason
  instead. The `Observed`/`payload`-vs-`value` divergence stands. The file
  remains read-only to this lane either way. **Nothing in this lane's code
  depended on the stale fact** — pokes are PRD 18 surface, not L4's.

- **Cycle shape — reinforces the constraint, changes the surrounding picture.**
  Agents may now continue running between resident cycles, and their identities
  and the resident's plan are ordinary checkpointed data. What still never
  crosses a cycle boundary is unchanged: an attached Haskell handle, a parked
  Haskell continuation, or an event subscription. A subscription is per-cycle,
  full stop. Nothing in this design leans on a subscription outliving its cycle
  or on the unfold/fold completing inside one — `withHandler` is lexically
  scoped and its registry lives and dies with the handler stack for the cycle.

  The change does put weight on one distinction that would be easy to implement
  away, so it is recorded as a design constraint rather than left implicit:
  **"no replay" is a rule about JOURNAL ROWS, not about observations.** Because
  re-registering from `State` each cycle is now a real repeated path rather than
  a recovery story, there is a real window between cycles with running agents
  and no subscribers. If the monitor's baseline were process memory, the first
  reconcile of a new cycle would conclude nothing moved and every commit in that
  window would vanish — breaking "commits are never silently dropped" in the
  harder-to-notice direction. The monitor's own contract already says the right
  thing (`tidepool-worktree/src/monitor.rs`: the restart baseline is the last
  JOURNALLED observation, never an in-memory one). With that, a gap movement
  surfaces in the new cycle as a genuinely new observation rather than a
  replayed row, and both rules hold at once for the same reason: the journal is
  the durable baseline, the queue is per-subscription. The event lane was told
  to state explicitly which baseline its adapter uses.
