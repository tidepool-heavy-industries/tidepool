# Wave 8 observations (run fa78a7c7, 2026-09-25)

## 14:49Z wake

Alive; 12.6 GiB available; 7 windows; no latched machine; one tool error;
no rejections; one watchdog nudge. The runtime fixes hold: 98 calls, checkout
wait p50 0 ms and max 0.4 s (wave 7: max 61 to 105 s); the root's slowest
compile 5.7 s over 8 compiles, slowest root cell 11 s (wave 7's late root
binds compiled for 45 to 100 s). The handoff worked: the accepted finalize
branch was merged in the first two minutes (9e2f8a2, 14:39Z) and wired
through the Engine seam (be50cdd, 14:46Z); the replay candidate is being
re-checked by one reviewer while a replay-provider repair leaf and its
contract review run, and the finalize-engine seam has its own review. No
delta worth a note.

## 15:19Z wake

Alive; 9.5 GiB available; 8 windows; no latched machine; no rejections; four
watchdog nudges on the replay leaf in 20 s (the "tried this before" noise
again). Runtime stays fast: 249 calls, checkout wait p50 0, max 1.6 s; the
slowest compile 7.9 s over 9 compiles. Progress in 30 minutes: the replay
provider was repaired in three amend commits, reviewed and integrated as a
Store-backed replay (a70f12f, 15:14Z); the finalize conflict with a
configured finalize tool was fixed (dd3907b); the root scaffolded the
offline release-gate test module (a6ac194) and forked the vertical-gate leaf
plus an independent audit. That is the release test itself, the last step of
the slice. No delta worth a note; the root is on the critical path.

## 15:49Z wake

Alive; 8.9 GiB available; 7 windows; no latched machine; no rejections; one
nudge. Runtime fine (177 calls, wait p50 0, max 2.9 s; slowest compile
8.4 s). No master commits in 30 minutes. The vertical release gate has a
leaf, an audit, a review and a separate "fallback review" (two reviewers on
one candidate), and the root is 25 minutes into one turn running cargo
tests, past its own 15-minute rule, so the gate's replies wait unseen in its
queue. Costliest delta: the root's long turn. Sent one note into the root's
pane (lands after its next tool call): end the turn; keep one reviewer.

## 16:19Z wake: milestone done, interview sent

The note landed: the root ended its long turn, merged the reviewed offline
release test (703005f, 15:49Z) and wrote a stop handoff (83624df). The
adapter-readiness harness-side slice is integrated, 1h12m after launch
(finalize 14:39Z, Engine seam 14:46Z, Store-backed replay 15:14Z, release
test 15:49Z). All children have retired; only the root remains. Sent the
wave-8 interview (fastest and why, waits, the double review on the gate,
rules that helped or hurt, what the exomonad-side adapter needs from the
harness, next milestone, open).
