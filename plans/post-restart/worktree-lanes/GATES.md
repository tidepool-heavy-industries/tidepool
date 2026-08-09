# worktree-wave — one-failure-mode gates, certified by name

Per root's standing receipt rule (originated extract-wave, adopted swarm-wide):
a gate that exists to catch ONE specific failure mode is established only by
**that test passing BY NAME with its own pass line**, never by the aggregate
that contains it. A rename, an `#[ignore]`, a `cfg`, or an env-gated early
return leaves the aggregate green while the guard never executed — so
"50 tests run: 50 passed" cannot distinguish "the guard held" from "the guard
silently stopped existing".

Cross-lane guards additionally name the base commit: **base proves the tree,
name proves execution — both, or neither is established.**

## Writing gates: a green result cannot distinguish right-reason from wrong-reason

The named-guard rule says a one-failure-mode gate must pass BY NAME. That is
about receipts. This is the same idea one level down, about gate DESIGN, and it
is the thing to internalize before writing a gate set:

**A passing gate tells you the assertion held. It does not tell you the
assertion held FOR THE REASON YOU INTENDED.** A green result cannot distinguish
those two, so the gate SET has to.

Three habits, each earned from a real case in this wave:

**1. Add a wrong-reason guard.** L4's discriminating gate asserts that a
vocabulary-only RepoEvent does NOT hide the Prelude's `(<|>)`. But that gate
would ALSO pass if the vocabulary split were broken outright and RepoEvent
simply never appeared. So a second gate,
`vocab_only_repoevent_still_emits_its_gadt`, pins that the effect really is
present and only its HELPERS are withheld — establishing that the first gate
tests the helper-emission condition rather than mere absence. Ask of every gate:
what ELSE would make this pass? If the answer is "the feature being broken in a
different way", you need the guard.

**2. Label a non-discriminating gate in-source.** The matched-pair gate passes
under either candidate predicate, so it is no evidence for choosing between
them. L4 marked it as such in the source, not only in a receipt. A gate that
looks like evidence and is not will be cited as evidence by someone who did not
write it.

**3. Prefer an AUTHORED-SURFACE gate to a GENERATOR-OUTPUT gate.** L4's gate 2
compiles PRD 19's own example the way an author writes it. It caught that the
approved `(<|>)` fix was MIS-SCOPED — hiding the operator inside the generated
`Tidepool.Effects` governs only that module's body, while the ambiguity arises
in the author's module, which does its own imports. A gate asserting on the
generator's output STRING would have gone green, because the generator emitted
exactly what it was asked to emit. The generator was correct and the design was
wrong, and only a gate that exercises the authored surface can tell those apart.
Where both kinds are available, the authored one is the real gate.

**4. CHECK DURATION AGAINST THE WORK CLAIMED.** The cheapest instrument there
is, and in this wave the only one that caught a vacuous green. L4's seven-gate
re-run returned 23/23 PASS, started == run, no truncation, correct count, every
gate by name with a real pass line — and every one had SKIPPED. The harness
early-returns and PASSES when `TIDEPOOL_EXTRACT` is unset, and it was unset.

A skip spelled as a pass is structurally IDENTICAL to a pass, so it defeats
every check layered above: named execution, real pass lines, started-vs-run,
completion figures, named instruments. None of them can distinguish
ran-and-held from skipped-and-passed. What gave it away was 0.006–0.011 s per
gate against 9.2–16.6 s previously — a test driving real GHC → extract → JIT →
a temp git repository cannot finish in 6 ms.

So: know roughly what your gate SHOULD cost, and treat an unexplained
order-of-magnitude drop as a finding rather than good luck. Report durations,
because the next person's baseline is your reported timing — that is how this
one was caught at all.

REFINEMENT, found by applying this rule to this crate's own run: FAST IS NOT
AUTOMATICALLY SUSPICIOUS. The check is duration against THE WORK THE GATE
CLAIMS, not against some absolute floor. `tidepool-worktree`'s fastest gates run
in 5–9 ms — `reconcile_on_unregistered_worktree_returns_worktree_not_registered`,
`journal_open_reports_typed_failure_when_a_file_blocks_the_directory`, the
`storage_errors` family — and every one of them SHOULD be that fast, because
each tests an EARLY-RETURN or filesystem-failure path that by design never
spawns git. A gate proving "this refuses before doing the work" is correctly
cheap; if it were slow, THAT would be the finding.

