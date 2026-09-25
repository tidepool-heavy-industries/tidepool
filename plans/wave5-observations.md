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

## 03:45Z correction and unstick

The 03:15 reading above is wrong in one step: Codex never marked the row
Unknown, because the input was never admitted. `~/.codex/queue_1.sqlite`
has host-input rows for actor 3 (both presented) and none for actor 2. The
TUI's input-control handler cancels an active `haskell` call before
admitting hosted input; the host answered NotSleeping for the computing
39 s fork cell; the TUI then waited for a terminal settlement that the
normal completion path never records (`complete_from_call` has no
production caller), so the exchange hung, the host's 35 s deadline
expired, and every 1 s query since returns EvidenceUnavailable, which the
host reports as Unknown. Full chain and the host-only fix are in
next-wave-inputs.md. No cancel line appears in the host log (the cancel
route does not log), and the Codex session log shows no StartOrSteer
submission for actor 2 at any time.

State at 03:42Z: every pane idle; the whole run had been quiet since the
root's 03:29 check-in. Actor 2's two Lunas had both finished (b: a1f976f
committed; c-contract review returned) and their results sat fenced in
actor 2's inbox with five root messages. Unstick: pasted the seven fenced
payloads into actor 2's composer as an operator note telling it not to
wait for or fork further children and to reply to root through its
intact outbound path. Actor 2 resumed (tests, cells) within seconds and
root was working again by 03:47Z.

## 03:49Z wake

Alive (tmux, host process), 16 GiB free, 7 windows, no Failed/Retired, no
latched machine. Root active (bash calls, working). Four new master
commits since the unstick: 1cc372a (core inbox-fence report and
checkpoint), ffe20e5 and d8097c3 (async-schema slice integrated), 8d45d32
(open live gate recorded). 28 calls since 03:45: checkout_wait p50 0 ms,
max 216 ms. No intervention.

## 03:56Z root recommends stand-down

Core lead replied Blocked (notification 7 to root): b integrated at
d8097c3, findings candidate 1513e95 merged by root at 06148a1, c/d
unimplemented because the settings work needs a harness-authored
provenance/initial-pin seam across Item, Store, Engine, verbs and fork
runtime, and no further children were allowed. Root then answered the
operator's question: stand down from implementation, shift to the
retrospective/UX interview, master is clean and recoverable, next run
starts a fresh routable lead from the documented master state after the
fence is diagnosed. New friction: the watchdog classified core's
sendMessage checkpoint as destructive_command (0.9) from quoted text;
root recorded it in docs/exomonad-friction.md at 0553f8c. Fix lanes
running off main: delivery-fence, agent-ux, observability.

## 04:15Z wake

Alive, 17 GiB free, 7 windows, no Failed/Retired, no latched machine. Root
idle since 04:02Z after committing its round-two design sketches
(95daec8). Since then the harness master gained 48b738f, "steering after
run 2: answer Q4/Q5, unblock (c) provenance, rescope probe, rewrite
NEXT.md", the operator's brief for the next run. No actor activity since
04:02Z; 20 calls since 03:49 with checkout_wait p50 0 ms, max 216 ms. The
run is in stand-down; no intervention.

## 04:47Z wake (intervened: memory)

Run alive, root idle since 04:02Z, 7 windows, no Failed/Retired, no
latched machine, no actor calls since 04:15Z, no new master commits.
Free memory was 2 GiB with swap 38 of 39 GiB used. Cause: the gate run I
had stopped at 04:40Z left two cargo-nextest runners and 19 exomonad-actor
test binaries alive (re-parented after the shell died); each test had
spawned its own extractor endpoint, 27 endpoints with 31 GHC workers at
1 to 2 GiB each, on top of the redeploy's Nix build. Killed the runners,
test binaries, endpoints and every worker not owned by the two live
daemons (the other run's a8f054b6 daemon and the shared one). After:
23 GiB free, swap 17 GiB, 3 workers. The wave's host and the redeploy were
untouched; the redeploy is still building the extractor package.
