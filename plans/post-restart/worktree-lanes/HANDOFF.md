# worktree-wave → successor handoff (written for `agent-core`)

PRD 19's substrate is landed. This file is what a successor would otherwise
re-derive: measurements with their instruments, the invariant gate names, and
the reasoning behind decisions whose failure modes are silent.

Design authority remains
`plans/self-iterating-harness/19-managed-worktrees-events-prd.md` (root's
rewrite). Per-lane detail is in `L1`–`L8` receipts here; the by-name gate record
is `GATES.md`; operational rules are in `README.md`.

## What exists, and where the seam is

`tidepool-worktree` is a GHC-free crate holding all git truth: creation,
durable registry, restart lookup, dirty snapshot, event monitor, journal,
binding. `Tidepool.Worktree` and `Tidepool.Event` are the authored surface;
`withHandler` runs on the realm's parked path.

**The coupled-spawn seam is deliberately NOT built.** `binding.rs` implements
the state machine — one worktree per agent, second binding fails
`WorktreeBusy` naming the holder, rebind only after `Terminal` or `Released` —
and proves it against a scripted writer using an opaque `AgentRef` newtype.
That newtype is a placeholder on purpose: coupling this module to an agent
handle type that had not been designed would have had to be undone. Wiring it
to real spawns is `agent-core`'s work, and the enforcement it needs already
exists and is gate-covered.

`workspaceOf :: WorktreeHandle -> Workspace` is likewise absent, not
forgotten — `Workspace` is PRD 18's type and the conversion was held pending
the joint seam.

## Measurements, with their instruments

A number without its instrument cannot be checked. These are the wave's:

- **Poll interval `DEFAULT_POLL_INTERVAL_MS = 5_000`.** Chosen from
  repository-observation reasoning, NOT from Exomonad's 15 s inbox backstop
  (different urgency profile — root ratified treating 15 s as context, not
  precedent). Exported so a driver overrides it rather than burying a literal.
- **No-op reconcile cost: 6.85 ms per pair.** Instrument: a
  `rev-parse HEAD` + `symbolic-ref --short HEAD` pair against a real temporary
  repository, timed over 200 iterations with `date +%s%N` deltas around the
  loop, on this box at load ~35–55. Derived duty cycle at a 5 s interval:
  **1.6 % of one core at 12 worktrees, 3.3 % at 24, 6.6 % at 48.**
  This figure is process-spawn dominated, so it measures THIS box under load,
  not a property of the operation — an idle box is faster.
  An earlier revision claimed "well under a percent of a core at dozens of
  worktrees". That was an unmeasured estimate stated as a measurement and was
  wrong by ~3× at 24 worktrees. The conclusion (single-digit percent is cheap;
  cost is not a reason to widen the interval) survived; the claim did not.
  Applying the name-the-instrument rule is what caught it.

## Invariants, and the gates that hold them

Full list with per-gate failure modes and pass lines: `GATES.md`. Summary of
what must not regress:

| Invariant | Gates |
|---|---|
| Retain-first (nothing deleted or silently recreated) | `hand_deleted_worktree_is_lost_not_recreated`, `list_never_fails_when_one_of_several_worktrees_is_lost`, `an_unregistered_id_is_not_reported_as_a_lost_worktree` |
| Isolation (one worktree, one agent) | `binding_refuses_second_agent_and_permits_rebind_after_settle`, `settling_a_released_binding_also_permits_rebind` |
| Never dirty the source | `clean_creation_from_current_repository_leaves_source_untouched`, `dirty_source_untouched_after_snapshot`, `refuses_dirty_submodule_and_leaves_source_untouched`, `refuses_when_source_is_mid_merge_and_leaves_it_untouched`, `refuses_when_source_is_mid_rebase_and_leaves_it_untouched`, `snapshot_ref_lives_outside_refs_heads_and_never_in_branch_list`, `registry_open_refuses_a_root_inside_a_working_tree` |
| No replay | `fresh_subscription_sees_none_of_the_prior_rows`, `first_reconcile_after_register_establishes_baseline_without_emitting` |
| Honest observation | `commit_yields_commit_and_head_changed_sharing_one_event_id`, `two_commits_between_polls_coalesce_into_one_advanced`, `unreachable_old_head_after_gc_yields_unknown_change`, `unrelated_history_on_the_same_branch_yields_unknown_change_rather_than_a_guess` |
| Fresh reads (`worktreeHead` is not cached) | `worktree_head_is_a_fresh_read_distinct_from_source_head`, `worktree_head_reflects_movement_the_monitor_never_reconciled`, `worktree_head_on_detached_head_returns_the_commit`, `worktree_head_of_a_lost_worktree_fails_consistently_with_lookup` |
| `withHandler` semantics | `no_replay_of_events_observed_before_registration`, `one_commit_broadcasts_to_both_registered_handlers`, `queued_observations_invoke_in_order_across_a_handler_suspension`, `body_end_drains_before_it_unregisters`, `handler_failure_fails_the_enclosing_scope`, `bounded_queue_overflow_fails_loudly_rather_than_dropping_a_commit`, `rooting_receipt_holds_across_an_interleaved_park_and_resume` |

