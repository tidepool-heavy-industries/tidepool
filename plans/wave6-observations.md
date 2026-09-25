# Wave 6 observations (run 02a1c2fd, 2026-09-25)

## 05:15Z operator intervention

- The harness's `.exomonad/workspace` submodule was still at a5bbb3b at
  launch, so the run compiled the old workspace: none of the template fixes
  (unique constructor names, watchdog tool-first, discard-hold rollback
  intent, review and command skill text) were live. The deployed binary's
  pin (46de15c) only governs `exomonad new`. Bumped the submodule to
  46de15c and committed the gitlink on harness master (0831b07).
- Two exact-commit reviewers (14@1, 16@1) returned Blocked: the harness's
  review prompt only described `ReviewTask` input, and `reviewCommit`
  forks with `CommitReview`, whose acceptance needs the reviewer to build
  its own Task with the `task` defaults constructor (documented only in a
  Types.hs comment). Added an "Exact-commit reviews" section to
  prompts/review.md (harness 20a5a4f, template ade0e14a0). One reviewer had
  already verified the expected-red test (trial 2 evidence: the red test was
  named as such and confirmed offline).
- The compaction implementer (12@1) returned Blocked before any work: the
  contract needs `schemars` and its owned paths exclude Cargo.toml and
  Cargo.lock. Correct "Blocked early" behaviour from the new prompts; the
  fix belongs to its lead.
- Prompts are compiled into the workspace at run start, so the edits reach
  new forks only after the root calls `reload_agent_spec` (run scope; the
  reload typechecks and refuses on failure). Pasted an operator note into
  the root asking for the reload, the two re-reviews, and the manifest
  ownership fix.
- State at 05:15Z: 16 actors, no fenced or waiting inboxes, root busy;
  run-map reports 19 rejected units and 3 unclassified dispatch failures,
  to be read at the next wake.

## 05:16Z wake

Alive, 12 GiB free (swap 12 GiB), 12 windows, no Failed/Retired, no latched
machine. Root mid-cell (44 s) after the operator note; harness master gained
ee27b86 (item2 live-gap no-spend finding integrated) and 8779c47 (item2
trace scaffold) besides the two operator commits. run-map: actor 18@1
(item2-trace-impl) shows a submitted delivery with host_input=ready since
05:12 (Codex admitted it, queued behind the thread's turn; run-map renders
any in-flight row as fenced, the status view does not); watching, no
action. Rejections: 9@1 reply UpdatePending x2 (wave 5 had x4 with a
watchdog scold; no nudges this time). Checkout wait since 04:47: 382 calls,
p50 20 ms, max 23.5 s (root cell at 05:03 waited 23 s, core lead 11 s):
co-resident contention at 16 actors, as expected without fresh machines.
Trial 2 held: the red-review reviewer named the failing test as the
expected red and refused to call it product verification. Trials 1, 3, 4:
no evidence yet.

## 05:46Z wake

Alive; 6 GiB free but swap 21 GiB; 18 windows (about 22 actors); no
Failed/Retired, no latched machine. Root mid-cell. Master since 05:16Z:
the root's interview answers (ddacd42, 260b694), the schemars scaffold
(c427057), plus the operator's prompt merges. run-map: 26@1
(core-here-snapshot) shows a submitted row with host_input=ready since
05:36, in flight not fenced. Rejections: UpdatePending x2 on 9@1 and 18@1
(no loops); one after-tool nudge on 2@1 at 05:28. One cancellation at
05:16:19: the root's cell answered NotSleeping when the operator interview
paste arrived (the composer path, which completes normally). Contention is
now severe: 228 calls since 05:16 with checkout_wait p50 3.7 s and max
161 s; the core lead's fork cell took 271 s (143 s wait, 66 s hold, 49 s
compile over 8 compiles) and an item2 cell 218 s. Causes: 22 co-resident
actors on one machine, plus the operator's ten engine lanes compiling in
worktrees on the same box (swap). No intervention by the observer rule
(memory above 3 GiB, root not stuck), but the wave is past its useful load
and is due to be wound down. Trial 4: the core lead sent admission and
settlement checkpoints (root interview); the root itself did not. Trial 3:
no real fence yet.

## 06:16Z wake

Alive; 8 GiB free, swap 25 GiB; 21 windows; no Failed/Retired, no latched
machine. Root mid-turn (5m48s, a 103 s cell); master gained b3c3546
(item2-trace amend). run-map (deployed binary) labels the root's own row
fenced: ref36 submitted, host_input=ready, 3 behind, since 06:11; by the
host's rules that is in flight, not fenced (the corrected run-map on main
renders it open). No rejections, nudges or cancellations in the window.
Contention unchanged: 109 calls since 05:46, checkout_wait p50 6.7 s, max
152 s; the core lead's two largest cells took 319 s and 271 s, each about
145 s waiting and 49 s compiling. The operator lanes' builds and the
wave's 21 actors share the box. No intervention; wind-down still pending
the operator. No new trial evidence.

## 06:46Z wake

Alive; 8 GiB free, swap 24 GiB; 23 windows (about 35 actors so far); no
Failed/Retired, no latched machine. Root nine minutes into a turn (an 18 s
lookup, a 96 s cell), not stuck. Master gained 859258f (item-2
trace-design interview recorded). run-map (deployed binary) labels the
root's and the core lead's own rows fenced; both are admitted in-flight
rows (host_input=ready) that the corrected run-map renders open. No
rejections, nudges or cancellations. Contention: 143 calls since 06:16,
checkout_wait p50 5.2 s, max 174 s (a core-lead cell waited 174 s for a
3 s compile). No intervention; the wave is past its useful load and the
wind-down is the operator's call. No new trial evidence.
