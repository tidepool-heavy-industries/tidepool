# Dev spec: finalize-template-pin

The answerer prompt prescribes an answer shape that **does not compile**. Three
inference calls + three extract compiles (~90s wall) are burned per typed hole,
on the harness's own instructions.

Recovered from a live dogfood session's durable log: five model attempts on the
wizard's first hole. Attempts 2–4 are the identical failure, and it is ours.

## The defect

The answerer prompt says, verbatim: *"Reply with exactly:
`finalize @Contribution (Contribution { ... })`"*. The model complied exactly.
GHC:

```
Ambiguous type variable 'a0' arising from a use of 'toJSON'
prevents the constraint '(ToJSON a0)' from being solved.
Relevant bindings include _r :: a0 (bound at Expr.hs:62:3)
```

The model escaped only by stumbling onto an explicit `:: M Contribution`
annotation on attempt 5 — a shape the prompt never mentions.

`toJSON _r` is the TEMPLATE's own result-rendering line
(`tidepool-mcp/src/eval_prep.rs`, `template_haskell_impl` ~366-380):

```haskell
result :: Eff <stack> Value
result = do
  _r <- __user
  paginateResult 4096 (toJSON _r)
```

`finalize :: forall v a effs. Member (Finalize v) effs => v -> Eff effs a`
has a FREE result tyvar `a` (it is a terminal verb — it never returns). So
`_r :: a0` is unconstrained except by `ToJSON a0`, and GHC refuses.

