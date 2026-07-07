# 06 — tidepool-repl session correctness

Response-shape data loss and contract drift in the block runner. The recent
`it`-binding / tenure / large-value work was scrutinized hard and came out
CLEAN (see Verified clean) — these findings are in the older block-runner and
meta paths.

## ANTI-PATTERNS

- Do NOT hold the `SharedState` mutex across an `.await` (load-bearing
  invariant, `state.rs` module docstring).
- Do NOT widen `tidepool-mcp` visibility to fix repl issues — the dispatcher
  duplication is deliberate (see tidepool-repl/CLAUDE.md).
- F1's fix must key off WHICH item produced `last_value`, not re-derive "last
  ok item" — that heuristic is the bug.

## READ FIRST

- `tidepool-repl/CLAUDE.md` (response shape, item classification)
- `tidepool-repl/src/session.rs` — `run_block` (:400-530), `decl_head`
  (:2604-2644), Reset arm (:1545-1566)
- `tidepool-repl/src/truncate.rs` — stub shapes (:174-181, :296)

---

## F1 (MEDIUM, verified): `run_block`'s value-dedup strips fields from the WRONG item — destroys `:stub` payloads

**Where:** `session.rs:507-520`. The comment says "suppress `value` from the
last ok VALUE item's slim result," but the loop finds the last ok item OF ANY
KIND and unconditionally `obj.remove("value")` / `obj.remove("truncated")`.

**Failure A (data loss):** `{items: ["<expr whose result truncates>",
":stub 0"]}` — item 0 (`TurnOutcome::Value`) sets `last_value`; item 1's Meta
result from single-page `stub_fetch` (`truncate.rs:296`) is
`{"stub": 0, "value": <full subtree>}` — the loop strips ITS `"value"`, so the
caller gets `{"stub": 0}` with the fetched content silently gone, while
top-level `value` shows item 0's TRUNCATED value. Any Meta payload carrying a
`value` key after a value-producing expression is vulnerable. Existing tests
miss it: `stub_fetch.rs` always fetches in a separate 1-item block.

**Failure B (duplication):** `["2+2", "x <- e"]` — the strip hits the bind
item (no-op), the expr item keeps its inline `value`, and top-level `value`
repeats it — exactly the duplication the code exists to eliminate.

**Fix:** record the `results` INDEX of the item whose `TurnOutcome::Value` set
`last_value` and strip exactly that entry.
**Verify:** red test — two-item block `[big-expr, ":stub 0"]` asserts the stub
result retains its `value`; plus `[expr, bind]` asserts no duplication and
correct top-level.

## F2 (MEDIUM): three-way doc/code split on "block ending in a bind leaves `value` null"

