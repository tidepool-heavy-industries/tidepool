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

## From wave 3 (2026-09-24, run 8a782b2b)

Twelve actors on warm workers (zero replacements, lowering median 1.4 s):
the daemon fix held. The tree then serialized on one machine checkout:
every child gets its parent's `SessionId` unconditionally
(`resident_workbench.rs` request build, `start.rs` `capture_decoded`), so
all 12 actors queue on one registry slot. 6741 admissions in 30 minutes,
2879 s cumulative wait, average wait per call about 17 s with ten actors
active; `checkout_wait_ms` was most of every bash, cell and lookup call.
A `selected`-context child (the `lunaTask` default) uses none of the shared
session's scope chain or generations; it only needs its own machine, carrier
mounts and the compiled workspace, which the daemon memo and build products
already share across sessions. First step: mint a fresh session for
`SelectedContext` children; `InheritedContext` children keep the parent's.
- **What holds the machine.** Of about 1250 s of checkout hold in the same
  30 minutes, 344 s was the Cranelift compile inside `install_prepared`
  (`tidepool/runtime/src/session/prepared.rs`, `compile_for_install` after
  `link_program`; 504 installs, median 79 ms, 16 over 3 s totalling 172 s,
  max 14.9 s) while the install itself never exceeded 91 ms; 283 s was
  compiles run through `with_machine_wait` (fork release and activation
  turns, which bypass the off-checkout split); the rest was cell execution
  steps. Per-child sessions are not a shortcut: request delivery is gated on
  session equality and mailbox values are native to one machine's heap
  (`resident_actor.rs` request submission, `mailbox.rs`), so that route needs
  a cross-heap transfer primitive first.
- **Root interview, wave 3 (07:36Z).** Slow at the coordination boundary:
  waiting on children, then reviewing candidates built against older
  branches and extracting only owned files. Most useful: explicit file
  ownership, exact submitted commits, `respond`, watch-before-wait. Least
  useful: the review recipe assumes `sessionInput :: Task` and a `Candidate`,
  which a root lacks. Hand-building `Task` records and fork-group paths slowed
  delegation and blocked one child's micro-fork. Asked for: a Task-from-brief
  constructor (brief, owned paths, acceptance, `currentCheckout`), a review
  entry taking an exact commit, a path-scoped integration step that refuses
  unowned changes, watch notices distinguishable from already-consumed
  results, bounded hook diagnostics. Project-local shapes for the harness
  workspace's Project.Work, not engine helpers.
- **After-tool hook budget.** 107 of 503 after-tool slot invocations failed
  with `observation budget 100000 exhausted` after delivering a response, and
  each appended "[after-tool] This result is unannotated ..." to the model's
  tool result. Lane in flight.
- **Fresh-context children do not know the cell environment.** A Luna
  review child in wave 3 failed six cells in a row on `Data.Text.pack`,
  `Text.unlines`, and `String` versus `Text`; the cell offers Text only as
  `T` and nothing in a fresh context says so. Either expose `Text` as an
  alias next to `T` in the cell preamble, or make a not-in-scope error for a
  `Data.Text` name state the alias that exists.

## Wave-3 interviews and log dives (2026-09-24)

