# Dev spec: registry-unify (F2 + F1)

Make `SessionRegistry` the ONE session-lifecycle truth (eliminate
`take_session`/`put_session`), and introduce ONE `terminate_node` retirement
op that terminalizes the tree entry, logs the event, and removes the session
atomically.

Source findings: `plans/post-restart/harness-lifecycle.md` §Scope items 1 (F1)
and 2 (F2). Both are external-review findings verified at HEAD.

## Boundary — read before touching anything

You work in `tidepool-harness` ONLY. A sibling dev is concurrently converting
`src/selfharness/driver.rs` to async and deleting its `block_in_place`
bridges. **DO NOT restructure `driver.rs`.** Your only `driver.rs` edit is the
body of `retire_answerer` (step 7) — one function, a couple of lines.

Do NOT re-fix what the durability wave already landed (see
`plans/self-iterating-harness/12-robustness-wave-receipt.md`): F4
append-fails-turn, the generation-tagged checkpoint, F6/F7 ordering, and the
per-node `TurnLease` are DONE. The `TurnLease` stays exactly as it is — it
guards the *turn span* (snapshot → provider await → log append → resident run
→ outcome publish); the registry guards the *machine*. They are different
things and both stay.

## The mechanism (decided — do not re-derive)

`crate::registry::SessionRegistry<M>` (`src/registry.rs`, fully built and
unit-tested) models `Idle | Running | RunningChild | Suspended{hole}` with
atomic checkout/restore. It already lives inside `NodeTree<M>`
(`src/forcing.rs:160`, exposed via `NodeTree::registry()`), and
`NodeTree::force(node, actor, machine)` mints the `SessionId` and calls
`registry.insert_idle(session, machine)`.

But the live `Harness` instantiates `tree: NodeTree<()>` — so that registry
holds unit, never a session. The real sessions live in `Harness::convos:
Mutex<HashMap<NodeId, NodeConvo>>`, where `NodeConvo.session:
Option<ResidentSession>` is hand-managed by `Harness::take_session` /
`put_session` (`src/harness.rs` ~2910; call sites ~1135, 1234, 1421, 1728,
1804, 2241, 2469, 2687 — **re-locate them, the line numbers pre-date your
branch**).

That hand-rolled discipline is the bug. A panic or a `JoinError` between the
take and the put wedges the session at `None` FOREVER — every later call on
that node returns `HarnessError::NoSession`, which is a lie (the node has a
session; it is gone). The `NodeConvo.session` doc comment already admits the
gap is observable mid-turn.

**The fix: `Harness.tree: NodeTree<ResidentSession>`.** `Harness::force_inner`
(`src/harness.rs` ~761-777) already builds the `ResidentSession` BEFORE it
calls `self.tree.force(node, actor, ())`, so that call simply becomes
`self.tree.force(node, actor, session)?` with no reordering. `NodeConvo` then
loses its `session` field entirely, and every take/put pair becomes a registry
checkout/restore.

**Why F1 and F2 are one job:** the panic/JoinError recovery path needs a
place to put a node whose machine is gone — that place is `terminate_node`.
And `terminate_node`'s "remove the session" half IS `SessionRegistry::remove`.
Doing them separately means writing the same code twice.

## Steps

1. **`Harness.tree` becomes `NodeTree<ResidentSession>`.** Change the field,
   the two constructors (`Harness::new` and the test-harness constructor
   ~3079), and `pub fn tree(&self) -> &NodeTree<ResidentSession>`. Fix the
   in-file test call sites that pass `()` to `force` (~3116, ~3177).
   `ResidentSession` is already imported in `harness.rs`.

2. **Delete `NodeConvo.session`.** Delete its doc comment (it documents the
   bug you are removing). Delete `take_session` and `put_session`.

3. **Every take/put pair becomes a checkout/restore.** Map by intent, not
   mechanically:
   - a fresh top-level turn → `checkout_run(sid)`
   - a resume of the node's own pending hole → `checkout_resume(sid, &hole)`
   - a nested child run against a suspended parent → `checkout_child(sid)`
     (this is `run_child`; the parent must stay `Suspended`)
   - completion → `Checkout::restore_idle()`
   - suspension → `Checkout::restore_suspended(hole)`

   Get the `SessionId` from `NodeTree::session_of(node)` (`src/forcing.rs`
   ~554). `Checkout::take()`/`put()` exist for moving the machine onto a
   blocking thread and back — use them where the current code moves the
   session onto `spawn_blocking`.

   The `suspend_table`/`suspend_asks` writes that `put_session` did stay in
   `convos` — only the *session* moves to the registry. Keep those writes at
   the same points in the control flow.

