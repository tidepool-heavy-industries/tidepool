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

## From wave 4's first hour (2026-09-24, run 535e56ca)

- **Agent spec vs child effect rows is a launch-time check, not a first-
  admission failure.** The harness `AgentSpec` required `Journal` for its
  after-tool hook; children carry no `Journal`, so the first wave's
  admissions failed and the root spent its first minutes amending the spec
  (`babfb4d`). `exomonad check --workspace` compiles the spec but never
  resolves it against the effect rows the workspace's fork helpers produce.
  Resolve each configured role's row against the spec's constraints at
  check time and fail there.
- **The watchdog abstains on size.** 185 of 186 after-tool dispositions in
  the first 40 minutes were `Abstained` because the tool result exceeded the
  evidence bound (`Project.Watchdog` 8000 chars), so the hook judged almost
  nothing; the one `Annotated` was the whole yield. Bound by selecting (the
  `sift` scorer, head+tail, or the receipt's summary), not by abstaining.
- **Long bash jobs read as failures.** Actor 9 (core-correction) showed 19
  "tool execution failures" that were 30 s observation windows expiring on
  cargo builds, followed by `write_stdin` waits — the intended pattern, but
  each expiry costs a turn and reads like an error. Default the yield window
  for `cargo`/`nix` invocations higher, or return the retained-job receipt
  as a normal result rather than a failed observation.

## Label versus path at the fork API (2026-09-24, wave 4)

- Actor 20 failed a cell with `InvalidKebabName "correction-20260924/core-execution"`:
  a child copied its own group path from the activation into a place that
  takes a single kebab label (`batch`/`subgroup`/`[label|..|]`). The rejection
  is right; the message is not: it should say that labels are one kebab
  segment, that a path is built by `batch campaign group` or `subgroup`, and
  which argument was wrong. Consider letting `subgroup` accept a path
  literal directly, since children always have their own path at hand.

## Wave-4 slowdown, measured (2026-09-24, run 535e56ca)

- Bash cells went from ~125 ms average (19:40Z) to ~1.9 s average, 33 s max
  (20:20Z); whole turns from 3.5 s to 21 s average, 217 s max. Two causes,
  both measured in the run's compiler log:
  1. **Every compile re-lowers four workspace modules.** `Project.Work` and
     `Project.Review` use `[label|..|]`, which the memo classifies as
     `untracked-compile-time-execution`, so they miss on every request
     (682/683), and `Project.Routing`, `Project.Observe` and each cell's
     `Expr` miss by dependency (689 each): 3.5-4 s per request, from the
     first cell. Fix: a library quoter declared pure counts as tracked
     (lane `memo-quasiquote`).
  2. **Shared-session checkout waits return at scale.** Checkout waits were
     ~0 with 5 actors and 65 ms→1.5 s average (max 59 s) with 16 actors on
     one session. The persistent daemon's other worker slots sit idle
     (workers 0 and 1 served everything; slot 2 never) because compiles
     serialize behind the one machine. Per-child sessions (parcel 3) is the
     fix; the pool is fine.
- Not the cause: Jev (157 hook calls, ~20 s total), the daemon (0 ms queue,
  no rejections), model latency (separate: the root's turns show ~7 min
  between tool calls late in the run, worth its own look).

## One bash call is four checkout entries (2026-09-24, wave 4, traced)

- A 124 s `git status` call decomposed: ~88 s in 13 machine-checkout
  waits, 34 s in one compile (the quasiquote memo miss), <2 s everything
  else. One bash call issues four `Commands` effects (`tryStart`, await,
  output, present — `Tidepool/Command/Tools.hs` ~126-163, `Command.hs`
  `observeWith`), and each effect boundary re-enters the shared checkout
  (`resident_workbench.rs` `with_host_machine` ~2106; the comment at ~4398
  calls the per-effect cost known). With ten actors on one session and
  compiles of 3-34 s holding the machine for installs, every boundary queues.
  Fixes, in order of leverage: per-child sessions (parcel 3) so nothing
  queues run-wide; the quasiquote memo fix so holds are short; and a cell
  should take the checkout once per execution and keep it across
  consecutive effects, releasing only at a real yield (a command wait, a
  request), not per effect. The daemon pool is not the bottleneck (slot 2
  never served). Missing spans: the command service's process launch, and
  one per-cell sum of checkout waits.

## Per-actor machines with an evacuation bus (2026-09-24, direction chosen)

- Decision: remove the shared machine by giving each actor its own
  `PreparedMachine`, and make the bus between machines a copying-GC
  primitive — evacuate the graph reachable from a handle into another heap
  (constructors copied; closures and thunks copied with shared code;
  MutVars snapshotted; static-region objects shared by reference, never
  copied). Mailbox delivery, replies (`RootCustody`/`ExitCell`) and exits
  become evacuate-on-delivery, which dissolves the three walls the
  per-child-sessions lane hit. Boundaries are quiescent, so no blackhole
  crosses. Arbitrary values still cross: anything the heap can hold, at a
  cost proportional to the reachable non-static graph; only identity of
  mutable cells changes (copy, not share). Fable drives the GC-invariant
  work; Sonnet takes mechanical parcels. Frozen regions (generalizing
  `static_region` to live values: shared workspace image, O(1) context
  inheritance, overlay tables for thunk updates) stay the longer target.
- Companion facts from the Opus reviews: the after-tool hook never runs for
  `haskell` cells (`run_after_tool` only takes hosted tool calls,
  resident_actor.rs ~5370); informational tools (`sendMessage`, `readWork`,
  `lookup`) pay blocking compiles while `status` returns instantly — take
  them off the compile path; spec preparation per child (689 s total, JIT
  under the checkout) should be cached per layer revision; the daemon ran 2
  workers for 16 actors; model turns are flat (3-6 s), tool time is 82% of
  actors' wall time.

## Root friction file, correction wave (2026-09-24, judged)

Source: `~/dev/exomonad-harness/docs/exomonad-friction.md` (the root's own
notes). Acted on now: `&&` gating, `Blocked` is not a transport, owner-scoped
formatting, event waiting as the default (core prompt + coordinate skill),
spec preflight per role in `exomonad check`, label-vs-path and ambiguous-name
teaching errors, `reviewCommit` from the root, notice previews naming the
child's path and source revision, after-tool hook on `haskell` cells.
Deferred, one card each:

- **Failure streak to the owner.** A per-actor count of failed checks and
  time since the last candidate, visible in `status` and nudged at a
  threshold. Try the prompt rule (stop-and-ping after two failed rounds)
  for one wave first; mechanize only if interviews show it ignored.
- **Typed incorporation acknowledgement.** A `sendMessage` proves delivery,
  not that the recipient rebased or changed behavior. The reply already must
  say whether an unowned change was applied; a typed receipt tied to the
  commit is a workflow helper the workspace can write before the engine.
- **Expected-red gate.** A marked failing test with owner and expiry in
  integration status is project policy: a `Candidate` gate field in the
  harness workspace, not an engine feature.
- **Bigger experiments** (the root's list: friction-to-experiment compiler,
  promotion ladder, typed event algebra, behavioral replay, continuity
  inspector, delegation preflight, uncertainty ledger). Feature work; the
  delegation preflight's first slice is the spec-preflight lane. The next
  wave brief may pick one; the harness `NEXT.md` carries prompt-level
  trials of the rest.

## Wave 4 host died of OOM and could not restart (2026-09-24 21:54Z)

- The host was OOM-killed (2.5 GB RSS, 3.9 GB swap peak; box at 24 of 31 GB)
  while a side lane ran a diagnostic compile daemon with two 7 GB GHC
  workers next to the run's own daemon and three cargo test builds. Rule for
  lanes during a live swarm: no daemon above one worker, and the box's
  compile load is budgeted from what `free` shows, not assumed.
- systemd restarted the host five times and each start failed with "frozen
  workspace library differs from this build". The run was launched from a
  dev build (`target/debug/exomonad`, 12:34), whose library identity hashes
  the checkout's `bridge/haskell/{lib,actors}` at startup; commit 379da60e6
  (labels) changed `lib/` at 20:29Z, so the identity moved under a running
  swarm. Structural fix: a run materializes its library trees the way it
  freezes the workspace (`FrozenWorkspace`), so every process of the run
  reads the frozen copy and a checkout edit cannot invalidate a restart;
  until then, launch dogfood runs from the deployed embedding build only.

## Per-actor machines follow-ups (2026-09-24, from Astra's parcel-7 review)

- Idle last-drop teardown: when the last `RootCustody` on a retired dedicated
  session drops and nothing checks the session out again, the machine stays
  idle until process exit. The custody drop path for a session pending
  teardown runs the retirement check itself. With it: a lifecycle test where
  an inherited child retires while its parent stays active on the dedicated
  session, then the parent resumes.
- Import-environment test: one image, a fresh receiver and an already-installed
  receiver, two arrivals carrying different imported values (a mutated
  imported MutVar); both received closures read the receiver's seeded value.
  Rule stated in `tidepool/codegen/src/prepared_program/evacuation.rs`.
- Flaky test: `resolve_codex_binary_unset_is_none` (exomonad/agent
  backend/codex/process.rs) mutates a process environment variable while
  sibling tests run on parallel threads in the same binary; it failed once
  and passed on rerun (2026-09-24). Serialize its env access or take the
  variable as an argument.
- From Sol's quality pass (2026-09-24), for later review:
  `version_ladder::found_version` treats an invalid present version as
  unstamped version 0; and KV `flush` logs write failures (including the
  refused flush after a failed load) while its callers still report success,
  though the effect contract says I/O faults abort.
- Label quasiquote ambiguity (2026-09-24): since c1be8bfce `[label|x|]` is
  polymorphic over `IsWatchLabel`, so `let l = [label|x|]` in a cell that
  does not itself use `l` is rejected as ambiguous; a binding meant for a
  later turn needs `:: Label`. Models see GHC's "use a type annotation"
  diagnostic, so it is teachable, but the natural form fails. Decide:
  default the unconstrained case to `Label` (a defaulting rule the workbench
  owns), or make the quasiquote monomorphic and give watch labels their own.
- Gate failures carried into wave 5 (2026-09-25), all failing identically on
  main; none is a parcel 7/8 regression:
  - five `actor_host::jev_tests` and `command_skill_examples_execute...`:
    test backends see a `noul` request (the destructive-command check) they
    do not script, and a skill example's memory limit (256 MiB) differs from
    the test's constant (1 GiB);
  - `accepted_stdin_is_acknowledged...`: "Variable not in scope: job1" in a
    cell's second statement; already failing at the wave-4 deploy ad856115c;
  - `exomonad_control_contract::fork_options_are_optional...`: ForksStartWith
    has 16 arguments, the test expects 15; predates today's work.
  The notification-barrier regression (473a76215) is fixed in 8dfa4f6ba.
- Flaky under load (2026-09-25): extractor daemon test
  `an_idle_pooled_slot_pre_warms_from_the_first_requests_include_set`
  polls for a pre-warm compile against a deadline; it failed the redeploy's
  Nix build once while recipe runs loaded the host, with no daemon code
  change since the wave-4 deploy. Wait on the pre-warm's completion event
  instead of a deadline.
- Delivery fence on a hosted input during a computing cell (wave 5 stall,
  2026-09-25; corrected after reading Codex's queue db: no host-input row
  was ever admitted for core-lead, so the earlier "Codex marked it Unknown"
  reading was wrong). Chain, verified in code and evidence:
  1. The pump submits every tracked message as StartOrSteer through the
     TUI's input-control socket with a 35 s operation deadline
     (`exomonad/agent/src/backend/codex/controller.rs`).
  2. The TUI's `control` handler runs `InputSettlementGate::before_input`
     before admitting the input. When a `haskell` dynamic tool call is
     active it does not queue behind it: it cancels it
     (`tui/src/host_dynamic_tools/cancellation.rs`, `cancel_before_input`)
     and waits for the call to reach a terminal settlement.
  3. The host answers the cancel of a computing (not sleeping) cell with
     NotSleeping (`exomonad/actor/src/resident_tools.rs`,
     `cancel_workbench`), so the TUI parks in AwaitingTerminal.
  4. Nothing ever records the terminal: `ActiveHostedCall::complete_from_call`
     has no production caller; the normal completion path
     (`tui/src/app/event_dispatch.rs`, DynamicToolCallCompleted) only calls
     `finish_cancellable_call`, which resolves a Terminal phase and clears
     the gate but never notifies a waiter. The exchange hangs forever, so the
     input is never admitted. Only the sleeping-cell path (Cancelled/Expired)
     records a terminal and works.
  5. The host stops waiting at 35 s but deliberately leaves the connection
     open (7ca95b644), marks the durable row Unconfirmed, and re-queries
     every 1 s. Codex answers EvidenceUnavailable (no row); the host maps
     both EvidenceUnavailable and Unknown to `InputAdmission::Unknown`
     (`exomonad/agent/src/backend/codex/input_control.rs`), which the pump
     treats as "still pending" with no withdraw and no timeout. Every later
     tracked message, including child replies, sits behind it; only
     settlement/watch notices overtake (8dfa4f6ba).
  Trigger condition: a tracked message reaches an actor while that actor is
  inside a `haskell` cell that is computing rather than sleeping and that
  outlives the exchange. Bash cells are not gated (actor 3 recovered).
  Fix without a Codex rebuild (host side only):
  - The pump defers a Submit while the target's active workbench execution
    is a computing cell (the host owns `active_workbench` and its phase);
    a sleeping cell is still interrupted as designed.
  - The host stops collapsing EvidenceUnavailable into Unknown; after a
    grace period (2x the deadline) with no evidence it withdraws: a
    Tombstoned withdrawal proves the input was never admitted, so the row is
    confirmed withdrawn and the payload re-delivered under a fresh sequence;
    an Unknown(record) withdrawal re-delivers labeled "possibly already
    seen" (LateUpdateEvidence::Unconfirmed fits).
  - The WARN repeats with a count and age instead of deduping forever.
  Codex-side follow-up for the fork (later, needs a rebuild): the normal
  completion must call `complete_from_call` so a parked cancel wakes.
  In-run recovery used tonight: an operator paste into the fenced actor's
  composer with the fenced payloads (scratchpad wave5/poke-core-lead.txt);
  the actor's outbound path is unaffected, its inbound stays fenced for the
  run. Also: a /goal-style standing objective the runtime re-asserts on
  idle, mined from Codex.

## Wave 5 root interview (harness master cffd505, docs/exomonad-interviews.md)

Twelve answers, mapped to lanes. Quotes are the root's.

- **Message lifecycle observable and actionable** (the root's "one
  change"): states receipt, queued, presented, acknowledged, incorporated,
  fenced, "with the last provider turn and one supported recovery action".
  The status view line it wanted: "core-lead: request pending; provider
  idle 27m; notification inbox fenced; last message received by host but
  not presented; recover by [action]". Lane: delivery-fence (status view
  per child from the durable rows), observability (run-map deliveries).
- **Replace-before-presentation** for a queued message (it sent a literal
  `$(git rev-parse HEAD)` then a correction); after presentation, a
  superseding correction linked to the first, never silent retraction.
  Card: `replaceMessage` on an unpresented sequence, else a linked
  correction; the queue db already keys rows by producer/sequence.
- **Versioned standing-assignment update**: new owned paths, acceptance,
  source, an effective-from boundary, a presentation ack and a separate
  incorporation ack; never rewrites work committed under the old
  assignment. `updateRequest` exists but its receipt says UpdateUnconfirmed
  and the root could only verify through the child's reply and the
  cumulative diff. Card: request-update lifecycle exposed like messages.
- **Batched admission**: "one batched admission call for a ready frontier,
  with per-child admission results and a fast return that does not wait for
  each child's provider startup". It already used an applicative unfold;
  the 55 s / 39 s cost is provider startup inside the cell. Card: fork
  returns admission immediately and provider readiness as a later typed
  result (pairs with non-blocking cells: "start it, do independent work,
  then receive a retained typed result").
- **Standing goal** (/goal): the text it would have set is quoted in the
  interview; reminders wanted on child idle with a pending request, on a
  leaf commit appearing without review or integration, before a destructive
  reset, and before a final answer implying completion; "cite the changed
  fact and the next owner, not replay the whole plan". Card kept.
- **Ownership gate as a primitive**: "base-to-candidate ownership gate with
  source-bound review/integration states, not inferred from branch names".
  The owned-path rule caught a real unowned edit only through a cumulative
  diff (tip 77c6372 carried ancestor a1b10c3). Card: Git from the parent's
  view reports the cumulative diff against the assignment base and its
  owned-path verdict as data.
- **git reset --hard dropped committed work** (6667ddc) while the worktree
  was clean; the watchdog alerted after the fact. Card: the Git effect
  refuses history-discarding commands on a branch with unmerged commits
  unless the call names the OID being discarded; a hold, not a nudge.
- **Brief hygiene**: stale "last run stopped" instructions stayed visible
  after they were done; add "if a lead's inbox is fenced, stop treating
  receipts as presentation; record the source and use an explicit recovery
  or handoff"; delete the stale format-first instruction. Lane: wave-6
  brief.
- **Pre-fork blocker check**: the cache probe lacked its byte-level
  reference before it was forked. Brief item, not harness.
- **Watchdog**: no nudge prevented a mistake; one false positive
  (destructive_command on message text, now in agent-ux). Keep the reset
  alert but make it a pre-execution hold (above).
- **Review pattern**: independent review of a findings-only probe would be
  waste; inline inspection of (b) was "weaker than an independent review,
  not equivalent". Skill text, not code.

Round 2 (harness master 95daec8, "Round 2: shapes"): the root sketched the
types for each card above: `MessageState`/`messageState` (A), `replaceMessage`
returning Replaced or LinkedCorrection (B), `AssignmentDelta` with a
`CellBoundary` effective-from and two acks (C), `forkBatch` returning
`Admitted` with a retained `providerReady` (D), `Goal`/`GoalTrigger`/
`Reminder` (E), `CandidateView` with a cumulative parent-view diff and the
stage machine committed → reviewed → merged → verified (F), `DiscardIntent`
required before history-discarding Git operations, refusal text included
(G), and the exact per-child status line (H). Sol owns these shapes; engine
work implements what they need. Sequence: F and G first (no overlap with the
delivery lanes), A/B/C after the delivery-fence lane lands (same inbox
rows), D with per-actor machines, E as its own design.

## Fork-cell latency (wave 5, root 54.7 s, core lead 39.0 s)

Timelines from the host and compiler logs; no per-child compiles, no
provider startup inside either cell (both launched Codex after the cell
settled), git capture 1 to 2 s total.
- **Root, about 40 s:** two per-worker memo misses, one per GHC worker,
  mislabeled `required-interface-not-retained`. Real cause (memo-interfaces
  lane, verified in the compiler log): the pre-warm and ordinary evals
  compile only modules the target uses, so about 60 unused library modules
  (Project.*, Jev.*, Tidepool.*) were memoized as type-check facts with no
  body or interface; the session cell compiles every module, so each was a
  miss (lowering 10 to 14 s plus interfaces 5 to 7 s per worker; the
  worker-0 miss was the display render, holding the checkout 24.6 s). The
  leaf-interface elision only ever skipped the target module and was not
  involved. Fix (8878d3b89): with a resident memo active, an ordinary
  compile also builds body and interface for unused modules (output
  unchanged, one-shot compiles unaffected); misses without a body now
  report `executable-body-not-prepared`; the request logs
  `memo_completion_modules count=N` and a `memo_completion` timing under
  lowering. Measured on the stdlib: second request 2.92 s to 0.01 s, first
  request 0.76 s to 3.88 s. Expected: pre-warm or a worker's first cold
  eval grows 20 to 25 s; the root setup cell loses about 40 s.
- **Core lead, about 22 s:** the split-compile stale-retry loop. An
  InheritedContext fork shares the root's scope chain; the root committed a
  cell every 7 to 10 s, `compile_relevant_eq` (runtime/src/session/view.rs:239)
  compares visible_values, so each commit invalidated the install; three
  attempts of 8 sequential GHC round trips (~7 s each, MAX_SPLIT_ATTEMPTS = 3,
  resident_workbench.rs ~3203) all went stale, then the fourth ran under the
  checkout. Per-actor machines do not touch this (inherited forks are
  ineligible). Fix: fall through to the single-checkout compile after the
  first Stale. Est. 39 s to about 25 s. Follow-ups: narrow the stale test to
  "a newly visible module exports a name the cell references" (attempt 1
  would install, ~22 s); batch the per-item GHC requests (0.5 s fixed cost
  each).
- **Per provider child, about 2 s unlogged** inside `prepare_captured` /
  `settle_publication` (actor_host/workspace.rs ~686-706: bubblewrap
  prepare_view, mounts, install_view). Could leave the cell and settle with
  the provider launch. Needs a phase log first.

## From wave 6 (2026-09-25, first hour)

- **Launch checklist gap:** the harness project's `.exomonad/workspace`
  submodule must be bumped to the synced workspace revision before launch;
  the binary's DEFAULT_WORKSPACE_REV only scaffolds new workspaces. Add a
  preflight to `exomonad init` (or the launch record) that compares the
  project's workspace gitlink with the deployed pin and warns on drift.
- **CommitReview acceptance was undocumented** for the model: reviewers
  forked by `reviewCommit` returned Blocked because the review prompt and
  skill only covered `ReviewTask`. Fixed in the prompt; the skill's review
  section should carry the same branch, and `exomonad check` could refuse a
  workspace whose review prompt does not mention every assignment input
  type a fork constructor uses.
- **Manifest ownership:** an assignment that adds a trait derive or a new
  crate must own the crate manifest and lock file, or the lead adds the
  dependency before forking. Brief and lead prompt item.

## First-turn orientation (waves 5 and 6, rollouts, 2026-09-25)

Median time to the first productive action: root about 28 s / 4.5 calls,
leads 25 s / 2.5, reviewers 20 s / 4, Luna implementers 68 s / 10 (wave 6
rose to about 86 s / 13). Recurring reads forced by the harness or brief,
each seen at least twice:
1. `inspectFull sessionInput` on 14 of 14 children: the activation text
   truncates the Task ("additional detail omitted"). One child returned
   Blocked prematurely on the truncated text. Fix: do not truncate the Task.
2. `parentAgent` is Nothing for `lunaTask` (selected-context) children, yet
   task.md and the leads' obligations told them to sendMessage the parent:
   3 to 10 calls per Luna hunting the parent (lookups, status lineage), and
   checkpoints silently dropped in the Nothing branch. Fix: every child
   gets a parent handle (its supervisor), independent of context
   inheritance; until then, prompts say reportProgress / respond Blocked.
3. `.exomonad/plans/language.md` re-read by 8 Lunas for nothing.
4. Reply construction: Candidate/Outcome/ReviewDecision constructor lookups
   by 8 actors; ambiguous `Blocked`/`Accepted` failed 5 cells (fixed by the
   workspace pin); CommitReview reviewers derived the base with merge-base
   and one treated the input as ReviewTask (prompt fixed).
5. PRD named by section without a path (3 hunts).
6. Both roots re-derived the fork-cell recipe from the fork and coordinate
   skills plus two lookups each.
Prompt paragraphs for root, lead, luna and reviewer are in the wave6-prompts-2
branch; engine fixes 1 and 2 in the parent-handle branch.
- **Cell memo is unsound as a cache (cell-memo lane, no code):** every
  compiled item is tied to the value generation it was compiled for (bare
  expressions bind as `observation<n>` in the generation-n module; binder
  ids are module plus name; `set_val_gen` only rises), so a memo hit cannot
  install at a fresh generation. `compile_relevant_eq` deliberately omits
  the generation. Only host carriers reuse compiled programs, because their
  stub module carries no generation fact. Options: memoize the check only
  (one round trip); rebase compiled items to a new generation the way
  carriers do (runtime plus extractor design); or remove the retry, which
  the UpdatePending refusal text now does. Decision: no cache; revisit
  rebasing only if per-actor machines leave cell latency compile-bound.

## Wave 6 root interview (harness ddacd42, 260b694; digest in
plans/wave6-root-interview-digest.md)

- **Reload is not atomic (engine, bug class):** `reload_agent_spec` was
  refused ("prepared engine: missing imported value
  Project.Shell.presentSelected") after it had already published the new
  source layer, leaving the old typed tool record active; "after the
  reload" became ambiguous for the root. Make publication and spec rebuild
  one transaction, or roll the layer back on refusal and name the stale
  symbol.
- **Revision-identity view (workspace/engine):** six partially independent
  revision identities (checkout head, child head, assignment base, branch
  tip, review seed, source layer) desync silently; the root wants one
  "what revision am I using, what would this action publish" view.
- **Background jobs (engine, lane background-jobs):** the root's notice
  contract is in the digest; carries source revision so a stale pass can be
  refused.
- **Prompt-level (lane wave6-prompts-2):** first-call-ready brief template;
  one-page next-run entry at the top of NEXT.md; the root's own admission
  checkpoint.
- **Typed message states** and **manifest ownership in the admission
  checklist**: cards, wait for the new harness.
- **Root checkpoints have no target (engine, card):** the root sends no
  admission checkpoint because it has no parent; give it the operator as a
  notify target (the operator socket exists), rendered in run-map and the
  Host window, so the root's own decisions are data instead of pane text.
  Interim: the root writes its checkpoint into NEXT.md's obligations table
  (wave6-prompts-2).
- **Turn ends without respond (seen in waves 0 and 6):** workspace code
  cannot observe it; the only slot is `afterTool` (Contract.hs ~690-713),
  and turn completion is engine-only (`ProviderTurnState::Succeeded` in
  runtime_observation.rs; request openness in request.rs `OwnerState`).
  Card: an `onTurnEnd` spec slot with its own dispatcher entry, fired from
  the provider-turn completion in actor_host.rs, is the idle hook the
  goal-style mechanism needs; wait for the new harness. Interim (lane
  turn-end-reminder): the host itself pushes one line when a provider turn
  ends with the actor's own request still open.
