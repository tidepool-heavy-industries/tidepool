# Wave 7 launch record (2026-09-25 08:12Z)

| Item | Value |
|---|---|
| Run id | 26122845-fcbe-4431-b2e2-560776b0c154 |
| tmux session | wave7 (root in window 3) |
| Deployed binary revision | tidepool main 3ab8b13f8 + 4dcb4d7d2, redeployed 08:08Z |
| Workspace pin | 7a3b11b82db5c321be5c45c8ca43fe39c94584fa (checkout matches; launch preflight) |
| Harness master | 763764d (prompts -1/-2/-3, dogfood-requirements, workspace pin a8c5c0a) |
| Brief | "Read NEXT.md in the repository root and follow it." |
| Log path | .exomonad/logs/26122845-fcbe-4431-b2e2-560776b0c154.log |
| Run dir | ~/.cache/tidepool/exomonad/runs/26122845-fcbe-4431-b2e2-560776b0c154 |
| Observer cron | 11732ef1 at :13/:43 → plans/wave7-observations.md |

Wave 6 (02a1c2fd) stopped at 08:10Z mid-turn after about 3h15m; its work is
integrated on harness master through e4466b3 (item2 trace and live-gap
amendments).

New in this build versus wave 6: every cell unit compiles off the shared
checkout (six-unit cell holds it about 35 ms instead of about 170 s);
`parentAgent` is the supervisor for every child; activations carry the whole
Task; readable receipts, handles and whole replies up to 8 KiB; status as a
diff plus a revisions view; turn-end reminder; run-map fence labels follow the
host; launch preflight; accurate UpdatePending refusal text. Prompts: measured
lookup coverage per role 91 to 100 percent, fork recipe in NEXT.md, friction
lines in replies, root admission checkpoint. docs/dogfood-requirements.md maps
waves 5 and 6 onto harness PRD amendments.

Not in this build (merged after deploy or pending): delivery pump drops
in-flight work on retirement (4c7d46e53), worktree tests reuse the persistent
daemon (6087e2315), the throwing-Show display guard fix, solTaskFrom admitting
leads with the lead prompt (NEXT.md's recipe wraps leads meanwhile),
background jobs (in review fixes). Known: the Codex fork shows queued input
to a busy actor only at its turn end; carded as a harness mailbox amendment.

Watch: checkout_wait p50/max against wave 6 (about 6 s / 170 s); Luna first
productive action against 68 s / 10 calls; no parent-hunting lookups; at most
one UpdatePending retry; friction lines in replies.
