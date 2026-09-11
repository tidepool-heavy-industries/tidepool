# Foreground matched release

Status: accepted source and package; not hot-deployed into the commissioning
swarm.

## Matched inputs

- Tidepool integration includes the reviewed applications repair
  `bd3cfe36eece990c5dcd63e2a61654dc04e88741`, the reviewed sleep lane
  `6b6aa0dddbbd6f04180ed0dd54159fdcb776c04e`, and the main raw-tools line
  through `d66cd42709aa8fd40d85f1485e7f5c8b20e57fd8`.
- `flake.nix` and `flake.lock` select external Codex
  `db2442eccc5da8561b3d922dccfa1a67654beb6e`, with locked NAR hash
  `sha256-2/rc+OdRqX/1PqWOzgO3jLSDSZSj9QAweJFOv/CZy8I=`.
- The accepted native executable is
  `/tmp/tidepool-native-package-db2442ec/bin/codex`, SHA-256
  `b411cce7f18bddded8baac539fa7846071d765249035385e09f3bd0103c65205`.

The combined source keeps main's runtime/resource and raw-tool owners, the
applications custody/recovery owners, and sleep's resident evaluation owner.
Engine and shared-server work remain excluded.

## Acceptance evidence

- On the reviewed repaired Tidepool source and accepted native executable, the
  uncancelled real fifteen-minute actual-TUI case passed 1/1 in 936.158 seconds:
  no intermediate provider inference, then one Haskell suffix and one final
  tool result.
- The matched human, actor-notification, and progress-notification interruption
  cases passed 3/3 on the reviewed source. After the final pin, the coordinator
  reran those exact ignored tests with the accepted executable and closure:
  3 passed, 253 skipped, in 64.321 seconds.
- Exact execution identity and foreign-context lifecycle checks passed 2/2.
  Sleep's actor-owned terminal journal, cancellation/expiry ownership,
  post-response/disconnect reconciliation, retirement/sibling progress and
  handler sequentiality checks passed at the reviewed lane source.
- Native cancellation/retention, human and actor input ordering, command Jobs,
  formatting and warning-clean `codex-tui` checks passed at
  `db2442eccc5da8561b3d922dccfa1a67654beb6e`.
- On the final combined source, `bash scripts/dev-shell.sh cargo check -p
  tidepool` passed. `just fixtures-check` ran 217 tests: 217 passed.
  `git diff --check` passed.

Two earlier coordinator short-test attempts selected zero tests because their
module path and ignored-test invocation were wrong; both exited 4 and are not
acceptance evidence.

## Release and handoff

The matched source and pin are ready for the existing release workflow. Running
binaries, compiler services and the canonical `.shoal` were not replaced during
this swarm.

Retirement cleanup remains unconfirmed for actors 94, 159, 341, 383 and 632,
which retain sockets; actors 159 and 341 additionally retain Git-operation/tmux
custody uncertainty. They must not be reused or described as quiescent without
the external custody owner resolving that evidence.

Later shared-server work inherits the documented session/completion/environment
owners only. It must not infer durable suspended programs, replay permission,
authority or cleanup from this live-session release.