4. **A busy node returns a distinct Busy error, never `NoSession`.** Map
   `CheckoutError`:
   - `Unknown(_)` → `HarnessError::NoSession(node)` (the honest case: the node
     really has no session)
   - `Running(_)` / `RunningChild{..}` → `HarnessError::TurnInFlight(node)`
     (this variant already exists and already means exactly this)
   - `Suspended{..}` / `NotSuspended(_)` / `WrongHole{..}` → a variant that
     names the mismatch. Add one if none fits; do NOT collapse these into
     `NoSession`.

   Add a `From<CheckoutError> for HarnessError` so the mapping lives in ONE
   place and cannot drift per call site.

5. **Make `Checkout` panic-safe.** Today, dropping a `Checkout` without
   restoring leaves the slot `Running` forever — a `#[must_use]` catches the
   *forgotten* case at compile time but not the *unwound* case. Add
   `impl<M> Drop for Checkout<'_, M>`: if `self.machine` is still `Some` when
   the checkout drops, restore it as `Idle`. That makes a panic mid-turn
   recover to a usable session instead of wedging. The explicit
   `restore_idle`/`restore_suspended`/`abandon` paths all `take()` the machine
   first, so `Drop` sees `None` and does nothing — verify that is true of each
   before you rely on it.

   The `take()`-onto-a-blocking-thread case is different: the machine is off
   the checkout, so `Drop` cannot restore it. On a `JoinError` (the blocking
   task panicked and the machine is genuinely gone) call
   `terminate_node(node, …)` from step 6 — do not `put` a machine you do not
   have, and do not leave the slot `Running`.

6. **Add `Harness::terminate_node` — the ONE retirement path.** Signature
   roughly `fn terminate_node(&self, node: NodeId, reason: &str) -> Result<(),
   HarnessError>`. It must, atomically from a caller's point of view:
   - terminalize the tree entry: if `tree.state(node)` is already terminal
     (`Done` / `Cancelled`) leave it; otherwise `tree.node_cancelled(node,
     reason)` — which is also what logs the durable event.
   - remove the session from the registry (`tree.registry().remove(sid)`),
     dropping the machine.
   - remove the `convos` entry.

   It is **idempotent**: calling it on an already-terminated or unknown node
   is `Ok(())`, not an error. Use it from cancellation, from finalization
   teardown, from the `JoinError` path in step 5, and from `retire_answerer`.
   Delete `drop_session` (or reduce it to a call into `terminate_node`) —
   there must not be two ways to retire a node when you are done.

7. **F1, the ghost-node fix — `retire_answerer`.**
   `src/selfharness/driver.rs` ~899: it calls `self.agent.drop_session(node)`,
   which drops the `NodeConvo` and leaves the `NodeTree` entry `Running` or
   `Suspended` **forever**. The self-iterating harness's forever-loop retires
   one answerer per cycle, so it accumulates a ghost node every cycle.
   Change the body to call `terminate_node` with a reason naming the
   retirement. That is the ONLY change you make in `driver.rs`.

8. **Update the docs that now lie.** `tidepool-harness/CLAUDE.md` §"Machine
   lifecycle — `convos`, not the registry, on the harness's live path" is the
   exact claim you are inverting; rewrite that section to describe what is
   true after your change. Do not leave a "formerly" narration — describe what
   IS (repo comment policy).

## Verify

Full harness acceptance, per the change-class rule (this is
semantics-touching). Set up ONCE:

```
export PATH=/nix/store/i7xkw0wd599j23fbsz8ydmsfj4dp9831-ghc-native-bignum-9.12.2-with-packages/bin:$PATH
export TIDEPOOL_EXTRACT=/home/inanna/dev/tidepool/haskell/dist-newstyle/build/x86_64-linux/ghc-9.12.2/tidepool-extract-0.1.0.0/x/tidepool-extract-bin/build/tidepool-extract-bin/tidepool-extract-bin
```

That extract binary is SHARED and READ-ONLY. Never rebuild it, never touch
`haskell/`.

