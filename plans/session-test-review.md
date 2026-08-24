# Session-scoped test review: do we need all of them?

Status: PROPOSAL ONLY. Nothing in this document is implemented. Every verdict
below is a recommendation for the operator to pick from — read-only review per
`plans/test-time-cut.md`'s finding that 41% of sampled compile wall time is
session-scoped (`--session-root`/`--inject-val`) and structurally uncacheable,
concentrated in `tidepool-harness`/`tidepool-repl` turn-lane tests.

## Method

**Enumeration.** Every `.rs` file directly under `tidepool-harness/tests/` and
`tidepool-repl/tests/` was read in full (not grepped, not inferred from
filenames) — 42 files in `tidepool-harness/tests/` (one, `compile_fail.rs`, is
a `trybuild` harness with **zero** `tidepool-extract` spawns of any kind — it
drives `rustc` compile-fail fixtures, not Haskell, so it is out of scope and
excluded from the count below) and 27 files in `tidepool-repl/tests/`. That's
**41 + 27 = 68 in-scope files**, covering (per file `#[test]`/`#[tokio::test]`
enumeration) **≈146 test functions in `tidepool-harness`** and **≈139 test
functions in `tidepool-repl`** — close to the prior diagnosis's shape-census
counts (162 and 141) modulo a handful of `#[ignore]`d/helper functions.

**"Session-scoped" classification.** A test drives a session-scoped compile
when it runs a real turn through `Harness::run_block`/`run_multi_item_block`,
`SelfHarnessDriver::run_one_loop_iteration`, or `tidepool-repl`'s
`session_run`/`session_resume` — each such call compiles against the node's
live decl-plane/value-plane state (`--session-root`/`--inject-val`), which the
compile memo's argv allowlist structurally excludes (`tidepool-harness/
CLAUDE.md`'s "Compile memo" section). A minority of tests in both crates
instead call a **lower-level PROBE path directly** — `engine::compile_turn`/
`compile_turns`/`EngineConfig::turn_target` with no `Harness`/session context
(harness), or a meta-command that never reaches the JIT at all — `:bindings`,
`:reset`, `:stub`, `:i`, `:program` (repl, confirmed by reading `session.rs`'s
`run_meta`: only `:t` compiles). These are marked **N** (not session-scoped)
in the tables below and are **not** part of the session-scoped spawn count,
even though several still cost a real (cacheable-in-principle) GHC spawn.

**"Turns driven"** counts distinct compile-triggering events: one
`Harness::run_block`/`run_multi_item_block` call, one `SelfHarnessDriver`
loop-iteration's model round, or one `session_run`/`session_resume` dispatch
(a multi-item block in ONE `session_run` call is **one** turn, not one per
item — this distinction matters a lot and is called out per-test below). A
hole **resume that continues an already-compiled fragment does not
recompile** (`Harness::resume_parent_input`'s own doc: "the ALREADY-COMPILED
fragment... it does not recompile") — confirmed directly in
`tidepool-harness/src/harness.rs`, and this correction is applied throughout
(a naive per-dispatch count would overstate several files' true turn cost).

**"Turns load-bearing"** is the minimum turn count that still proves the
test's *stated* property. A property that is explicitly about CROSS-TURN
behavior (declaration in turn N visible/used in turn N+k, GC survival across a
real gap, checkpoint restore across a process boundary) needs ≥2 turns **by
definition** — reducing those changes what the test proves, and is called out
explicitly, never hand-waved. A property that only needs facts reachable
within one compiled block (most "bind X, then read X back three ways" shapes)
does not need separate turns just because the test happened to write it that
way.

**Verdicts** (one of five per test):
- **KEEP AS-IS** — turn count is already minimal/necessary for the property.
- **REDUCE TURNS (N→M)** — same property, fewer turns; what changes is named.
- **MERGE WITH `<sibling>`** — this test's property is provable inside a
  sibling's existing run, or the two are close enough to combine without loss.
- **DEMOTE TO CHEAPER LAYER** — the *specific* pure-Rust seam that would prove
  the same property without a GHC compile is named.
- **DELETE (covered by X)** — the property is already fully proven elsewhere.

Every REDUCE/MERGE/DELETE verdict below names the specific sibling or layer.
Nothing here proposes deleting a genuinely cross-turn property's turn count.

---

## Summary and projection

| Crate | Files (session-scoped-relevant) | Test fns | Turns driven (est.) | Turns load-bearing (est.) | Potentially prunable |
|---|---|---|---|---|---|
| `tidepool-harness` | 41 (+1 out-of-scope, `compile_fail.rs`) | ~146 | ~240 | ~165 | ~75 turns (~31%) |
| `tidepool-repl` | 27 | ~139 (+1 internal census, `batch_turns_spawn_census`) | ~430 (+~20 in the census) | ~260 | ~170 turns (~40%) |
| **Total** | **68** | **~285** | **~670** | **~425** | **~245 turns (~37%)** |

These are **turn counts**, not raw `tidepool-extract` spawns — the original
diagnosis measured 31/49 (63%) of `acceptance_cross_turn.rs`'s spawns and
22/31 (71%) of `batch_turns_spawn_census`'s spawns as session-scoped, at
~5.3s/spawn average. Taking that per-spawn cost as a rough per-turn proxy (a
turn is very close to 1:1 with a spawn once resume-continuations are excluded,
per the Method section above), **pruning ~245 turns projects to roughly
1,300s (~22 minutes) of serial GHC wall time removed from these two crates'
GHC-heavy tiers** — this is a *ceiling*, since it assumes every REDUCE/MERGE/
DEMOTE proposal below is taken and none of them re-add turns elsewhere; a
realistic take-up (see Top-5 below) is smaller but still substantial, and the
number is directional, not a re-measurement (nobody reran the instrumented
shim across all 68 files — see "What wasn't done" at the end).

**What this does NOT touch:** per `test-time-cut.md`'s item #3, the
session-scoped compile's *per-turn cost* (~5.3s, uncacheable by construction)
is untouched by every proposal here — these are all turn-COUNT reductions,
not turn-COST reductions. The only lever on cost itself is widening the
compile memo's argv allowlist to admit session state safely, which
`test-time-cut.md` already scoped as a separate, much larger design lane.

### Top-5 quick wins

1. **`value_fidelity.rs` (tidepool-repl, 15 tests, ~41 turns → ~20-24
   turns).** The large majority of its "bind a value of shape X, read it back
   3 ways" tests (Maybe/Either/JSON/list/nested-ADT/function-applied-at-bind/
   all 5 ToWire tests) spread 2-4 independent `.eval()` calls across separate
   turns for facts that don't cross a turn boundary — a single multi-item
   `.run()` block proves the same thing in 1 turn. Only the genuinely
   cross-turn tests (`text_bind_with_prior_live_binding_diagnostic`,
   `closure_captures_binding_survives_gc`, `list_bind_survives_gc`) must stay
   multi-turn. Projected: ~17-20 turns removed, zero property loss, no code
   risk (test-only change).

2. **`multi_binder.rs` (tidepool-repl, 5 tests, 18 turns → ~9-10 turns).**
   `tuple_bind_both_components` alone runs 8 turns where 3 prove the same GC-
   survival claim (pre-GC checks combine to 1 turn, the GC-forcing turn must
   stand alone, post-GC reads combine to 1 turn); `let_single_bind`/
   `let_tuple_works`/`three_tuple_works` make no cross-turn claim at all and
   are two turns each for a one-block proof. Largest single-file over-
   decomposition ratio found in the review.

3. **`shadow_rebind.rs` (tidepool-repl, 11 tests, 49 turns, heaviest file in
   the repl suite).** Not a blanket reduction candidate — most of its tests
   are genuinely about turn-to-turn rebind semantics — but
   `bindings_after_rebind_lists_once`'s `:bindings` assertion is pure Rust
   (`iter_current()` dedup over an in-memory map) and could drop its 1 free
   meta-read entirely as a pure unit test with no session at all;
   `redefine_type_old_binding_orphaned_gracefully`'s pre-redefine baseline
   check (3rd of 8 turns) is incidental scaffolding, not load-bearing to its
   3 stated claims. Small win (~2 turns) but flagged because this file is the
   single biggest time sink in the repl suite and deserves a closer pass.

4. **`selfharness_lifecycle.rs` (tidepool-harness) — MERGE.**
   `errored_cycle_leaves_lifecycle_failed_and_next_cycle_recovers` and
   `retired_answerer_nodes_are_terminal_across_cycles` drive the *identical*
   `FlakyProvider(fail_first=1)`-then-recover, 2-cycle shape and differ only
   in which assertion they check (lifecycle state vs. tree-node terminality).
   One execution, two assertion blocks. Saves 1 real turn (the second test's
   compiling cycle) at zero property loss.

5. **`decl_plane_run_scoping.rs`'s sibling probe pattern, extended:**
   `state_injection_memo_hit.rs`'s `second_cycle_outer_compile_is_a_memo_hit_
   with_fresh_state` drives 2 real session-scoped answerer turns to set up a
   property that is explicitly about the OUTER (non-session-scoped) fused
   `render+loop` compile's memo-hit behavior — it could call
   `SelfHarnessDriver::compile_loop_entry` (or the equivalent lower-level
   fused-compile function) directly with two hand-built state JSONs instead
   of driving the answerer through `ReplayProvider`. Removes 2 of the test's
   4 total compiles while proving the identical memo-hit claim — this is
   exactly the "standalone PROBE compile" pattern several other tests in the
   same crate (`agent_stack_scoping.rs`, `finalize_type_pinning.rs`,
   `dogfood_harness_typecheck.rs`) already use correctly.

**Explicitly NOT a quick win, called out per the boundary's warning:**
`acceptance_cross_turn.rs`'s 5-test decl-salvage family (error-in-first-item,
ending-in-decl, decl-before-failure, decl-after-failure, decl-then-typed-use-
failure) is the single biggest overlap CLUSTER in the harness suite (21 turns
across 10 tests total in the file), but each of the 5 pins a genuinely
distinct regression per its own doc comment — reducing THEIR turn counts is
possible in 2 of 5 cases (see the per-test table) but merging them would lose
real coverage. `answerer_async_fork.rs`'s 13 tests (~46 turns, the single
largest turn count in either crate) are almost entirely genuine multi-child/
multi-cycle acceptance tests for real concurrency, budget, and rotation
properties — turn count IS the property in the large majority of them.

---

## Catalog: `tidepool-harness/tests/`

### `acceptance_askuser.rs`, `acceptance_boot_compile_count.rs`, `acceptance_cross_turn.rs`, `acceptance_finalize.rs`, `acceptance_lazy_boot.rs`, `acceptance_multi_target.rs`, `acceptance_selfharness.rs`

| File | Test | Property | Session-scoped | Turns (driven→LB) | Verdict |
|---|---|---|---|---|---|
| acceptance_askuser.rs | `askuser_operator_form_round_trip_and_ws4_log` | note→askUser(reprompt)→chooseMany→finalize round trip, FormShape derivation, WS4 log | Y | 2→2 | KEEP AS-IS |
| acceptance_askuser.rs | `root_maybe_form_shape_and_decode_round_trip` | root `Maybe Text` derives `OptionalShape`, decodes both arms | Y | 2→2 (could shrink to a bare `run_to_hole_or_done` skipping loop-entry/finalize scaffolding) | REDUCE TURNS — drop the full self-iterating-loop scaffold, drive a bare root turn |
| acceptance_askuser.rs | `choose_with_no_options_fails_loud_before_suspending` | `choose []` fails before ever suspending | Y | 1→1 | KEEP AS-IS |
| acceptance_askuser.rs | `prd_example_adts_compile_with_the_bare_derive_contract` | example ADTs w/ vendored generic FromJSON compile | N (direct probe) | 0 | KEEP AS-IS (already cheap layer) |
| acceptance_askuser.rs | `maybe_unit_field_is_a_compile_error_naming_the_field` | `Maybe ()` field rejected, names the field | N | 0 | KEEP AS-IS |
| acceptance_boot_compile_count.rs | `boot_pays_pre_model_extract_compiles_matching_baseline` | pins `PRE_MODEL_EXTRACT_COMPILES`=2, cold memo forced | boot-only, not a driven turn | 0 turns (2 boot spawns) | KEEP AS-IS — purpose-built spawn-count receipt, explicitly not a consolidation target |
| acceptance_cross_turn.rs | `a_declaration_persists_into_the_next_turn` | decl in turn1 resolves in turn2 | Y | 2→2 | KEEP AS-IS — canonical cross-turn minimum |
| acceptance_cross_turn.rs | `multi_block_reply_runs_in_order_and_fails_with_resume_point` | multi-block ordering + corrective salvage | Y | 5→5 (all 3 rounds needed for the combined claim) | KEEP AS-IS |
| acceptance_cross_turn.rs | `multi_item_block_decl_then_expr_completes_in_one_reply` | one block, decl+expr, one spawn family | Y | 1→1 | KEEP AS-IS |
| acceptance_cross_turn.rs | `multi_item_block_contiguous_binds_persist_across_rounds` | GHCi-style contiguous binds split+persist across a turn | Y | 2→2 | KEEP AS-IS — genuinely cross-turn |
| acceptance_cross_turn.rs | `single_item_turn_still_costs_one_extract_spawn` | pins: single-item block = 1 spawn, no pre-pass | Y | 1→1 | KEEP AS-IS — purpose-built spawn-count pin |
| acceptance_cross_turn.rs | `multi_item_block_error_in_first_item_stops_and_corrects` | item-1 failure stops block, item-2 never runs | Y | 2→2 | KEEP AS-IS |
| acceptance_cross_turn.rs | `multi_item_block_ending_in_decl_retries_and_survives` | block ending in bare decl is compile-class, retries | Y | 2→2 | KEEP AS-IS — but its "last item is Decl → reject" half is a `classify_block`-verdict check, DEMOTABLE if a narrower unit exists |
| acceptance_cross_turn.rs | `multi_item_block_decl_before_failing_item_is_named_kept` | decl before a failing item survives, named "kept" | Y | 2→1 | REDUCE TURNS (2→1) — only round 1 proves the stated naming claim; round 2's corrective is incidental (the naming is checked from reply1's own corrective text) |
| acceptance_cross_turn.rs | `multi_item_block_salvages_decl_after_earlier_item_fails` | later decl still commits after an earlier item fails (THE fix) | Y | 2→2 | KEEP AS-IS — round 2 proves the salvaged binding actually resolves |
| acceptance_cross_turn.rs | `multi_item_block_decl_then_failing_typed_use_persists_decl` | a TYPE decl (not value) survives into corrective | Y | 2→2 | KEEP AS-IS — least redundant of the salvage family (only one testing a type decl) |
| acceptance_finalize.rs | `finalize_hands_up_a_plain_data_value` | Finalize routing/termination/double-take mechanics | Y | 1→1 | KEEP AS-IS |
| acceptance_finalize.rs | `finalize_accepts_function_typed_site_where_runllmturn_rejects_it` | extract's relaxed function-arrow rule | N | 0 | KEEP AS-IS |
| acceptance_finalize.rs | `finalize_closure_crosses_by_reference` | closure crosses by reference, not deep-forced | Y | 1→1 | KEEP AS-IS |
| acceptance_finalize.rs | `finalize_closure_full_round_trip` | full arithmetic round trip through a finalized closure | Y | 1→1 | **FLAG, separate from turn-count**: doc comment says "IGNORED — boxed-arg application case-traps" but the test has no `#[ignore]` and asserts success — likely doc/code drift, worth a real CI run to confirm before touching turns |
| acceptance_lazy_boot.rs | `no_machine_after_force_a_machine_after_the_first_turn` | lazy boot: no machine until first real turn | Y | 1→1 | KEEP AS-IS |
| acceptance_lazy_boot.rs | `outer_session_boots_from_pure_render_then_loop_suspends_on_a_real_hole` | outer session lazy-boots off `render`'s pure fragment, then a real `runLLMTurn` suspends on the SAME machine | Y | 1 (one `run_one_loop_iteration`)→1 | KEEP AS-IS — file's own doc explains why this is NOT redundant with other single-cycle tests (pins the `collectTransitiveDCons` 5-constructor closure invariant specifically) |
| acceptance_multi_target.rs | `multi_target_fails_on_any_bad_target` | all-or-nothing multi-target compile | N (direct `compile_turns`) | 0 | KEEP AS-IS — purpose-built batching-mechanism test |
| acceptance_multi_target.rs | `multi_target_asks_stay_distinct` | per-target asks stay distinct | N | 0 | KEEP AS-IS |
| acceptance_selfharness.rs | `selfharness_multi_cycle_state_accumulates_across_loop_boundaries` | State accumulates across 2 repeated loop boundaries | Y | 2→2 | KEEP AS-IS — genuinely tests turn-to-turn accumulation |
| acceptance_selfharness.rs | `machine_rotation_between_cycles_preserves_durable_state` | machine rotation at fragment ceiling preserves state | Y | 2→2 | KEEP AS-IS — rotation is the property |