All 25 live actors were interviewed (root three rounds, 24 children two
rounds) and thirteen log dives were spot-checked; digests and analysis are
retained under the session scratchpad (`wave3/interviews/ANALYSIS.md`,
`DIVES-ANALYSIS.md`). Only 8 of 32 actors forked. The actors' own reasons,
in order: follow-ups arrived by mailbox to the existing owner ("I let
continuity become a default"); one owned file read as one indivisible task;
delegation cost three to four root turns and 32-184 s of admission cell per
fork; cells waited 30-60 s on the checkout; nothing in the prompt demanded a
fork. Admission cost is per cell, not per child, and compile grew 1.75x over
the run with session size.

- **Engine defects (fix wave, 2026-09-24).** `respond` unmounted while a
  native mailbox delivery to the actor was pending (Receiving standing
  selects the request-less workbench); `lookup` never sees activation
  bindings (Lookup boundary hardcodes `application_workbench`); a two-unit
  cell failure hides that unit 1 submitted the reply; a trailing operator
  becomes a valid section and a type error; stale watch notices after the
  owner polled Ready; reviewers forked from master could not run the
  candidate's tests (`reviewCandidate` seeds at the commit; the root built
  review Tasks by hand because the recipe assumes `sessionInput`); four
  operator questions queued with no delivery (decided: prompt-only, ask
  your parent, the root's parent is the operator).
- **Turn waste.** Root: 43 percent of active time in watch/poll cells (each
  a full compile), 32 of 46 workspace checks re-run with no change, 17
  turns with no decision; swarm-wide 19 of 165 turns made no tool call
  (stale wakes and standing owners answering informational relays).
  Fix: native `status watches` view, wave-sized `unfold`, prompt rule to
  fork review/test children before implementing.
- **Unused capabilities.** `focus` 0 of 1,102 bash calls (137 truncations,
  77 unrecovered); `read_output` once; `write_stdin` 118 times as a wait
  call; Sift, `Cmd.run` in cells, Project.Investigate/Merge/Review/Search,
  `followWork`, `consultDesign` never; skills command, coordinate,
  orchestrate, jev never loaded. Fix: descriptions that say when to use
  them; root-facing recipes.
- **Cell rejections.** 124: 48 type mismatches (21 Text), 30 parse, 26
  not-in-scope (`respond` 7). Fix: `Text` by name in the preamble,
  alias hints on not-in-scope, dangling-operator diagnostic, per-unit
  receipts.
- **Hand-off gaps.** Three of four seam guesses were right but incomplete
  and extended later without a rejection; two children forked into the
  same file blind; the manifest block cost 13 minutes and the child had
  `sendMessage` but responded "Blocked". Fix: sibling roster at activation,
  task prompt tells children to state seam assumptions and to request
  owner changes; Task constructor with defaults and explicit effort.
- **Deferred.** New Rust modules cannot be compiled without a `mod` line in
  an unowned parent file (three false "tests passed"; two children did 8
  and 21 backup-and-restore cycles on main.rs); watch delivery redesign
  (the new harness's async tool calls replace polling);
  `request_user_input` surfacing to the operator.

## Operator-side typed requests (2026-09-24, idea)

- **`exomonad ask`**: an external command that reserves a typed request on a
  named actor (`--actor <path> --type <Reply>`, message text as the
  assignment), waits for the settlement, and prints the reply. It is the
  actors' own `request`/`respond` pair with the operator as caller, so the
  brief paste (tmux load-buffer, settle, Enter, re-Enter) and interview pane
  scraping both become one RPC with a typed answer. Wave-3 interviews and
  the wave-4 launch still went through tmux.

## Compile cache and library identity (2026-09-24)

- The compile cache under `~/.cache/tidepool` is not keyed on the Haskell
  library's source identity. Changing `bridge/haskell/lib` under a live
  session (the `AttemptReplyWith` arity change landing while a recipe check
  ran) let cached prepared artifacts meet the new source in one session and
  fail as a `DataConTable` collision. `scripts/redeploy.sh` clears the cache
  so deployed runs are safe; dev checks are not. Key the cache (or the
  session's memo namespace) on `haskell_sources::source_identity()`, which
  `FrozenWorkspace` already computes.

## Cleanup refusal order (2026-09-24)

- `executeCleanup` on a stale plan refuses with "actor N has no confirmed
  idle provider turn" before it reports `CleanupStalePlan`. The facade test
  `typed_reply_settles_response_and_wakes_registered_watch` (now ignored
  with this reason, after its cells were brought up to `forkGroupHandle ::
  Maybe` and a typed `[label|..|]`) expects staleness first. Decide which
  refusal a stale plan should surface, then re-enable the test.

## Nudge ledger and child effect rows (2026-09-24, wave 4)

- The harness nudge layer (`.exomonad/Project/Nudges.hs`, 67e082e) writes its
  ledger through the `Journal` effect, which children do not carry, so the
  after-tool hook could not install on any child. Wave 4's root repaired it
  in its first minutes (`amend(correction): allow child startup without
  Journal effect`) by reverting to `Project.Watchdog`, so the ledger never
  ran. Either children get `Journal` in their rows or the ledger uses the
  doc's fallback (a file appended by pathspec). Verify with a harness recipe
  that admits a child under the spec, not only the replay test.

## Jev over any text, and Jev-steered pagination of structured values (2026-09-24, idea)

- `Project.Sift.sift :: Text -> Int -> Text -> Eff effects Text` already
  staples Jev section scoring onto any text under a byte budget, and the
  `bash` tool's `focus` is built on it. Wave 4 shows `focus` in use (17
  calls in the first 40 minutes) but no direct `sift` use: advertise it in
  the command and workbench skills as the way to bound any large value
  (`sift focus 4000 =<< readFile ...`, a `lookup` result, a diff).
- Structured pagination: a `Generic`/`ToJSON` value should page itself
  under Jev steering — render to JSON, split on structure (top-level keys,
  list elements) rather than lines, score sections against the focus with
  `each` (Jev.Operators already batches per-item questions), and pack to
  the budget with a marker naming what was left out. One operator,
  `siftValue :: ToJSON a => Text -> Int -> a -> Eff effects Text`, over the
  same scoring as `sift`; the model then reads a `Candidate`, a roster or a
  settlement value as the two or three pages Jev picked instead of a
  truncated dump. Pairs with the "text as artifact" card: once command output
  and previews are Rust-held artifacts, the same pager serves them.
