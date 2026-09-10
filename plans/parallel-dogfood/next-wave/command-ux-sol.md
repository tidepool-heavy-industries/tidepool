# Command jobs UX run and paused execution handoff

Date: 2026-09-10  
Starting source: `28ec1c6eebbf00e7916b0955aeea52ac4e926d1c`

This wave ended before actor fan-out or megatask implementation. No child actors
were started. Existing product plans and source were preserved.

## Observed command behavior

### Interrupted wait and retained job

The focused warm command was:

```haskell
let warmMain = withMemory (GiB 8) [bash|bash scripts/dev-shell.sh cargo check -p tidepool --bin tidepool|]
warmJob <- Cmd.start warmMain
warmDone <- Cmd.await warmJob
```

The model turn containing `Cmd.await` was externally interrupted after about 107
seconds. In the same resident session, the existing binding remained usable:

```haskell
warmStatusAfterInterrupt <- Cmd.status warmJob
```

returned:

```text
CommandFinished (CommandResult {
  commandOutcome = CommandExited 0,
  commandCleanup = CommandClean
})
```

`Cmd.output warmJob 12000` reported the successful `cargo check`, including
`Finished dev profile ... in 1m 56s`, with `commandTruncated = True`. The job had
already finished when inspected, so it was not cancelled. This proves handle
reuse after an interrupted wait in this live resident session. It does not prove
handle durability across a workbench, actor, host, or process restart.

### Short failure

```haskell
let shortFailure =
      withMemory (MiB 64)
        [bash|printf 'ux-stdout\n'; printf 'ux-stderr\n' >&2; exit 7|]
shortFailureResult <- Cmd.run shortFailure
```

returned `Finished`, `CommandExited 7`, `CommandClean`, distinct stdout and
stderr, and `commandTruncated = False`. A nonzero exit is an ordinary typed
outcome rather than a failed Haskell effect.

### Output tail and display boundary

`numberedJob` printed 2,400 fixed-format numbered lines to each of stdout and
stderr. After its clean exit:

```haskell
numberedSmall <- Cmd.output numberedJob 2048
```

returned tails ending at `OUT-2400` and `ERR-2400`. Both began in the middle of
a line and `commandTruncated = True`; callers must not infer a line-aligned tail.

An attempted `Cmd.output numberedJob 300000` was rejected with:

```text
CommandInvalid "read at most 65536 output bytes"
```

The existing job remained usable. Repeating with the documented maximum bound:

```haskell
numberedLarge <- Cmd.output numberedJob 65536
```

successfully bound the result. A generic `inspectFull numberedLarge` then failed
in preview/display with a stack-overflow diagnostic. It did not invalidate the
binding. The compact projection:

```haskell
inspectFull
  ( T.length (Cmd.commandStdout numberedLarge)
  , T.length (Cmd.commandStderr numberedLarge)
  , Cmd.commandTruncated numberedLarge
  )
```

returned `(32768,32768,True)`. With this equal-volume fixture, the 65,536-byte
request yielded 32,768 characters for each stream. The separate bare-`Text`
expressions:

```haskell
inspectFull (Cmd.commandStdout numberedLarge)
inspectFull (Cmd.commandStderr numberedLarge)
```

both succeeded and displayed their complete captured tails. This is consistent
with the operator's finding that generic record display uses `Text.pack (show
value)`, while bare `Text` bypasses `Show`; the evidence locates the observed
failure in whole-record display, not command capture. The failing whole-record
expansion was not retried.

### Live inspection, await, and reuse

```haskell
let sleepingCommand =
      withMemory (MiB 64)
        [bash|printf 'sleep-start\n'; printf 'sleep-start-err\n' >&2;
              sleep 5;
              printf 'sleep-end\n'; printf 'sleep-end-err\n' >&2|]
sleepingJob <- Cmd.start sleepingCommand
sleepingLive <- Cmd.status sleepingJob
sleepingLiveOutput <- Cmd.output sleepingJob 4096
```

observed `CommandRunning` and only the two start lines. Later,
`Cmd.await sleepingJob` returned `CommandExited 0`/`CommandClean`, and
`Cmd.output` through the same handle contained both start and end lines.

`Cmd.run` on a two-second command returned `Cmd.Pending job`, confirming the
documented short observation window. Extracting the job required the qualified
constructor:

```haskell
let Cmd.Pending runPendingJob = runTimeoutResult
runPendingDone <- Cmd.await runPendingJob
```

An initial `let Pending ...` failed because the constructor was not in unqualified
scope even though the rendered result says `Pending (...)`.

## Freeform workflows actually tried

Commands compose cleanly as inspectable Haskell values:

```haskell
inspectFull (Cmd.describe shortFailure)
```

showed argv, directory, environment, the 64 MiB limit, and closed stdin before
execution.

Dynamic arguments remained data rather than shell syntax:

```haskell
let argumentProbe =
      withMemory (MiB 64)
        (Cmd.inDirectory "plans"
          (Cmd.withEnvironment [("UX_SENTINEL", "env value")]
            (Cmd.withArguments ["arg with spaces; $(printf unsafe)"]
              [bash|printf 'pwd=%s\nenv=%s\narg=%s\n'
                       "$PWD" "$UX_SENTINEL" "$1"|])))