**Note:** `selfharness_spine.rs` (catalogued below) is a near-identical
"one render→loop→finalize→render cycle" fixture to `acceptance_selfharness.rs`'s
own baseline shape — flagged there, not here, since it's a different file.

### `agent_stack_scoping.rs`, `decl_plane_run_scoping.rs`, `companion_collapsed_slice.rs`, `companion_mount_spike.rs`, `companion_scope_trees.rs`, `delegate_positive_path.rs`, `delegate_type_pinning.rs`, `answerer_async_fork.rs`

| File | Test | Property | Session-scoped | Turns (driven→LB) | Verdict |
|---|---|---|---|---|---|
| agent_stack_scoping.rs | (9 tests: `run_llm_turn_is_a_member_error...`, `tidepool_harness_module_is_importable...`, `fork_child_leaf_row_cannot_fork`, `finalize_compiles_in_the_answerer_stack`, `askuser_raw_compiles_in_the_answerer_stack`, `noteraw_and_getstatejson_compile...`, `fork_all_compiles_in_the_answerer_stack`, `ask_is_a_compile_error_in_the_answerer_stack`, `base_effect_is_a_compile_error_in_the_answerer_stack`, `ask_is_a_compile_error_in_the_harness_stack`, `finalize_is_a_compile_error_in_the_harness_stack`, `run_llm_turn_compiles_in_the_harness_stack`) | structural effect-row scoping (answerer vs. harness-only stack), all via direct `compile_against`/`compile_pinned` probes, no `Harness` | N — every test | 0 (all N) | KEEP AS-IS — this whole file is already the correctly-cheap PROBE pattern; a model for others to copy |
| decl_plane_run_scoping.rs | `concurrent_harnesses_do_not_delete_each_others_decl_plane` | F3: two `Harness` instances sharing a cache root must not delete each other's node-0 decl plane | Y | 4 (A:3 turns + B:1 turn)→4 | KEEP AS-IS — the property is literally about cross-harness, cross-turn collision; every turn is load-bearing |
| companion_collapsed_slice.rs | `seeded_turn_forks_and_stores_the_roots_typed_answer_as_is` | seeded turn forks 2 children, root's typed answer stored as-is | Y | 1 cycle→1 | KEEP AS-IS |
| companion_collapsed_slice.rs | `fresh_boot_seeds_the_question_through_the_operator_gate` | fresh boot presents exactly one seed-gate form, then turn 1 | Y | 1 cycle→1 | KEEP AS-IS |
| companion_mount_spike.rs | `mounted_closure_survives_retirement_and_suspension` | C1 mount spike: closure crosses window retirement + a GC-risk suspend/resume | Y | 3 (P, Z, C)→3 | KEEP AS-IS — each of P/Z/C is a distinct, necessary step in the crown-jewel sequence |
| companion_scope_trees.rs | `locked_decision_4_holds_through_the_real_compile_path` | all 4 name-visibility clauses of locked decision 4, as real compiled values | Y | 7→7 | KEEP AS-IS — the exact ORDER of 7 turns (mint both siblings before either defines, etc.) IS the proof; not compressible |
| companion_scope_trees.rs | `escaped_closure_outlives_its_childs_window_and_scope` | crown jewel: a mounted closure survives its producing child's scope+realm retirement | Y | 4 (producer, placeholder, child-worker, consumer)→4 | KEEP AS-IS — every node is a distinct accounting step the acceptance depends on |
| companion_scope_trees.rs | `multiple_mounts_in_one_window` | two mounts (Toolkit + Focus) live in ONE scope, every field called separately | Y | 5 (2 producers + 2 placeholders + 1 consumer)→5 | KEEP AS-IS — "every field called separately" is explicitly the point (a sentinel bug would only surface per-field) |
| delegate_positive_path.rs | `root_session_delegates_and_finalizes_on_the_result` | full `delegate` saga through MockBackend, typed result returns inline | Y | 1 cycle→1 | KEEP AS-IS |
| delegate_positive_path.rs | `direct_subagent_send_dispatches_within_the_answerer_row` | isolating control: same row, direct `send` instead of `delegate`'s reinterpret | Y | 1 cycle→1 | KEEP AS-IS — deliberate control, not redundant with the test above |
| delegate_type_pinning.rs | (5 compile-probe tests: `delegate_call_compiles_against_the_narrow_row`, `annotated_m_type_compiles_against_the_narrow_row`, `plain_finalize_still_compiles_under_the_wrap`, `answerer_turn_combining_fork_and_delegate_compiles...`, `note_getstatejson_and_askuserwith_all_compile...`, `raw_worktree_and_subagent_are_unnameable`, `worktree_verb_compiles_when_the_row_actually_carries_worktree`) | row-truth/unnameability compile-level acceptance | N (all direct `compile_delegating_turn`/`compile_delegating_answerer_turn`) | 0 (all N) | KEEP AS-IS — correctly cheap already |
| delegate_type_pinning.rs | `data_declaration_defines_and_is_usable_in_a_delegating_window` | a top-level `data` decl defines and persists into a later turn in a delegating window | Y | 2→2 | KEEP AS-IS — genuinely cross-turn |
| delegate_type_pinning.rs | `recursive_companion_harness_source_loads` | shipped harness source still typechecks | N | 0 | KEEP AS-IS |
| answerer_async_fork.rs | `async_fork_composition_two_children_typed_results_cross` | 2 async-forked children, typed results cross to the right handle | Y | 3→3 | KEEP AS-IS |
| answerer_async_fork.rs | `async_fork_overlap_two_children_drive_concurrently` | genuine concurrency (not sequential) — `max_concurrent()>1` is the receipt | Y | 3→3 | KEEP AS-IS — the LATENCY property, distinct from the composition test above |
| answerer_async_fork.rs | `two_waves_of_fork_fold_fork_carry_results_across_waves` | fork→fold→fork within one block; wave 2's brief is computed from wave 1's fold | Y | 4→4 | KEEP AS-IS |
| answerer_async_fork.rs | `fork_child_that_forks_is_depth_refused_and_recovers` | depth-1 refusal + recovery | Y | 3→3 | KEEP AS-IS |
| answerer_async_fork.rs | `two_level_fork_chain_succeeds_at_default_caps` | 2-level fork chain succeeds at default caps | Y | 3→3 | KEEP AS-IS |
| answerer_async_fork.rs | `second_fork_past_subtree_cap_refuses_with_tree_wide_corrective` | subtree cap=1 refuses 2nd direct fork, tree-wide corrective | Y | 3→3 | KEEP AS-IS |
| answerer_async_fork.rs | `async_fork_over_subtree_cap_refuses_with_tree_wide_corrective` | F8 regression: SAME subtree refusal via the async path, must carry tree-wide wording not per-window | Y | 3→3 | KEEP AS-IS — distinct code path from the direct-fork subtree test above (async servicing arm) |
| answerer_async_fork.rs | `async_fork_over_budget_refuses_loudly_and_window_survives` | per-window fork budget refusal, window survives | Y | 3→3 | KEEP AS-IS |
| answerer_async_fork.rs | `settled_threads_leave_the_machine_quiescent_for_rotation` | F1: a settled green thread's realm must close, machine stays quiescent for rotation | Y | 6 (2 cycles × 3)→6 | KEEP AS-IS — rotation across 2 cycles IS the property |
| answerer_async_fork.rs | `async_fork_child_round_exhaustion_aborts_block_with_corrective_and_run_survives` | starved async child aborts the block, run survives, sibling unaffected | Y | 3 compiling (+3 free NoBlock rounds)→3 | KEEP AS-IS |
| answerer_async_fork.rs | `fork_child_failure_abort_still_leaves_machine_quiescent_for_rotation` | rotation survives a fork-child-failure abort (not just a success) | Y | 6 (cycle1: 3 compiling +3 free; cycle2: 3)→6 | KEEP AS-IS — complements the settle-only rotation test above; failure-path rotation is a distinct code path |
| answerer_async_fork.rs | `fork_child_asks_route_to_its_own_derived_gate_and_finalizes` | fork child gets its own derived per-node GUI gate, seeds/finalizes/retires correctly | Y | 3→3 | KEEP AS-IS — folded in from a retired standalone file (`fork_child_gui.rs`) on purpose, saving a binary |
| answerer_async_fork.rs | `overlapping_fork_children_journal_replays_clean_through_checkpoint_resume` | interleaved concurrent-writer log folds cleanly; checkpoint survives a fresh-process restore | Y | 3 (+0 for the fresh restore-only driver)→3 | KEEP AS-IS |

