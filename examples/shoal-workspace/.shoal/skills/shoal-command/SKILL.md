---
name: shoal-command
description: Run shell commands through resident Haskell; consume structured results, navigate retained output, or control long-running and interactive jobs.
---

`Cmd` is already `Tidepool.Command`; `bash`, `withMemory`, `MiB` and `GiB`
are loaded. `T` is the shared qualified Text import. Commands are reusable,
inspectable values; effects launch them. Start with an ordinary read:

```haskell
result <- Cmd.run [bash|git status --short|]
result
```

Results display readable output while remaining Haskell values. `Cmd.run` waits
up to one second, returning `Cmd.Finished`, `Cmd.Pending` or `Cmd.Unavailable`.
`Cmd.job result` retrieves the same job in every case. When waiting is intended:

```haskell
finished <- Cmd.await (Cmd.job result)
let lines = T.lines <$> Cmd.stdout finished
lines
```

`Cmd.stdout` is pure: complete stdout from exit zero, or an explicit output issue.
It never waits, reads more, reruns, or turns failure into empty text. Stderr
completeness and descendant cleanup are separate from successful stdout.
`Cmd.decodeWith (Cmd.asJSON @Value) (Cmd.stdout finished)` decodes JSON without
nested error plumbing; `Cmd.asJSON` also accepts ordinary Text. Use `T.lines`
and normal Haskell functions for filtering. Retain results rather than copying
rendered output into another shell command.

Quotations preserve literal Bash, including multiline scripts and heredocs.
Haskell does not interpolate shell `$variables`, backticks or indentation.
Bash retains its normal exit/pipeline behavior; choose `set -euo pipefail` when
appropriate. Pass dynamic values as arguments:

```haskell
let preview path = Cmd.withArguments [path] [bash|sed -n '1,20p' -- "$1"|]
Cmd.describe (preview "a path; not shell syntax")
```

`Cmd.argv [program,arg1,arg2]` bypasses Bash. `Cmd.inDirectory` and
`Cmd.withEnvironment` customize a command value. The native owner resolves the
directory at launch: omitted means its configured workspace; relative paths are
relative to that workspace. Constructing a Command does not snapshot a directory
or inherited environment. Reusing it preserves explicit arguments/overrides, but
each launch resolves the owner's environment policy again. `Cmd.describe` shows
intent, not an execution receipt; use `pwd` in the job when location is evidence.
Ordinary commands use 256 MiB;
choose explicit realistic memory for substantial work, e.g.
`Cmd.start (withMemory (GiB 8) [bash|cargo build|])`. The shared pool queues
admission automatically; memory is both a hard limit and admission weight.

For longer work, bind `job <- Cmd.start command`, do other work, then
`Cmd.await job`. Interrupting an observation does not cancel the command; inspect
`Cmd.status job` using the retained handle. `Cmd.cancel job` accepts cancellation
intent; status/await report the eventual outcome and cleanup. Live handles are
not a promise of recovery after host restart. The one-second `run` window limits
observation, not execution or cleanup. No execution-deadline modifier is implied.

`traverse Cmd.run commands` launches sequentially, waiting briefly on each; jobs
that return Pending may overlap. To launch all before waiting, retain
`jobs <- traverse Cmd.start commands`, then `results <- traverse Cmd.await jobs`.
Results follow input order. A nonzero exit is a result, not sibling cancellation;
if collection is interrupted, the retained jobs remain the recovery path.

Each result initially captures an 8 KiB tail per stream. Display shortening is
separate from capture omission and retention loss. Read more without rerunning:

```haskell
page <- Cmd.readOutput Cmd.Stdout (Cmd.job finished)
page
next <- Cmd.nextPage page
let relevant = filter (T.isInfixOf "error") (T.lines (Cmd.pageText next))
relevant
```

`readOutput` begins at byte zero; `Cmd.tailOutput Cmd.Stderr job` selects a tail.
Pages are immutable, non-consuming 8 KiB windows. `Cmd.pageDetails` exposes
positions, loss and fragment markers. Current end while running differs from
terminal EOF. Positions/counts are bytes, not Text character indices. Valid UTF-8
is preserved across ordinary forward page boundaries; invalid or already-lost
boundary bytes display with replacement characters and explicit lossiness.
`Cmd.stdout` rejects lossy capture. A gap means retention passed the requested cursor; it is not
recoverable by expanding the display. Retention is 256 KiB per stream and the
latest 32 completed jobs. Write logs to a chosen file when longer retention is
needed. Partial text is diagnostic data, not a complete JSON document.

Use `Cmd.withStdin` for a pipe and `Cmd.withTerminal` for a PTY initially sized to
the owning TUI. Retain the job for `Cmd.sendInput`, `Cmd.closeInput` and
`Cmd.resize`. PTYs use terminal EOF input instead of `closeInput`.

`Cmd.completion job :: R.EventSource Cmd.CommandResult` supplies one retained
terminal result to a record actor, including attachment after completion.
Load `shoal-define-actors` when defining a router; ordinary commands need none.
Captured jobs do not transfer control authority. Finish collectors once their
remaining obligations are settled.
