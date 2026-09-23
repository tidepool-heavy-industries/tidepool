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

- **Compile memo is path-sensitive.** Recommendation in
  [compile-memo-evidence.md](compile-memo-evidence.md): compare direct
  dependencies by content fingerprint while keeping `HomeDependency` (module
  name and source kind) as identity. The five-tree reproduction and GC cost
  attribution from the original brief were not run.
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
- **Failed launches leave state a retry trips over.** Seen on 2026-09-23
  starting the exomonad-harness wave: (1) `exomonad init --recreate` on a
  session whose first host failed before the root ever bound reads the stale
  `.exomonad/sessions/<name>/root-binding.json` and fails with "cannot resume
  the requested root conversation" instead of starting fresh; (2) a host
  launched through `systemd-run` outlives `tmux kill-session` and keeps the
  workspace's `actor-worktrees/<hash>/bindings/.owner.lock`, so the next run
  fails with "another process already owns this binding". Class: a failed or
  killed run must release everything a fresh run of the same workspace needs,
  and a retry must not resume state a failed run never completed.
- **Memory admission versus compiler workers.** The persistent test daemon's
  three warm GHC workers (about 5.5 GiB each) plus a run's own daemon left
  3.4 GiB on a 31 GiB box and the root actor failed admission (7 GiB
  needed). A run's daemon and the test daemon size themselves independently;
  one budget should cover both, or a run should refuse to start next to a
  warm test daemon and say so.