Root's ruling on the skip sites that prompted this: missing required
environment FAILS LOUDLY, naming the variable. That was ALREADY the stated
convention (root `CLAUDE.md`: tests without `TIDEPOOL_EXTRACT` "fail loud") —
the skip-as-pass sites were nonconforming, not a competing style. Worth noting
the shape: the rule existed and was silently violated, which is the expensive
kind, because everyone assumes a stated rule is being followed.
`TIDEPOOL_EXPENSIVE_TESTS` gating remains the sanctioned exception.
`tidepool-worktree` was checked and is clean of the pattern.

The hazard is a gate that is fast while claiming work it could not have done in
the time — 6 ms for a gate driving GHC → extract → JIT → a temp repository.
Applied as "fast = bad" this rule produces false alarms and gets ignored, which
is worse than not having it.

**5. Say when your gates cannot settle the question at all.** Earlier in the
same lane, the conditionality gates were built on an entry point that passes one
list for both parameters, making them non-discriminating rather than merely
incomplete — and the receipt said so. That distinction is the difference between
a receipt that overstates its evidence and one that can be trusted.

The failure this guards against is specific and quiet: a wrong predicate here
produces a wrong generated preamble, not a build error and not a failing test.
Nothing fails loudly, so the gate set is the only thing standing between the
mistake and shipping it.

## Base commit

`20086b2eaa5175accc8cb85be8e3e48c78c25a35`

Every block below was re-run at this base after `worktreeHead` landed. The
file deliberately carries ONE base rather than a per-section history: a
certification against an older tree is not a certification of this one, and a
reader should not have to work out which sections are current.

Reproduce any block below with:

```bash
/home/inanna/dev/tidepool/scripts/ghc-slots.sh run -- \
  cargo nextest run -p tidepool-worktree -E 'test(=<exact_name>)'
```

The slot wrapper is required while root's throttle is active — it covers
`cargo nextest run` at any tier, not only GHC-heavy work. Drop it only once
root lifts the throttle.

## Retention — retain-first is a locked decision

Nothing may delete or silently recreate a worktree, branch, ref, or record.

| Gate | Failure mode it exists to catch |
|---|---|
| `hand_deleted_worktree_is_lost_not_recreated` | a removed worktree being silently recreated instead of reported |
| `list_never_fails_when_one_of_several_worktrees_is_lost` | one lost tree hiding every other retained tree |
| `an_unregistered_id_is_not_reported_as_a_lost_worktree` | a typo masquerading as data loss, hiding real loss behind it |

```
PASS tidepool-worktree::worktree_core hand_deleted_worktree_is_lost_not_recreated
PASS tidepool-worktree::worktree_core list_never_fails_when_one_of_several_worktrees_is_lost
PASS tidepool-worktree::worktree_core an_unregistered_id_is_not_reported_as_a_lost_worktree
```

## Isolation — one worktree, one agent

| Gate | Failure mode it exists to catch |
|---|---|
| `binding_refuses_second_agent_and_permits_rebind_after_settle` | two writers in one worktree |
| `settling_a_released_binding_also_permits_rebind` | a retained worktree becoming permanently unusable after release |

```
PASS tidepool-worktree::worktree_core binding_refuses_second_agent_and_permits_rebind_after_settle
PASS tidepool-worktree::worktree_core settling_a_released_binding_also_permits_rebind
```

## Never dirty the source

| Gate | Failure mode it exists to catch |
|---|---|
| `clean_creation_from_current_repository_leaves_source_untouched` | creation mutating the repository it reads |
| `dirty_source_untouched_after_snapshot` | the synthetic commit disturbing branch/HEAD/index/bytes |
| `refuses_dirty_submodule_and_leaves_source_untouched` | capturing a gitlink whose content was never captured |
| `refuses_when_source_is_mid_merge_and_leaves_it_untouched` | a half-merged tree becoming a reproducible base for the wrong program |
| `refuses_when_source_is_mid_rebase_and_leaves_it_untouched` | same, mid-rebase |
| `snapshot_ref_lives_outside_refs_heads_and_never_in_branch_list` | a Tidepool ref appearing in the operator's `git branch` |
| `registry_open_refuses_a_root_inside_a_working_tree` | the registry itself dirtying a source tree |

