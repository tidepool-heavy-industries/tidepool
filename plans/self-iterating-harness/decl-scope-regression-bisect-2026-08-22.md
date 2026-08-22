# Decl-scope regression bisect — 2026-08-22

Bisect receipts for two currently-red `tidepool-harness` tests surfaced by the
registry-capstone lane's full-shard battery run:

- `tidepool-harness::acceptance_cross_turn multi_block_reply_runs_in_order_and_fails_with_resume_point`
- `tidepool-harness::companion_scope_trees locked_decision_4_holds_through_the_real_compile_path`

No code changes in this commit — bisect findings only, ahead of the mechanism
fix.

## The one mechanism, two trigger paths

Both reds trace to the same defect in `Harness::run_multi_item_block`
(`tidepool-harness/src/harness.rs`): when a block's items split into more
than one (`engine::split_block_items`), the block is driven item-by-item, and
consecutive `Decl`-classified items batch into one `define_scoped_in` call. If
that batched decl run reaches the END of the item list — i.e. the block's
last item is a declaration — the whole turn is rejected as a **compile-class
failure** ("This block's last item is a declaration, not something that
runs...") and fed back through the corrective-retry loop.

That rejection fires **unconditionally**, with no check for whether any
non-decl (bind/expr) item ran EARLIER in the same block. So it fires
identically for two very different shapes:

1. A **trailing decl after real content** — the model answered, then left a
   stray declaration hanging (`pure (toJSON (1 :: Int))` then `sq x = x * x`).
   This is a genuine model mistake; rejecting it is correct
   (`multi_item_block_ending_in_decl_retries_and_survives` pins this).
2. A **purely declarative block** — every item is a declaration, nothing
   else. This is semantically identical to a single-item decl turn (which
   `run_block`'s single-item `TurnResult::Decl` arm completes directly, no
   corrective retry) — the only difference is that `split_block_items` found
   more than one line/paragraph in the text. Rejecting this is wrong: it
   consumes an extra corrective-retry round, which (with a
   `ReplayProvider` driving scripted tests) desyncs the reply queue —
   every subsequent scripted turn runs on the wrong node against the wrong
   text, and any REAL model traffic pays an unnecessary round trip for a
   block that was never actually malformed.

Case 2 is what both failing tests hit: every scripted declare-only reply in
`companion_scope_trees.rs`'s `locked_decision_4_...` test (ROOT declaring
`helper`+`shared`, A declaring its own `helper`, B declaring its own
`helper`) and the first block of `acceptance_cross_turn.rs`'s
`multi_block_reply_runs_in_order_...` test (`nums = ...` / `double x = ...`)
are purely declarative, multi-item blocks.

## Observed failure signatures, and why they differ from the task brief

Live-instrumented trace (temporary `eprintln!`s in `run_block`/
`run_multi_item_block`/`live_turn_context`/`session_decl_context`, removed
before this commit) confirmed the mechanism directly: `locked_decision_4`'s
ROOT-node decl turn hits the "ends in decl" rejection, its corrective retry
consumes the NEXT queued reply (originally meant for sibling A), which is
*also* a declare-only multi-item block, so it hits the SAME rejection and
consumes a THIRD reply (originally meant for B's first probe) — which
finally succeeds (single item, a bare expression). The net effect: ROOT's
node ends up owning TWO decl generations (its own, plus A's misrouted
"helper x = x + 10"), and when scope A/B are minted afterward, they inherit
ROOT's now-corrupted tip. B's "first use of `helper`" therefore resolves A's
body (`11`) instead of ROOT's (`101`) — not because scope-tip seeding is
broken (`tidepool-runtime`'s own `session_decl_scope_tree.rs` tests, which
exercise `SessionLib`/`PersistentSession` directly with no harness turn
routing in between, all pass), but because the REPLAY QUEUE was already
desynced by the time any scope was minted. Same mechanism explains
`multi_block_reply_runs_in_order_...`'s "got: 18" (turn 1's decl block gets
rejected, its corrective retry consumes reply 2's block 1 AND then reply 2's
failing block 2, and reply 3's corrective finally lands, so turn 1 reports
reply 3's value `sum tripled = 18` instead of reply 1's own `sum (map double
nums) = 12`).

