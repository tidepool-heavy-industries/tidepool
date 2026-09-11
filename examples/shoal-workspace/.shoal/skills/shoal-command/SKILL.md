---
name: shoal-command
description: Use when composing commands as Haskell values, recovering retained output, controlling PTY/stdin, or routing command completion. Ordinary shell calls use the direct tool schemas without loading this skill.
---

Use the direct shell tools for ordinary shell work. `bash` accepts literal
scripts, including multiline Bash and heredocs. `exec_command` adds `workdir`,
`environment`, `memory_mib`, `tty` or piped `stdin`. No Haskell wrapper is needed.
These are compiled Haskell handlers over the same command owner as `Cmd`.

For example, call `exec_command` with:

```json
{"cmd":"git status --short"}
```

For an expected long check, start once with an explicit memory limit and a short
initial observation (choose the limit for the actual check):

```json
{"cmd":"cargo test -p my_crate --lib","memory_mib":4096,"yield_time_ms":1000}
```

Use the returned `session_id` verbatim. `write_stdin` with omitted `chars` polls;
with `chars` it sends input. For piped stdin, `close_stdin: true` sends any final
chars before EOF. PTYs reject this flag; use explicit terminal input there.
If a write is acknowledged but EOF fails, retry close-only, without chars.
Acknowledgment means the backend accepted the write, not that the child consumed it.
An uncertain write must not be replayed automatically.
`cancel_command` requests cancellation of the same job, regardless of stdin mode;
its receipt distinguishes the request from terminal outcome and cleanup. Repeated
close/cancel is safe; cancellation preserves an already-finished outcome.
`read_output` with that ID and `stream: "Stderr"`
reads diagnostics from the beginning; continue at the returned `next_offset`.
None of these operations reruns the command. A finished nonzero exit is a command
result; inspect its diagnostics. Running or queued means the same job remains
owned. Do useful independent work or route completion rather than repeatedly
polling through model turns. `Cmd.completion` is an actor EventSource, not an Await
value for `watch`; see the routing example linked below.

Starting with no output is ordinary progress. Readable-but-empty output has byte
positions; an unavailable-output error is different and keeps the same job.

Defaults: 256 MiB and a 30-second observation. Expiry leaves the command alive.
`max_output_bytes` is a byte budget, not a token count. Direct execution responses
use at most 32 KiB; oversized foreground displays use an 8 KiB preview. Shortened
output is recoverable only to the extent the job still retains it; follow the
reported output position or gap. Do not rerun merely to obtain hidden output.

Use Haskell for reusable command values, data-dependent follow-ups, or typed
completion routing. `Cmd` is `Tidepool.Command`; `bash`, `withMemory`, `MiB`,
`GiB` and qualified Text as `T` are loaded:

```haskell
result <- Cmd.run [bash|git status --short|]
```

Output appears automatically, including for a bound result. The result remains
available as Haskell data; displaying it again does not execute the command.
Inside an effectful block, `print value` emits bounded `Display` output in execution
order, including output before a later failure. It uses the existing Console effect;
it is not Prelude's `Show`-based IO print. State-machine actors log this output without
waking a model. Large values still need projections or explicit pages.
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
