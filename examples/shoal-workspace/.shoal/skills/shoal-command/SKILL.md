---
name: shoal-command
description: Compose shell commands in resident Haskell, consume results as data, or control retained jobs and output from the Bash tool.
---

Use the `tidepool_actor` tools: `bash` accepts literal scripts, including multiline Bash and
heredocs. Use `exec_command` for `workdir`, `environment`, `memory_mib`, `tty`
or piped `stdin`. These are compiled Haskell handlers over the same command
owner as `Cmd`; ordinary shell use needs no Haskell wrappers.

Defaults: 256 MiB and a 30-second observation. Give builds/tests a realistic
`memory_mib`. Execution returns a `session_id`; use `write_stdin` with that ID
and `chars` to send input, or omit `chars` to poll. Observation expiry leaves the
command alive. `read_output` reads retained stdout from `offset: 0`; select
`stream: "Stderr"` for diagnostics. None of these observations reexecutes a script.
Output limits are byte budgets (`max_output_bytes`), not token counts. Automatic
responses stay within 32 KiB; oversized command displays use an 8 KiB preview.

Use Haskell for reusable command values, data-dependent follow-ups, or typed
completion routing. `Cmd` is `Tidepool.Command`; `bash`, `withMemory`, `MiB`,
`GiB` and qualified Text as `T` are loaded:

```haskell
result <- Cmd.run [bash|git status --short|]
```

Output appears automatically, including for a bound result. The result remains
available as Haskell data; displaying it again does not execute the command.
Use `Cmd.quiet action` when only the data matters. Quiet is scoped to that action
and does not hide a stopped computation or its recovery receipt. Nonzero process
exits remain in the retained result even when routine presentation is quiet.

```haskell
let changed = T.lines <$> Cmd.stdout result
changed
```

`Cmd.stdout` purely extracts complete stdout from exit zero, or an explicit issue.
It never waits, reads more, reruns, or substitutes empty text. Stderr completeness
and cleanup are separate. `Cmd.decodeWith (Cmd.asJSON @Value) (Cmd.stdout result)`
decodes JSON; ordinary Text supports `T.lines`, filtering and other composition.
If capture is incomplete, `Cmd.readStdout (Cmd.job result)` explicitly reads
complete retained stdout, or reports why it is unavailable. Repeated `await`
is not a way to enlarge capture.

`Cmd.run command` starts and waits up to 30 seconds; `Cmd.await job` observes an
existing job for the same window. Both return completed results, including
nonzero exits. On overrun, the interactive workbench stops the current computation
and installs a real `jobN :: Cmd.Job` binding, named in its receipt. The command
continues. The enclosing result is not bound and subsequent statements do not
run. Inspect `Cmd.status jobN`, read `Cmd.output jobN`, or later `Cmd.await jobN`.
That later observation does not resume the discarded continuation. Do not rerun
the command to recover output. Earlier committed bindings remain available.
A Haskell actor handler instead fails normally; it has no interactive remediation
binding. Use `Cmd.start` and completion events there for unattended long work.
`Cmd.observe (Cmd.Observation 250 8192) job` instead returns the current status
normally after a bounded wait, displaying available output without stopping the
enclosing Haskell program.

Commands are reusable values. Quotations preserve literal Bash, including
multiline scripts, heredocs and indentation. Haskell does not interpolate shell
variables or backticks. Bash retains ordinary exit/pipeline semantics; choose
`set -euo pipefail` when appropriate. Pass dynamic values as arguments:

```haskell
let preview path = Cmd.withArguments [path] [bash|sed -n '1,20p' -- "$1"|]
Cmd.describe (preview "a path; not shell syntax")
```

`Cmd.argv [program,arg1,arg2]` bypasses Bash. `Cmd.inDirectory` and
`Cmd.withEnvironment` customize intent. The native owner resolves the directory
at launch: omitted means its workspace; relative paths start there. Constructing
a command does not snapshot inherited environment or location. `Cmd.describe`
inspects intent without executing. Use `pwd` in a command when location is evidence.

Ordinary commands use 256 MiB. Choose realistic explicit memory for builds/tests,
e.g. `job <- Cmd.start (withMemory (GiB 8) [bash|cargo build|])`.
`start` returns immediately; admission queues automatically. Retain the job,
do other work, and observe it later. Memory is a hard limit and admission weight.
`traverse Cmd.run commands` works sequentially until completion or a foreground
stop. To start all first, retain `jobs <- traverse Cmd.start commands`, then
collect with `traverse Cmd.await jobs`. Collection follows input order; nonzero
exit does not cancel siblings. Interrupted observation leaves jobs available.
`Cmd.cancel job` requests cancellation; status/await reports outcome and cleanup.
Live handles do not promise recovery after host restart.

Completed results capture up to 1 MiB per stream. Automatic display has a shared
64 KiB budget per Haskell tool response; shortening display does not discard captured
data. `inspectFull` also has a display allowance; use pages or Haskell projections
for larger values. Foreground observations skip fully displayed pages; shortened captures remain
available for explicit navigation. Explicit reads do not consume output. Read without executing again:

```haskell
page <- Cmd.output (Cmd.job result)
let relevant = filter (T.isInfixOf "error") (T.lines (Cmd.pageText page))
relevant
next <- Cmd.next page
```

`output` begins stdout at byte zero; `next` advances the page.
`Cmd.readOutput Cmd.Stderr job` and `Cmd.tailOutput Cmd.Stderr job` explicitly
select stderr or a diagnostic tail. Pages are immutable, non-consuming 64 KiB
windows. `Cmd.pageDetails` reports byte positions, gaps and fragments. Current
end while running differs from terminal EOF. Valid UTF-8 is preserved across
forward page boundaries; invalid or lost boundary bytes have replacement text
and explicit lossiness. Complete-stdout extraction rejects lossy content.

Retention keeps a 16 MiB prefix plus 256 KiB tail per stream, subject to a
128 MiB owner budget and the latest 32 completed jobs. Completed logs can be
evicted; active streams continue draining when retention fills. A reported gap
cannot be repaired by expanding display. Choose a workspace log file for larger
or longer-lived evidence. Partial text is diagnostic data, not complete JSON.

`Cmd.withStdin` provides a pipe; `Cmd.withTerminal` provides a PTY initially sized
to the owning TUI. Retain the job for `Cmd.sendInput`, `Cmd.closeInput` and
`Cmd.resize`. PTYs use terminal EOF input instead of `closeInput`.
`Cmd.completion job :: R.EventSource Cmd.CommandResult` supplies one retained
terminal event, including attachment after completion. Load `shoal-define-actors`
for custom routing. Captured handles do not transfer authority; finish collectors
when their remaining obligations are settled.

For project-authored direct tools, see [Defining compiled tools](references/hosted-tools.md).