```
PASS tidepool-worktree::worktree_core   clean_creation_from_current_repository_leaves_source_untouched
PASS tidepool-worktree::dirty_snapshot  dirty_source_untouched_after_snapshot
PASS tidepool-worktree::dirty_snapshot  refuses_dirty_submodule_and_leaves_source_untouched
PASS tidepool-worktree::dirty_snapshot  refuses_when_source_is_mid_merge_and_leaves_it_untouched
PASS tidepool-worktree::dirty_snapshot  refuses_when_source_is_mid_rebase_and_leaves_it_untouched
PASS tidepool-worktree::dirty_snapshot  snapshot_ref_lives_outside_refs_heads_and_never_in_branch_list
PASS tidepool-worktree::worktree_core   registry_open_refuses_a_root_inside_a_working_tree
```

## No replay

A subscription registered now must never see a row written before it.

| Gate | Failure mode it exists to catch |
|---|---|
| `fresh_subscription_sees_none_of_the_prior_rows` | journal history replaying into a new handler |
| `first_reconcile_after_register_establishes_baseline_without_emitting` | a fabricated "first sight" transition |

```
PASS tidepool-worktree::event_monitor fresh_subscription_sees_none_of_the_prior_rows
PASS tidepool-worktree::event_monitor first_reconcile_after_register_establishes_baseline_without_emitting
```

## Honest observation

Coalesced deltas, and degradation rather than a guess.

| Gate | Failure mode it exists to catch |
|---|---|
| `commit_yields_commit_and_head_changed_sharing_one_event_id` | two views of one change looking like two changes |
| `two_commits_between_polls_coalesce_into_one_advanced` | a delta stream pretending to be exhaustive history |
| `unreachable_old_head_after_gc_yields_unknown_change` | inventing a shape for an unrecoverable movement |
| `unrelated_history_on_the_same_branch_yields_unknown_change_rather_than_a_guess` | same, for unrelated history |

```
PASS tidepool-worktree::event_monitor commit_yields_commit_and_head_changed_sharing_one_event_id
PASS tidepool-worktree::event_monitor two_commits_between_polls_coalesce_into_one_advanced
PASS tidepool-worktree::event_monitor unreachable_old_head_after_gc_yields_unknown_change
PASS tidepool-worktree::event_monitor unrelated_history_on_the_same_branch_yields_unknown_change_rather_than_a_guess
```

## Fresh reads — `worktreeHead` is not a cached field

PRD 19 added `worktreeHead` so a resident spanning cycles can compare the
current head against its checkpoint before re-registering, closing the window
where `HEAD` moves while no subscription exists. A cached or stale answer
silently reopens exactly that window, and nothing else would fail.

| Gate | Failure mode it exists to catch |
|---|---|
| `worktree_head_is_a_fresh_read_distinct_from_source_head` | returning the seed commit instead of the current head — a subtly useless alias |
| `worktree_head_reflects_movement_the_monitor_never_reconciled` | answering from the monitor's baseline, so the between-cycle gap stays hidden |
| `worktree_head_on_detached_head_returns_the_commit` | assuming a symbolic ref and failing on a detached HEAD |
| `worktree_head_of_a_lost_worktree_fails_consistently_with_lookup` | inventing a failure mode for a lost worktree instead of reusing `WorktreeLost` |

```
PASS tidepool-worktree::worktree_head worktree_head_is_a_fresh_read_distinct_from_source_head
PASS tidepool-worktree::worktree_head worktree_head_reflects_movement_the_monitor_never_reconciled
PASS tidepool-worktree::worktree_head worktree_head_on_detached_head_returns_the_commit
PASS tidepool-worktree::worktree_head worktree_head_of_a_lost_worktree_fails_consistently_with_lookup
```

## Surface — `Event`'s `(<|>)` in the generated preamble (L4, authorized 2026-08-08)

PRD 19's `(<|>) :: Event a -> Event a -> Event a` collides with the
`Control.Applicative` operator that `Tidepool.Prelude` re-exports
(`Prelude.hs:118`, `:317`) and every eval auto-imports (`eval_prep.rs:126`).
GHC cannot disambiguate: an ambiguous occurrence is name resolution, before
typechecking. So the PRD's own example fails to compile in a RepoEvent row.

