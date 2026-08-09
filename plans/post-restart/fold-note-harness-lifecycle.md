# Fold note: root.harness-lifecycle → harness-interaction-surface

Read this BEFORE folding this branch. The merge has one silent-failure mode
and one loud one, and the loud one is a feature.

## The conflict: F2 (this lane) vs phase-b (root's tip)

This lane's F2 eliminated `Harness::take_session` / `put_session`, replacing
them with `checkout_run` + `run_checked_out` over `SessionRegistry` — an RAII
`Checkout` whose `Drop` restores the session even on panic. That is what makes
the ghost-node fix real: a panicking turn can no longer strand a session
outside the registry.

Root's tip (`harness-interaction-surface` @ 3bf402a4) was written against a
base predating that fold, so it **still calls `take_session`/`put_session` at
13 sites** in `harness.rs` and still defines them (~2964/2970 on that tip).

phase-b (`6ada5dab`, one-spawn-per-turn) rewrote `harness.rs` 1100-1420 —
`run_block`, `run_bind_turn`, and it DELETES `classify_turn`. This lane edited
1089-1460 in the same file. They overlap textually.

**The intents are orthogonal.** phase-b changes how many extract spawns happen
per turn and which template compiles. F2 changes how the session is owned.
Resolution rule: **take phase-b's control flow, re-apply the checkout
discipline onto it.** Not one or the other. Concretely, each surviving
phase-b site of the shape

```rust
let mut session = self.take_session(node)?;
let (session, r) = tokio::task::spawn_blocking(move || { … }).await…;
self.put_session(node, session, …);
```

becomes

```rust
let checkout = self.checkout_run(node)?;
let r = self.run_checked_out(node, checkout, move |mut session| { … }).await?;
```

`finish_run` must NOT regain its `session: Session` parameter — this lane
dropped it because the epilogue no longer hand-restores.

## Loud failure mode (good)

This lane deletes the `take_session`/`put_session` METHOD DEFINITIONS. Any
phase-b call site that survives the merge is a **compile error**. So
`cargo check --workspace --all-targets` is a real gate here, not a formality.

A compile failure naming `take_session` is the merge working. **Fix it by
converting the call site, never by restoring the method.**

## Silent failure mode (the one to actually check)

If the merge resolves by KEEPING root's method definitions, every phase-b call
site compiles fine, half the harness bypasses the registry, F2 is silently
undone, the ghost-node fix is half-dead — and the tree is green.

**Mechanical check, non-negotiable:**

```
git grep -n "take_session\|put_session" -- tidepool-harness/
```

Must return **ZERO** hits under `tidepool-harness/src/` after the fold. Also
check `tidepool-harness/CLAUDE.md` (~54-58 on root's tip), which still
documents take/put as the ownership mechanism; this lane rewrote that prose,
and if root's stale version wins the merge the doc now lies about the
mechanism.

Third check: `fn finish_run` has no `session` parameter.

## Do not "deduplicate" the two reasoning lines

`engine.rs::drive_model_turn` carries two lines that both mention "reasoning"
and look redundant:

- root's `tracing::info!("model reasoning:\n{r}")` — the HUMAN-FACING summary.
- this lane's `reasoning_items` threading — the ENCRYPTED echo payload that
  providers require returned in position.

They are different things. Deleting the second silently kills reasoning
continuity, with no test failure that names it.

## Verification that was actually run on this branch

Post-merge of the previous tip (cd0f4002), on this branch's HEAD:

- quick tier: 1825 run / 1825 passed / 9 skipped
- `golden_path` 2/2, `acceptance_selfharness` 1/1,
  `selfharness_lifecycle` 4/4, `turn_lease` 3/3

After folding, re-run those four plus phase-b's own named receipts
(`acceptance_value_bind`, `turn_splice`) — `run_bind_turn` sits in the
conflict zone and is covered by both sets.

**Scheduling:** run these DETACHED. `selfharness_lifecycle` needs ~596s, past
this environment's ~380s background kill; foreground it dies mid-suite and
looks like a failure by exit status. Gate on tests-RUN counts.
`golden_path` is a 2-test binary — 2/2 is complete, not truncated.
