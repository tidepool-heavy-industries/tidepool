---
name: shoal-command
description: Run builds, tests and interactive commands through Shoal's Haskell command jobs. Load for memory limits, retained jobs, stdin, PTY or typed completion routing.
---

`Cmd` is already imported as `Tidepool.Command`; `bash`, `withMemory`, `MiB`
and `GiB` are in scope. Commands are inspectable values; effects start them.
Use Haskell commands for builds/tests and other potentially expensive processes.
Native shell tools remain a 256 MiB fallback; `apply_patch` remains available.

```haskell
let buildCommand = withMemory (GiB 4) . Cmd.withEnvironment [("CARGO_BUILD_JOBS", "2")]
let focusedCheck = buildCommand [bash|just test-lib tidepool-node 'test(command_oom)'|]
job <- Cmd.start focusedCheck
```

Choose a realistic hard memory limit. It also determines admission weight. The
shared pool queues commands automatically when capacity is occupied; retain the
job, do other work or end the turn. Do not resubmit because it is queued.
`Cmd.run command` waits up to one second and returns `Finished` with result/output
or `Pending job`. If observation fails after starting, `Unavailable job error`
retains the same job for inspection/cancellation. Bind the result rather than
discarding its handle. `Cmd.await job` deliberately waits for completion.

```haskell
result <- Cmd.await job
Cmd.output job 8192
```

Output reads accept 0–65536 bytes and return a tail and truncation flag, not a
cumulative transcript. Read the amount needed for the next decision. To display that bounded
tail without automatic observation summarization, use
`inspectFull <$> Cmd.output job 8192`. Output retention is
bounded; write large logs to a chosen workspace file when they must outlive jobs.
Completion preserves exit, OOM, cancellation and unconfirmed outcomes, separately
from descendant cleanup. After uncertain execution, inspect/cancel the same job;
do not silently run a replacement. `Cmd.cancel job` accepts cancellation intent;
`Cmd.status job` reports the subsequent result and cleanup.

The quoter is literal: shell `$variables` and backticks are shell syntax, not
Haskell interpolation. Bash uses its ordinary exit/pipeline semantics; put
`set -euo pipefail` in a script when that is the behavior you want. Use one script
for shell-local `cd`/variables, and Haskell bindings for values reused across jobs.
Pass dynamic values as arguments, preserving exact bytes:

```haskell
let showPath path = Cmd.withArguments [path] [bash|printf '%s\n' "$1"|]
Cmd.describe (showPath "a path; not shell syntax")
```

`Cmd.argv [program,arg1,arg2]` avoids a shell. `Cmd.inDirectory path` and
`Cmd.withEnvironment [(key,value)]` customize the description. Ordinary commands
close stdin; use `Cmd.withStdin` for a pipe or `Cmd.withTerminal` for a PTY,
initially sized to the owning TUI. Retain the returned job for `Cmd.sendInput`,
`Cmd.closeInput` and `Cmd.resize`. PTYs use terminal input such as EOF rather than `closeInput`.

`Cmd.completion job :: R.EventSource Cmd.CommandResult` composes with the existing
record actor API. Supply it to an `Event Cmd.CommandResult` field with
`R.on (Cmd.completion job) handler`. It delivers one retained terminal result even
when attached after completion; the job's creator authorizes attachment. The
handler can retain/route that result and wake an agent for a real decision.
Load `shoal-define-actors` when defining a custom router. A captured job grants no
control authority to a different actor. Finish the collector when its job is done.
