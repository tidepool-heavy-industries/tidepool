# Wave 2 — `boot-lazy`: item 0 steps 1–3 (+6)

**Status: GO** (2026-08-09). The base merge has landed —
`root.extract-wave` @ `2e34ed10` (which carries `harness-lifecycle`'s async
driver rewrite, the F2 registry unification, and the F2/phase-b conflict
resolution) is merged into this branch at `83cf03c7`.

Every line number below was re-derived by grep **in this worktree, after that
merge**, not carried over from an earlier read. They drifted twice already
during the hold; a line number is only true against a named commit. Re-grep
before you trust any of them.

## Goal

Delete both pre-model boot seeds by making `ResidentSession` construction stop
demanding a program. The lazy lifecycle already exists one layer down; the
harness is the only caller that wraps it in an eager program-shaped
constructor, and to fit that API slot it manufactures a fake program
(`pure (toJSON (0 :: Int))`) and pays a full GHC extract compile for it. Twice,
once per stack, before the first model call.

This is the "Eliminate" rung of the D7 fix ladder. Not the cache rung — root
has ruled the cache interim OFF for this lane.

## Post-async anchors (verified at `root.harness-lifecycle`)

| File | Line | What |
|---|---|---|
| `tidepool-runtime/src/session/persistent.rs` | 269–271 | `PersistentSession::new` → `machine: None`. The laziness is already here. |
| `tidepool-runtime/src/session/persistent.rs` | 393 | `bootstrap_if_needed(expr, table)` — no-op when already live. The hook. |
| `tidepool-runtime/src/session/resident.rs` | 208 | `ResidentSession::bootstrap` — the eager constructor to replace. |
| `tidepool-runtime/src/session/resident.rs` | 220 | its `core.bootstrap_if_needed(...)` + `core.seed_session_table(table)`. |
| `tidepool-runtime/src/session/resident.rs` | 333 / 374 / 428 / 464 | `run` / `run_bind` / `run_child` / `run_child_pure` — the first-real-run hooks. |
| `tidepool-repl/src/session.rs` | 975, 1105, 1257, 1327, 1469, 2002 | the REPL's bootstrap-from-first-real-compile. **This is the pattern to mirror.** |
| `tidepool-harness/src/harness.rs` | 416 | the `boot: Arc<compile::CompiledTurn>` field. |
| `tidepool-harness/src/harness.rs` | 444–486 | `Harness::new` — builds `boot_src` (457), `compile_turn` (460), stores it (486). **Seed #2.** |
| `tidepool-harness/src/harness.rs` | 806 | `force()` — the ONLY production consumer of `self.boot`; its `ResidentSession::bootstrap` call is at 817–819. |
| `tidepool-harness/src/harness.rs` | 3251, 3294 | test fixtures fabricating a `boot` field. |
| `tidepool-harness/src/harness.rs` | 3315 | `fake_session` (its `ResidentSession::bootstrap` at 3320). |
| `tidepool-harness/src/harness.rs` | 3012, 3068 | `checkout_run` / `run_checked_out` — the CURRENT session-ownership API. |
| `tidepool-harness/src/selfharness/driver.rs` | 547 | `SelfHarnessDriver::bootstrap` (still sync post-async). |
| `tidepool-harness/src/selfharness/driver.rs` | 566–588 | `boot_src` + `compile_turn`. **Seed #1.** |
| `tidepool-harness/src/selfharness/driver.rs` | 600 | `crate::harness::Session::bootstrap(&boot.expr, boot.table, …)`. |
| `tidepool-codegen/src/jit_machine.rs` | 1940–1956 | `add_function` re-resolves ConTags against each fragment's table. |

## What the async conversion did NOT change (checked, so the recipe stands)

- `force()` still bootstraps from `self.boot` at the same point in the same
  sync function.
- `run_one_cycle` still calls `bootstrap()` and then `render_framing`, so the
  outer session's first REAL compile is still the pre-loop render.
- `compile_outer` / `render_framing` / `run_loop_fragment_inner` bodies are
  byte-identical to pre-async apart from `async` on their callers.

So the hook does not move: **the first real run is still where the machine
should boot.** Recorded because the wave TL asked specifically. Re-confirmed
after the `2e34ed10` base merge: every `driver.rs` anchor held to the line.

## Session ownership changed under this spec — read before you start

`take_session` / `put_session` **no longer exist** (zero occurrences under
`tidepool-harness/src/`, verified post-merge). Ownership is now
`checkout_run` / `run_checked_out` over `SessionRegistry`
(`harness.rs` 3012 and 3068; `registry.rs` ~150).
`tidepool-harness/CLAUDE.md`'s ownership section (~lines 51–71) is current
again — **read it first.**