**Where:** `session.rs:482-491` (code: `last_value` set from ANY
value-producing item) vs `server.rs:1052-1054` (tool description) and
`tidepool-repl/CLAUDE.md:47-48` (both promise null); `command.rs:260` ("last
value-yielding Stmt") matches the code.

**Failure:** `["someExpr", "x <- e"]` returns `value = someExpr's value`; a
caller scripting on `value == null` to detect "ended in a bind" (the
DOCUMENTED contract) misattributes an earlier expression's value to the block
outcome.

**Fix (pick one, then align all four surfaces):** either take `last_value`
only when the FINAL ok item is a Value (recommended — matches the documented
contract and GHCi intuition), or fix the tool description + CLAUDE.md +
command.rs wording. Coordinate with F1 (same code region).

## F3 (LOW-MED): `decl_head` returns `"--"` for comment-prefixed declarations

**Where:** `session.rs:2604-2644`. An LLM-natural item like
`-- slugify a title\nslug t = ...` classifies as Decl via GHC (correct), but
`decl_head` extracts `"--"`. Four consequences:
(a) slim result reports `{"decl": "--"}` instead of `{"decl": "slug"}`;
(b) `defined_outcome` (:687) runs a wasted ~6s type probe for `--` (always
fails → the #317 type-painting is silently LOST for every comment-led decl);
(c) the within-block redefinition splitter (:401-409) sees two unrelated
comment-prefixed decls as sharing head `--` with `defines_head` true, splits
the batch, and defeats whole-block decl elaboration;
(d) `stale` computation via `mentions_word(src, "--")` false-positives on any
live bind whose defining text contains a comment.

**Fix:** strip leading comment/blank lines in `decl_head` before token
extraction.
**Verify:** decl item with a leading `--` comment → response carries the real
head + painted type; two comment-led decls in one block stay one batch.

## F4 (LOW): in-block `:reset` leaves inconsistent state on two edges

**Where:** `session.rs:1545-1566`.
(a) machine/bindings/val_gen/stubs/pure_binds are cleared BEFORE
`SessionLib::open` is attempted; if that open fails (IO error), `self.lib`
keeps the old decl log — a half-reset session (old decls resolve, every bind
they referenced gone) behind an error implying nothing changed. Fix:
rebuild-then-swap (open the new lib first, mutate only on success).
(b) `self.machine = None` bypasses `publish_cancel`, so the shared
`CancelSlot` retains the dropped machine's stale `CancelHandle` until next
bootstrap; a server-side timeout in that window cancels a dead flag, the grace
re-wait can't self-heal, and the session is marked `Wedged` unnecessarily.
(Server-side `session_reset` is fine — fresh worker/slot; only the in-block
`:reset` meta.) Fix: clear the slot in the Reset arm.

## Opportunities

- **Triple extract spawn per stmt item:** an `Auto` item classified Stmt pays
  `classify_turn` in `decl_shaped_text`, then `run_def`'s binder-extraction
  failure, then `classify_turn` AGAIN in `run_eval` — three process spawns
  before the real compile. Plumb the verdict from `decl_shaped_text` into
  `run_one_item`. (~seconds per stmt item; biggest repl-latency win found.)
- **Hidden `:stub 0` on huge values:** `truncate_for_it`'s hint
  (`truncate.rs:174-181`) mentions only `it`; the module doc promises the full
  value is "fetchable via `:stub 0`" but the response never says so. One
  clause in the hint closes the discoverability gap.
- **Bare-expr error fidelity:** `run_bare_expr` reports the pure-wrap error on
  double failure (`session.rs:1394`). For a well-typed EFFECTFUL expression
  whose result merely lacks `Show`, the faithful error is the monadic wrap's
  ("no Show instance for X"); surface `_monadic_err` when the pure error names
  `Eff`.
- **Per-item `it` rebinds in multi-item blocks:** every non-final bare
  expression pays a full bind-turn compile + tenure to rebind an `it` the next
  item immediately clobbers; binding `it` only for the block's final Value
  item saves a compile per intermediate expression.
- **CLAUDE.md drift:** tidepool-repl/CLAUDE.md doesn't mention the `it`
  binding, `truncate_for_it`/`HUGE_CEILING`, or that bare expressions now take
  a bind-turn (compile-cost) path — plus the F2 `value`-null claim.

## Verified clean — do NOT re-audit

Tenure forward-skip fix (`old_space.rs:175-187`) — fix lives in `tenure()`
itself so all three bind primitives share it; forward chains impossible.
`run_fragment_and_bind_render` — field0 rooted across the field1 bridge,
read-before-tenure holds under aliasing; covered by `it_binding.rs` CASE 5/7.
`it` semantics — not rebound on error turns (GHCi parity); `bind_materialized`
retracts a user decl-plane `it` exactly as GHCi clobbers it; OVERLAPPABLE
`Show` floor on `toWire` (`tidepool-mcp/src/preamble.rs:290`) degrades
no-ToWire types exactly like GHCi's no-Show errors. Session state machine —
every transition is lock→move-out→unlock→await; suspension owns
`response_tx`+`session_rx` so every teardown path structurally unparks the
worker; Busy guard / wedge / self-heal / detached-resolver all resolve state.
One benign race: a turn completing inside the post-timeout grace window is
reported "timed out and was aborted" though its bindings landed — tiny window,
self-consistent. Decl error paths — `define_batch` rolls back the log and
deletes the gen module on validation failure; runtime-failed binds never enter
the `BindingTable`; orphaned `Val.G<g>.hi` at a reused gen is inert.

## DONE CRITERIA

- [ ] F1+F2 fixed together; stub-payload red test + bind-final-null test green;
      tool description / CLAUDE.md / command.rs aligned
- [ ] F3 fixed; comment-led decl gets real head + painted type
- [ ] F4 rebuild-then-swap + cancel-slot clear
- [ ] Opportunities triaged (at minimum: file the triple-spawn latency fix)
- [ ] `cargo nextest run --ignore-default-filter -p tidepool-repl` green
