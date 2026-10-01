# Focused test helper seed

The root's `.exomonad/helpers/` draft can be published with `reload_helpers`.
Forked actors receive its selected snapshot; later edits require explicit delivery.
Set the package, target, filter, expected count, exact source and memory limit for
an actual focused check. Use the repository's current runner and isolated outputs.

`Project.TestEvidence` chooses the project's `scripts/cargo-focused-test` argv.
`Exomonad.Contrib.Check.Cargo` owns source, artifact, selected/executed counts,
original command identity, output completeness and cleanup evidence. The runner
embeds its JSON in the original terminal output; collectors do not open another
actor's working files. Authored check descriptions remain claims.

For a sequential check, call `startFocused memory spec`, handle typed setup
refusal, await `Cmd.await (runJob run)` and call `collectFocused run` on that same
original handle. `focusedPassed` checks clean exact source, successful exit and
executed counts in code. Zero matches, missing metadata, dirty source or lost
output remain explicit failed or unknown evidence. `diagnoseFocused` projects
retained failure evidence; semantic remedy policy belongs to the project.

For ongoing collection, `Exomonad.Contrib.CheckResults.watchChecks` attaches
named original runs. Handle empty/duplicate-name and attachment refusal; retain
every admitted job and refused start. `readChecks` keeps terminal receipt, source,
artifact, selected and executed counts, log path and notification receipt.
`finishChecks` drains its actor after obligations settle, without deleting command
evidence or establishing resource release.

`Exomonad.Contrib.CheckPlan.startGate runner owner name memory spec` composes one
runner and collector. `GateWatchRefused` retains the original run; `GateWatching`
retains run and watcher. `readGate` dispatches typed failure or unknown callbacks
over the retained result. `reopenGate` observes that same run without relaunching it.

For several checks, author `[PlanCheck]` with explicit `planRunner`, source-dependent
`planSpec`, `planMemory` and `WithoutPreparation` or `PrepareWith`. Call
`startCheckPlan owner candidate checks`, inspect its typed refusal and retain every
`planStarts` entry. `readCheckPlan` and `planSummary` expose the aggregate evidence;
unknown, failed, pending or refused members never produce `planPassed = True`.

These focused helpers use explicit actor-owned command policy for collection
across turns. Ordinary `Cmd.start` remains invocation-owned: do not return
unfinished handles without awaiting them or inspecting a successful detach receipt.
A completion notice is a reason to inspect, not evidence that tests passed. Keep
original retained checks separate from reported candidates, reviewed proof and
checks executed on the integration head.
