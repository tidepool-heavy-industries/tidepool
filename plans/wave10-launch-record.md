# Wave 10 launch / RSI iteration 2

Status: running. Root task_started verified at 2026-09-25T18:39:40.923Z.
Brief: "Read NEXT.md and follow it for wave 10."
Root thread: 01a0d9dc-d54a-75a3-9259-5ee6b10b2c14; terminal wave10:3.1.

- Run: bde50ec9-f17b-4c29-ba28-3ad1bf3485b5; tmux `wave10`.
- Harness launch source: 076684b7ad5d858a0fc119663d2c4ec1b1ada251 (clean).
- Workspace pin: 730747816073b27843d0fbd29aec830fc3eff4b1.
- Tidepool launch source: 1c499a4f1c02ae1aca01fdd7d9eeec9e49947c7b;
  subsequent 7e604656a changes RSI documentation only. User-owned
  plans/harness-adoption.md remains modified and untouched.
- Local Exomonad binary SHA256:
  e8c70d0d851b2174e2368c631f51a8863699133229e024ce3ad6bbc62ab6b01e.
- Command: just exomonad-init --workspace /home/inanna/dev/exomonad-harness
  --session wave10 --no-attach. Matched local incremental build.
- Root: Sol Medium. Compiler: one worker, 7168 MiB rotation threshold.
- Core catalog: 38. Project prompt revisions belong to harness launch source.
- Product: typed follow-up/finalization lifecycle through service, Engine, driver,
  and durable Store. Shared contract before dependent implementation forks.
- Execution: bounded implementers return Outcome Candidate; root owns review and
  integration. Unified ReviewRequest/ReviewBasis, ordinary event-driven review.
- Automatic-review wrapper remains blocked and is not exercised.
- Checks before launch: reviewProvenance 6/6 in Tidepool and harness; pin test
  1/1; template equality; diff checks; workspace/Codex/compiler launch preflight.
- No new credentialed/live adapter authorization; coding actors authorized.
- Observer: external supervisor with read-only native subagent milestone monitor.
- Host log: harness/.exomonad/logs/bde50ec9-f17b-4c29-ba28-3ad1bf3485b5.log.
- Launch log: /tmp/rsi-wave10-launch-final.log.

## Failed first attempt

b48bf7e2-a7c5-4eef-875a-19b3f5eddc6b never bound a native root. It hit completed
wave 9's binding lock, then a lifecycle-journal recovery error. CLI stop and
recreate refused an already-absent host unit. After checking inactive/not-found
unit and sole compiler pane, supervisor terminated that exact compiler; its tmux
session closed. All run evidence is preserved. See rsi-iteration-2.md for cards.
Wave 9 was deliberately stopped through CLI before the fresh successful host.
