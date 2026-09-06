# Accepted usage baseline incorporation and cleanup

Incorporated exact root baseline `f2e3e9a79de9a9bd05a146189e2a7915c0779359`
via fast-forward merge, preserving candidate history. No source changes.

Direct incorporation checks at that revision (repository Nix environment):
- `git diff --exit-code 653cc396e8f192afc2dd9d0d4c34b41e74845b50 HEAD -- haskell/actors/evidence prompts/shoal/docs/watch.md tidepool/src/actor_host/documentation_tests.rs tidepool/src/actor_host/watch_documentation_progress.hs tidepool/src/actor_host/watch_documentation_settled.hs scripts/codex-worktree-guidance.md`: passed, owned implementation unchanged.
- Both previously documented `runghc -Wall -Werror` consumers: passed, 14 synthetic
  logic cases and historical survey assertions; GHC 9.12.2 inherited Nix toolchain.
- `cargo fmt -p tidepool --check` and `git diff --check`: passed; checkout clean.

Root's six-test execution at `551adab1b663d1620a3df1f3fe82c8386815b89c`
is attributed here and retained in `root-progress-doc-integration.md`; no repeated
extractor-backed test battery or unrelated run-map rerun was needed.

## Retained cleanup receipt

After their obligations settled, applied `traverse stopAgent` over actual
`forkedActor` handles in this exact order:

1. `helperWorker` (evidence-helpers): `StoppedNow`
2. `recipeWorker` (focused-recipes): `StoppedNow`
3. `helperReviewer` (helper-review): `StoppedNow`
4. `recipeReviewer` (recipe-review): `StoppedNow`

Live receipt is retained as `usageCleanup`; original delivery/review/watch values
remain retained by this lead. No specialist has a concrete remaining obligation,
so none was kept merely speculatively. This lead remains available for root.
No peer outside this subtree was targeted; no branch, worktree, commit or evidence
file was deleted. Retirement outcomes are lifecycle evidence, not a separate
process-by-process leak audit. Context recreation may lose arbitrary live values;
this committed summary retains the observed outcomes, not a serialization of them.

No composition/cleanup discovery friction occurred: ordinary list construction
and `traverse` over retained handles worked on the first call. No bulk registry or
new API was needed. No token savings, native service acceptance, or future-user
UX measurement follows from these checks.
