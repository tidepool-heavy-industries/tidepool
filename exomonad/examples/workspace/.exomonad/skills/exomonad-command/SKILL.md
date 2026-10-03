---
name: exomonad-command
description: Use when composing commands as Haskell values, recovering retained output, controlling PTY/stdin, or routing command completion. Ordinary shell calls use the direct tool schemas without loading this skill.
---

Use hosted `bash` for direct repository commands. Its structured `cmd` field
contains literal Bash, including multiline scripts and heredocs; optional fields
select workdir, environment, memory, PTY, and stdin. Haskell `Cmd` composes the same
command owner when results feed a program.

For Haskell composition, prefer `Cmd.bashCommand script` when the script is a
`Text` value. It uses the same Bash command owner without executing a quasiquoter
at compile time. Bash quotations remain convenient for raw multiline literals,
but require the compiler to provision executable quotation dependencies.

Call `bash` with:

```json
{"cmd":"git status --short"}
```

For a build, test run, log or diff that may be large or failing, say what you
are looking for with `focus`; the result keeps the relevant sections and names
what it omitted, so there is no need to rerun with `sed` ranges:

```json
{"cmd":"cargo test -p my_crate --lib","memory_mib":4096,"focus":"the failing test, its assertion and panic message"}
```

For an expected long check, prefer the project's compiled Haskell focused-check
composition: start once, route completion and retain source/count evidence. For
a direct command, give an explicit memory limit and let it manage completion:

```json
{"cmd":"cargo test -p my_crate --lib","memory_mib":4096}
```

Use the returned `session_id` verbatim. `write_stdin` with omitted `chars` polls;
with `chars` it sends input. For piped stdin, `close_stdin: true` sends any final
chars before EOF. PTYs reject this flag; use explicit terminal input there.
Known rejection explicitly reports that neither chars nor EOF was submitted; correct
the request. If a write is acknowledged but EOF fails, retry close-only, without chars.
Acknowledgment means the backend accepted the write, not that the child consumed it.
An uncertain write must not be replayed automatically.
`cancel_command` requests cancellation of the same job, regardless of stdin mode;
its receipt distinguishes the request from terminal outcome and cleanup. Repeated
close/cancel is safe; cancellation preserves an already-finished outcome.
`read_output` with that ID and `stream: "Stderr"`
reads diagnostics from the beginning; continue at the returned `next_offset`.
An inherited job can be inspected with `Cmd.status`, `Cmd.await`, `Cmd.output`,
and positioned reads without moving the owner's display cursor. Input, EOF,
resize, and cancellation remain with the owner. A fresh command constructed
from an inherited helper runs in the calling actor's checkout; an explicit
directory stays fixed.
Recovery reads are contiguous, with an 8 KiB default display budget;
`max_output_bytes` clamps into 1024..32768 bytes including metadata; any positive
value is accepted. They never use a
head/tail preview. Positions are original bytes, even for lossy UTF-8.
None of these operations reruns the command. A finished nonzero exit is a command
result; inspect its diagnostics. Terminal receipts always show cleanup separately.
Running or queued means the same job remains
owned. Do useful independent work or route completion rather than repeatedly
polling through model turns. `Cmd.completion` is an actor EventSource, not an Await
value for `watch`; see the routing example linked below.

The updated workspace package includes this compiled staged background example
at `.exomonad/workspace/checks/background-command-example.hs` (template source:
`exomonad/examples/workspace/.exomonad/checks/background-command-example.hs`):
start one invocation-owned command, suspend with `awaitCommandEvidence`, and
retain its exact completion receipt and capture. `completionProjection` supplies
a compact view. Its owning checks cover nonzero exit and unavailable capture;
an unavailable read remains an explicit issue. For ongoing observers use a
record actor and an explicit actor-owned job instead. Routine waits need no
model polling or forwarding actor.

Starting with no output is ordinary progress. Readable-but-empty output has byte
positions; an unavailable-output error is different and keeps the same job.

