# Usage lead scaffold

Keep helpers task-local under `haskell/actors/evidence/`: no new public library
API, manifest change, auto-import, registry, launcher or runtime response state.
`Selection.hs` owns selected/executed counts; unknown/zero/partial/inconsistent
counts do not prove a check completed. Root's WaveContract remains unchanged.

Helper worker owns this directory and real coordinator/reviewer consumers and
focused pure tests. Use observed versus expected outcome separately; a reproduced
failure is not product success. Compile/load consumers through repository Nix
setup. No fabricated backend evidence; synthetic cases are logic tests only.

Recipe worker owns `scripts/codex-worktree-guidance.md` and a focused evidence
report under `plans/next/usage-measurement.md`. Measure existing focused commands
with exact identities and cache conditions; no new launchers or dependency surgery.
Avoid shipped prompt changes unless a concrete uncovered requirement needs them;
report any proposed prompt delta to TL for single-owner integration.

TL owns integration and fresh independent review after both candidates exist.
