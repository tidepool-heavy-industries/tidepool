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