The direct `bash` tool defaults to 1024 MiB. Haskell commands built with
`Cmd.argv`, `Cmd.bashCommand` or a Bash quotation (`[bash|...|]`) default to 256 MiB. Set an
explicit limit for builds and tests. The direct tool waits until terminal
completion, preserving the invocation and presenting output once. An explicit
`yield_time_ms` (0..300000) requests bounded observation without automatic
notification; if the job is still live, the tool detaches it to actor-owned
lifetime before returning.
`background: true` starts actor-owned work and returns immediately with completion
delivery; focus, yield and output-budget presentation options do not apply there.
In Haskell, `Cmd.observe`, `Cmd.observeWith` and `Cmd.observeCompletion`
return bounded status normally and never detach. Explicit ownership transfer is
required before returning unfinished owned work. Use ordinary `Cmd.await` for a
dependent continuation, or completion events for ongoing observers. Waiting alone
does not collect test evidence.
`max_output_bytes` is a byte budget, not a token count. Direct execution responses
use at most 32 KiB (default 32 KiB). Output that fits `max_output_bytes` is shown
whole. Without `focus`, output over budget is shown as a head and a tail with a
marker naming the omitted byte range per stream, plus a recovery pointer; no
Jev call and no sectioning happen on this path. `focus` filters the output to
the sections relevant to that text (example: "the failing test and its
assertion"): the output is split into sections, each scored by Jev for
relevance to the focus and recent conversation, and the highest-relevance
sections are packed to fit `max_output_bytes`, with an `omitted:` marker
naming the sections left out. A `focus`ed call still shows everything, without
scoring, when it already fits the budget. Shortened
output is recoverable only to the extent the job still retains it; follow the
reported output position or gap. Do not rerun merely to obtain hidden output.

Use Haskell for reusable command values, data-dependent follow-ups, or typed
completion routing. `Cmd` is `Tidepool.Command`; `bash`, `withMemory`, `MiB`,
`GiB` and qualified Text as `T` are loaded:

```haskell
result <- Cmd.run [bash|git status --short|]
let changed = T.lines <$> Cmd.stdout result
```

Command event output remains visible in execution order, including output before
a later failure. A command result bound in a cell retains its complete observation;
project the fields you need and use `display result` for bounded structured output.
`display` returns a handle whose fields can be expanded independently. State-machine
actors log this explicit display without waking a model. Use `display (show value)`
when you need Haskell's textual `Show` form. Large values still need projections.
Use `Cmd.quiet action` when an unbound command's observation is unnecessary, or
when suppressing routine presentation inside a larger effectful computation.
Quiet is scoped to that action and does not hide a stopped computation or its
recovery receipt. Nonzero process exits remain in the retained result.

```haskell
let changed = T.lines <$> Cmd.stdout result
display changed
```

`Cmd.stdout` purely extracts complete stdout from exit zero, or an explicit issue.
It never waits, reads more, reruns, or substitutes empty text. Stderr completeness
and cleanup are separate. `Cmd.decodeWith (Cmd.asJSON @Value) (Cmd.stdout result)`
decodes JSON; ordinary Text supports `T.lines`, filtering and other composition.
If capture is incomplete, `Cmd.readStdout (Cmd.job result)` explicitly reads
complete retained stdout, or reports why it is unavailable. Repeated `await`
is not a way to enlarge capture.

`Cmd.run command` starts and suspends until terminal completion; `Cmd.await job`
suspends on the existing job. The same Haskell continuation resumes, including
later statements, and both return completed results including nonzero exits.
This contract also applies to tool bodies and record-actor handlers. A handler
remains serialized while suspended: do not wait for its own mailbox to advance.
`Cmd.observe (Cmd.Observation 250 8192) job` returns the current status normally
after a bounded wait. It never detaches or cancels the job.

`Cmd.start` and `Cmd.tryStart` create invocation-owned work. Scope exit cancels
unfinished owned jobs and retains outcome and cleanup. Await them in the same
invocation, or use `Cmd.detach job` (`tryDetach` returns typed refusal) before
returning. `Cmd.background command` and `tryBackground` explicitly start
actor-owned work and install completion delivery. Returning or capturing a job
handle does not change its lifetime. Borrowed handles permit observation while
available; cancelling a borrowed waiter releases that wait and cannot cancel
the owner's command.

Command reports omit source provenance unless requested. Wrap a command in
`Cmd.withSource` when its report needs the starting directory, Git revision and
dirty state; this runs a separate admitted source probe before the command.
Ordinary commands avoid that extra process.
The direct `bash` tool's `background: true` path requests source capture for its
completion notice, so it runs the extra probe. `Cmd.background` does so only
when its command is wrapped in `Cmd.withSource`.

Commands are reusable values. Quotations preserve literal Bash, including
multiline scripts, heredocs and indentation. Haskell does not interpolate shell
variables or backticks. Bash retains ordinary exit/pipeline semantics; choose
`set -euo pipefail` when appropriate. Pass dynamic values as arguments:

```haskell
let preview path = Cmd.withArguments [path] [bash|sed -n '1,20p' -- "$1"|]
display (show (Cmd.describe (preview "a path; not shell syntax")))
```

`Cmd.argv [program,arg1,arg2]` constructs arguments without shell parsing or
interpolation. Before execution, selected Git `reset`, `rebase`, `branch`, and
`push` forms are currently wrapped in Bash to enforce the discard hold, so those
commands may not reach the backend unchanged. `Cmd.inDirectory` and
`Cmd.withEnvironment` customize intent. An omitted directory means the actor's
own workspace, and relative paths start there. For an actor with an agent
process of its own that workspace is its sandbox; an actor started with
`R.start` has no process and no sandbox, so its commands run in whatever
worktree for which it holds an owned handle, or in the source checkout when it holds none —
which is why such an actor can write in the worktree it was given
(`git reset --hard` in its own checkout works) and nowhere else. Constructing
a command does not snapshot inherited environment or location. `Cmd.describe`
inspects intent without executing. Use `pwd` in a command when location is evidence.

A `git` call that would take committed work off a ref — `reset --hard` to a
commit that lacks `HEAD`, `rebase --onto` or `--skip` that drops commits,
`branch -D` of a branch's last ref, a force push over published commits — is
held until the command names the ref's actual tip:
`withDiscardIntent (DiscardIntent expectedTip target reason)` on the command, or
`EXOMONAD_DISCARD_EXPECTED_TIP=<tip>` in a `bash` call's environment. The
refusal names the actual tip and the commits that would lose the ref; a clean
worktree does not waive it.

An actor with no process of its own also has no terminal: run its commands
with piped or closed input, never `TerminalInput`.

Choose realistic explicit memory for builds/tests, e.g.
`job <- Cmd.start (withMemory (GiB 8) [bash|cargo build|])`.
`start` returns immediately; admission queues automatically. Memory is a hard
limit and admission weight. `traverse Cmd.run commands` runs sequentially to
terminal completion. To start all first, retain `jobs <- traverse Cmd.start commands`,
then collect with `traverse Cmd.await jobs` in the same invocation. Collection
follows input order; nonzero exit does not cancel siblings. For work across
invocations use `Cmd.background` or explicit `Cmd.detach`; cancellation of an
owned invocation stops its unfinished commands.
`Cmd.cancel job` requests cancellation; status/await reports outcome and cleanup.
Live handles do not promise recovery after host restart.

Completed Haskell results capture up to 1 MiB per stream. Explicit display is
bounded; shortening it does not discard captured data. Use Haskell projections
for larger values. Foreground observations skip output already delivered through
the command event stream; shortened captures remain available for explicit
navigation. Explicit reads do not consume output. Read without executing again:

```haskell
page <- Cmd.output (Cmd.job result)
let relevant = filter (T.isInfixOf "error") (T.lines (Cmd.pageText page))
display relevant
next <- Cmd.next page
display (Cmd.pageText next)
```

`output` begins stdout at byte zero; `next` advances the page.
`Cmd.readOutput Cmd.Stderr job` and `Cmd.tailOutput Cmd.Stderr job` explicitly
select stderr or a diagnostic tail. Pages are immutable, non-consuming 64 KiB
windows. `Cmd.pageDetails` reports byte positions, gaps and fragments. Current
end while running differs from terminal EOF. Valid UTF-8 is preserved across
forward page boundaries; invalid or lost boundary bytes have replacement text
and explicit lossiness. Complete-stdout extraction rejects lossy content.

When commands use the `HostCommand` backend, each stream retains a rolling 4 MiB
and reads report dropped bytes. Other backends may retain output differently;
treat reported gaps and unavailable reads as authoritative. Display limits do
not enlarge retention. Write larger or longer-lived evidence to a workspace log
file.
Partial text is diagnostic data, not complete JSON.

`Cmd.withStdin` provides a pipe; `Cmd.withTerminal` provides a PTY initially sized
to the owning TUI. Retain the job for `Cmd.sendInput`, `Cmd.closeInput` and
`Cmd.resize`. PTYs use terminal EOF input instead of `closeInput`.
`Cmd.completion job :: R.EventSource Cmd.CommandResult` supplies one retained
terminal event, including attachment after completion. To continue automatically:

1. Capture the original job in the handler; the event contains outcome and
   cleanup, not the job or captured output.
2. Read retained stdout with `Cmd.readStdout job`; include `Commands` in the
   handler's effect row. Handle `Left` explicitly rather than substituting empty
   evidence. `Cmd.stdout` accepts `Cmd.RunResult`, not the completion payload.
3. `Cmd.readStdout` requires successful completion and answers
   `Left (Cmd.Unsuccessful outcome)` for anything else, so a failed command's
   diagnostic output is read with `Cmd.readOutput`/`Cmd.next` instead, retaining
   outcome and cleanup separately.
4. Interpret the available evidence and execute the prepared follow-up in that
   handler. Preserve outcome and cleanup separately from a semantic judgment;
   stdout alone is not a complete diagnostic bundle for commands using stderr.
5. Retain the result and finish the collector when its obligations are settled.

Use `exomonad-define-actors` for handler construction. Captured jobs permit
inspection while available; they do not transfer command control.

For project-authored direct tools, see [Defining compiled tools](references/hosted-tools.md).
