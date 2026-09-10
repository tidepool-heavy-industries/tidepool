Tidepool/Codex dogfood verification policy (operator instruction for this project):

Use targeted checks throughout both megatasks, including lane and coordinator
integration. Do not run complete Cargo/Cabal workspace suites or an unfiltered
`just test`/`just check` unless the human explicitly requests that run. A general
instruction to implement, verify, integrate or finish a milestone is not such a
request. This policy overrides generic repository advice to run a full suite.

Map changes to owning mechanisms and plausible directly affected consumers. When
changing a shared type, search all constructors/callers and compile affected test
targets before execution. Select actual target/test names; confirm the filter ran
the intended tests. Use the repository's focused recipes or
`bash scripts/dev-shell.sh COMMAND...`: committed flake inputs supply the environment,
while the command operates in your checkout. Never use `nix develop path:.` on
the mounted workspace; it copies warm artifacts into the Nix store. An inherited
`IN_NIX_SHELL` marker alone does not prove that GHC packages are available.

Prove changed behavior plus consequential failure, cancellation, cleanup and
integration paths. Unit checks do not replace a necessary real consumer test.
For extractor/serialization changes, retain the required `just fixtures-check`
boundary; that focused corpus is not permission for a workspace-wide battery.

Reuse exact-source evidence. After incorporating children, check the changed joins
and invalidated evidence rather than replaying every child's tests. A warm build
and usable shared contract are enough to fork; a broad green battery is not a
fork prerequisite. Do not clear shared caches or create fresh target directories.

When a check fails, preserve the result, diagnose the smallest failing case, and
rerun that case plus consumers affected by its repair. Distinguish environment
failure, compile-only evidence, executed failure and executed success. Do not
silently skip failures or widen to a full suite as a debugging strategy. Detailed
recipes and evidence rules: scripts/codex-worktree-guidance.md.
