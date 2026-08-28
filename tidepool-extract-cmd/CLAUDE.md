# tidepool-extract-cmd — the ONE `tidepool-extract` invocation builder

**Charter.** Belongs: binary resolution (the strict `$TIDEPOOL_EXTRACT`
policy), typed request construction and encoding (`ExtractCmd`), and the spawn + spawn
counter — a std-only leaf with zero dependencies. Parsing extractor output
belongs to callers; the compile cache belongs to `tidepool-toolchain`, which
uses this crate's `argv()` in its cache key.

## Resident compile daemon

`ExtractCmd::run()` (never `run_with` — see below) transparently tries the
resident compile daemon before falling back
to a direct spawn: when `$TIDEPOOL_EXTRACT_DAEMON_SOCKET` names a path, it
connects, sends `(cwd, worker request)` over the wire the `daemon` module owns
(length-prefixed frames — see that module's doc for the exact byte shape,
mirrored byte-for-byte by `haskell/src/Tidepool/DaemonServer.hs`), and
synthesizes the daemon's response into the same `ExtractRun{output, elapsed}`
shape a spawned process produces (`std::os::unix::process::ExitStatusExt::
from_raw` over a wait(2)-style status, NOT a bare exit code — see
`daemon::encode_wait_status`'s doc for the exact encoding and its
`exit_status_round_trips_0_1_2`/`..._negative_code_truncates...` unit tests).

**At-most-once semantics, never a hang.** Every daemon-unavailable signal —
the env var unset, connect failure, an I/O timeout, or an unexpected EOF
mid-response (`daemon::DaemonError::Crashed` — the daemon crashed or was
killed mid-request) — causes `run()` to fall back to `self.launcher`
(`Direct`, the same launcher every existing caller already had) for that ONE
request, never a retry against the same daemon and never surfaced as this
call's own error. The spawn counter (`extract_spawn_count`) increments
exactly once per logical request either way — on the daemon's own success,
or on the Direct fallback's — see `run`'s own doc for why `run_via_daemon`
counts a daemon-served request as a real spawn (design §5.2: the counter
means "an invocation was served," not "a process was forked").

**`Launcher::Daemon(PathBuf)`** is the third launcher variant, structurally
not `Command`-shaped (no process to build — `Launcher::command()` panics on
it; `ExtractCmd::run_with` special-cases it before ever reaching
`command()`). It exists for a caller that wants to target a SPECIFIC daemon
socket explicitly rather than `run()`'s own env-gated discovery; unlike
`run()`, `run_with(&Launcher::Daemon(path))` reports a daemon failure as this
call's own `SpawnError` — an explicit launcher is an explicit request, so a
failure is reported honestly rather than silently retried through Direct.
No caller does this today; it exists so the type is total rather than a
landmine.

**Zero new dependencies** (the charter above still holds): the wire codec is
hand-rolled (`u32`-LE length-prefixed frames), never `serde`/`serde_json`,
even for the response's small `{exit_code, stdout, stderr}` shape — see
`daemon`'s module doc for why the frame boundaries alone are enough
structure that no interchange format is needed (both endpoints are in-repo).