`worktreeHead` deserves a note: it exists so a resident spanning cycles compares
the current head against its checkpoint BEFORE re-registering, closing the
window where `HEAD` moves while no subscription exists. It is a fresh git read.
An implementation returning `source_head` would pass every other test in the
crate — which is why that one gate is load-bearing.

## The `(<|>)` predicate story — read this before touching that conditional

The failure mode here is SILENT (a wrong generated preamble, no build error, no
failing test), and two agents got it wrong in a row. The reasoning matters more
than the conclusion.

1. **The collision.** PRD 19's `(<|>) :: Event a -> Event a -> Event a` collides
   with `Control.Applicative`'s, which `Tidepool.Prelude` re-exports and every
   eval auto-imports. GHC cannot disambiguate — an ambiguous occurrence is name
   resolution, before typechecking — so the PRD's own example fails to compile.
2. **The priced trade.** In a RepoEvent row there is exactly ONE unqualified
   `<|>` and it is `Event`'s, because the merge has no alternative spelling
   while `Maybe`/list fallbacks have `fromMaybe`/`maybe`/patterns, and the
   qualified `Control.Applicative` route stays open. Renaming the operator was
   rejected (shrinks the DSL); a fake `Alternative` was rejected (`Event` has no
   honest `pure` — a lie in the type system).
3. **The predicate: `row_effects`, not `vocab_effects`.** Reversed after being
   endorsed and recorded. Hiding must track the operator's PRESENCE, not the
   program's capability — that principle is right. But presence is ROW-GATED:
   `eval_prep.rs` emits an effect's helpers when `in_row ||
   helpers_row_polymorphic`, a row-closed helper only typechecks in the row, and
   RepoEvent is not row-polymorphic. Keying on `vocab_effects` would hide the
   Prelude's operator while emitting no replacement — the exact failure the
   predicate exists to prevent. **Derive, don't declare:** the hiding term calls
   `emits_helpers_for`, boot-vocab's single source of truth, never a restatement.
   A restatement diverges the moment RepoEvent becomes row-polymorphic, which is
   precisely what the vocabulary-without-row story enables.
   Hazard space is narrower than first thought: `vocab_effects` must be a
   superset of `row_effects` (asserted), so only ONE mismatched pair is
   constructible.
4. **The scope: author-facing, not the generated module.** Hiding inside the
   generated `Tidepool.Effects` governs only that module's body; the ambiguity
   arises in the AUTHOR's module. The conditional belongs on `eval_import_lines`
   (`preamble.rs`) and the Orchestrate module. Blast radius is the DEFAULT
   vocabulary, not just explicit-import authors: `Tidepool.Effects` has no
   export list, so it exports the generated `(<|>)`, and the preamble imports
   both it and `Tidepool.Prelude`.
5. **The tell, worth internalizing.** The positive typecheck probe compiled only
   because `hiding ((<|>))` was hand-written AUTHOR-SIDE. That the workaround had
   to go author-side WAS the evidence the fix belonged author-side. The
   workaround was recorded without the inference being drawn. When you write a
   workaround, ask what its LOCATION is telling you.

Both errors above came from reasoning about a relayed signature that was
accurate about SHAPE and silent about what decides the answer. **"Read the
committed function" has to mean read it, not read about it.**

## Known gaps and deferred questions

- PRD deferred questions 1–5 stand (GC/retention interface, durable agent
  handles across cycles, git hook protocol, richer events, cross-process
  orchestration). Nothing here pre-empts them.
- Polling only. No hook adapter, no notify path — PRD acceptance criterion 8
  covers hooks-as-wake-up, and its test obligation arrives with that lane.
- The Exomonad decision record (`L5-exomonad-decision-record.md`) is
  REFERENCE ONLY. Inanna's resolution: nothing is ever adopted from Exomonad;
  its `inbound.rs` property analysis is a checklist of reliability properties
  Tidepool must satisfy NATIVELY, never mechanisms to port.
- `dev-tree/Harness.hs` (PRD acceptance 9) is unedited and does not compile
  yet — it awaits agent-wave and Inanna's coupling sync pass. Known divergences:
  it writes `payload observed` where `Observed` names its field `value`, and
  spells the poke `sendMessage`, which no longer exists as a separate operation
  (it is `pokeAgent`, a durable per-agent queue).
