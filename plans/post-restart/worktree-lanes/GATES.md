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

## Still owed

`withHandler`'s semantics (lane L4) are one-failure-mode gates too and are not
yet certified here — no-replay to a fresh subscription, broadcast to two
handlers, in-order queueing when a handler suspends, drain-then-unregister,
handler failure failing the enclosing scope, bounded-queue overflow failing
loudly, and the rooting receipt `stowed_roots_count() == parked_count()`.
That lane has the rule.