`tidepool-harness/CLAUDE.md`'s finalize-pin section claims the opposite: *"`a`
free (so the template's `toJSON _r` defaults it rather than demanding `ToJSON
T`)"*. That claim is false in practice, and correcting it is part of this job.

## Where I got to — start here, do not re-derive

The claimed mechanism (defaulting) **is configured and still is not firing**,
which makes this far more tractable than a `finalize` redesign:

- `ExtendedDefaultRules` is ALREADY enabled, in both pragma blocks —
  `preamble.rs`'s `EVAL_PRAGMAS` (~38) and `eval_prep.rs`'s inline list (~118).
- BUT both also emit an explicit **`default (Int, Double, Text)`** declaration
  — `preamble.rs::PREAMBLE_DEFAULT_DECL` (~25) and `eval_prep.rs` (~146).

So the question is not "how do we redesign `finalize`" but **"why does
defaulting not resolve `ToJSON a0` here"**. Leading hypotheses, in the order I
would test them:

1. `ToJSON ()` is missing (or odd) in the vendored Aeson, so the first
   candidate `()` fails and the search does not reach `Int`. Note
   `plans/post-restart/README.md`'s "Small decided items" has `FromJSON ()`
   under active revision — the `()` instances there are known-incomplete.
2. The interaction between the explicit `default (Int, Double, Text)` and
   `ExtendedDefaultRules` is not what we assume (whether `()`/`[]` prepend to
   an explicit list or the explicit list replaces the standard one).
3. Defaulting is not attempted at all for this tyvar because of where `_r` is
   bound (a monadic bind inside `result`, not a generalized binding).

**Step 1 is a minimal GHC repro, not a code change.** Build the smallest module
that reproduces `Ambiguous type variable 'a0' … toJSON`, then vary one thing at
a time until you know which hypothesis is true. Report the finding BEFORE
implementing. Use the shared extract's GHC:

```
export PATH=/nix/store/i7xkw0wd599j23fbsz8ydmsfj4dp9831-ghc-native-bignum-9.12.2-with-packages/bin:$PATH
export TIDEPOOL_EXTRACT=/home/inanna/dev/tidepool/haskell/dist-newstyle/build/x86_64-linux/ghc-9.12.2/tidepool-extract-0.1.0.0/x/tidepool-extract-bin/build/tidepool-extract-bin/tidepool-extract-bin
```

That extract binary is SHARED and READ-ONLY — never rebuild it in the main
tree.

## The constraint that rules out the obvious fix

Do NOT annotate `_r` at a concrete type in the shared template. **One template
serves every eval in the system** — a concrete annotation there breaks every
ordinary `eval` whose result is a `Value`, an `Int`, a record, anything. Any
fix must either:

- make defaulting actually work (preferred — it is the mechanism the design
  already claims, and it fixes every unconstrained-result shape at once), or
- apply ONLY to a turn compiled against a `Finalize T` row, where the harness
  already knows the turn is answering a typed hole
  (`EngineConfig::turn_target` / `RowArgs` already carry exactly that fact).

Pick based on what step 1 finds. Say which and why.

## If the fix needs `haskell/`

Prefer a Rust-side fix. If step 1 says the real fix is a missing Aeson instance
in `haskell/lib`, **report before implementing** — that changes the prelude,
means rebuilding the extract in YOUR OWN worktree (never the main tree), and
may collide with the extract-wave lane. I will route it.

## Acceptance — the prompt and the template must agree by construction

The real requirement is that these two can never drift apart again.

- **A test that compiles the prompt's own example string.** Take the prescribed
  answer shape from wherever the prompt text is generated — do not retype it as
  a string literal in the test, DERIVE it from the same source the prompt does,
  or the test pins a copy while the prompt drifts. Compile it through the real
  turn path against a `Finalize T` row. It must succeed.
- Mutation-close it: revert your fix → that test must go RED. Report the exact
  assertion message the mutant produced.
- Fix `tidepool-harness/CLAUDE.md`'s false claim about `toJSON _r` defaulting.
  State what is actually true after your change. Describe what IS — no
  narration of the old behavior; that goes in the commit message.

Also verify the bare shape works WITHOUT the `:: M T` annotation the model had
to discover — that annotation is the workaround, not the fix.

## Out of scope — do not take these on

- **Error-feedback coordinates.** The fed-back GHC error points at
  template-space (`Expr.hs:62`, `_r`) the model cannot see. Real, lower
  priority, and it lives in `harness.rs`, which another dev is rewriting right
  now. Not yours.
- Changing `finalize`'s signature. The tyvar shape is documented as
  load-bearing for reasons beyond this bug (`finalize @T x` binding `v` first,
  the `Member` dictionary riding as the leading value arg for `Translate.hs`'s
  head-swap to `finalizeSited`). If your step-1 finding genuinely says the
  signature is the problem, STOP and report — do not change it unilaterally.

## Verify

1. `cargo check --workspace --all-targets`, `cargo fmt --all -- --check`,
   `cargo clippy --workspace`. Three clippy warnings are pre-existing and not
   yours (tidepool-codegen `large_enum_variant`, `engine.rs` `TurnOutcome`
   `large_enum_variant`, `selfharness_compaction_fixes` `type_complexity`).
2. Quick tier: `cargo nextest run`. Report the tests-RUN count.
3. GHC-heavy, in shards under the ~380s process kill, each through the slot
   script at its absolute path:

   ```
   /home/inanna/dev/tidepool/scripts/ghc-slots.sh run -- \
     cargo nextest run --ignore-default-filter -j1 -p tidepool-harness \
     -E 'binary(finalize_type_pinning) | binary(acceptance_finalize)' \
     --no-fail-fast
   ```

   Cover: `finalize_type_pinning`, `acceptance_finalize`,
   `acceptance_selfharness`, `acceptance_askuser`, `golden_path`,
   `acceptance_cross_turn`, plus whichever binary holds your new test.
   You changed a template every eval shares, so also run `-p tidepool-runtime`
   and `-p tidepool-mcp` GHC-heavy shards — a template regression there is the
   real blast radius of this change. NOT `selfharness_compaction` (known
   open-intermittent, ~200s); leave `TIDEPOOL_EXPENSIVE_TESTS` unset.

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
  scratch. Commit with `--no-verify` (the hooks run tests; standing directive).
- A flaky test never lands. Fix it, or narrow it to a documented
  non-property, repetition-gated 15+ runs.
- Two sibling devs are live: one owns `tidepool-harness/src/harness.rs`, the
  other owns `tidepool-harness/src/selfharness/driver.rs` and `replay.rs`.
  Stay out of all three files.

## Done criteria

- Step-1 finding reported: WHY defaulting does not fire, with the minimal GHC
  repro that shows it.
- Bare `finalize @T (...)` — exactly as the prompt prescribes, with no `:: M T`
  annotation — compiles through the real turn path.
- The prompt's example and the template are pinned together by a test that
  derives the example from the prompt's own source, mutation-closed.
- `tidepool-harness/CLAUDE.md`'s `toJSON _r` claim corrected.
- check / fmt / clippy clean; quick tier + GHC-heavy shards (including
  tidepool-runtime and tidepool-mcp) reported with per-binary tests-RUN counts.
