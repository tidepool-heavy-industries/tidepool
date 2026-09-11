# Foreground commands and retained background handoff

Implement on main and the matched native command-jobs continuation. Preserve paused
product branches and running packages. Launch the next RSI wave only after the
exact four-file read and foreground handoff pass through an actual TUI with a
scripted provider. Earlier command-output checks do not prove this revision.

## Interaction contract

`Cmd.run command` and `Cmd.await job` observe for up to 30 seconds and return a
completed result. Nonzero exits retain ordinary typed results and diagnostics.
`Cmd.start` is the explicit immediate-background path. Commands remain reusable
values; reading a result or retained output never reruns a command.

When observation expires, the command owner retains the exact job and stops the
suspended computation without executing its continuation. The active execution
boundary decides remediation:

- Interactive workbench: install an actual collision-free `jobN :: Cmd.Job`
  binding through the existing lexical-binding owner. Return its receipt and
  available output in the current tool response. State that the enclosing result
  was not installed and later statements did not run. `Cmd.await jobN` observes
  that job; it never resumes the abandoned computation.
- Haskell actor handler: propagate a typed failure through ordinary supervision.
  Do not manufacture interactive bindings or message another model. Retain the
  command; an observation deadline is not cancellation.

This is an execution-boundary distinction, including synchronous calls from a
Codex actor into a Haskell handler. It is not a role or model-presence flag.
Host-controlled stops must not be swallowed by Haskell exception handling.
Preserve committed prefixes and exact transport-retry receipts. Installation
failure must preserve the job and must never claim an alias was installed.
Reuse an existing automatic alias only while it still identifies the exact job;
respect user shadowing and collisions.

## Output and storage

- Show command results even when bound. Reuse the current tool response, without
  extra wake messages or duplicate echo. Scoped `Cmd.quiet` suppresses routine
  output, never necessary handoff information.
- Share a 64 KiB UTF-8 display budget across one tool response. Medium file reads
  should display completely; larger output uses line-aligned head/tail with exact
  omission and navigation information. No implicit LLM summarization.
- Capture up to 1 MiB per stream in completed results, independent of display.
- Existing native job ownership retains a file-backed 16 MiB prefix and 256 KiB
  diagnostic tail per stream, with a 128 MiB aggregate actor allowance. Use
  private runtime storage outside snapshots. Evict completed output first; bound
  active writes while continuing pipe drainage. Report retention gaps explicitly.
  Clean storage through existing lifecycle ownership; promise no restart recovery.
- `Cmd.stdout` is pure successful, complete stdout extraction. Stderr completeness
  and cleanup are independent. `Cmd.readStdout job` explicitly retrieves retained
  complete successful stdout without waiting or rerunning.
- `Cmd.output job` and `Cmd.next page` navigate stdout from the beginning in
  64 KiB pages; explicit stream/tail selectors remain available. Preserve byte
  positions, decoding boundaries and honest gaps.
- Repeated foreground observations show newly observed bytes; explicit reads
  remain immutable and non-consuming.

## Implementation and acceptance checklist

1. Complete typed command observation and workbench-only handoff, binding/receipt
   settlement and failure paths. Remove public Pending/Unavailable result cases.
2. Complete native bounded storage, complete capture and read/navigation semantics.
3. Complete automatic bounded presentation, scoped quiet and observation cursors.
4. Update shipped guidance, skills and examples in place. Teach cheap reads first;
   preserve literal multiline Bash, safe arguments, memory, stdin/PTY, cancellation
   and completion routing. No mandatory inspection or memory ceremony.
5. Run focused tests covering expression/bind/nested stops, unexecuted suffixes,
   committed prefixes, exact aliases/collisions/shadowing, repeated observations,
   finish races, interrupted installation, binding failure, handler failure,
   retained-session usability and continuation/root cleanup.
6. Exercise storage quotas, UTF-8, gaps, complete capture, collections and total
   display budgets. Execute exact conversational examples with resident Haskell.
   Parser-only behavior stays in low-level parser tests.
7. Run actual binary/mock-provider acceptance for the four-file launch read and
   foreground handoff; verify recovery uses the same job without reexecution.
8. Format, review affected consumers, commit the matched main/native revisions,
   freeze the package and prepare the next dogfood launch.

