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
