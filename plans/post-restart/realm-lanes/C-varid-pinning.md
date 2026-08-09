# Lane C — the Item-2 SessionVarId pinning test (verdict §7 step 5)

Crates: `tidepool-codegen` (unit) + `tidepool-runtime` (real-path integration).
The runtime half is GHC-heavy — it needs a ghc-slots slot and `TIDEPOOL_EXTRACT`.

## WHY — the property holds today by accident of a differently-motivated change

The realm design leans on VarId-keyed cross-realm isolation: two independent
realms sharing one `BindingTable` must never see each other's bindings seeded
into their compiled fragments. `realm-checklist.md` Item 2 establishes that this
property holds today, but ONLY as a corollary of two things that were built for
other reasons — fresh-id minting, plus commit `5d070690` ("D9"), which narrowed
`BindingTable::seed_external_env` from an unconditional sweep of every live
binding to the intersection with the fragment's referenced VarIds
(`tidepool-codegen/src/binding_table.rs:192-200`; wrapper at
`tidepool-runtime/src/session/persistent.rs:686-688`).

D9's own commit message states its motivation plainly: a compile-time cost
problem proportional to total live bindings. Nothing to do with realm isolation.
Its tests pin "narrowing the seed doesn't narrow GC-root retention" — NOT "realm
B's bindings stay out of realm A's env". The checklist says exactly what to do
about that, and the verdict makes it step 5:

> land a test that states the property in its own terms (two scopes, colliding
> local names, assert neither's `ExternalEnv` ever contains the other's
> `SessionVarId`) rather than inferring it from D9's own proportional-cost-focused
> tests, which could be satisfied by a future change (e.g. a per-fragment env
> cache keyed differently) that reintroduces the leak while still passing every
> test D9 added.

So the bar for this lane is NOT "a test that passes". It is **a test that would
FAIL if the leak came back**. Everything below serves that.

## SCOPE

Two tests. Both new files; edit no existing test.

### C1 — unit level, `tidepool-codegen` (pure Rust, fast tier)

New `tidepool-codegen/tests/binding_table_realm_isolation.rs` (or a `#[cfg(test)]`
module in `binding_table.rs` alongside the existing D9 tests — your call, but if
you add to `binding_table.rs` you may only APPEND; do not touch the existing
tests there).

- Build one `BindingTable` holding two disjoint "scopes" whose bindings COLLIDE
  on display name: scope A binds `x` and `tmp`, scope B independently binds `x`
  and `tmp`, each with freshly-minted `SessionVarId`s.
- For each scope, call `seed_external_env(referenced)` with only that scope's
  referenced VarIds.
- Assert the resulting `ExternalEnv` contains that scope's ids and **contains
  none of the other scope's ids**. Assert on ids/slot addresses, not on counts —
  a count assertion passes vacuously if both scopes' ids happen to be seeded and
  one is also missing.
- Assert the same in BOTH directions (A's env free of B's ids AND B's env free of
  A's), because the leak is not symmetric under all plausible regressions.

### C2 — real path, `tidepool-runtime` (GHC-heavy)

The unit test alone would pass against a hand-wired table that no production code
path builds. The property must be pinned where callers actually compute
`referenced`. New `tidepool-runtime/tests/realm_varid_pinning.rs`.

- Drive a real session through its production entry point, binding the SAME
  display name twice as two independent scopes (the callers that compute the
  referenced slice live at `resident.rs:338-339` and `:377-378` —
  `tidepool_repr::free_vars::free_vars(expr)` then `seed_external_env(&referenced)`).
- Compile a fragment that references the SECOND scope's `x`, capture the
  `ExternalEnv` that fragment is compiled against, and assert the FIRST scope's
  `SessionVarId` is absent from it.
- Model the existing runtime session tests for setup (look at
  `tidepool-runtime/tests/session_seed_external_env_root_retention.rs` — it is
  D9's own test and shows how to reach the env; do not edit it).
- If reaching the `ExternalEnv` from a test requires a new accessor, add a
  narrow, documented one rather than making a field `pub`. Say in the doc comment
  that it exists so the isolation property can be asserted directly.

### THE FALSIFICATION CHECK — required, reported, not committed

Before you report done: make the leak come back and confirm BOTH tests die.

Temporarily revert `seed_external_env` to its pre-D9 behavior — sweep
`self.live.values()` unconditionally, ignoring `referenced` — and re-run C1 and
C2. Both must fail, and fail by finding the foreign scope's `SessionVarId` in the
env. Record the failure lines. Then REVERT the patch; it is not committed
(production code carries no test-only escape hatch — same discipline as the
falsifier's negative control in `realm-prototype.md`).

If a test stays green under that patch, it is not pinning the property and must
be rewritten. Report that honestly rather than shipping a vacuous green.

## VERIFY (receipts are per-binary PASS COUNTS, never exit codes)

```
cargo nextest run -p tidepool-codegen -E 'binary(binding_table_realm_isolation)'
cargo nextest run -p tidepool-codegen
/home/inanna/dev/tidepool/scripts/ghc-slots.sh run -- cargo nextest run --ignore-default-filter -p tidepool-runtime -E 'binary(realm_varid_pinning)'
cargo clippy -p tidepool-codegen -p tidepool-runtime --all-targets
cargo fmt --all -- --check
```

`tidepool-runtime` is in nextest's `default-filter` skip set, so a bare `-p
tidepool-runtime` reports "0 running, N skipped" — `--ignore-default-filter` is
required. Report the "N tests run: N passed" line for each command, plus the two
falsification failure lines.

Do NOT run a bare `scripts/battery.sh` — it is unbounded and this environment
hard-kills background processes at ~380s.

## BOUNDARIES

- Zero pre-existing tests edited, in either crate.
- Do NOT touch `tidepool-runtime/src/session/resident.rs`'s `pending` /
  `ChildSuspended` machinery — that conversion is HELD on a cross-lane signal.
  Reading `resident.rs` to find the referenced-slice call sites is fine.
- Do NOT change `seed_external_env`'s behavior. This lane pins the current
  property; it does not alter it. The only edit to production code you may make
  is a narrow test accessor if one is genuinely needed.
- Do NOT add a realm/owner field to `BindingTable` — that is lane D's question
  (display names) and it is currently deferred.
- Do NOT merge source-level capability rows.
- Comments describe what IS — invariants, not the story of the change.

## OPERATIONAL (verbatim)

- Every GHC-heavy run goes through
  `/home/inanna/dev/tidepool/scripts/ghc-slots.sh run -- <cmd>`
  (absolute path). NEVER `exclusive` mode.
- `export XDG_CACHE_HOME="$PWD/.cache"` before any tidepool-harness
  test shard (persistent per-worktree, not mktemp).
- Spawns pass an explicit `model: sonnet` (or `opus` for sub-TLs);
  never fable.
- Never path-unscoped `pkill -f`; scope kills to PID or full worktree
  path.
- Commit with `--no-verify`. Never `git add -A`. Repo-root `tmp/` is
  protected. Grep/Read over LSP; no per-worktree rust-analyzer.
