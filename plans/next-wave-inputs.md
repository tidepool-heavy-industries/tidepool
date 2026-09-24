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

- **Internally tagged serde enums with floats fail to decode.** The workspace
  enables `serde_json`'s `arbitrary_precision` (root `Cargo.toml`, required by
  `bridge/mcp` and `tidepool/bridge`); Cargo feature unification applies it to
  every crate. Under it, a `#[serde(tag = "...")]` enum containing an `f64`
  field fails to decode ordinary JSON (`invalid type: map, expected f64`).
  `exomonad/jev-integration/src/interpret.rs` was fixed by hand in 4df6dbbe1;
  28 other `#[serde(tag = ...)]` sites across 20 files are unaudited. Class
  fix: a test that round-trips every tagged wire enum with a float, or scope
  `arbitrary_precision` to the values that need it (`RawValue`/`Number`
  wrappers) instead of the whole workspace.

- **Typed errors.** About 245 `io::Error::other(format!(..))` domain errors and
  about 871 text-matching test assertions. Convert crate by crate as typed
  error enums land.
- **Timeout policy.** Deadlines are chosen per call site; give each subsystem
  one named budget set.
- **Unbounded deployments channel** (`exomonad/actor/src/resident_actor.rs`)
  and **cleanup guard without expiry** (`exomonad/actor/src/request.rs`).
- **Retirement deadline** (`bridge/facade/src/actor_host/scoped_custody.rs`):
  recovery can consume the whole budget and leave none for finalize.
- **Launcher TODOs.** `bridge/handlers/src/handlers/exec.rs` and
  `bridge/facade/src/exomonad.rs` spawn processes outside the launcher; each
  site carries a `TODO(launcher)` allow reason (`grep -rn "TODO(launcher)"`).

## From the exomonad-harness wave (2026-09-23)

The first build wave outside this repository: a GPT-6 Sol root, a core lead and
two leaves in `~/dev/exomonad-harness`. Their WIP is kept there on `master` and
the `exomonad/wave0/*` branches. Every agent was interviewed while paused; the
fixes the run motivated are in git history. Open items:

- **Host-authored bindings still compile per call.** Naming a hosted tool's
  command job (`bind_command_job` → `compile_host_binding`) runs GHC every call,
  now outside the machine checkout (12b90f6ee). Reusing one compiled binder with
  a relabelled generation does not work: GHC writes a `Val.G<gen>.hi` interface
  keyed by the exact `--bind-gen`, and later turns import it by that name
  (`tidepool/runtime/src/session/turn.rs` TurnRequest, `extract-cmd` lib.rs
  `--session-root`). Removing the compile needs a binding path whose interface
  does not depend on the generation.
- **No way to list an actor's live descendants.** The root searched for one to
  see whether its lead's leaves had started, and found none.
- **Ending a turn looks like finishing.** A leaf ended its first turn without
  `respond`, although it knew `respond` settles its assignment: "the normal
  final-answer UI made the opposite feel plausible in the moment." Deferred to
  the standalone harness, which owns turns.
- **Briefs with split ownership.** "This file is yours" together with "its
  public signatures belong to the lead" made a leaf ask instead of act, then
  wait. A brief should say which of the two wins.
- **Two extractor memo modes fail on main.** `--prepared-session` (missing
  `tidepool-prepared-interface-elided ... reason=no-later-home-importer`) and
  `--validation-memo` (missing `front_compiles=1 core_compiles=1
  prepared_compiles=1`) fail with the same messages at aad52db33, before the
  content-keyed memo (e3ca7803d). Its own `--path-insensitive-witness` mode and
  `--untracked-compile-time` pass. Root cause not yet found.
- **Operator input.** In a Codex pane, Enter steers a running turn; Tab queues
  until the turn ends, which can be many minutes.

How the run was observed, for the next one: tmux holds no scrollback for Codex
panes (alternate screen); each agent's Codex rollout in `~/.codex/sessions`
is the complete record, and forks carry `parent_thread_id`. Prompt caching
across forks held at about 99% from a child's first request.
