# Focused test helper seed

The root actor starts with these files in its `.exomonad/helpers/` draft. Edit
them there and publish with `reload_helpers`; forked actors inherit the draft
as it stands when they fork. Set the
package, target, filter, expected count, source, and a memory limit for the
component under test. The helper starts `scripts/cargo-focused-test` as a
background command; its retained JSON contains the selected artifact, source,
matched tests, execution summaries and output log path.

Call `startFocused (Cmd.GiB 2) spec` with a memory limit appropriate to the
check, and handle `Left (NonPositiveExpected n)` before using the `Right`
`FocusedRun`. To work while it runs, give the retained run a name and attach
`Right watcher <- watchChecks owner NotifyProblems [("name", run)]` from
`Project.CheckResults` after handling its typed empty/duplicate-name refusal.
The actor receives `Cmd.completion` even if it attaches after the job finishes.
`readChecks` retains each terminal receipt, source, artifact digest, selected
and executed counts, output log path, and notification receipt. Several named
runs can yield one final summary in input order with `NotifySummary`. Use
`NotifyAllTerminal` when every completion should notify, or `NotifyProblems`
for individual failed and unknown outcomes. It reports executed assertion
counts separately from source assurance, so a passing test in a dirty checkout
stays visible while the strict acceptance verdict is unknown. Finish the actor with
`finishChecks` after reading its final state. For a direct sequential check, wait with `Cmd.awaitFinished`
and call `finishFocused` on the same `FocusedRun`.
`focusedPassed` checks exit, clean source identity, selected count and executed
count in code. The optional `finishFocused` path asks Jev to classify the
retained diagnostic on command failure; an unavailable or doubtful judgment stays in `focusedFailure` and
does not erase the command or JSON evidence. Missing JSON is reported as
missing evidence. `Project.TestEvidence` owns this verdict; the seed reexports
it. The caller should read the completion receipt's source and cleanup before
relying on the result.
