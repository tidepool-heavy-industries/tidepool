# Astra command UX / preserved planner handoff

2026-09-10. Operator ended product execution early for command usability.
No new agents or builds were launched for these experiments. No runtime,
compiler socket or canonical package was changed.

## Preserved work

Planner commit `28ec1c6eebbf00e7916b0955aeea52ac4e926d1c`, branch
`plan/command-jobs-20260910`, contains `../execution-contract.md` and
`../execution/{applications,engine}.md`. Main remains the launch source
`3a500da7c2649a04cd4795ef6346615a185e2b33`.
Coordinator was commissioned with the selected recipe, Sol Medium, selected
context and `projectHead`. Both product proposals were **not reviewed** and broad
implementation was **not released** by this planner. Operator directly changed
coordinator scope; no duplicate steering or product-proposal request was sent.
Planner's initial `review` collector was drained with `finishWork review`.
Coordinator owns its preserved work/build outcome; this document does not claim
it completed reconciliation or product acceptance. Resume product planning from
the committed contract plus coordinator's eventual handoff in a later run.

## Focused exercise: observed

Bound and reused the same command value twice:

```haskell
let repoRead = withMemory (GiB 1) [bash|git status --short; git log -1 --format='%h %s'|]
readOne <- Cmd.run repoRead
readTwo <- Cmd.run repoRead
```

Both returned `Finished`, exit 0, `CommandClean`, untruncated output. A Haskell
projection compared their stdout as `Just True`. `Text.lines` detected the exact
`?? .shoal/` status line; that Boolean selected a follow-up `git ls-files` command
for the three committed plan files. This was real data-dependent composition,
not copying rendered shell output back into a command.

First projection deliberately extracted stdout from any `Finished`; that is
adequate for reading text but **not** a successful-command predicate. The later
freeform exercise demonstrated why this distinction matters.

Discovery cost: two tool calls / five signature-info queries to find result
constructors/accessors. `:info Cmd.RunResult` returned unknown, while
`:info RunResult` worked; `Cmd.Finished` and `Cmd.capturedOutput` were available.
This inconsistent discoverability is observed, not evidence of a broken runner.

Native comparison: one `exec_command` containing the same status/log and eight
engine-plan lines displayed plain multiline Unicode text. Haskell
`inspectFull (readText enginePreview)` displayed `Just "...\\n..."` and numeric
escapes for §, en dash and λ. The retained value is useful; the default textual
presentation is substantially harder to scan. No latency/token benchmark was
performed; call counts here describe this session, not a controlled comparison.

## Examples that increased friction

* The command skill starts with a build/`Cmd.start` example. For tiny repository
  reads, a first `Cmd.run` example showing result plus safe text extraction would
  have avoided the constructor/accessor discovery round.
* Earlier in planning, one `Cmd.run` concatenated the skill, two plans and the
  large selection manifest. Its captured tail was truncated; `inspectFull` did
  not recover omitted bytes. I reread the needed documents via native shell.
  This was an overlarge read plus insufficient output-budget awareness, not an
  observed command execution failure.
* `inspectFull` successfully expands retained values, but adds ceremony for the
  common “read this file” case. The skill's actual
  `inspectFull <$> Cmd.output failureJob 8192` expression **worked** when tried;
  no additional binding/manual expansion was needed there.
* Explicit GiB 1 was convenient but arbitrary for these tiny reads. It also
  represents admission weight; repeating a build-sized recipe for reads is not
  an attractive default. I did not test smaller limits or admission contention.

## Freeform spin: actually tried

Useful reusable preview:

```haskell
let previewFile path = withMemory (GiB 1) $
      Cmd.withArguments [path] [bash|sed -n '1,8p' -- "$1"|]
Cmd.describe (previewFile "plans/parallel-dogfood/execution/engine.md")
```

`describe` exposed the exact `bash --noprofile --norc -c ... shoal-bash PATH`
argv, closed stdin and memory without executing. This makes `$1` binding
understandable. A separate `printf` with argument
`spaces; $(not-a-command) 'quoted' λ` returned it literally; no shell
interpolation or extra quoting code was needed. This is an attractive affordance.

Then deliberately ran stdout + stderr + `exit 7`. Result preserved both streams,
`CommandExited 7` and `CommandClean`. A completion receipt is not command success;
cleanup success is separately useful. Re-reading its output and `Cmd.status`
worked on the same retained job; no rerun was necessary.

Used a stricter local projection, preserving original receipts:

```haskell
let successfulText result = case result of
      Cmd.Finished _ status output -> case Cmd.commandOutcome status of
        Cmd.CommandExited 0 ->
          if Cmd.commandTruncated output then Nothing
          else Just (Cmd.commandStdout output)
        _ -> Nothing
      _ -> Nothing
let planFiles = maybe [] Text.lines (successfulText readThree)
planPreviews <- traverse (Cmd.run . previewFile) planFiles
```

The exit-7 result projected to `Nothing`. All three plan previews returned text.
This small composition felt better than a shell loop for retaining one outcome
per file and reusing it in later decisions. It is sequential, not a parallelism
test. The projection intentionally collapses failures/pending for this bounded
experiment; it should not replace typed failure handling in production routing.

All nine experiment `Cmd.run` invocations finished synchronously: eight exit 0,
one deliberate exit 7; cleanup was clean. No pending experiment job remains.
No PTY, stdin, cancellation, OOM, event routing or large-output recovery experiment
was run; this note makes no new claims about those boundaries.

## Suggestions, not implemented

1. Render short finished commands as compact outcome/cleanup metadata followed
   by literal stdout/stderr blocks, while retaining the typed receipt for
   composition. Avoid Unicode/nested-string escapes for ordinary file reading.
2. Put a cheap `Cmd.run` repository read first in the skill, followed by a
   complete pattern match showing success, pending and unavailable without
   losing handles. Show `RunResult` discovery using the name that works.
3. Explain the initial capture tail/budget next to `Cmd.run`, not only in the
   `Cmd.output` section. Make truncation conspicuous before the reader assumes
   `inspectFull` can restore omitted bytes.
4. Make the recommended small-read memory choice clear and cheap. Keep explicit
   memory for builds and preserve typed outcomes; no need for a new job registry
   or a broad convenience API to obtain a good default.

The strongest feature is commands as reusable values plus retained result data.
The weakest part of this trial is reading prose through generic `Show`-like
rendering. Native shell remains lower-friction for one-off human-readable reads;
Haskell becomes compelling when the next decision consumes the result.