### `dogfood_harness_typecheck.rs`, `dogfood_observability.rs`, `finalize_type_pinning.rs`, `fork_child_decl_plane_type.rs`, `minimal_watch_list.rs`, `nested_async_repro.rs`, `node_mailboxes.rs`, `outer_effects.rs`

| File | Test | Property | Session-scoped | Turns (driven→LB) | Verdict |
|---|---|---|---|---|---|
| dogfood_harness_typecheck.rs | `companion_typechecks`, `dev_tree_typechecks`, `recursive_companion_typechecks` | 3 shipped harnesses' `Harness.hs` typecheck against the real outer row | N (direct `compile_turn`) | 0 each | KEEP AS-IS |
| dogfood_harness_typecheck.rs | `dev_tree_resume_decisions_execute` | `resumePlanFor`/`amendmentIsNewest` precedence over 12 cases, on real JIT | N (`compile_and_run_pure`) | 0 | KEEP AS-IS — model example of the cheap-layer pattern (12 cases, 1 compile) |
| dogfood_harness_typecheck.rs | `dev_tree_child_allowance_never_overspends_parent_cap` | `childAllowance`'s `max 0` fix | N | 0 | KEEP AS-IS |
| dogfood_harness_typecheck.rs | `dev_tree_journal_event_round_trips` | 10 journal event shapes round-trip to legacy wire JSON | N | 0 | KEEP AS-IS |
| dogfood_observability.rs | `narration_and_transcript_fold_both_hold_for_one_retry_cycle` | console narration + `transcript.jsonl` telemetry fold for a 1-retry cycle | Y | ~3→2 | DEMOTE TO CHEAPER LAYER (partial) — the telemetry-fold half (`fold_answerer_rounds`/`first_compile_success_rate`/`retries_per_hole`) reads `transcript.jsonl` TEXT only; a hand-authored fixture JSONL proves it with zero compiles. Keep 1 real turn for the narration-content half |
| finalize_type_pinning.rs | (8 tests: `correctly_typed_finalize_still_compiles_when_pinned`, `wrong_typed_finalize_is_a_compile_error`, `wrong_typed_finalize_compiles_when_the_row_names_text`, `pinned_finalize_needs_the_type_in_scope`, `answer_contract_puts_the_type_in_scope`, `author_module_edit_between_compiles_is_picked_up_by_the_second`, `prompts_prescribed_finalize_typed_request_prompt_shape_compiles_when_pinned`, `bare_finalize_with_no_annotation_compiles_when_pinned`, `bare_non_bind_askuser_form_compiles_when_pinned`) | Finalize row-pinning acceptance, all direct probes | N — every test | 0 (all N) | KEEP AS-IS — correctly the cheap layer already; `unresolvable_pinned_type_fails_with_the_not_in_scope_plus_type_name_shape` (in `fork_child_decl_plane_type.rs`, below) is this file's own sibling pattern extended |
| fork_child_decl_plane_type.rs | `fork_child_answer_type_declared_as_decl_plane_alias_resolves` | fork child's answer type as a decl-plane `type` alias resolves end-to-end | Y | 4→4 | KEEP AS-IS — needs decl-round + fork-round + child's own probe against session decl-plane together |
| fork_child_decl_plane_type.rs | `fork_child_answer_type_declared_as_decl_plane_data_resolves` | same, for a decl-plane `data` decl | Y | 4→4 | KEEP AS-IS — parallel, not duplicate (different extract-side resolution path: alias vs. own-module data) |
| fork_child_decl_plane_type.rs | `fork_naming_a_never_declared_type_corrects_the_parent_and_survives` | unresolvable fork type fails as an ordinary corrective (not process-fatal) | Y | 3→3 | KEEP AS-IS — the driver's corrective-fold behavior isn't covered by the unit-level sibling below |
| fork_child_decl_plane_type.rs | `unresolvable_pinned_type_fails_with_the_not_in_scope_plus_type_name_shape` | unit-level pin: unresolvable pinned type fails with the exact detected string shape | N (`cfg.turn_target` direct) | 0 | KEEP AS-IS — this IS the cheap-layer precondition-check for the test above; good in-file example |
| minimal_watch_list.rs | `minimal_watch_list_round_trips` | smallest known repro: a list captured by `async` survives GC tenure+resume | Y | 1→1 | KEEP AS-IS — same GC-tenure bug family as `nested_async_repro.rs`/`node_mailboxes.rs`, each proving a distinct composition |
| nested_async_repro.rs | `a_green_thread_body_can_fork_another_green_thread` | nested `async` inside a green thread body, no GC-forwarding corruption | Y | 1→1 | KEEP AS-IS |
| node_mailboxes.rs | `a_parent_selects_over_message_and_deadline` | mailbox select over {message, deadline}, 3 scenarios | Y (architecturally) | currently `#[ignore]`d (unrelated burst-coalesce bug) — 0 executed | KEEP AS-IS once un-ignored — doc already states the verb-surface half is covered by `SubscriptionRegistry` unit tests; this file's unique value (end-to-end `forkNode`/`sendUp`) can't move lower |
| outer_effects.rs | `outer_loop_effects_round_trip_through_the_driver` | ONE compile proves the full outer effect row + green scheduler + select | Y | 1→1 | KEEP AS-IS — deliberate family-bundle baseline (many assertions, one compile) |
| outer_effects.rs | `outer_worktree_without_handler_errors_legibly` | missing Worktree handler → legible error, never a hang | Y | 1→1 | KEEP AS-IS |
| outer_effects.rs | `outer_journal_without_handler_errors_legibly` | missing Journal handler → legible error | Y | 1→1 | KEEP AS-IS — parallel to the Worktree test, different effect |
| outer_effects.rs | `resume_boot_fold_fresh_then_resumed_appends_only_the_delta` | fresh boot + resumed boot fold correctly, delta-only; refused-boot case pays 0 compiles | Y (a,b); N (c) | 2→2 | KEEP AS-IS — already demonstrates the ideal "prove refusal before paying for a compile" pattern in its own (c) case |

### `outer_fanout.rs`, `outer_subagent.rs`, `provider_behavior.rs`, `reinterpret_rowchange_repro.rs`, `selfharness_budget.rs`, `selfharness_compaction.rs`, `selfharness_compaction_fixes.rs`, `selfharness_context_window.rs`