```

`Cmd.describe` exposed the exact composition. `Cmd.run` printed the selected
directory and environment, and printed the argument literally without executing
its shell-looking contents. A direct invocation also worked:

```haskell
Cmd.run
  (withMemory (MiB 64)
    (Cmd.argv ["git", "rev-parse", "--short=12", "HEAD"]))
```

returning `28ec1c6eebbf`.

Independent jobs can be launched and collected applicatively:

```haskell
(parallelAJob, parallelBJob) <-
  (,) <$> Cmd.start parallelA <*> Cmd.start parallelB
parallelLive <-
  (,) <$> Cmd.status parallelAJob <*> Cmd.status parallelBJob
parallelDone <-
  (,) <$> Cmd.await parallelAJob <*> Cmd.await parallelBJob
```

Both were observed running together, then exited cleanly with their separate
outputs. This is an attractive affordance for focused independent checks without
shell backgrounding, PID handling, or output-file bookkeeping.

Piped stdin also felt natural:

```haskell
let stdinCommand = withMemory (MiB 64) (Cmd.withStdin
      [bash|while IFS= read -r line;
              do printf 'echo:<%s>\n' "$line"; done;
              printf 'stdin-closed\n' >&2|])
stdinJob <- Cmd.start stdinCommand
Cmd.sendInput stdinJob "first line\nsecond line\n"
Cmd.closeInput stdinJob
stdinDone <- Cmd.await stdinJob
```

Live output contained the echoed lines; after closing input the command exited
cleanly and emitted `stdin-closed` on stderr.

Cancellation was exercised on a sleeping command after observing its ready line:

```haskell
cancelIntent <- Cmd.cancel cancelJob
cancelDone <- Cmd.await cancelJob
```

`Cmd.cancel` returned `()`. The final outcome was `CommandCancelled` with
`CommandClean`. A Bash `TERM` trap did not emit its marker, so no claim is made
about which signal or process-group shutdown sequence implements cancellation.

## UX assessment and smallest coherent improvements

The core model is good: Bash remains familiar while jobs gain typed identity,
explicit memory admission, independent stdout/stderr, cancellation, cleanup
evidence, and reusable handles. `Cmd.start` plus applicative composition feels
substantially safer than shell background jobs. `Cmd.run` is convenient for short
commands without hiding the retained handle when work exceeds its observation
window.

The smallest coherent improvements are:

1. Give `CommandOutput` a safe compact presentation: outcome-independent stream
   lengths, truncation, and small tails, while keeping explicit bare-`Text`
   projection for full bounded content. Avoid generic `show` of large `Text`
   fields in the workbench display path.
2. Make interruption wording explicit: an interrupted `await` stops that
   observation, not necessarily the command; direct the user to `Cmd.status` on
   the same binding. Keep restart durability explicitly out of that promise.
3. In the command guide, use qualified `Cmd.Finished`, `Cmd.Pending`, and
   `Cmd.Unavailable` in the first pattern-matching example because that is what
   the actual scope requires.
4. Document beside `Cmd.output` that its byte cap is 65,536, that truncated
   tails may begin mid-line, and how the allowance is divided between stdout
   and stderr. The current error is precise but is discovered only after the
   rejected effect.

These changes improve the existing concepts and examples; no additional job
helper family appears necessary.

## Paused product handoff

- Exact pre-feedback source: `28ec1c6eebbf00e7916b0955aeea52ac4e926d1c`.
- Main ancestry check against
  `3a500da7c2649a04cd4795ef6346615a185e2b33` succeeded.
- The focused `tidepool` binary check completed successfully and warmed the
  checkout, but no applications or engine lead was admitted.
- Applications reconciliation/acceptance and engine M6/M7 production execution
  remain wholly pending under
  `plans/parallel-dogfood/execution-contract.md` and
  `execution/{applications,engine}.md`.
- The combined initial planning proposal and planning-review question were not
  created because the operator paused before fan-out.
- All explicitly started experiment jobs were terminal when checked: seven
  exited zero with clean cleanup and one was cancelled with clean cleanup.
  Commands completed through immediate `Cmd.run` were also terminal; the
  intentional short failure exited 7 with clean cleanup.
- Existing untracked `.shoal/` content was not modified as product source.

