# Wave 7 observations (run 26122845, 2026-09-25)

## 08:56Z wake

Alive; 16 GiB available, swap 9.5 GiB; 12 windows; no latched machine; one
Failed/Retired match is 18@1's degraded cleanup (a retired child). Checkout
wait across the run: p50 0 ms, p90 8.3 s, max 61 s (wave 6: p50 about 6 s,
max about 170 s): the off-checkout compile fix holds; the tail is one 8-compile
cell. Progress: (c) store drop integrated at 08:23Z; settings preflight lead
(12@1) and a set_effort verb leaf with its review running; (d) compaction lead
with server and contract leaves plus two reviews; prior-run stacks 5876336
((c)) and caddc4c ((d)) recorded as retained candidates.

Delta: since 08:28Z the master gained 12 commits, 11 of them NEXT.md
checkpoint docs from the root and none an integration. My own owner.md rule
("write the admission checkpoint into NEXT.md and commit it") caused it. Fixed
the rule (harness 5045fdc: commit NEXT.md only with an integration or at turn
end) and sent the root one note pointing it at the retained stacks as
integration candidates. Also: 7@1 (compaction server) hit UpdatePending x3 on
a repair steer (turn-end delivery; it was forked before the short-turn rule);
one watchdog nudge on 16@1.

## 09:26Z wake

Alive; 13.8 GiB available; 15 windows; no latched machine; no rejections or
nudges this window. (d): the Compactor Server component was implemented,
repaired, reviewed and integrated (03beea9 merged at c3f29d0, 09:03Z), with
one docs commit after it: the root followed the note and the new rule. The
remaining (d) gates are the Engine::run consumer (the root owns that seam)
and the unanswered-call experiment. (c): the settings lead runs three parallel
waves (set_effort verb with its review, visibility, Here-fork rebase with a
pending-claims review); the retained prior stack 5876336 is still unmerged,
being recovered slice by slice. The root is reassigning a provenance repair
to a retained owner.

Costliest delta: shared-checkout contention from stale split compiles. Since
launch, 206 splits went stale, 205 on `CompileView`, and 150 fell back to
compiling under the checkout (about 14 s hold each); two cells at 08:57 waited
79 s and 81 s. Even selected-context Luna leaves go stale, so the check is
coarser than what their compiles depend on. Median wait stays 0 ms. Engine
lane stale-view started to narrow `compile_relevant_eq` to the actor's own
scope and the cell's imports. No note to the root: nothing is stalled and
integrations are landing.

## 09:56Z wake

Alive; 13.4 GiB available; 16 windows; no latched machine; no rejections.
Checkout wait since 09:26: 180 calls, p50 0 ms, max 32 s. (c): the settings
lead's reviewed stack (visibility, model-facing set_effort verb, effort
provenance, forged-settings drop at envelope ingress) was integrated at
09:49Z (f1334be); the root aligned it with the Compactor source first
(b0e1480). Remaining (c): the Here-fork gate, reassigned to a fresh Here
wave (test design plus recovery) whose conflicts sit in the Here owner's
files. (d): Server component integrated at 09:03Z; the Engine::run consumer
seam is the root's own and was untouched.

Costliest delta: the root announced it would wait for the Here candidate
while its own (d) seam sat idle. One note sent: do the Engine::run consumer
now, return to Here when its candidate arrives. Also: the set_effort reviewer
(21@1) got the "tried this before" watchdog nudge 8 times in 2.5 minutes;
carded as watchdog noise to look at after the run.

## 10:26Z wake

Alive; 13 GiB available; 20 windows; no latched machine. The 09:56 note
landed: the root wired the Server Compactor into Engine::run with a persisted
window boundary (2b5e152, 10:08Z) and integrated a production demo consumer
(5c17907, 10:19Z); its repair review is pending with the same reviewer (the
one-review rule held). (c): the root passed durable request identity to the
spawn verbs for the Here seam (e28126e); the Here wave is active. One
UpdatePending (28@1, once); no nudges.

Costliest delta: checkout tail, not behaviour. Since 09:56, p50 6 ms but the
root's own cell waited 96 s at 10:22 and another took 45 s of compile. That
is the stale-split fallback class fixed on main (7d4a108b4, 43d320dbe) and not
in this build. No note: nothing stalled, integrations landing, (d) is one
review from done and (c) one seam from done.

## 10:56Z wake

Alive; 12.2 GiB available; 21 windows; no latched machine; no rejections.
(d) is done: the replay repair (c8c7fef) was Accepted by the same reviewer on
the exact commit and integrated (f504dd0, 10:38Z); the unanswered-call
experiment ran offline and is recorded (f9d49a7). (c): everything but the
Here seam is integrated; Here is on a bounded rescue wave with an unmerged
red active-Here test and broad engine test failures recorded for its owner.
(b): the root admitted a bounded item-13 trace sufficiency audit and found
a request-correlated Store usage surface to use instead of JSONL. The root
also recorded its own node interview and friction (47df0cd).

Deltas: the NEXT.md-per-step commits came back (7 of the last 12 commits
touch only NEXT.md) despite the 08:56 note; the root called
reload_agent_spec, outcome not visible in the log. Minor cost. The costlier
one is structural: the compaction reviewer (31@1) held the shared checkout
60 s in one cell (compile 9 s), i.e. long work inside a Haskell cell blocks
every co-resident actor; the root's own cell waited 96 s at 10:22 behind
such holds. No note: nothing stalled, (d) closed and (c) is one seam from
closing.

## 11:26Z wake

Alive; 13 GiB available; 21 windows; no latched machine. One commit in 30
minutes (a NEXT.md note); 70 calls; checkout wait p50 0, max 4 s. (c)'s Here
seam is the whole critical path: the rescue owner 34@1 produced fa649cd, which
the root judged not review-ready (output still follows the pin; the
write-output/append race test is missing; post-rebase checks not run). The
root and lead sent 34@1 a short-turn steer and the repair requirements, but
34@1 was 14 minutes into a turn of more than 60 responses, so the host queue
holds both until its turn ends (the Codex turn-end delivery defect; 34@1 also
hit UpdatePending x2 and three watchdog nudges). The root is waiting on it.

Costliest delta: steering cannot reach a child in a long turn. Drove: pasted
the root's requirements directly into 34@1's pane as an operator relay; the
Codex TUI submits composer input after the next tool call, not at turn end,
so this path reaches the child in seconds. Lesson for the harness: the
mailbox Steer class is the fix (docs/dogfood-requirements.md amendment 1);
interim prompt lever: "end the turn after 30 tool calls" did not hold for
a child forked before the rule reached leads' obligations.

## 11:56Z wake: (c) and (d) done; interview sent

The relay worked: 34@1 ordered the Here output before the fresh pin
(b4257da, 11:30Z, four minutes after the operator relay reached it at its
next tool call); reviewer 37 accepted the exact commit; the root merged it
(2e456e3, 11:43Z) and verified Here 6/6 and the full harness library (95
passing). (c) and (d) are integrated on master, 3h31m after launch (wave 6
integrated neither in 3h15m). Remaining open: (b)'s live gates (manual
wait_agent continuation, item-13 trace), which need operator-run live calls.
Sent the root the wave-7 interview (fastest and why, where it waited, which
notes and rules changed behaviour, the three-owner Here seam, prompt edits,
the first adapter-readiness slice, open). Answers go to
docs/exomonad-friction.md; read and analyze at the next wake.