| File | Test | Property | Session-scoped | Turns (driven→LB) | Verdict |
|---|---|---|---|---|---|
| outer_fanout.rs | `outer_fanout_children_serviced_concurrently_completion_order_insensitive` | completion order across 9 children never reaches observable output | Y | 20 (2 runs×10)→6 | REDUCE TURNS — file's own header frames the 9-wide fixture as SHARED-for-memo-economy; the property only strictly needs 2 re-ordered children × 2 runs + 2 fused compiles ≈ 6. The other 14 are deliberately shared fixture overhead — reducing them is a judgment call the operator should make explicitly, not a bug |
| outer_fanout.rs | `outer_fanout_respects_concurrency_cap` | peak concurrent provider calls with 9 children, cap=8, is exactly 8 | Y | 10→10 | KEEP AS-IS — cap+1 concurrent attempts is structurally required to prove both bounds |
| outer_fanout.rs | `outer_fanout_round_exhausted_child_folds_as_data_without_erasing_siblings` | one child's round exhaustion folds as Either data at its OWN position, siblings unaffected | Y | 9→~3 | REDUCE TURNS — minimally only 2 neighbor children (either side of the failing one) are needed to prove "both sides preserved"; same shared-fixture caveat as above |
| outer_subagent.rs | `outer_loop_spawn_agent_round_trips_through_the_driver` | full spawnAgent→Subagent suspension→saga→typed receipt→resume round trip | Y | 1→1 | KEEP AS-IS |
| outer_subagent.rs | `outer_spawn_without_handler_errors_legibly` | missing Subagent handler → legible error, no hang | Y | 1→1 | KEEP AS-IS |
| provider_behavior.rs | (12 tests: `api_key_provider_*` ×3, `oauth_provider_*` ×8, `oauth_token_lands_0600_under_config_dir_convention`) | ModelProvider-trait/HTTP-transport behavior against a local mock server | N — every test (no `Harness`, no compile at all) | 0 (all N) | KEEP AS-IS — already the cheapest layer; the project's own `.config/nextest.toml` already special-cases this binary as not GHC-heavy |
| reinterpret_rowchange_repro.rs | `reinterpret_handler_send_classifies_correctly` | JIT `reinterpret`-lowered `send` classifies correctly (PRD 21 C5 differential repro) | Y | 1→1 | KEEP AS-IS — deliberate isolating pair with the control below |
| reinterpret_rowchange_repro.rs | `direct_note_send_classifies_correctly` | isolating CONTROL: same `send` written directly, no `reinterpret` | Y | 1→1 | KEEP AS-IS — needed together with the test above to attribute any difference to `reinterpret` specifically |
| selfharness_budget.rs | `answerer_nudged_at_16_and_hard_fails_at_32` | nudge-then-hard-fail glide, caps deliberately LOWERED (3/6) to keep this cheap | Y | 9→9 | KEEP AS-IS — every round is essential to the exact nudge/hard-fail timing under test; caps are already minimized on purpose to avoid the production 34-round cost |
| selfharness_compaction.rs | `compaction_fires_mid_loop_in_place_and_reaches_next_render` | mid-loop compaction resets context in place, summary reaches the next render | Y | 3→3 | KEEP AS-IS |
| selfharness_compaction.rs | `c1_multiround_highwater_does_not_overcount` | threshold measure is last-turn HIGH-WATER, not a running sum | Y | 3→3 | KEEP AS-IS — DEMOTE candidate for a NARROWER unit (the high-water-vs-sum arithmetic itself is a pure function over `Usage` values) but the multi-round-shape realism still argues for keeping this integration test too |
| selfharness_compaction_fixes.rs | `c2_summarize_turn_counts_against_inference_cap` | the summarize turn's own call counts against the per-loop inference cap | Y | 2→2 | KEEP AS-IS |
| selfharness_compaction_fixes.rs | `checkpoint_commit_pairs_state_and_compaction_from_one_cycle` | State + compaction summary commit together in ONE checkpoint generation | Y | 3 (+0 for the pure restore half)→3 | KEEP AS-IS — the restore half is already free; the producing half needs a real compacting cycle |
| selfharness_compaction_fixes.rs | `c4_compaction_trigger_event_carries_payload` | `Event::CompactionTrigger` carries a populated payload after the summary exists | Y | 3→3 | KEEP AS-IS |
| selfharness_context_window.rs | `second_hole_sees_first_holes_exchange` | 2 sequential holes in ONE loop iteration share one accumulating transcript | Y | 3→3 | KEEP AS-IS — structurally needs 2 holes in the same iteration; already minimal |

**MERGE candidate flagged across these two compaction files:** per the
review, `CompactingProvider` (`selfharness_compaction_fixes.rs`) and
`InPlaceProbeProvider` (`selfharness_compaction.rs`) are near-identical
structs with verbatim-copied helper functions — a "kept-in-sync copies"
violation per the root `CLAUDE.md`'s Mechanism Index rule, independent of any
test-redundancy question. Worth a real fixture-sharing cleanup; does not by
itself change any turn count above.

### `selfharness_decl_plane_replay.rs`, `selfharness_fn_finalize_spike.rs`, `selfharness_framing.rs`, `selfharness_lifecycle.rs`, `selfharness_persistence.rs`, `selfharness_spine.rs`, `stable_effects_core_decl_plane.rs`, `state_injection_memo_hit.rs`, `timing_emission_pin.rs`, `turn_lease.rs`

| File | Test | Property | Session-scoped | Turns (driven→LB) | Verdict |
|---|---|---|---|---|---|
| selfharness_decl_plane_replay.rs | `decl_in_window_one_resolves_in_window_three_same_cycle` | a decl commits BEFORE the nudge error; visible 2 hole-boundaries later, same cycle | Y | 4→4 | KEEP AS-IS — file's own doc distinguishes this deliberately from the cross-CYCLE version in `selfharness_fn_finalize_spike.rs` |
| selfharness_fn_finalize_spike.rs | `fn_finalize_crosses_two_cycles_and_composes` | closure-valued finalize crosses via handle, composes across cycles | Y | 3→2 | REDUCE TURNS (3→2) — cycles 1-2 prove the compose claim; cycle 3 is a weaker stability/no-poison check on repeated retirement |
| selfharness_fn_finalize_spike.rs | `turn_record_delivers_directive_list_beside_closure` | data+closure MIX in one value routes as a whole via handle delivery | Y | 1→1 | KEEP AS-IS — distinct payload shape from siblings |
| selfharness_fn_finalize_spike.rs | `record_of_functions_crosses_and_both_fields_apply` | a record of TWO closures crosses and composes across 2 cycles | Y | 2→2 | KEEP AS-IS |
| selfharness_fn_finalize_spike.rs | `living_helper_survives_loop_boundary_and_rotation` | named decl-plane helper survives the loop boundary AND a forced rotation | Y | 3→3 | KEEP AS-IS — rotation-survival IS turn 3's whole point |
| selfharness_fn_finalize_spike.rs | `ooda_pipeline_conditional_phases` | one loop fragment drives 1-3 sequential typed windows per cycle, model-chosen shape, 3 cycles cover 3 tempos | Y | 6→6 | KEEP AS-IS — 3-way branch coverage is the property; largest single-test turn count outside `answerer_async_fork.rs`/`selfharness_persistence.rs`, justified |
| selfharness_framing.rs | `render_output_is_the_answerer_system_message` | answerer's System message IS `render`'s output, advertises only its own row | Y | 1→0 | DEMOTE TO CHEAPER LAYER — pure STRING content of an assembled `TurnRequest`; a unit test of the request-assembly/framing function directly (fixture render text + decl list) proves this with zero compiles |
| selfharness_lifecycle.rs | `errored_cycle_leaves_lifecycle_failed_and_next_cycle_recovers` | errored cycle publishes `Failed`, not cosmetic `Idle`; next cycle re-bootstraps | Y | 1 (cycle2 only; cycle1 fails pre-compile)→1 | MERGE WITH `retired_answerer_nodes_are_terminal_across_cycles` — identical `FlakyProvider(fail_first=1)` 2-cycle driving shape, differ only in assertion |
| selfharness_lifecycle.rs | `fresh_driver_bootstrap_failure_is_failed_not_idle` | a fresh driver's bootstrap failure is `Failed`, not `Poisoned` | Y | 1→1 | KEEP AS-IS |
| selfharness_lifecycle.rs | `poisoned_driver_refuses_entry_points` | recovery-from-Failed that ALSO fails escalates to `Poisoned`, refuses every entry point | N/A (no compile ever succeeds) | 0→0 | KEEP AS-IS — deliberately failure-only, already free |
| selfharness_lifecycle.rs | `retired_answerer_nodes_are_terminal_across_cycles` | ghost-node hazard: node must terminalize on both a failed AND successful cycle | Y | 1 (cycle2 only)→1 | MERGE WITH `errored_cycle_leaves_lifecycle_failed_and_next_cycle_recovers` (see above) — combined execution saves 1 compiling turn |
| selfharness_persistence.rs | `restart_with_a_prior_checkpoint_gates_before_any_turn_work` | uniform-restart-gate: restored checkpoint gates BEFORE any turn work | Y | 2→2 | KEEP AS-IS |
| selfharness_persistence.rs | `old_format_checkpoint_still_gates_before_the_first_post_restore_cycle` | a pre-unification checkpoint format still gates correctly | Y | 1→1 | KEEP AS-IS |
| selfharness_persistence.rs | `operator_steering_text_reaches_the_next_cycles_framing` | between-loops steering text reaches the VERY NEXT cycle's framing | Y | 2→2 | KEEP AS-IS |
| selfharness_persistence.rs | `ask_ids_strictly_increase_across_a_restart` | AskId counter seeds from checkpoint high-water, never resets | Y | 2→0 | DEMOTE TO CHEAPER LAYER — id-seeding is pure Rust arithmetic on `Checkpoint`; a unit test constructing a `Checkpoint` with a given high-water mark proves the seeding with no askUser round-trip or GHC at all |
| selfharness_persistence.rs | `committed_cycles_restore_state_and_summary_from_the_same_generation` | state+summary+iteration commit per-generation together; a failed 3rd cycle doesn't corrupt the last good checkpoint | Y | 2→2 | KEEP AS-IS — well-designed, proves a lot per turn, not padded |
| selfharness_persistence.rs | `crash_before_cycle_commits_restores_prior_generation_not_a_mixed_pair` | a mid-cycle crash never leaks a newer in-memory summary paired with older committed state | Y | 3→3 | KEEP AS-IS — all 3 needed to reach a genuine mid-cycle in-memory mutation before crash |
| selfharness_persistence.rs | `truncated_checkpoint_is_a_typed_error_and_writes_leave_no_tmp_behind` | corrupted checkpoint → typed error; atomic write leaves no `.tmp` | N | 0 | KEEP AS-IS — already cheapest layer |
| selfharness_persistence.rs | `default_checkpoint_path_is_under_the_cache_dir` | default path stable, non-empty, under cache dir | N | 0 | KEEP AS-IS |
| selfharness_persistence.rs | `stale_fingerprint_state_carries_forward_and_falls_back_on_decode_failure` | fingerprint-mismatched checkpoint's state carries forward for the decode attempt | Y | 1→1 | KEEP AS-IS — paired/complementary with the test below, not redundant |
| selfharness_persistence.rs | `state_decode_failure_retries_once_from_fresh_state_instead_of_killing_run_loop` | undecodable State retries exactly once from fresh state, not a crash | Y | 1→1 | KEEP AS-IS |
| selfharness_persistence.rs | `crashed_cycle_keeps_its_lease_and_the_resumed_run_does_only_the_delta` | crash-resume acceptance: durable journal, unretired lease, delta-only resume, retire-then-mint on completion | N (outer-fragment compiles only, non-session-scoped) | 0 session-scoped | KEEP AS-IS — comprehensive by design; complements the pure-Rust journal tests below deliberately (E2E + unit, a good pattern) |
| selfharness_persistence.rs | `torn_final_line_folds_to_its_complete_entries_and_leaves_its_step_undone` | a torn final journal line is skipped, not fatal | N | 0 | KEEP AS-IS |
| selfharness_persistence.rs | `torn_line_before_the_last_fails_the_boot_loudly` | non-final corruption fails loudly, names file+line | N | 0 | KEEP AS-IS |
| selfharness_persistence.rs | `a_torn_tail_never_poisons_a_later_boot_through_the_driver_seam` | segments let arbitrarily many further boots (5 tested) fold cleanly past the same tear | N | 0 | KEEP AS-IS — extends the single-boot test above; proves something it structurally cannot |
| selfharness_persistence.rs | `boot_fold_is_idempotent_and_sensitive_to_the_segments_physical_order` | driver-seam fold agreement: idempotent, order-sensitive by physical position not `seq` | N | 0 | KEEP AS-IS — own docstring notes core idempotence is pinned elsewhere; this test's unique value is the driver-seam agreement |
| selfharness_spine.rs | `selfharness_spine_one_cycle_render_loop_finalize_render` | one full render→loop→finalize→render cycle, nested ADT crosses, state persists, iteration advances | Y | 1→1 | **FLAG for cross-check, not a firm verdict**: near-identical "one cycle / examples-harness / Decision-Confidence" fixture to `acceptance_selfharness.rs`'s own baseline cycle and to `selfharness_framing.rs` — if genuinely subsumed by `acceptance_selfharness.rs`'s golden path, DELETE (covered by `acceptance_selfharness.rs`); if this is the crate's designated canonical spine test (name suggests so), KEEP AS-IS. Needs an operator/maintainer call, not a mechanical one |
| stable_effects_core_decl_plane.rs | (6 tests: `member_form_effectful_helper_validates...`, `m_form_effectful_helper_validates_and_persists`, `m_form_effectful_helper_visible_in_a_later_window...`, `unannotated_effectful_helper_still_validates...`, `helper_missing_from_a_later_narrow_row_fails_at_use_not_define`, `pure_helper_still_validates_and_persists`) | stable-Core decl-plane validation, direct `SessionLib::define` probes | N — every test | 0 (all N) | KEEP AS-IS — file's own doc frames itself as the surgical replacement for a full driver-cycle test that was tried and removed for hitting an unrelated bug; exactly the right layer |
| state_injection_memo_hit.rs | `second_cycle_outer_compile_is_a_memo_hit_with_fresh_state` | outer fused render+loop compile is a genuine memo HIT on cycle 2, despite different State | Mixed: 2 session-scoped answerer turns drive realism; the MEASURED property (outer compile) is explicitly non-session-scoped | 4 total (2 Y + 2 N)→2 (the 2 N compiles) | DEMOTE TO CHEAPER LAYER — call `SelfHarnessDriver::compile_loop_entry` (or equivalent) directly with 2 hand-built state JSONs, skip the `ReplayProvider`/answerer turns entirely; removes the 2 session-scoped turns while proving the identical memo-hit claim |
| timing_emission_pin.rs | (3 tests: `record_stage_emits_the_pinned_target_message_and_field_shape`, `record_stage_renders_real_node_and_round_as_decimal`, `record_extract_phases_forwards_each_phase_under_the_extract_prefix`) | `timing::record_stage`'s tracing shape is pinned | N — every test | 0 | KEEP AS-IS — already the cheap layer, exactly the pattern others should follow |
| turn_lease.rs | `concurrent_drive_turn_on_one_node_serializes` | per-node turn lease serializes the full snapshot→provider→compile→run→publish span | Y | 1→1 | KEEP AS-IS — needs the real compile to prove the WHOLE span is guarded, not just lease acquisition |
| turn_lease.rs | `a_failed_turn_releases_its_lease` | a provider-level failure releases the lease, not stranding it | Y | 1→1 | KEEP AS-IS |
| turn_lease.rs | `panic_mid_turn_recovers_via_drop_or_reports_busy_never_no_session` | manually-held checkout reports `TurnInFlight`; `Drop` recovers to `Idle`, never wedged | Y | 1→0 | DEMOTE TO CHEAPER LAYER — the property is a registry-level `Drop for Checkout` mutation, pure Rust; test's OWN doc comment explicitly notes it chose the real-`Harness` layer over "a synthetic `FakeMachine`" on purpose — if such a `FakeMachine` twin exists in `tidepool_runtime::session::registry`'s own tests, this could demote; if not, this is the flag to build one |

