# Wave 5 observations (run c22fa217)

Launched 2026-09-25T02:58Z from main 1884c03c8, co-resident, workspace
a5bbb3b; record in plans/wave5-launch-record.md. Observer checklist:
~/dev/exomonad-harness/docs/observer.md.

- 03:00Z: root read NEXT.md and committed its first named step (a4f3cb9,
  format the correction-wave tests).
- 03:15Z wake: host alive, 12 GiB available, 8 actors, no Failed/latched
  lines, no harness commits since b99337e (03:05). STALLED since 03:05:
  every actor idle, host log silent. Cause: core-lead (2@1) inbox fenced at
  cursor 1 behind row 2 (root's "Plan accepted", steered mid-turn at ~03:00)
  in phase Unconfirmed; the one provider query at 03:01:24 returned input
  state Unknown and nothing retries it. Rows 3-7 (both children's replies
  a1f976f and the c-contract review, three root messages) are never
  submitted. Same defect class as wave 4. Timings before the stall: cell
  p50 3.5 s (wave 4 first 8 min: 19.1 s), checkout-wait share 9.8%.
  Intervention options put to the user; none taken yet (first stuck wake).
- 03:34Z root cause of the stall (two lanes; evidence in scratchpad
  stall-evidence.md). Root notified core-lead at 03:00:48 while core-lead
  was inside a 39 s tool call (03:00:44-03:01:23). The host's submit waited
  and timed out at 35 s ("operation exceeded 35s", log line 3558); Codex's
  start_or_steer_turn failed and marked the host-input row Unknown
  (vendor/codex ext/queue service.rs:580-613). An Unknown row can never be
  claimed again (claim requires state 'ready'), so it is Unknown forever.
  Tidepool's pump re-queries every 1 s forever, gets Unknown, dedups the
  identical WARN (actor_host.rs:6137-6149), and never evaluates later rows;
  nothing calls withdraw_input, and Unknown has no reconciliation branch.
  The model never saw the message ("Plan accepted" absent from its
  transcript). Every later message to core-lead, including the root's
  03:29 check-in, sits behind it. Only actor 2 is fenced.