Current evidence: actor crate and the application library test target compiled.
Resident tests passed for output-observation failure and the actual 30-second
deadline: recovery binding usable, committed prefix preserved, nested continuation
and later statements skipped, command retained without cancellation or rerunning.
Cancellation before backend admission also passed: awaiting returns a completed
result with empty output, and late backend supply cannot start the command.
The handler boundary passed through a synchronous record-actor call: an explicitly
authorized command observer fails through normal supervision without installing
interactive recovery bindings, and the caller workbench remains usable.
Native storage is now wired through the existing job owner: anonymous prefix
files, bounded diagnostic tails, aggregate accounting and completed-output
eviction. The five shared byte-paging tests passed after extracting their common
segment reader. All nine focused TUI storage/ordering tests passed, including
complete capture beyond tail capacity, explicit gaps, zero allowance, Unicode,
sustained output bounds and completed-before-active quota eviction. Evidence:
`/tmp/foreground-native-storage.log`; no real-TUI acceptance claim yet.
Automatic output, scoped quiet, and retained quiet results passed in the resident
workbench (`/tmp/command-presentation-test.log`). All four rewritten command-skill
code blocks executed successfully (`/tmp/command-skill-foreground-test.log`).
Repeated output-failure handoff now reuses the exact declared alias; materialized
user shadowing preserves the user's value and installs a fresh alias. The same
resident test proves recovery still launches only one command
(`/tmp/command-alias-reuse-test.log`). Four low-level display tests passed,
including UTF-8 budgets, head/tail preservation and aggregate allowance.
Resident observation checks passed for repeated awaits, non-consuming explicit
reads and timeout output (`/tmp/command-observation-cursors-test.log`). Failed
binding installation preserves the existing job and makes no alias claim
(`/tmp/command-binding-failure-test.log`).

The native/context audits found that successful hosted custom responses already
project away receipt metadata, but failures and fallback JSON lacked a final cap.
A final hosted cap now covers all paths; five context/prefix checks passed
(`/tmp/command-context-prefix-test.log`). `inspectFull` now retains a renderer and
uses a finite allowance rather than eagerly serializing with maxBound. The large
value/JSON fixture passed with bounded display and unchanged retained data.
Command presentation is retained at the input-unit owner before resuming Haskell,
so a subsequent failure cannot discard already-completed command output.
The typed hosted failure projection preserves that output ahead of bounded error
detail and optional operation metadata. A 60 KB command result plus a large
diagnostic and 1,000 operation receipts passed the final response-budget check;
the resident later-failure regression also passed
(`/tmp/command-failure-display-test.log`, `/tmp/command-final-failure-test.log`).
A disconnected caller at the observation-to-handoff boundary passed exact retry:
the returned receipt was unchanged, its installed alias remained usable, and
only one command executed (`/tmp/command-disconnected-handoff-test.log`). This
exercises caller cancellation while the kernel retains ownership; it does not
claim recovery after killing the host or its binding worker.
The exhausted-display-budget regression also passed: a command hidden after a
large explicit value inspection remained available to the next foreground
observation without reexecution (`/tmp/command-hidden-output-test.log`).

Native history applies a separate model policy even when TUI/rollout shows full
output. The existing `tool_output_token_limit=16384` override is now uniform in
fresh/resumed/forked hosted launch commands; its focused test passed
(`/tmp/command-native-history-budget-test.log`). No protocol extension is needed.
Actual acceptance must inspect the subsequent normalized provider request.
The real-TUI resource fixture passed against the matched native and Shoal binaries
(`/tmp/command-full-tui-foreground-acceptance-r2.log`, 228.26 seconds, scripted
local provider, no paid inference). The next normalized provider request retained
the exact 59,935-byte four-file read. A real foreground deadline installed a usable
binding; recovery ran the original job exactly once and left the abandoned suffix
untouched. The same fixture exercised command OOM, resource admission, stdin,
cancellation, completion routing and PTY dimensions. Native tail metadata now
reports the retained prefix accurately; all five focused storage tests passed
(`/tmp/command-native-tail-metadata-test.log`). The native revision is
`80e36633f515b03e11189e8516be21065e73335e`, published to the Codex fork and pinned
by main's flake inputs.

The prepared immutable development runner is
`/nix/store/crrcqgisalq6h2fca2c0ipj4ggpz3j2v-shoal-command-foreground-runner`.
Its exact-source selection and package checks belong in
`target/command-foreground-runner-20260911/`. Commit/push main and complete those
launch-package checks before declaring the wave ready.

No entirely hidden command presentation may advance its cursor; head/tail
omissions are explicit intentional skips, not implied full delivery. Historical feedback is in
`plans/parallel-dogfood/next-wave/command-ux-{astra,sol}.md`.
