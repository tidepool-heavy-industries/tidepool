# Provider/matching mechanism survey — `tidepool-harness/tests/*.rs`

Groundwork for the scripted-provider de-jank lane. Every suite that drives a
`Harness`/`SelfHarnessDriver` is inventoried by its provider mechanism, its
text-coupling exposure, and a migration verdict. `companion_recursive_slice.rs`
(commit b135d174, "re-key scripted provider on (path, phase) structure, prune
text pins") is the DONE precedent this lane generalizes.

Mechanism key:

- **structural** — keys a reply on a harness-rendered `(path, phase)`
  identity marker, never on scattered prompt substrings (`support::scripted_provider::KeyedProvider`).
- **record-replay** — `ReplayProvider`, strict FIFO order. The best available
  mechanism when nothing else rides the wire (`TurnRequest` carries only
  `messages`/`max_tokens` — no `NodeId`/`SiteId`); text coupling, if any, is
  in the hardcoded Haskell *content* of each reply, not in how a reply is
  selected.
- **needle (production prose)** — matches a substring of HARNESS-RENDERED
  prompt text (a header, a nudge, a compaction prompt). Breaks on a legitimate
  prompt-wording change elsewhere — the real hazard class.
- **needle (test-owned content)** — matches a substring that IS the test's
  own authored prompt/label (e.g. a fanout child's own prompt text, `"BRANCH-3"`).
  Immune to unrelated renames — it only breaks if the test edits its own
  prompt, which is self-consistent, not a hazard.
- **call-order/count** — no text inspection at all (index into a reply list,
  a call counter). No coupling.
- **none / not applicable** — no scripted `ModelProvider` (typecheck-only,
  compile-fail, a real (non-test-double) provider's own HTTP-shaped behavior).

| File | Mechanism | Text-coupling | Verdict |
|---|---|---|---|
| `companion_recursive_slice.rs` | structural | none (parses ONE canonical header) | **DONE precedent** — now imports the shared `support::scripted_provider` instead of its own private copy (behavior unchanged, verified: 9/9 green) |
| `delegate_positive_path.rs` | was needle (production prose: `"NODE root — DISCOVER"`, `"— FOLD"`) | 2 needles/site, re-derived by hand from companion's OLD pre-fix pattern | **MIGRATED** to `support::scripted_provider::KeyedProvider` — also fixed a pre-existing drifted fixture (see below) |
| `delegate_merge_fold.rs` | was needle (production prose, needle SETS: `["NODE root/2-", "— DISCOVER"]`) | 5 needle-sets, same re-derivation as above | **MIGRATED**, same fix |
| `acceptance_fanout.rs` | record-replay (`ReplayProvider`) | verb-call literal (`runLLMTurnFanout`/`resume`) repeated 9× — commit 79cd3adc's rename touched 10 lines here | **MIGRATED** reply *construction* onto `support::haskell_call` builders (`fanout_bind`/`resume_call`) — matching mechanism itself (order) was already correct and unchanged; see "What this cannot fix" below |
| `golden_path.rs` | record-replay | verb-call literal repeated 5× — same rename touched 8 lines here | **MIGRATED** onto the same builders |
| `branch_fanout.rs` | needle (test-owned content: e.g. `"BRANCH-3"`, the fanout child's own prompt, matched via `transcript.contains(needle)`) | needle is the SUITE's own authored prompt text, not harness prose | **NOT migrated** — already immune to renderer/rename churn by construction (own module doc explains why order-based `ReplayProvider` cannot serve concurrent children at all); would need a NEW extractor shape (whole-transcript `.contains`, not a single header) to fold into the shared utility for marginal benefit. Left as a documented follow-up, not touched (no half-migration) |
| `outer_fanout.rs` | needle (test-owned content), same shape as `branch_fanout.rs` (its own module doc says `branch_fanout.rs` adapted this file's provider verbatim) | same | same verdict as `branch_fanout.rs` |
| `selfharness_budget.rs` | needle (**production prose**: `"approaching this window's round limit"`, driver.rs:4502/4704) | 1 site | **Flagged, not fixable test-side** — see "Residual production-prose pins" below |
| `selfharness_compaction.rs` | needle (production prose: `is_summarize_prompt` → `"Summarize EVERYTHING above"`, driver.rs:5722) + needle (test-owned: `"SECOND-HOLE"`) | 1 production site + 1 test-owned site | same residual note for the production-prose half; test-owned half is fine |
| `selfharness_compaction_fixes.rs` | same shape as `selfharness_compaction.rs` | same | same |
| `selfharness_context_window.rs` | needle (test-owned: `"SECOND-HOLE"`, `"FIRST-HOLE"`) | test-owned only | fine as-is |
| `selfharness_persistence.rs` | needle (production prose, `is_summarize_prompt`) + call-order (`OrderLoggingProvider`) + needle (test-owned) | 1 production site | same residual note |
| `selfharness_framing.rs` | call-order/count (captures system messages, no reply selection by content) | none | fine as-is |
| `selfharness_lifecycle.rs` | call-order/count (`FlakyProvider`, fails first N calls) | none | fine as-is |
| `turn_lease.rs` | call-order/count (`GatedProvider`/`FailFirstProvider`) | none | fine as-is |
| `turn_splice.rs` | call-order/count (`SpliceProbeProvider`, indexed) | none | fine as-is |
| `acceptance_fork.rs` | record-replay + a pass-through `CapturingProvider` wrapper | none (no selection logic) | fine as-is |
| `acceptance_boot_compile_count.rs` | none (single-shot `SnapshotOnFirstCall`, terminates by design) | none | fine as-is |
| `acceptance_askuser.rs`, `acceptance_selfharness.rs`, `acceptance_consent_integrity.rs`, `acceptance_value_bind.rs`, `acceptance_cross_turn.rs`, `acceptance_finalize.rs`, `acceptance_run_llm_turn.rs`, `acceptance_fork_combinators.rs`, `acceptance_lazy_boot.rs`, `decl_plane_run_scoping.rs`, `companion_mount_spike.rs`, `companion_context_ref.rs`, `companion_snapshots.rs`, `companion_scope_trees.rs`, `outer_subagent.rs`, `outer_effects.rs`, `node_mailboxes.rs`, `labeled_branch.rs`, `minimal_watch_list.rs`, `reinterpret_rowchange_repro.rs`, `nested_async_repro.rs`, `dogfood_observability.rs`, `delegate_type_pinning.rs`, `selfharness_spine.rs`, `selfharness_decl_plane_replay.rs`, `selfharness_fn_finalize_spike.rs` | record-replay (`ReplayProvider`) | verb-literal duplication exists but far below the churn `acceptance_fanout.rs`/`golden_path.rs` hit (1-3 sites each, not the specific file 79cd3adc's diff touched) | not migrated — lower priority than the mandated pair; `support::haskell_call` is available for any of these to adopt later without another survey |
| `acceptance_multi_target.rs`, `agent_stack_scoping.rs`, `dogfood_harness_typecheck.rs`, `finalize_type_pinning.rs`, `stable_effects_core_decl_plane.rs`, `timing_emission_pin.rs`, `compile_fail.rs`/`compile_fail/` | none (typecheck-only / compile-fail / no driven model call) | n/a | not applicable |
| `provider_behavior.rs` | n/a — exercises the REAL `ApiKeyProvider`/`OauthProvider` HTTP-shaped behavior, not a scripted test-double | n/a | not applicable (this file tests production provider code, not a fixture) |

## What this lane fixed beyond matching mechanism

While migrating `delegate_positive_path.rs`/`delegate_merge_fold.rs`, both
suites' scripted `finalize @FoldDecision (...)` replies carried two fields —
`foldEditsInOrder`/`foldProposed` — that the fold-lineage rewrite (commit
61cc10ae) deleted from `FoldDecision` (`HarnessTypes.hs:317-321` now declares
only `foldSynthesis`/`foldTensions`). This was a **pre-existing, unrelated
failure** (confirmed by reverting to HEAD's original files and reproducing
the same `KeyedProvider: no scripted reply matches` / round-exhaustion
failure before touching anything) — exactly the "hardcoded reply drifts after
a real type/field rename" hazard class this lane exists to reduce, just on a
record field rather than a verb name. Fixed by dropping the two stale fields
to match `HarnessTypes.hs` and `companion_recursive_slice.rs`'s own
already-correct `fold_reply()`.

## What this lane cannot fix (documented, not attempted)

- **Verb-call literals in `ReplayProvider` fixtures** (`acceptance_fanout.rs`/
  `golden_path.rs`, and every other `record-replay` suite): a scripted reply
  IS Haskell source under test; its correctness is inherently coupled to the
  real verb name. `support::haskell_call`'s builders centralize the call
  SYNTAX so a rename is one edit instead of N, but cannot reach zero-edits —
  there is no wire-level indirection for reply *content*, only for reply
  *selection*, and exposing one would mean touching production code, which is
  out of bounds for this lane.
- **Residual production-prose needles** (`selfharness_budget.rs`,
  `selfharness_compaction.rs`, `selfharness_compaction_fixes.rs`,
  `selfharness_persistence.rs`): these match a substring of driver-generated
  prose (the round-limit nudge, `driver.rs:4502`/`:4704`; the compaction
  prompt, `driver.rs:5722`) because the driver offers no other signal that a
  given turn IS a nudge/compaction turn — no wire field, no distinct request
  shape. Deriving the expectation by calling the renderer (rule 3) would
  require exposing a small predicate/constant from `driver.rs`, which is a
  production-code change to make tests easier — explicitly out of bounds
  here. Flagged per the boundary's "STOP and report" rather than attempted.

## Zero-fixture-churn bar — per migrated suite

A suite reaches the bar when a PURE rendering/rename change in production
code requires zero fixture edits here.

| Suite | Reaches the bar? |
|---|---|
| `companion_recursive_slice.rs` | **Yes** — keyed on `(path, phase)`, parsed off one stable header format |
| `delegate_positive_path.rs` | **Yes** — same key |
| `delegate_merge_fold.rs` | **Yes** — same key |
| `acceptance_fanout.rs` | **No** (documented) — `ReplayProvider`'s order-keying is already immune to rendering changes, but a VERB rename still touches `support::haskell_call`'s two builder definitions (not zero, but centralized — was 10 call sites, now 1 definition) |
| `golden_path.rs` | **No** (documented), same reason — was 8 call sites, now 1 definition |