---

## Catalog: `tidepool-repl/tests/`

### `ask_resume.rs`, `auto_verdict_dispatch.rs`, `bare_expr_retry_census.rs`, `batch_turns_spawn_census.rs`, `block_value_semantics.rs`, `cancel_lifecycle.rs`, `decl_plane.rs`

| File | Test | Property | Session-scoped | Turns (driven→LB) | Verdict |
|---|---|---|---|---|---|
| ask_resume.rs | `object_reply_is_extractable_value` | BUG-9: object `ask` reply delivers as a structured, optic-extractable Value | Y | 2→2 | KEEP AS-IS |
| ask_resume.rs | `scalar_reply_extracts_via_double` | scalar `ask` reply extracts via `_Double` | Y | 2→2 | KEEP AS-IS |
| ask_resume.rs | `invalid_reply_does_not_consume_continuation` | invalid reply doesn't consume the continuation; retry with valid reply succeeds | Y | 3→3 | KEEP AS-IS — 3 dispatches (suspend, bad resume, good resume) are each load-bearing to the retry claim |
| auto_verdict_dispatch.rs | `auto_expr_verdict_skips_declaration_probe` | codex review item 13: an `Auto`-classified `Expr` verdict skips the doomed decl probe | Y | 1→1 | KEEP AS-IS — needs a real turn through a logging extract wrapper to observe actual spawn argv |
| bare_expr_retry_census.rs | `pure_bare_expr_costs_two_turn_compiles` | mechanism pin: a PURE bare expr costs 3 spawns (1 classify + 2 compiles: doomed-monadic then real-pure) | Y | 1→1 | KEEP AS-IS — measurement test, spawn count IS the assertion |
| bare_expr_retry_census.rs | `monadic_bare_expr_costs_one_turn_compile` | a MONADIC bare expr costs 2 spawns (no retry needed) | Y | 1→1 | KEEP AS-IS |
| bare_expr_retry_census.rs | `monadic_bare_expr_effect_actually_runs` | a bare monadic expr's effect actually RUNS, not just typechecks | Y | 2→2 | KEEP AS-IS — the 2nd turn (separate `kvGet`) is what proves the effect actually landed |
| batch_turns_spawn_census.rs | `spawn_census_per_item_shape` | measures real spawn counts across 6 item shapes (decl, 3-decl batch, pure bind, effectful bind, bare expr, mixed 5-item) | Y (mixed) | 1 nextest test, ~6 shapes, ~31 real spawns internally per the original diagnosis | KEEP AS-IS — purpose-built measurement test; explicitly not a correctness assertion, would defeat its own purpose if consolidated |
| block_value_semantics.rs | `stub_item_after_truncating_expr_keeps_its_own_value` | F1: `:stub 0` after a truncating expr keeps its OWN value (not stripped by unrelated dedup) | Y | 1 block (2 items, 1 turn)→1 | KEEP AS-IS |
| block_value_semantics.rs | `block_ending_in_bind_leaves_top_level_value_null_no_duplication` | F2: a block ending in a bind leaves top-level `value` null, no leak from an earlier expr | Y | 1→1 | KEEP AS-IS |
| block_value_semantics.rs | `block_ending_in_expression_populates_top_level_and_suppresses_item_value` | control: a block ending in a bare expr still populates top-level value correctly | Y | 1→1 | KEEP AS-IS — good-path control for the F1/F2 fix above |
| cancel_lifecycle.rs | `cancel_midturn_preserves_heap_and_unwedges` | dropping a turn's RPC future mid-compile doesn't wedge the session or lose heap state | Y | 3 (bind, dropped turn, recovery)→3 | KEEP AS-IS |
| cancel_lifecycle.rs | `cancel_does_not_poison_next_turn` | a cancel flag doesn't poison the NEXT turn | Y | 3 (warmup, cancelled, recovery)→3 | KEEP AS-IS |
| cancel_lifecycle.rs | `cancel_during_resume_preserves_session` | cancelling `session_resume` mid-turn recovers the same way as `session_run` | Y | 3 (ask, cancelled resume, recovery)→3 | KEEP AS-IS — the 2nd `drive_detached` call site, genuinely distinct code path from the test above |
| decl_plane.rs | `defs_accumulate_and_interact` | multiple defs accumulate and interact across turns | Y | 3→3 | KEEP AS-IS |
| decl_plane.rs | `forward_reference_across_turns_poisons` | a forward reference across turns is rejected at define-time, no poison | Y | 4→4 | KEEP AS-IS |
| decl_plane.rs | `mutual_reference_single_turn_works` | mutual reference WITHIN one turn works (contrast to the test above) | Y | 2→1 | REDUCE TURNS (2→1) — no cross-turn claim; def+eval could be one multi-item block |
| decl_plane.rs | `multicon_adt_value_and_case` | a multi-constructor ADT is a stable type across turns, both immediate and reference-path use | Y | 4→3 | REDUCE TURNS (4→3) — the def+immediate-use pair share no cross-turn dependency and could be one block; the bind+later-case-match pair is the genuine cross-turn half |
| decl_plane.rs | `type_alias_and_newtype` | type alias + newtype defined, both usable | Y | 4→2 | REDUCE TURNS (4→2) — both defs can share one block; both uses can share one block; no cross-turn claim anywhere |
| decl_plane.rs | `record_syntax_selectors_localized` | record selectors/case work on session-bound values across every path (pure, Eff-case, Eff-selector) | Y | 7→5 | REDUCE TURNS (7→5) — def+fresh-use combine (1); bind stands alone (cross-turn setup, 1); the 3 reference-path reads (ref_a/ref_b2/ref_b) are each independent and could combine into 1 block; the final "survive" pure-path check needs to stay a separate turn (proves recovery after the historical crash path) |
| decl_plane.rs | `record_selector_on_bound_value_via_eff_path` | Eff-path record selector on a session-bound value (the historical kind=4 crash) | Y | 3→3 | KEEP AS-IS — def, bind, and read are each a distinct facet of the regression; MERGE candidate with `record_syntax_selectors_localized`'s `ref_b` case (same assertion, narrower scope) — flagged, not forced |
| decl_plane.rs | `class_instance_describe` | class+instance define, methods visible via `(..)` export fix | Y | 4→2 | REDUCE TURNS (4→2) — the 3 defs (class, data, instance) share no cross-turn dependency and can be one block; the use is a 2nd turn |
| decl_plane.rs | `class_instance_poisons_until_reset` | class+instance compile; an UNRELATED eval after is not poisoned | Y | 5→3 | REDUCE TURNS (5→3) — same 3-def-combine as above, plus the describe-use and no-poison-use stay 2 separate turns (both needed: the no-poison claim needs a turn genuinely AFTER the instance compile) |
| decl_plane.rs | `decl_prelude_collision_is_graceful` | BUG-7: user decl shadowing a Prelude re-export resolves unambiguously, session stays usable | Y | 4→4 | KEEP AS-IS — def, use, a 2nd unrelated def, and a 2nd use are each proving a distinct facet (shadow works; session still usable after) |
| decl_plane.rs | `empty_and_garbage_decls_survive` | RE-1: empty def is a no-op (no gen bump); garbage def fails cleanly; session survives both | Y | 6→6 | KEEP AS-IS — the `:bindings` before/after comparison genuinely needs to bracket the empty-def turn to prove no gen bump |
| decl_plane.rs | `bad_decl_does_not_poison_log` | a bad decl (parse error) doesn't poison the decl log | Y | 3→3 | KEEP AS-IS |
| decl_plane.rs | `sig_and_binding_split_across_items` | M1: signature + binding split across items of ONE block typecheck together | Y | 2→2 | KEEP AS-IS — the property is explicitly about within-ONE-block batching; the 2nd turn (use) is the genuine cross-turn confirmation |
| decl_plane.rs | `mutual_recursion_across_items` | M1: mutually-recursive functions split across items of one block resolve | Y | 2→2 | KEEP AS-IS |
| decl_plane.rs | `define_then_call_in_one_block` | sig+binding+call all in ONE block (the tool's recommended idiom) | Y | 1→1 | KEEP AS-IS |
| decl_plane.rs | `decl_paints_inferred_type` | #317: a value decl's inline `type` is painted at compile time | Y | 2→2 | KEEP AS-IS — 2 independent decl-painting cases (signature vs. bare-inferred), each its own turn is reasonable since they're testing distinct code paths (explicit sig vs. inference) |
| decl_plane.rs | `non_value_decl_omits_type` | #317: a non-value decl (data/class/etc.) omits the `type` field | Y | 1→1 | KEEP AS-IS |
| decl_plane.rs | `mutual_recursion_and_call_in_one_block` | mutual recursion + a trailing call in one block, call doesn't poison the batch | Y | 1→1 | KEEP AS-IS |
| decl_plane.rs | `pure_numeric_bind_generalizes` | M2: a numeric pure bind generalizes (not frozen to Int), both within-block and across calls | Y | 3→3 | KEEP AS-IS — the cross-call half is explicitly the point of the 2nd/3rd turns |
| decl_plane.rs | `pure_numeric_bind_type_generalizes_in_display` | NMR probe: generalized type displays correctly (not defaulted) | Y | 1→1 | KEEP AS-IS |
| decl_plane.rs | `local_input_param_not_confused_with_payload_lane` | the `input`-lane materialize guard keys on the compile error, not a text scan for "input" | Y | 3→3 | KEEP AS-IS — generalization-usable-at-2-types across 2 later turns is the actual proof of "stayed a decl, not materialized" |
| decl_plane.rs | `record_dot_helper_binds_and_shows_type` | record-dot (`h.path`) helper binds on the decl plane, shows constrained type | Y | 1→1 | KEEP AS-IS |
| decl_plane.rs | `colliding_pure_bind_shadows_gracefully` | a pure bind colliding with an in-scope import (Prelude `reverse`) shadows gracefully | Y | 5→5 | KEEP AS-IS — the non-colliding-name-still-usable-across-a-turn check is a genuine, distinct cross-turn claim from the shadow-works claim |
| decl_plane.rs | `pure_polymorphic_bind_instantiates_per_use` | M2: a polymorphic empty-list bind instantiates per use, across a later call | Y | 3→3 | KEEP AS-IS — the cross-call instantiation is explicitly the point |

### `do_block_invariant.rs`, `effects_smoke.rs`, `error_recovery.rs`, `gc_field_replay.rs`, `gc_heap_verify_stress.rs`, `info_introspect.rs`, `it_binding.rs`

| File | Test | Property | Session-scoped | Turns (driven→LB) | Verdict |
|---|---|---|---|---|---|
| do_block_invariant.rs | `cross_call_scoping_both_planes_persist` | effectful bind (materialize) and pure bind (decl) both persist across separate calls | Y | 4→3 | REDUCE TURNS (4→3) — checking `n` alone is nearly subsumed by the later `n+m`-together check |
| do_block_invariant.rs | `input_payload_lane_in_scope` | `input` JSON payload is in scope for the block | Y | 1→1 | KEEP AS-IS |
| do_block_invariant.rs | `user_binding_named_input_not_confused_with_lane` | a user's own `let input=` isn't confused with the payload lane | Y | 2→2 | KEEP AS-IS |
| do_block_invariant.rs | `all_statement_forms_consecutive` | one block mixing decl/let/effectful-bind/bare-expr all classify+run together | Y | 1→1 | KEEP AS-IS |
| do_block_invariant.rs | `per_item_failure_granularity_and_clean_diag` | mid-block failure: earlier item's ok reported, later item skipped, clean diagnostic | Y | 1→1 | KEEP AS-IS |
| do_block_invariant.rs | `plane_opacity_binds_interchangeable` | pure and effectful binds of the same type are fully interchangeable later | Y | 5→3 | REDUCE TURNS (5→3) — checking `a` alone and `b` alone is diagnostic redundancy over what the combined-use turn `c` already proves |
| do_block_invariant.rs | `plane_opacity_pure_bind_shadows_prelude_name` | a pure bind of a Prelude-reexported name shadows Prelude, same as effectful bind | Y | 2→2 | KEEP AS-IS |
| do_block_invariant.rs | `plane_opacity_open_hasfield_helper_binds_cold` | a fully-open HasField record-dot helper generalizes even cold-started | Y | 1→1 | KEEP AS-IS |
| effects_smoke.rs | `full_stack_effects_reachable_through_session` | full effect stack reachable, composes over persistent session state | Y | 9→8 | REDUCE TURNS (9→8) — the KV set→get pair (2 turns) is structurally necessary (proves persistence), but the 3 `kvGetAs` branch checks could plausibly share fewer turns; keep as a soft target, not firm |
| effects_smoke.rs | `session_def_sees_full_eval_vocabulary` | a decl item sees the full eval vocabulary (M, run, L., Set., Git.) | Y | 3→2 | REDUCE TURNS (3→2) — the 2 "call sh"/"call uniqSorted" turns could merge into one 2-item block |
| effects_smoke.rs | `block_runner_input_and_type_cleanups` | 3 distinct block-runner bugfixes (input decode+let-scope, bare-expr type, monadic-expr-with-where type) | Y | 3→3 | KEEP AS-IS — 3 separate regressions for 3 separate fixes; uses the heavier full-stack server for none of the 3 (minor setup-cost note, not a turn issue) |
| error_recovery.rs | `undefined_var_then_recover` | scope error is a clean MCP error; session survives (prior binding still resolves) | Y | 3→3 | KEEP AS-IS |
| error_recovery.rs | `type_error_then_recover` | ill-typed expr fails gracefully; session recovers | Y | 2→2 | KEEP AS-IS |
| error_recovery.rs | `bad_decl_then_recover` | bad decl doesn't poison the decl log for later good decls | Y | 3 (normal path)→3 | KEEP AS-IS |
| error_recovery.rs | `bind_of_bottom_is_lazy_then_clean_on_force` | lazy bind of `error`; clean error on force, not a crash | Y | 3→3 | KEEP AS-IS |
| error_recovery.rs | `deep_recursion_yields_cleanly` | 2M-deep non-tail recursion yields a clean stack-overflow error, session recovers | Y | 3→3 | KEEP AS-IS |
| error_recovery.rs | `empty_and_whitespace_eval` | empty/whitespace-only items don't panic; session recovers | Y | 3→2-3 | REDUCE TURNS (soft, 3→2) — the 2 edge-input turns could merge into one 2-item block, though the file deliberately isolates them so one input's misbehavior doesn't obscure the other's; keep separate if that isolation is valued over the 1-turn saving |
| error_recovery.rs | `failed_bind_leaves_no_state` | a failed-typecheck bind leaves NO state; prior binds unaffected | Y | 4→4 | KEEP AS-IS |
| error_recovery.rs | `drop_without_close_does_not_hang` | dropping a session without explicit close doesn't deadlock | Y | 1→1 | KEEP AS-IS |
| gc_field_replay.rs | `field_session_replay_bridged_substrate_verified` | turn-for-turn replay of a real 2026-07-10 SIGSEGV sequence over real bridged effects | Y | 7→7 | KEEP AS-IS — literal historical-incident replay; turn-for-turn fidelity IS the point |
| gc_field_replay.rs | `field_session_replay_split_turns_control` | CONTROL: same replay with every item its own turn (no within-block closure handoff) | Y | 5→5 | KEEP AS-IS — genuine ablation, ISOLATES whether the bug needs same-block binding; complementary, not redundant, with the test above |
| gc_heap_verify_stress.rs | `session_text_substrate_folds_heap_verified` | field crash shapes reproduced over a large synthetic Text corpus, heap-verifier on | Y | 12→6 | REDUCE TURNS (12→6) — turns 1-6 (build corpus, reproduce both crash shapes) are load-bearing; the 6-iteration repeat-fold loop (turns 7-12) is the weakest-justified loop in the whole review: the corpus built in turn 2 already exceeds the 2 MiB nursery on its own, and the loop's stated purpose ("vary GC-trigger phase") is plausible but not load-bearing to the file's core claim |
| gc_heap_verify_stress.rs | `session_rebind_accumulator_heap_verified` | repeated self-rebind exercises cross-generation forwarding-stub tenure churn | Y | 8→~5 | REDUCE TURNS (soft, 8→5) — the 6-iteration loop IS the allocation driver here (unlike the sibling above) and REPEATED rebinding is explicitly tied to the bug pattern, so this is more defensible; still, 3-4 reps would plausibly still trigger multiple verified GCs given per-iteration size (~1MB+ under a 2MB nursery) |
| info_introspect.rs | `info_resolves_stdlib_proc` | `:i Proc` resolves the real stdlib record (fields+types verbatim) | Y | 1→0 | DEMOTE TO CHEAPER LAYER — `:i` resolution is a static source-scan over `.hs` files (`introspect::stdlib_info`); testable directly against fixture files with no compiled session at all |
| info_introspect.rs | `info_resolves_stdlib_hit` | `:i Hit` resolves another stdlib record type | Y | 1→0 | DEMOTE TO CHEAPER LAYER — same static-scan reasoning; MERGE candidate with the test above as one table-driven pure-Rust check |
| info_introspect.rs | `info_constructor_only_hit` | a constructor-only name resolves to its enclosing data decl + `constructor` key | Y | 1→0 | DEMOTE TO CHEAPER LAYER — same reasoning |
| info_introspect.rs | `session_decl_shadows_stdlib` | a session-declared type shadows the stdlib hit of the same name (source flips) | Y | 3→3 | KEEP AS-IS — the pre-decl turn genuinely needs a live compile to prove the shadow priority end-to-end; the resolution-priority MERGE logic itself is separately unit-testable, but proving the session decl actually compiles isn't |
| info_introspect.rs | `info_miss_carries_hint` | a total miss returns a self-explaining hint naming the searched lanes | Y | 1→0 | DEMOTE TO CHEAPER LAYER — same static-scan/miss-path reasoning |
| it_binding.rs | `bare_expr_binds_it_and_is_usable_next_turn` | bare final expr binds `it`, usable next turn, shows in `:bindings`, rebinds latest-wins | Y | 6→6 | KEEP AS-IS — each step proves a distinct facet (bind, visibility, use, rebind, re-establish, chained use) |
| it_binding.rs | `trailing_named_bind_does_not_bind_it` | `x <- e` does NOT bind `it` | Y | 2→2 | KEEP AS-IS — paired with the discard-bind test below, complementary not redundant |
| it_binding.rs | `discard_bind_does_not_bind_it` | `_ <- e` does NOT bind `it` either | Y | 2→2 | KEEP AS-IS |
| it_binding.rs | `discard_bind_runs_its_effect_both_forms` | discard bind still RUNS its effect, both single and tuple forms | Y | 5→5 | KEEP AS-IS — each `get` is a separate later-turn proof the effect actually landed |
| it_binding.rs | `discard_bind_introduces_no_binding_either_form` | discard bind adds no `:bindings` entry, byte-identical before/after | Y | 4→4 | KEEP AS-IS |
| it_binding.rs | `effectful_bare_expression_runs_its_effect_exactly_once` | a bare final effectful expr's side effect fires exactly once, not twice | Y | 2→2 | KEEP AS-IS — the readback turn is what distinguishes "ran once" from "ran twice but overwritten" |
| it_binding.rs | `huge_value_is_header_only_bound_to_it_and_stub_fetchable` | an over-ceiling result renders header-only, stays bound in full, fetchable via `:stub` | Y | 3→3 | KEEP AS-IS — each turn proves a distinct facet |
| it_binding.rs | `bare_expr_towire_identity_alias_renders_and_binds` | `toWire`-identity aliasing doesn't corrupt on tenure | Y | 2→2 | KEEP AS-IS — specific GC/tenure-aliasing regression, needs the real heap |

### `lifecycle_meta.rs`, `lifecycle_state.rs`, `lost_session.rs`, `multi_binder.rs`, `name_shadowing.rs`, `repro_decl_library_import.rs`, `repro_t_multiline_sig.rs`

| File | Test | Property | Session-scoped | Turns (driven→LB) | Verdict |
|---|---|---|---|---|---|
| lifecycle_meta.rs | `reset_when_never_opened_is_graceful` | `session_reset` from a cold session is graceful, leaves it runnable | Y | 1→1 | KEEP AS-IS |
| lifecycle_meta.rs | `reset_is_fresh` | reset yields a fresh session, old name out of scope | Y | 3→2 | REDUCE TURNS (3→2) — the post-reset `:bindings` check and the x-gone check could combine (x-gone last, so ordering is safe) |
| lifecycle_meta.rs | `reset_clears_both_planes_and_is_reusable` | `:reset` clears decl+value planes AND leaves the session reusable | Y | 8→4 | REDUCE TURNS (8→4) — pre-reset setup+check combines to 1 turn; post-reset check combines to 1; reuse combines to 1; `:reset` itself needs its own boundary |
| lifecycle_meta.rs | `reset_after_gc_rebuilds` | reset after a GC-heavy turn rebuilds cleanly, no corruption | Y | 5→3 | REDUCE TURNS (5→3) — the GC-forcing bind+fold can combine to 1 turn; the post-reset rebuild bind+read can combine to 1 |
| lifecycle_meta.rs | `bindings_shape` | `:bindings` reports the documented shape (name/type/module/tier) | Y | 3→1 | DEMOTE TO CHEAPER LAYER (partial) — JSON-shape rendering is pure Rust logic, plausibly unit-testable on a synthetic BindingTable without any compile |
| lifecycle_meta.rs | `type_and_info_are_implemented` | `:t` and `:i` are real, not stubs | Y | 3→1 | REDUCE TURNS (3→1) — all 3 items (`:t`, bind, `:i`) fit one block, none depend on cross-turn persistence |
| lifecycle_meta.rs | `unknown_meta_command_is_clean_error` | an unknown `:command` is a clean error, never a panic | likely N (fast `MetaCommand::parse` failure) — needs source confirmation | 1→0 | DEMOTE TO CHEAPER LAYER (pending confirmation) — if parse-only as suspected, a direct `MetaCommand::parse` unit test needs no server/compile at all |
| lifecycle_meta.rs | `bindings_on_fresh_session` | a never-touched session reports empty bindings, generation 0 | Y (first call auto-opens) | 1→1 | KEEP AS-IS |
| lifecycle_meta.rs | `reference_path_type_metadata_trap` | REGRESSION: referencing an Eff-wrapped/decl function on the reference path no longer traps | Y | 4→2 | REDUCE TURNS (4→2) — setup (def+bind) combines to 1 turn, checks combine to 1 turn |
| lifecycle_meta.rs | `program_repaint_round_trips` | `:program` emits a replayable notebook; replaying into a FRESH session reproduces the value | Y, needs 2 distinct servers | 6→2 | REDUCE TURNS (6→2) — each server's own setup+check can combine internally; genuinely needs 2 real servers (can't demote below that) |
| lifecycle_meta.rs | `redefine_reports_stale_binds` | redefining a decl a live bind depended on marks it `stale`; unrelated redefines don't | Y | 4→2 | REDUCE TURNS (4→2) — setup is 1 turn, redefine+unrelated-redefine combine to 1 |
| lifecycle_state.rs | `reset_while_suspended_drops_ask_and_recovers` (H1) | reset while suspended on `ask` returns promptly, drops the pending continuation | Y | 3→3 | KEEP AS-IS |
| lifecycle_state.rs | `run_on_suspended_session_is_rejected` (M5) | a run while suspended is rejected by the busy-guard; original suspension still resumable | Y | 4→4 | KEEP AS-IS — the busy-guard check itself (`SessionManager::admit_run`) is separately unit-testable, but this test's own remaining turns are needed to prove the resumability half |
| lifecycle_state.rs | `timed_out_runaway_self_heals_to_idle` (H3) | a runaway turn cooperatively cancels at a JIT safepoint after timeout, self-heals | Y, deeply | 2→2 | KEEP AS-IS — needs the real JIT loop+timer; note this test is exceptionally expensive (75s turn budget, can run well over a minute) but not reducible without losing the property |
| lifecycle_state.rs | `abandoned_suspension_is_reaped_to_idle` (H2) | an abandoned suspension is reaped to Idle after TTL | Y | 2→2 | KEEP AS-IS — deliberately end-to-end per this crate's own doc ("reachable via the real production path") |
| lost_session.rs | `resume_with_no_session_errors` | resuming before any session has run errors cleanly | N (pre-compile) | 1→1 | KEEP AS-IS — already about as cheap as possible while going through real dispatch |
| lost_session.rs | `resume_when_not_suspended_errors` | resuming an idle (not suspended) session errors distinctly | Y (needs a real Idle session) | 2→2 | KEEP AS-IS |
| lost_session.rs | `resume_wrong_continuation_while_suspended_errors` | resuming with the WRONG continuation id errors, names the real pending one | Y | 3→2 | REDUCE TURNS (3→2) — the final cleanup resume is tidiness, not load-bearing to the stated property |
| multi_binder.rs | `let_single_bind` | single-binder `let` still works (control) | Y | 2→1 | REDUCE TURNS (2→1) — no cross-turn claim |
| multi_binder.rs | `tuple_bind_both_components` | BUG-5: tuple bind roots both names independently, GC-safe across a real minor GC | Y | 8→3 | REDUCE TURNS (8→3) — pre-GC checks combine to 1 turn, the GC-forcing turn stands alone, post-GC reads combine to 1; clearest over-decomposition example in the review |
| multi_binder.rs | `let_tuple_works` | `let`-tuple pattern binds both components | Y | 2→1 | REDUCE TURNS (2→1) — no cross-turn claim |
| multi_binder.rs | `three_tuple_works` | a 3-element tuple bind works | Y | 2→1 | REDUCE TURNS (2→1); MERGE candidate — same shape as `let_tuple_works`, could be table-driven cases of one parametrized test |
| multi_binder.rs | `mismatched_type_rejected_loudly` | a type-mismatched multi-bind is rejected, nothing leaks, session stays usable | Y | 4→2 | REDUCE TURNS (4→2) — the failed item needs its own turn; the 3 recovery checks combine into 1 later turn |
| name_shadowing.rs | `value_bind_shadows_prelude_name` | a session-bound name shadows a same-named Prelude import on a LATER turn, doesn't poison future turns | Y, genuinely cross-turn | 3→2-3 | KEEP AS-IS (soft) — the bind-turn and use-turn can't merge (must exercise a FRESH compile's regenerated hiding clause); the 3rd (unrelated follow-up) turn is a legitimately stronger regression guard against exactly the "poisons every later turn" failure mode, not pure padding |
| repro_decl_library_import.rs | `decl_resolves_library_reexported_type_without_explicit_import` | a decl referencing a Library-reexported type compiles without explicit import | Y | 1→1 | KEEP AS-IS turn-wise; **FLAG**: narrow single-bug repro, possible overlap with `decl_plane.rs`'s general import-scope coverage — needs cross-check, not a mechanical merge |
| repro_decl_library_import.rs | `decl_defining_a_library_reexported_name_does_not_collide` | a decl DEFINING a Library-reexported name doesn't trigger Ambiguous occurrence | Y | 1→1 | KEEP AS-IS turn-wise; same overlap flag as above. **Separately**: this file hand-rolls its own ~90-line server-builder nearly identical to `common.rs`'s `build_full_server` — a "kept-in-sync copy" per the root CLAUDE.md rule, independent of turn count |
| repro_t_multiline_sig.rs | `t_on_wide_multiline_signature_does_not_crash` | `:t` on a line-wrapped type doesn't crash on unescaped newlines | Y | 2→0 | DEMOTE TO CHEAPER LAYER — root cause is a hand-rolled Haskell string escaper missing `\n`; this is a pure JSON-escaping round-trip, testable by feeding a string with embedded newlines through the escaper/parser directly |
| repro_t_multiline_sig.rs | `t_on_m_returning_helper_does_not_trip_cross_row_guard` | `:t` on an M-returning expr is exempt from the cross-row bind guard, mutates nothing | Y | 4→1 | REDUCE TURNS (4→1); MERGE WITH `t_on_either_returning_effect_verb_does_not_trip_cross_row_guard` — same property, different type shape, high overlap, candidate for one table-driven test |
| repro_t_multiline_sig.rs | `t_on_either_returning_effect_verb_does_not_trip_cross_row_guard` | same cross-row-guard exemption for a stdlib `Either`-returning verb | Y | 3→1 | REDUCE TURNS (3→1); MERGE WITH the test above (see note there) |
| repro_t_multiline_sig.rs | `either_returning_verb_bind_now_persists_across_turns` | STABLE-EFFECTS-CORE: a real `Either ExecError Proc` bind persists into a genuinely LATER turn | Y, genuinely cross-turn | 2→2 | KEEP AS-IS — distinct property (actual persistence) from the other 3 `:t`-probe-purity tests in this file |

### `session_acceptance.rs`, `shadow_rebind.rs`, `stub_fetch.rs`, `text_bind.rs`, `value_binding_acceptance.rs`, `value_fidelity.rs`

**Correction confirmed while cataloguing this batch:** not every dispatch
compiles. `:bindings`, `:stub`, `:reset`, `:i`, `:program` are pure in-process
reads (confirmed against `session.rs`'s `run_meta`) — only `:t` among the meta
commands compiles. Free dispatches are marked accordingly below and excluded
from the turn count.

| File | Test | Property | Session-scoped | Turns (driven→LB) | Verdict |
|---|---|---|---|---|---|
| session_acceptance.rs | `session_multi_turn_real_path` | def+eval persist across turns on one machine; `session_reset` drops it, post-reset ref scope-errors | Y | 4→4 | KEEP AS-IS — sole owner of `session_reset` lifecycle/scope-error coverage in this batch |
| session_acceptance.rs | `reset_from_cold_start_then_run` | `session_reset` as the very first call leaves a runnable session | Y | 1→1 | KEEP AS-IS — unique cold-start-reset coverage |
| shadow_rebind.rs | `rebind_value_name_newest_wins` | rebinding `x` across 2 turns, newest value wins, not an ambiguous-occurrence error | Y | 3→3 | KEEP AS-IS |
| shadow_rebind.rs | `self_referential_rebind_reads_prior` | self-referential rebind reads the PRIOR value, doesn't blackhole | Y | 4→3 | REDUCE TURNS (4→3) — the 2nd accumulate step is reinforcement, not required to falsify the bug |
| shadow_rebind.rs | `rebind_value_different_type` | rebinding `x` at a DIFFERENT type, newest type wins, no stale-iface clash | Y | 3→3 | KEEP AS-IS |
| shadow_rebind.rs | `first_bind_text_no_rebind` | CONTROL: isolates whether the type-rebind crash needs a rebind at all | Y | 2→2 | KEEP AS-IS — deliberate control, cannot be cut |
| shadow_rebind.rs | `redefine_function_latest_wins` | redefining a function across turns: latest def wins at call | Y | 4→4 | KEEP AS-IS |
| shadow_rebind.rs | `redefine_type_old_binding_orphaned_gracefully` | redefining a `data` type mid-session: new-gen works, old-gen fails gracefully, session survives | Y | 8→7 | REDUCE TURNS (8→7) — the pre-redefine baseline check establishes attribution but isn't strictly required by the 3 stated claims |
| shadow_rebind.rs | `bindings_after_rebind_lists_once` | after a rebind, `:bindings` lists the name exactly once (dedup) | Y (2 evals) + free (`:bindings`) | 2→2 (compiles) | DEMOTE TO CHEAPER LAYER (partial) — the dedup assertion itself is pure Rust `iter_current()` over an in-memory map, unit-testable directly; the 2 compiling turns that PRODUCE the rebind state stay |
| shadow_rebind.rs | `migrated_name_read_from_later_let` | a self-ref-migrated decl-plane bind is correctly retracted so a later `let` sees the live value | Y | 5→5 | KEEP AS-IS |
| shadow_rebind.rs | `accumulate_then_let_fold` | building a list via repeated self-ref rebinds across MULTIPLE turns, a `let` fold sees every element | Y | 6→6 | KEEP AS-IS — multi-turn accumulation IS the property |
| shadow_rebind.rs | `def_referencing_migrated_name_closes_over_value` | a `def` referencing a migrated value closes over it at DEFINITION time, not retroactively | Y | 6→6 | KEEP AS-IS |
| shadow_rebind.rs | `let_referencing_unmigrated_decl_still_works` | REGRESSION GUARD: a `let` over a never-migrated decl still resolves there; fresh binds still generalize | Y | 5→5 | KEEP AS-IS — bundles 2 logically separate claims in one fn (splittable, but not a turn issue) |
| stub_fetch.rs | `stub_roundtrip_replace_and_unknown` | oversized field truncates+fetches, a 2nd truncation REPLACES the stash, unknown id errors | Y (2 of 5 dispatches) + free (`:stub`×3) | 2→2 (compiles) | DEMOTE TO CHEAPER LAYER (largely) — marker-format/replace-semantics/unknown-id-error logic all live in Rust `truncate.rs`, unit-testable with a constructed oversized Value; keep 1 real turn as an integration smoke check of the wiring |
| stub_fetch.rs | `small_result_has_no_truncation_key` | an in-budget value has no truncation marker | Y | 1→0 | DEMOTE TO CHEAPER LAYER — pure Rust size-comparison logic |
| text_bind.rs | `box_second_bind_replica` | CONTROL: identical turn shape to the headline test, using a library-independent `Box` — proves the fixed crash was library-resolution-specific | Y | 5→5 | KEEP AS-IS — deliberately mirrors the headline test's structure as its control |
| text_bind.rs | `eff_ref_pure_const_with_binding_live` | minimal repro: an unrelated `pure` reference yields correctly with any binding live | Y | 2→2 | KEEP AS-IS turn-wise; **overlap flag**: shares intent with `value_fidelity.rs`'s `text_bind_with_prior_live_binding_diagnostic` (this one is the plainer minimal repro, no Text) |
| text_bind.rs | `eff_ref_pure_const_no_binding_control` | CONTROL: same expression, no binding live | Y | 1→1 | KEEP AS-IS |
| text_bind.rs | `text_bind_headline_faithful` | full acceptance: Box control + Text bind + 4 independent read-back forms | Y | 8→5 | REDUCE TURNS (8→5) — the 4 read-back checks (length/unpack/toUpper/append) are independent reads of the same bound value and could fold into ONE multi-item block |
| text_bind.rs | `text_rebind_same_name` | same-name rebind Int→Text (same root cause as `shadow_rebind.rs`'s type-rebind case) | Y | 4→4 | KEEP AS-IS turn-wise; MERGE candidate flagged — near-duplicate of `shadow_rebind.rs::rebind_value_different_type` (same scenario, same turn shape, framed as a BUG-2 regression witness rather than shadowing-semantics) |
| value_binding_acceptance.rs | `value_binding_int_json_function_survive_gc` | headline sweep: Int + custom ADT + function all bind, survive an organic GC, read back correctly | Y (8 of 9 dispatches) + free (`:bindings`) | 8→8 (compiles) | KEEP AS-IS turn-wise; **overlap flag**: the `foldl' (+) (0::Int) [1..200000]` GC-forcing idiom + "function survives GC" claim is reused verbatim in `value_fidelity.rs`'s `closure_captures_binding_survives_gc` and `list_bind_survives_gc` — genuine cross-file duplication of the GC-forcing pattern, not just superficial |
| value_binding_acceptance.rs | `git_error_bare_either_bind_survives_into_next_turn` | a bare `Either` bind (no destructure) survives into a later turn — `GitError`'s move regression guard | Y | 2→2 | KEEP AS-IS — tests a real effect-handler + wire-type boundary, a different concern from the rest of this file; misplaced by name but not redundant |
| value_fidelity.rs | `text_first_class_bind_and_reference` | Text binds, 3 read-back forms all work | Y | 4→2 | REDUCE TURNS (4→2); MERGE candidate — near-subset of `text_bind.rs::text_bind_headline_faithful`'s length/unpack/toUpper pattern minus append/Box/prior-binding setup |
| value_fidelity.rs | `text_bind_with_prior_live_binding_diagnostic` | KEY DIAGNOSTIC: Text bind under a different name while a prior Int binding is live, in a separate earlier turn | Y | 3→3 | KEEP AS-IS — the cross-turn structure IS the hypothesis under test |
| value_fidelity.rs | `bind_references_earlier_binding` | a bind action reads an earlier binding via `ExternalEnv` | Y | 3→2 | REDUCE TURNS (3→2) — the bind and final-read turns can combine; only the earlier-turn setup is genuinely separate |
| value_fidelity.rs | `closure_captures_binding_survives_gc` | a closure capturing an earlier binding survives a forced GC | Y | 4→3 | REDUCE TURNS (4→3) — the 2 setup binds can combine; the GC-forcing/post-GC-read gap must stay separate. Duplicates the GC-idiom flagged under `value_binding_acceptance.rs` above |
| value_fidelity.rs | `nested_recursive_adt_bind_and_sum` | real constructor names resolve from the merged session DataConTable across turns | Y | 4→2 | REDUCE TURNS (4→2) — def+bind combine; def+use combine; the cross-turn boundary (heap persistence) is preserved either way |
| value_fidelity.rs | `maybe_bind_and_case` | Just/Nothing bind + case-match round-trip | Y | 2→1 | REDUCE TURNS (2→1); part of the Maybe/Either/JSON/list "shape" family, all structurally near-identical templates |
| value_fidelity.rs | `either_bind_and_case` | Left/Right bind + case-match round-trip | Y | 2→1 | REDUCE TURNS (2→1); same family as above |
| value_fidelity.rs | `structured_json_value_bind_and_read` | a JSON object binds; render + optics read both work | Y | 3→2 | REDUCE TURNS (3→2) |
| value_fidelity.rs | `list_bind_survives_gc` | a list binding survives a forced GC | Y | 4→3 | REDUCE TURNS (4→3) — the 2 post-GC reads (length/sum) are independent, could share one block. Same GC-idiom duplication flagged above |
| value_fidelity.rs | `function_applied_at_bind_time` | a closure applied AT bind time is rooted as Tier-0, not deferred | Y | 3→2 | REDUCE TURNS (3→2) |
| value_fidelity.rs | `towire_list_of_int_renders_as_json_array` | `[Int]` renders as a structural JSON array | Y | 1→1 | KEEP AS-IS |
| value_fidelity.rs | `towire_list_of_records_renders_as_array_of_show_leaves` | list-of-records renders as an array of Show-string leaves | Y | 2→1 | REDUCE TURNS (2→1) — mixed decl+expr items are allowed in one block |
| value_fidelity.rs | `towire_string_and_text_stay_bare` | String/Text render as bare JSON strings, not char-list arrays | Y | 2→1 | REDUCE TURNS (2→1) |
| value_fidelity.rs | `towire_maybe_renders_as_null_or_payload` | Just/Nothing unwrap to payload/null, not tagged strings | Y | 2→1 | REDUCE TURNS (2→1) |
| value_fidelity.rs | `towire_bare_show_only_adt_still_hits_the_floor` | a non-container Show-only ADT still renders via the Show floor | Y | 2→1 | REDUCE TURNS (2→1) |

**The 5-test ToWire family** (`towire_list_of_int_...` through
`towire_bare_show_only_adt_...`) is templated closely enough that, beyond the
per-test turn reductions above, the operator may want to consider whether
lower-tier `tidepool-codegen::heap_bridge` coverage of container ToWire
rendering already exists — if so, some of these 5 may be DELETE (covered by
X) candidates rather than just REDUCE TURNS; this review did not check
`tidepool-codegen`'s own test suite (out of the boundary's scope) so it is
flagged, not asserted.

---

## What wasn't done (explicit, per the boundary)

- **No test was run to re-measure actual spawn counts against these turn
  estimates.** The turn-driven/load-bearing numbers above come from reading
  every test's control flow (dispatch calls, `Harness`/`SelfHarnessDriver`
  construction, `ReplayProvider` reply lists), cross-checked against the
  `resume`-doesn't-recompile correction confirmed directly in
  `harness.rs`. `test-time-cut.md`'s own instrumented-shim method (reusable,
  read-only) would validate these estimates directly, at the cost of a full
  sequential GHC-heavy run across ~68 files — out of this lane's ~380s-budget
  sampling discipline.
- **No verdict here assumes a specific implementation mechanism.** "Combine
  into one multi-item block" and "call the lower-level compile function
  directly" are both changes to test files only — no production code change
  is implied or required by any REDUCE/MERGE/DEMOTE verdict above.
- **Cross-crate overlap** (e.g. whether `tidepool-codegen`'s own suite already
  covers some of `value_fidelity.rs`'s ToWire-family claims) was flagged where
  suspected but not verified — verifying it means reading a crate outside this
  lane's two named crates.
