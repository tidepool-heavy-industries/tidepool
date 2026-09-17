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
- `stopAgent` and each cleanup stop step wait for the host's release receipt
  (`ad4028072`). `StoppedNow` is final; `StoppedRetaining`/`StoppedReleasing`
  say otherwise, and only the latter is followed by a notice. The receipt
  separates "actor is stopped" from the retained components.
- `planCleanupFor` plans from a `Response`; the `doc cleanup` example had the
  exact type error the root hit. `doc unfold` shows two children with `<*>`
  and one watch over both.
- The display-failure notice no longer prints `()` as the value's type.
- `doc jev`: Cost (Jev is cheap, be Jev-dense), Calibration (a 1.0 mass with
  one contender means an under-specified pool) and a two-cell Jev-dense
  example compiled by a test.

Decided, not fixed:

- A bare `error "..."` cell is fully polymorphic; GHC cannot classify it as
  pure or effectful and defaulting does not apply to a custom class. `doc
  workbench` says to annotate it. Only `error`/`undefined` have that type.
- A cumulative Jev usage counter: dropped. Jev is cheap enough that the
  model should not meter it; `J.usage` per response remains.
- `J.explain` (policy rationale): a DSL addition, so it is a run-4 task for
  Sol, who owns the DSL design.

Open:

- Jev reports mass and confidence 1.0 for subjective prioritisation
  (provider-side; the doc pattern mitigates).
- Wall-clock is dominated by children's full `nix-shell --run ./check.sh`.
- `typed_reply_settles_response_and_wakes_registered_watch` fails on
  `EchoReport cache` vs `EchoReport "cache"`: a Text inside a Show'd record
  renders unquoted. Pre-existing; likely the same cause as run 1's
  character-list display.

## Run 4 (4ee4280b)

Sol low root, Luna children (`withModel (Literal "gpt-5.6-luna")`). Task:
review-and-merge the three branches from runs 1-3 into `shoal/integration`
through fresh Luna reviewers, a `J.accept` gate and `tryMerge`; the run was
stopped after that task to save quota (the polish and DSL tasks were the
wrong tier for Sol). Log: `~/dev/jev-dsl/.shoal/dogfood-notes-run4.md`;
observer notes: `~/.claude/jobs/4940a626/tmp/observer-run4.md`.

Verified: six stops, all `AgentStoppedNow`, no later release notice, final
roster root-only; `planCleanupFor`/`executeCleanup` on three groups; three
children admitted in one applicative `unfold` with one `watch`; `tryMerge`
fast-forward and two merge commits; a Luna checker's `check.sh` green on the
integrated revision. The Jev gate refused the reviewers' compact summaries
(confidence 0.26 to 0.34) and accepted the evidence packets (0.67 to 0.89):
gate on the artifact, not the narration.

Open, by time cost:

- The root's `boundWorktree` returned `Left (StorageFailure ".../worktrees/
  wt-02a87edd-.../.git" "mounted worktree ... requires filesystem
  recovery")`. That worktree id is run 3's coding child, whose retirement
  had failed; a stale retained worktree from an earlier run reached the
  next run's root binding. Investigate custody carry-over across runs.
- A worktree the root allocated with `createWorktree` is read-only to the
  root's own commands (`dist-newstyle: createDirectory: permission denied
  (Read-only file system)`), so the root could not run `check.sh` on the
  integrated revision and spent 13 minutes on a Luna checker instead.
  Decide whether root-allocated integration worktrees should be writable.
- Inferred selector declarations (`firstReview = \(a,_,_) -> a` without a
  signature) fail in the generated wrapper with `Not in scope: type
  constructor or class GHC.Types.ZonkAny`. Same family as run 1's
  `Jev.Core.Schema` import gap: an inferred type the wrapper cannot name.
  Either import it or reject the declaration with a message that says to
  add a signature.
- A cell that bound six 220-line file previews failed with `observation
  budget 100000 exhausted` before its Jev call. `doc jev` now says to bind
  short previews; consider not observing bindings the cell never displays.
- `listAgents` renders a full roster record per actor, usage observations
  included; five actors fill a screen. A compact default view.
- A watch forgotten by cleanup still delivered a `Pending → Ready` notice
  afterwards; `pollWatch` then answered `WatchUnavailable (WatchRejected
  ReplyStale)`. Suppress notices for forgotten watches.
- Sol's asks: `tryMerge` targeting a named branch directly; typed tuple
  selectors for multi-child responses; an `explain` for `J.accept`
  (handed to the DSL owner); a `reviewGate` helper bundling policy, packet
  and rationale.

Next run moves to `~/dev/shoal-evals/tui-test-app` (small Rust TUI, five
stub modules, seconds-fast `check.sh`): the orchestration loop is the object
of study, not the code.
