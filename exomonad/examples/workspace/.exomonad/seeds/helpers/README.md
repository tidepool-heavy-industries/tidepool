# Focused test helper seed

The root actor starts with these files in its `.exomonad/helpers/` draft. Edit
them there and publish with `reload_helpers`; forked actors inherit the draft
as it stands when they fork. Set the
package, target, filter, expected count, source, and a memory limit for the
component under test. The helper starts `scripts/cargo-focused-test` as a
background command. It writes JSON in its own checkout and includes that JSON
in the original job's terminal output, so a completion actor can decode the
selected artifact, source, counts and output log path without opening the
caller's files.

For one check, `Project.FocusedGateExample.startGate owner name memory spec`
starts the job in the calling actor's checkout and attaches a completion
watcher. Its result is `GateSetupRefused`, `GateWatchRefused` (which retains the
original run), or `GateWatching run watcher`. The notice has one compact
terminal line with matched, runnable and executed counts, source, exit,
cleanup and evidence state. After the notice, call
`readGate watcher onFailed onUnknown` to dispatch ordinary typed callbacks
over the retained `CheckEntry` and `CheckOutcome` and receive `Just summary`.
It returns `Nothing` if the watcher has not settled yet. A root actor and a
child actor each call `startGate` in their own checkout.

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
does not erase the command or JSON evidence. Missing or incomplete terminal
JSON remains unknown evidence. Raw job pages can expire later; the command
status and completed JSON remain separate facts. `Project.TestEvidence` owns
this verdict; the seed reexports
it. The caller should read the completion receipt's source and cleanup before
relying on the result.
