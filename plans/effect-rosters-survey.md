# Survey: advertised-vs-serviced effect gap (vestigial-subsystems review §4)

Scope: `standard_decls()` (`tidepool-mcp/src/eval_prep.rs`) and its production
consumer `EffectRoster::from_handlers` (`tidepool-mcp/src/server.rs`)
unconditionally append `Ask`, `RunLLMTurn`, `Fork` to every base stack. This
note pins the true serviced set per surface before changing any roster.

## Per-surface advertised vs. serviced (as of this branch, before the fix)

### One-shot MCP eval server (`tidepool` binary, `TidepoolMcpServer`)

- **Roster builder (production):** `EffectRoster::from_handlers`
  (`tidepool-mcp/src/server.rs`) — independent of `standard_decls()`, but
  appends the identical suffix `[Ask, RunLLMTurn, Fork]`.
- **Request parser (serviced set):** `tidepool_runtime::session::engine`'s
  `extract_ask_request` (`tidepool-runtime/src/session/engine.rs:1211`) —
  matches `con_name` against `"AskWith" | "RunLLMTurnWith"` only. Any other
  constructor (`ForkWith`, `ForkAllWith`, `RunLLMTurnFreezeWith`) hits the
  `other =>` arm and aborts the pending continuation with a parse error.
- **Gap:** `Fork` is advertised (GADT + `forkSited`/`forkAllSited` helpers +
  `Tidepool.Fork`'s `forkFilter`/`forkMap`/`forkCata` become nameable) but a
  suspend on `ForkWith`/`ForkAllWith` always fails at resume time — no
  scheduler exists on this surface to answer it.
- **Residual, not fixed here:** `RunLLMTurnFreezeWith` is a constructor
  *within* the `RunLLMTurn` GADT this surface legitimately needs (basic
  `AskWith`/`RunLLMTurnWith` suspension is genuinely serviced). Splitting a
  GADT's constructors across surfaces would need a new per-constructor
  gating mechanism `ROW_DEPENDENT_EFFECTS`-style; out of scope for an
  effect-family-level roster fix. Flagged, not solved.

### REPL (`tidepool-repl`)

- **Roster builder:** the SAME `EffectRoster::from_handlers` (confirmed via
  `tidepool-repl/src/main_setup.rs`, `session.rs`, `server.rs`, `manager.rs`
  — none of them call `standard_decls()`; all build via `EffectRoster`).
- **Request parser:** reuses the identical `extract_ask_request` codepath
  (`tidepool-repl/src/session.rs:1136` region — same `PendingTail`/`stow_ask`
  machinery as the one-shot engine).
- **Gap:** identical to the one-shot server — same mechanism, same fix site.
  Fixing `EffectRoster::from_handlers` fixes both surfaces at once (one
  mechanism, one home — no REPL-specific roster code exists or is needed).

### Harness Agent turn (`tidepool-harness::engine`, "the general Agent stack")

- **Roster builder:** `agent_decls()` (`tidepool-harness/src/engine.rs:1600`)
  = `standard_decls()` + `finalize_decl()`.
- **Hole classifier (serviced set):** `classify_hole`
  (`tidepool-harness/src/engine.rs:549`) genuinely matches `"ForkWith"` and
  `"ForkAllWith"` (lines 569, 582) and decodes `RunLLMTurnFreezeWith`'s
  payload via `classify_runllmturn_payload`. This is a DIFFERENT dispatcher
  than the ordinary session engine's `extract_ask_request` — `Harness::
  run_to_hole_or_done` drives it, not `SessionEngine`.
- **Conclusion:** this surface genuinely needs `Fork` (and the full
  `RunLLMTurn` GADT including `Freeze`) — its roster was already correct in
  *content*; the problem was only that it borrowed the wrong-shaped
  `standard_decls()` for its base rather than owning its addition
  explicitly.

### Selfharness answerer/outer rows (`tidepool-harness/src/selfharness/driver.rs`)

- Out of scope per task boundary (two concurrent owners on that file). Read
  only: `outer_decls()`/`answerer_decls()` do NOT call `standard_decls()` —
  they already build their own explicit rosters (`RunLLMTurn`+`AskUser` for
  outer; `AskUser`+`Fork`+`ReadState`+`Green`+`Finalize` for answerer). This
  confirms the review's claim that these two rows were already correct — no
  changes made or needed here.

## The fix

`Fork` was always the LAST element of the interposed suffix on every surface
that had it. Removing it from one family's roster while re-adding it
explicitly on the other preserves every OTHER effect's union tag exactly —
**no positional-tag hazard**: Console..Time keep tags 0-8, `Ask`=9,
`RunLLMTurn`=10 on both narrowed and widened rosters; only whether tag 11
(`Fork`) exists at all differs per surface, and (for the harness) `Finalize`
still lands at tag 12 same as before.

1. **`standard_decls()`** (`tidepool-mcp/src/eval_prep.rs`) narrows to
   `base9 + Ask + RunLLMTurn` (drops `Fork`) — this becomes, in substance,
   the ordinary one-shot/REPL session roster (matching the bulk of its 90+
   existing callers, which test exactly that surface). Kept the name: it is
   still "the standard row" in the sense that every OTHER surface's roster
   is defined as an explicit widening of it (harness adds `Fork`+`Finalize`;
   debug adds `Meta`).
2. **`EffectRoster::from_handlers`** (`tidepool-mcp/src/server.rs`) drops
   the `fork_decl()` push from its interposed suffix — the single
   mechanism shared by the one-shot MCP server and the REPL, so this one
   edit fixes both surfaces.
3. **`tidepool-harness::engine::agent_decls()`** now explicitly pushes
   `fork_decl()` (then `finalize_decl()`, unchanged) on top of
   `standard_decls()`, documenting that the harness Agent turn's wider
   vocabulary is a deliberate addition, not an inherited default.
4. **`tidepool/src/stack.rs`'s `debug_decls()`** needed no code change — it
   already splits `standard_decls()`'s output at `Ask` and reinserts `Meta`
   before the (now Fork-less) interposed suffix, so it tracks the narrowed
   roster automatically. Its pinned `EXPECTED_ORDER` test and doc comments
   are updated to match.
5. Every test call site that specifically exercises `Fork`
   (`acceptance_fork_combinators.rs`, `jit_surface.rs`'s `works_fork`/
   `works_fork_map`, four `Fork`-titled tests in `run_llm_turn_sidecar.rs`)
   now explicitly appends `fork_decl()` to `standard_decls()`'s result,
   naming the widening at the call site instead of inheriting it silently.
6. Protocol goldens (`tidepool-mcp/tests/goldens/protocol/*`) regenerated via
   `TIDEPOOL_REGEN_PROTOCOL_GOLDENS=1` — pure text diff, no GHC needed.

## Vocabulary vs. row (kept, per `tidepool-mcp/CLAUDE.md`'s stable-effects-core
section)

Nothing here removes `Fork`'s GADT/helpers from being NAMEABLE — a program
can still write `import Tidepool.Fork` and reference `forkFilter`/`forkMap` in
declarations that persist on the shared decl plane; it only fails to *resolve
a `Member Fork effs` constraint* on a surface whose row omits it, which is an
ordinary unsolved-`Member` compile error, not a define-time refusal. The
harness surface still declares it row-wide because it genuinely dispatches it.
