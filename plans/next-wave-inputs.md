# Inputs for the next planning wave

Well-defined bugs and structural follow-ups found during the 2026-09-23
review-and-fix wave, each with its evidence and the class it belongs to. The
wave's rule applies: fix the class, not the instance; delete a test when its
invariant is dead or covered elsewhere.

## Structural follow-ups

- **Two copies of the workspace.** `exomonad/examples/workspace/.exomonad`
  (template: scaffolded `AgentSpec.hs`, prompts, plans, and test copies of the
  Project modules) and the `.exomonad/workspace` submodule (what sessions
  compile and `exomonad new` installs) drift: the submodule has the
  compile-checked label fixes, the template has `Project.FieldNotes` and
  `Project.RebaseRouter`. Decide one owner for the Project modules and checks
  and make the other a derived copy or a pointer.

- **Typed errors.** About 245 `io::Error::other(format!(..))` domain errors and
  about 871 text-matching test assertions. Convert crate by crate as typed
  error enums land.
- **Timeout policy.** Deadlines are chosen per call site; give each subsystem
  one named budget set.
- **Retirement deadline** (`bridge/facade/src/actor_host/scoped_custody.rs`):
  recovery can consume the whole budget and leave none for finalize.
## From the exomonad-harness wave (2026-09-23)

The first build wave outside this repository: a GPT-6 Sol root, a core lead and
two leaves in `~/dev/exomonad-harness`. Their WIP is kept there on `master` and
the `exomonad/wave0/*` branches. Every agent was interviewed while paused; the
fixes the run motivated are in git history. Open items:

- **No way to list an actor's live descendants.** The root searched for one to
  see whether its lead's leaves had started, and found none.
- **Ending a turn looks like finishing.** A leaf ended its first turn without
  `respond`, although it knew `respond` settles its assignment: "the normal
  final-answer UI made the opposite feel plausible in the moment." Deferred to
  the standalone harness, which owns turns.
- **Operator input.** In a Codex pane, Enter steers a running turn; Tab queues
  until the turn ends, which can be many minutes.

How the run was observed, for the next one: tmux holds no scrollback for Codex
panes (alternate screen); each agent's Codex rollout in `~/.codex/sessions`
is the complete record, and forks carry `parent_thread_id`. Prompt caching
across forks held at about 99% from a child's first request.

## From wave 2 (2026-09-24)

A GPT-6 Sol root and one Sol child delivered the Responses transport slice in
`~/dev/exomonad-harness` (697727d). Two thirds of the root's wall time was its
seven Haskell cells at 90 to 150 s each; the daemon was replacing workers on
an RSS ceiling below a warm worker's footprint, so nearly every request ran
cold (fix in flight). Once workers stay warm, the remaining cell cost is:

- **Library lowering on every cold worker.** Request fa36fa4dfe3673f6 in the
  run's compiler log lowered 116 home modules, 81 of them `Tidepool.*`, for
  26.9 s of 37.4 s of module work. The worker's `GutsMemo` is process-lifetime
  only; `build_products_dir` persists GHC interfaces but not the prepared STG
  output, so a fresh worker redoes the lowering even when the interface is
  reusable. First bounded step: serialize the memo entry for stdlib modules
  only, keyed by the stdlib fingerprint and dflags, write once after a cold
  compile, load at worker start (touches `GhcPipeline.hs` and
  `PreparedStg.hs`; the prepared-module record has no serialization today).
  Installing the stdlib as a GHC package is the larger alternative: it changes
  when interfaces become visible, which the session `Val.G<g>` injection order
  depends on, and needs the fat-interface recovery path for every library call.
- **Three daemon round trips per cell.** `check_cell` and the pinned bind both
  parse and typecheck the same cell text in separate GHC processes; the check
  keeps only verdicts and binder pins. Folding them into one `PreparationKind`
  that continues past typecheck is possible, but it touches the generalization
  guarantee the pinned pass protects (`turn.rs` `run_turn_pinned`). About 3 s
  per warm cell; measure after the daemon fix before deciding.
- **A child's only cell is its `respond`.** The transport child paid one full
  cell (153 s cold) to deliver a value it had already computed.
- **One session module per carrier mount.** 45 bash calls left 46
  `Tidepool.Session.Val.G<n>` stub modules; every session request lists and
  compiles the live ones (16 in request fa36fa4dfe3673f6, growing with call
  count). Pooling several occurrences in one stub module is not an option:
  under the pipeline's forced `-O2` GHC merges the identical
  `x = GHC.Magic.lazy x` bindings, so a second carrier reads back the first
  carrier's value (caught by
  `host_carrier_mounts_json_text_and_job_payloads_from_one_compile_each`;
  `-fno-cse` is discarded like every per-module pragma). Remaining options:
  keep stub modules out of the downsweep when nothing in the request imports
  them, or retire superseded tool-call bindings at the workbench instead of
  keeping every one live.