**Root's standing instruction, in force, verbatim: if your work produces a
compile error naming `take_session` or `put_session`, convert the call site —
never restore the methods.** That loud failure is the merge design working.

This interacts with the fixture work in step 3: the three `boot`-fabrication
sites now sit in a file whose session handling was rewritten around them.
Re-read them before deciding the `#[cfg(test)]` shape — the right answer may
have gotten simpler.

## Steps

1. **Add `ResidentSession::unbootstrapped(...)`** next to `bootstrap`
   (`resident.rs` ~208): same arguments MINUS `expr`/`table`, constructing
   `PersistentSession::new(lib, ask_tag, nursery_size)` and returning the
   session with no machine and an empty session table. Document that the
   machine comes up on the first real turn.

2. **Bootstrap on the first real run.** In `run`, `run_bind`, and the child
   paths (`run_child`, `run_child_pure` — trace `prepare_child_fragment` too),
   call `self.core.bootstrap_if_needed(expr, table)` immediately BEFORE
   `add_fragment_session`. Two things make this the correct placement and both
   are load-bearing:
   - `bootstrap_if_needed` reaches `JitEffectMachine::compile_session`, which
     touches the `!Send` env, so it MUST run on the calling thread —
     `add_fragment_session` already runs there, and `on_eval_thread` (which
     moves the machine out) runs after. Do not hoist it past that boundary.
   - `merge_table` before bootstrap or after is a real decision, not a
     cosmetic one. State which you chose and why in your submit note.
   Mirror the REPL's sequencing rather than inventing one; read all six REPL
   call sites first, they are not all identical.

3. **Delete seed #2** (`Harness::new`, `harness.rs` 444–486): drop `boot_src`,
   the `compile_turn` call, and the `boot` field (416, 486). Point `force()`
   (817) at `unbootstrapped`. Then fix the test fixtures at 3251/3294 (they
   fabricate the field) and re-check `fake_session` (3320) — it exists so
   `NodeTree::force` has SOME machine to register.

   **Do not leave the production eager constructor alive as cover for a test.**
   That is the same defect one layer down: item 0 would report success while
   the mechanism it exists to delete stays reachable from production.

   If a test genuinely needs a live machine before any run, whatever path
   survives must be `#[cfg(test)]` — **gated at the compiler, not by
   convention**. A `pub` constructor that is merely named test-ish is reachable
   from production and therefore IS the eager constructor, whatever it is
   called. Naming is not a boundary.

4. **Delete seed #1** (`driver.rs` 566–588): drop `boot_src` + `compile_turn`;
   `Session::bootstrap` at 600 becomes `Session::unbootstrapped`.

5. **Step 6 falls out here — assert it, don't assume it.** `self.boot` had
   exactly one production consumer, so once `force()` builds an unbootstrapped
   session the answerer's machine comes up on its first real compile, which IS
   the model's first block. Add a test that pins it: after `force()`, the
   node's session has no machine; after its first turn, it does.

6. **Delete `ResidentSession::bootstrap` if nothing needs it.** If something
   does, say what and why.

## The ConTags leg — make the recovery OBSERVABLE

The outer machine will boot from `render`, whose expr is pure
(`pure (Loaded.render …)`) and may carry no RunLLMTurn ConTags. `add_function`
re-resolves ConTags against each fragment's table (`jit_machine.rs` 1940–1956),
so a `MissingConTags` boot self-heals when the loop fragment lands.

**Silent self-healing is exactly the failure shape this whole item is about** —
scaffolding that goes load-bearing because nothing names it. So:

- Pin BOTH legs by test: (a) a machine booted from a ConTags-free expr is in
  the missing state; (b) the next fragment carrying a table with tags heals it.
- Make the heal a NAMED event — a `tracing` breadcrumb at the refresh site, or
  an assertion that it happens exactly once. A future regression that stops the
  heal must surface here, not three files away as a confusing dispatch failure.

## Verification

- `cargo check --workspace`, `cargo clippy --workspace`,
  `cargo fmt --all -- --check`
- `cargo nextest run` (tier 1) — counts
- `export XDG_CACHE_HOME="$PWD/.cache"` then
  `scripts/battery-shard.sh tidepool-harness -E 'binary(/^acceptance_/)'` —
  per-binary counts, sharded further if over budget
- `scripts/battery-shard.sh tidepool-runtime`
- `scripts/battery-shard.sh tidepool-repl` — the REPL shares
  `PersistentSession`; a lifecycle change is downstream of it
- **The item's receipt**: `acceptance_boot_compile_count` (landed by
  `boot-count` in wave 1). It asserts the pre-model extract-spawn count against
  a single named constant. Lower that constant to what you actually measure and
  report the before/after as numbers. If it does not drop by 2, you have not
  finished.
- extract-fidelity-test — EVERY check passes; report the actual N/N (the total MOVES as checks are added; it is context, never a target)
