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

Implement it as the SAME expression that gates helper emission, never a
restatement. A restatement is a second source of truth that diverges the moment
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
