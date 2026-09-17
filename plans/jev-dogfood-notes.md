# Jev dogfood notes (2026-09-17)

Sol (low effort) roots Shoal in `~/dev/jev-dsl` and exercises the `Jev`
effect. Each run ends with an operator interview; the run's own log is
`~/dev/jev-dsl/.shoal/dogfood-notes.md`. This file records what was fixed and
what remains open.

## Run 1 (f1371f51)

Fixed after the run:

- Children spawned through the Haskell role plans (`ResearchEffects`,
  `CodingEffects`, ...) lacked `Jev`; the rows in
  `haskell/actors/Tidepool/Actors/Role.hs` now carry it.
- A notebook binding whose inferred type mentions jev-dsl internals failed to
  retain (`Not in scope: type constructor Jev.Core.Schema.Offer`). The
  workbench now imports the `Jev.Core*` modules qualified.
- `:=` and `:&` are in scope unqualified; `doc jev` says which names need `J.`.
- `exec_command`/`write_stdin`/`read_output`/`cancel_command` with an
  out-of-range `yield_time_ms` or `max_output_bytes` now answer with a text
  rejection instead of failing the cell with a bare runtime error. A refused
  command start (a research actor) says so in text.
- Socket directories are released once the process and hosted work are
  confirmed settled. The workspace retire error names its failing step, and
  the worktree-binding receipt no longer blames tmux.

Open:

- `nix develop` in an actor worktree failed with "Path 'flake.nix' ... is not
  tracked by Git" although the file is committed; `nix-shell` worked.
- A `String` cell result was shown as a list of characters once
  (`['L','e','f','t',...]`); not reproduced yet.
- Jev usage was not visible to the model without calling `J.usage`.

## Run 2 (634a580a)

Jev worked in research and coding children; the coding child's docs branch
passed `check.sh`.

Fixed after the run:

- `error` messages reach the notebook on the prepared route. After a call
  fails by raising, the exception is forced under a bounded budget and the
  failure becomes `RaisedExceptionMessage` with its text
  (`forcing::describe_raised_exception`); the corpus oracle treats it as a
  raise.
- Child retirement no longer fails `BuildResource` with ENOENT: the mount
  helper re-execs this binary by descriptor (`execveat`), because the
  retired view's `/proc/self` does not resolve.
- Research (inspection-only) actors can start commands. Their project view
  is mounted read-only; before, every command start was refused and they
  had no way to read files.
- `doc jev` has a complete pooled-packet cell (pool, `eachIn`/`askAbout`,
  `given`, `accept` projected with `selectedKey`), compiled by
  `doc_jev_pool_example_sends_one_request`, and explains the `let` layout
  rule behind run 1's `:&` parse error.

Open:

- `stopAgent` answers `StoppedNow` before cleanup settles; degraded cleanup
  arrives later as a notice with no handle to await or inspect.
- Evidence transcription (cells, commands, errors, Jev scores) into the log
  was the third-largest time cost; a notebook helper that appends a cell's
  source and result to a log would remove it.
- `spawnWatched` takes one child; launching a pair meant switching to
  `unfold` plus an applicative `watch`.
- Jev reported mass/confidence 1.0 for a subjective documentation choice.
- `documentation_tests::workspace_lead_repairs_locally_...` and
  `candidate_workspace_runs_its_own_model_free_recipes` fail on recipe type
  drift (`GitOid` vs `GitRef`, the `coding` signature); unrelated to this
  work and not yet checked against `main`.

## Run 3 (3ad454f7)

A research child triaged a test gap with Jev and read files through shell
commands; the root gated the hand-off with `J.accept`; a coding child added
`refKey`/`refPayload` coverage on a branch with `check.sh` green. A typed
`error` cell showed its message. The research child's cleanup settled.

Fixed after the run:

- Retirement failed with "Git operation active during retirement" when the
  root read the child's branch at the same moment; the capture now waits up
  to 60 s for running host Git commands (`GitCli::capture_within`).

Open:

- A bare `error "..."` cell fails to typecheck (overlapping
  `TidepoolCellExpression` instances for an ambiguous result); it needs a
  type annotation.
- After a display failure the notice reads `Value remains bound as
  observation50 ()`; the `()` is not the value's type.
- Jev reports mass and confidence 1.0 for subjective prioritisation.
- The root's top request across all runs: help choosing `J.Policy`
  thresholds from the distribution and the stakes, with a short rationale
  for acceptance or doubt.
- Wall-clock is dominated by children's full `nix-shell --run ./check.sh`.
