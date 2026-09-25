# Wave 8 launch record (2026-09-25 14:37Z)

| Item | Value |
|---|---|
| Run id | fa78a7c7-98f0-4067-bd3c-bf5f6ae2eeca |
| tmux session | wave8 (root in window 3) |
| Deployed build | tidepool main a30997ff9, redeployed 14:35Z |
| Workspace pin | 7a3b11b (preflight ok) |
| Harness master | 9ae7ed0 plus the previous root's uncommitted NEXT.md edits |
| Brief | "Read NEXT.md in the repository root and follow it." plus an operator handoff note (unmerged finalize 6d78cc5 accepted, merge first; replay 692adf4 to re-review) |
| Log | .exomonad/logs/fa78a7c7-98f0-4067-bd3c-bf5f6ae2eeca.log |
| Observer cron | 678ea03f at :13/:43 → plans/wave8-observations.md |

Wave 7 (26122845) was stopped at 14:28Z after my 13:56Z redeploy deleted the
stdlib cache it compiled against (redeploy.sh Step 5, now removed:
5f9b3bb66). A first wave 8 attempt (657019a1) failed at launch because the
bash tool description exceeded the 1024-character provider limit (fixed
a30997ff9; a test now guards it, desc-limit merge).

New in this build versus wave 7: session memo entries survive transactions
without an incarnation (root binds were 45 to 100 s of compile); split
staleness compares only what a scope can reach (about 160 under-checkout
fallbacks per hour); background command jobs; retirement drops in-flight
pump work; worktree tests reuse the persistent daemon; authored Show
suppresses the generated Display.

Milestone: adapter readiness, harness-side vertical slice. Release test: a
long cell survives three envelopes at successive request boundaries, yields
one strict final result, and survives a restart query.