## Bisect results

Both breaking commits are **older than the assigned range**
(`313ed240^..HEAD`) — i.e. ancestors of `313ed240`, not inside it. Confirmed
by direct checkout + `scripts/battery.sh -p tidepool-harness -E
'test(<name>)'` at each point (`TIDEPOOL_EXTRACT` pinned to the ambient
`~/.nix-profile/bin/tidepool-extract`, with-packages GHC prepended to `PATH`,
per `haskell/CLAUDE.md`).

### `locked_decision_4_holds_through_the_real_compile_path`

| commit | date | result |
|---|---|---|
| `40df01e3` (= `6acc340b^`) | 2026-08-17 | **PASS** |
| `6acc340b` — *"feat(harness): adopt the block lane for multi-item answerer turns"* | 2026-08-18 | **FAIL** — `Resident("This block's last item is a declaration...")` |
| `313ed240` (assigned range start) | 2026-08-22 | FAIL — `Engine(Provider(Api("replay queue exhausted")))` (later commits in-range changed HOW it fails, not THAT it fails) |
| `f0fc6e91` (HEAD) | 2026-08-22 | FAIL — `11` vs `101` (the signature quoted in the task) |

**Breaking commit: `6acc340b`.** It introduced `run_multi_item_block` and the
unconditional "block ends in decl → compile-class error" rule. ROOT's
declare reply already has a blank line between the `helper` and `shared`
groups, so it was ALREADY 2 items under the pre-existing (blank-line-only)
splitter — no later splitter change was needed to trigger this test's
breakage.

Within the assigned range, `7cd9188b` — *"fix(harness): decl-ending
multi-item blocks now persist before nudging"* (2nd commit in range, right
after `313ed240`) — is a real, independent fix: before it, a rejected
trailing-decl run's `define_scoped_in` calls were never persisted at all
(compounding into "replay queue exhausted" once enough corrective rounds ran
past the scripted reply list). After it, the declarations DO persist before
the rejection fires — which is correct and necessary — but without also
gating the rejection on "did earlier content already run this block", it
converted a loud failure (queue exhaustion / compile error) into the silent
wrong-scope leak the task describes (`11` vs `101`). `7cd9188b` is worth
keeping; it is not, by itself, suffient without the added guard.

### `multi_block_reply_runs_in_order_and_fails_with_resume_point`

| commit | date | result |
|---|---|---|
| `085991b3^` (= `b76f1ab7`) | 2026-08-18 | **PASS** |
| `085991b3` — *"fix(harness): give split_block_items GHCi statement semantics"* | 2026-08-20 | **FAIL** — `sum (map double nums) = 12 ..., got: 18` (exact signature quoted in the task) |
| `313ed240` (assigned range start) | 2026-08-22 | FAIL — same `18` signature |
| `f0fc6e91` (HEAD) | 2026-08-22 | FAIL — same `18` signature |

**Breaking commit: `085991b3`.** Before it, `split_block_items` split only on
blank-line paragraphs, so `"nums = [1, 2, 3] :: [Int]\ndouble x = x * (2 ::
Int)"` (two lines, no blank line between them) was ONE item — a single-item
decl turn, never routed through `run_multi_item_block` at all, so the
"ends in decl" rule (already present since `6acc340b`, just unreached for
this specific text) never fired. `085991b3` changed the splitter to GHCi
per-line semantics (every unindented line starts a new item, blank line or
not) — correct and necessary for the contiguous-bind-persistence contract
`multi_item_block_contiguous_binds_persist_across_rounds` pins — but it
newly exposed this test's decl-only first block to the same pre-existing
`6acc340b` defect.

## Mechanism fix (next commit)

Gate the "ends in decl" rejection in `run_multi_item_block` on whether any
non-decl item already ran earlier in this same block (i.e. the maximal
trailing decl run's `start` index is `> 0`), not merely on "the run reaches
the end of the item list". A block that is declarations from item 0 through
the end — nothing else ever ran — completes exactly as a single-item decl
turn does today: no corrective retry, no replay-queue desync.