1. `cargo check --workspace --all-targets`, `cargo fmt --all -- --check`,
   `cargo clippy --workspace`. Three clippy warnings are pre-existing and not
   yours (tidepool-codegen `large_enum_variant`, `engine.rs` `TurnOutcome`
   `large_enum_variant`, `selfharness_compaction_fixes` `type_complexity`).
2. Quick tier: `cargo nextest run` (pure-Rust crates; includes
   `registry.rs`'s own unit tests). Report the tests-RUN count.
3. GHC-heavy acceptance, in shards so nothing exceeds the ~380s process kill.
   Every GHC-heavy invocation goes through the slot script, absolute path:

   ```
   /home/inanna/dev/tidepool/scripts/ghc-slots.sh run -- \
     cargo nextest run --ignore-default-filter -j1 -p tidepool-harness \
     -E 'binary(golden_path) | binary(acceptance_cross_turn)' \
     --no-fail-fast
   ```

   Cover, across shards: `golden_path`, `acceptance_cross_turn`,
   `acceptance_fork`, `acceptance_forkall`, `acceptance_fanout`,
   `acceptance_finalize`, `acceptance_askuser`, `acceptance_selfharness`,
   `turn_lease`, `selfharness_lifecycle`, `selfharness_persistence`,
   `selfharness_spine`, `acceptance_value_bind`, `agent_stack_scoping`,
   `finalize_type_pinning`.
   NOT `selfharness_compaction` (a known open-intermittent, ~200s, and
   `TIDEPOOL_EXPENSIVE_TESTS` stays unset).

## Mutation checks (required — a green run is not the receipt)

Two new tests, each closed by a mutation, not by passing:

- **Ghost node, terminal after retire.** Drive the self-iterating harness (or
  the same `force` → `retire_answerer` sequence directly) through more than
  one cycle and assert every retired answerer node's `tree().state(..)` is
  terminal, and that the count of non-terminal nodes does not grow per cycle.
  Mutation: revert `retire_answerer` to `drop_session`-equivalent (no
  terminalization) → the test must go RED.

- **Panic mid-turn recovers-or-Busy.** Induce a panic (or a dropped
  `Checkout`) inside a turn on a node, then make a second call on that node.
  Assert the result is either a successful turn (recovered via `Drop`) or
  `TurnInFlight`/a Busy-flavored error — and specifically NOT
  `NoSession`, and not a permanent wedge. Mutation: remove the `Drop` impl
  from step 5 → the test must go RED.

Report both mutation results (the exact assertion message each mutant
produced) in your submit note. A test that passes with the mutant applied has
not closed anything.

## Contention rules — verbatim, non-negotiable

- Every GHC-heavy run goes through
  `/home/inanna/dev/tidepool/scripts/ghc-slots.sh run -- <cmd>` (absolute
  path). NEVER `exclusive` mode. `.config/nextest.toml`'s `ghc-heavy` group is
  default-deny and caps concurrent extract compiles — do not override it.
- No LSP / rust-analyzer. `grep` and `Read` only. A per-worktree
  rust-analyzer is 3-5 GiB and this box is shared.
- Scope every kill to your OWN PID or your OWN worktree path. NEVER a bare
  `pkill -f <pattern>` — those patterns match other agents' prompts and kill
  sibling worktrees' processes.
- `--no-fail-fast` on any suite with a known red. Gate on tests-RUN counts,
  never on exit codes. Capture full output to a file and extract afterwards —
  never pipe through `head`/`tail` at capture time.
- Never `git add -A`. Never force-push. Repo-root `tmp/` is protected human
  scratch. Commit with `--no-verify` (the hooks run tests; that is a standing
  directive).
- A flaky test never lands. Fix it, or narrow it to a documented
  non-property, repetition-gated 15+ runs.

## Done criteria

- `take_session`/`put_session` are gone; `NodeConvo.session` is gone; the
  registry is the one lifecycle truth. (If you conclude mid-work that a
  strictly-RAII checkout guard over `convos` is the better landing, that is
  the sanctioned fallback — but you must say WHY in the submit note, with the
  specific obstruction. "It was easier" is not a reason.)
- `terminate_node` is the ONE retirement path; `drop_session` no longer exists
  as an independent way to retire a node.
- A busy node returns a Busy-flavored error, never `NoSession`.
- Both mutation checks land, each with its mutant-red receipt.
- `tidepool-harness/CLAUDE.md` tells the truth about machine lifecycle.
- check / fmt / clippy clean; quick tier + the GHC-heavy shards reported with
  per-binary tests-RUN counts.