Decision (root, on the lane's proposal): `effects_module_source_at` emits
`hiding (error, (<|>))` when the row contains RepoEvent, unchanged otherwise —
the runtime grows so the DSL need not shrink. The priced trade: in a RepoEvent
row there is exactly ONE unqualified `<|>` and it is `Event`'s, because the
merge has no alternative spelling while `Maybe`/list fallbacks have
`fromMaybe`/`maybe`/patterns.

| Gate | Failure mode it exists to catch |
|---|---|
| red baseline | the collision being unreachable, making the green gate prove nothing — must assert the EXACT ambiguous-occurrence diagnostic, not merely a failed compile |
| green | the PRD's example still needing a hand-added `hiding` to compile |
| no-regression | the hiding being applied unconditionally, silently costing every other row its Alternative `<|>` |

The edit's target moved mid-flight: the region now lives in
`effects_module_source_with_vocab(row_effects, vocab_effects, row)`, with
`effects_module_source_at` reduced to a delegation (extract-wave's boot-vocab
lane, so the vocabulary/row distinction is explicit in code rather than only
in reasoning). That split makes "the row contains RepoEvent" TWO possible
predicates, and picking the wrong one is not a compile error — it silently
produces the wrong preamble in exactly the mismatched (row, vocab) pairs
boot-vocab's tests construct. The no-regression gate therefore has to cover a
MISMATCHED pair, not only a matched one.

~~DECIDED (L4): the predicate keys on `vocab_effects`~~ — **REVERSED, see the
correction below. The `vocab_effects` answer is WRONG.** The superseded
reasoning is kept because it was endorsed and recorded here, and a reader who
reconstructs it should meet the refutation rather than the original.

### CORRECTED: the predicate is the helper-emission condition itself

Verified in `d6fce023` (`tidepool-mcp/src/eval_prep.rs:273`), not relayed:

```rust
let in_row = row_effects.iter().any(|r| r.type_name == eff.type_name);
if !(in_row || eff.helpers_row_polymorphic) { continue; }
```

A row-CLOSED helper (`foo :: A -> M B`) only typechecks when its effect is in
the row, so a vocabulary-only effect's helpers are emitted ONLY if it declares
`helpers_row_polymorphic`. `(<|>)` is a RepoEvent HELPER and RepoEvent is not
row-polymorphic — so `Event`'s `(<|>)` is emitted exactly when RepoEvent is in
the ROW. Keying on `vocab_effects` would hide the Prelude's operator while
emitting no replacement for a vocabulary-only RepoEvent: the "costs
`Alternative` for nothing" failure, caused by the predicate meant to prevent it.

The PRINCIPLE was right and the ANSWER was wrong, which is worth separating:
hiding must track the operator's PRESENCE, not the program's capability.
Presence simply turned out to be row-gated, and that is only visible in code —
the relay was accurate about the SHAPE and silent about what decides the answer.

Implement it by CALLING boot-vocab's published predicate —
`emits_helpers_for(eff: &EffectDecl, row_effects: &[EffectDecl]) -> bool`,
returning `in_row || eff.helpers_row_polymorphic`, private or `pub(crate)` and
never `pub` (the hiding term lands in the same file, and a `pub` predicate would
commit to callers that do not exist). Never a restatement. A restatement is a second source of truth that diverges the moment
RepoEvent becomes row-polymorphic — plausible, since that is what the
vocabulary-without-row story wants. Derive, don't declare: the same discipline
the parking contract imposes on handled prefixes.

Hazard space is also SMALLER than first recorded: `vocab_effects` must be a
superset of `row_effects`, enforced by a loud assert (`eval_prep.rs:177`). So
row-with/vocab-without is UNCONSTRUCTIBLE — it panics. Exactly ONE mismatched
pair is constructible, vocab-with/row-without, where the correct behaviour is
NOT to hide.

Revised gates: matched → hides; mismatched vocab-only → does NOT hide and emits
no `(<|>)` (the DISCRIMINATING one — it fails under the `vocab_effects`
predicate); non-RepoEvent → unchanged; superset violation → panics.

RELOCATED (root, approved): the conditional does NOT belong in
`effects_module_source_*`. Hiding inside the generated `Tidepool.Effects`
governs only THAT module's body, while the ambiguity arises in the AUTHOR's
module. It goes on the author-facing imports — `eval_import_lines`
(`preamble.rs:61`) and the Orchestrate module (`preamble.rs:252`) — keyed on the
same `emits_helpers_for`. Both files are `tidepool-mcp`, so the earlier
`pub(crate)` visibility choice reaches the new site without widening, at a call
site nobody had identified when that constraint was set.

The blast radius is LARGER than first priced and the trade is unchanged.
`Tidepool.Effects` has no export list, so it exports the generated `(<|>)`, and
the eval preamble imports both it and `Tidepool.Prelude` by default — so the
collision is in the DEFAULT vocabulary once RepoEvent is in the row, not only
for an author who explicitly imports `Tidepool.Event`. The original pricing was
therefore against the real surface all along, merely attributed to the wrong
file. Rows without RepoEvent stay byte-identical.

Gates required at landing: the authored-surface RED must be the
DEFAULT-VOCABULARY case (RepoEvent in row, NO explicit `Tidepool.Event`
import) — the explicit-import reproduction understates the radius, so the gate
covers the worst case; non-RepoEvent-row BYTE-IDENTITY, proving conditionality
rather than asserting it; and a MANDATORY named gate proving generated
`Tidepool.Effects` compiles WITHOUT the old hiding before that hiding is
deleted (compiles-WITH is not compiles-WITHOUT).

SEQUENCING (root ruling): verification is split from landing. `d6fce023` is a
WIP commit spanning seven files across four crates, so landing it on the base
every lane rebases against would defeat the purpose of landing it early. The
discriminating gate is instead verified NOW in a DISPOSABLE worktree checked
out from extract-wave's ref and then discarded — nothing enters L4's diff, so
its refusal to merge another wave's branch stands, while the evidence transfers
under the receipt rules (gate named, base stated as `d6fce023`). The retarget
lands as a small commit after boot-vocab folds up as a checkpoint. Net effect:
the wrong-parameter risk is dead BEFORE the owed-retarget window opens, rather
than being carried through it.

Caution for that disposable worktree, since it is the same hazard class this
wave spent a day on: a throwaway checkout is another worktree, so it needs
`max-threads = 1` in its own `.config/nextest.toml` before running anything
GHC-heavy, must invoke `ghc-slots.sh` by ABSOLUTE path (the slot registry is a
box-wide arbiter; N worktree copies at N commits are N different arbiters), and
must be `git worktree remove`d rather than leaked.

Superseded reasoning follows.

DECIDED (L4, SUPERSEDED): the predicate keys on **`vocab_effects`**, not `row_effects`.
The hiding exists so an author can write `Event`'s `(<|>)` unqualified; that
operator is a GENERATED HELPER, so it is in scope exactly when RepoEvent is in
the VOCABULARY, whether or not the effect is in the sendable row. The predicate
tracks what determines the operator's PRESENCE (vocabulary), not what
determines the program's CAPABILITY (row). Both mismatched pairs misbehave
under the other choice, and neither is a build error:

- vocab-with / row-without → `(<|>)` is emitted but the Prelude's is no longer
  hidden, restoring the exact ambiguity the change exists to remove;
- row-with / vocab-without → the Prelude's `(<|>)` is hidden with nothing
  replacing it, costing `Alternative` for nothing.

OWED AT RETARGET: the edit currently sits in the old `effects_module_source_at`
(L4 committed it before the move was announced), where that function passes ONE
list for both parameters. That matters for what the existing gates prove: a gate
built on the single-list entry point is NON-DISCRIMINATING between the two
predicates — it passes under either — so it is not merely incomplete evidence,
it is no evidence for this question. Owed: move the conditional into
`effects_module_source_with_vocab`, key it on `vocab_effects`, and add the two
mismatched-pair gates, which are only expressible once that function exists.
The reasoning also lives at the call site in `eval_prep.rs`, deliberately:
whoever performs the mechanical retarget will be reading the code, and picking
the wrong parameter produces a wrong preamble rather than a build failure.

Owed by lane L4; not yet certified here.

## Still owed

`withHandler`'s semantics (lane L4) are one-failure-mode gates too and are not
yet certified here — no-replay to a fresh subscription, broadcast to two
handlers, in-order queueing when a handler suspends, drain-then-unregister,
handler failure failing the enclosing scope, bounded-queue overflow failing
loudly, and the rooting receipt `stowed_roots_count() == parked_count()`.
That lane has the rule.
