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

- **`error` messages are lost on the prepared route.** `raise#` records only
  `RuntimeError::RaisedException`; the exception object is rooted
  (`MachineState::prepared_exception`) but never rendered, so `error "..."`
  and every `checked`/`error . show` helper reaches the model as "Haskell
  exception raised". This cost the most time in run 1 and is why
  `command_receipts_preserve_owner_settlement_across_continuation_failure`
  fails (it expects the `CommandInvalid` detail). A fix needs either a
  bounded forcing observation of the exception after the failed call, or a
  projector-owned `error` that forces its message before raising.
- Child retirement reports `BuildResource: No such file or directory`; the
  failing step is now labelled, so the next run identifies it.
- The research role row contains `Commands`, but the runtime refuses every
  command start for inspection-only actors. Either drop `Commands` from that
  row or document the split.
- `nix develop` in an actor worktree failed with "Path 'flake.nix' ... is not
  tracked by Git" although the file is committed; `nix-shell` worked.
- A `String` cell result was shown as a list of characters once
  (`['L','e','f','t',...]`); not reproduced yet.
- Jev usage was not visible to the model without calling `J.usage`.
