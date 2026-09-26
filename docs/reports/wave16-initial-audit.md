# Wave16 initial supervisor audit

Run `e831016d-6121-4d7f-80b1-18cfd7f8b53d`, 2026-09-26.
Launch checkout: `/home/inanna/dev/exomonad-harness-runs/wave16`.
This audit reads the retained handoff/interviews and host/provider traces;
it does not independently rerun product acceptance or establish resource release.

## Product and retained feedback

Root reports completion at product revision `9123ffba6af1171f072b48c9e95db94786eabc67`,
handoff `21cb20e`. The deterministic before-request Inject slice has component
reviews and final-source serde/browser checks, each 1/1. The exact evidence
paths and earlier-source checks are in the launch checkout's
`docs/wave16-handoff.md`. Root and worker interviews are retained in
`docs/interviews.md`. No successor is launched by this audit.

## ReviewFlow activation: narrowed, not diagnosed

The handoff says the trial reviewer never became observable. Host evidence
narrows this further for actor `7@1`:

- 12:15:40.153 UTC: agent spec preparation succeeds.
- 12:15:40.187: resource admission granted.
- 12:15:40.676: interactive application launched, pane `%579`.
- 12:15:41.788: provider transcript session metadata created, thread
  `01a0dda4-8331-79d0-aad4-d9836eb9a322`.
- 12:15:42.587: resident actor enters interactive state for request 6.
- Durable actor inbox contains sequence 1, `sessionReady`, with ReviewRequest
  and candidate `7dd904f8279a17b0189817137e3465e7cb84ac56`.
- The retained provider transcript contains only session metadata: no first
  turn or user message. Inbox presence is not proof of delivery acknowledgment.
- 12:23:01.767: cleanup reports process/pane/delivery completed, but hosted
  work cleanup, socket, build resource and worktree binding retained.

Investigate activation delivery and provider readiness before altering the
ReviewFlow decision logic. Compare the successful ordinary reviewer's launch
and delivery sequence. Determine why the pending input never produced a first
turn, and separately whether retained hosted work ever settled. A binding file
or terminal actor state alone is insufficient release evidence.

Evidence: launch checkout `.exomonad/logs/<run>.log`, actor run directory
`~/.cache/tidepool/exomonad/runs/<run>/7-1/inbox.jsonl`, and private provider
transcript `~/.codex/sessions/2026/09/26/rollout-2026-09-26T05-15-41-01a0dda4-8331-79d0-aad4-d9836eb9a322.jsonl`.

## Helper efficacy

The Provider owner used Wave16Gate. An ambiguous effect type cost a rejected
cell; an explicit effect annotation recovered. The check executed 1/1, but
the helper expected the scaffold OID rather than the candidate OID and returned
unknown source assurance. Preserve that refusal. Bind future check specs to
the candidate supplied at invocation, and distinguish source mismatch from
dirty paths in the report. Do not silently weaken clean-source policy.

ReviewFlow produced no review verdict; fallback review completed the component.
Neither exposure nor successful offline recipe checks establish live savings.

## Command and timing baseline

Parsing completed `call timing` records yields 139 bash, 17 write_stdin,
81 cell, 26 status, 16 lookup, 7 read_output and 2 reload_helpers calls.
These are tool-call counts, not model-round counts. Nineteen records exceed
10 seconds. The largest is root cell ending 12:31:23.977: 63,095 ms total,
62,244 ms compilation across seven compiles, zero checkout wait. Several
other long calls spend most of their time in command execution.

Next audit should pair each write_stdin with its originating command and
explicit observation settings. Count avoidable follow-ups only after that
pairing; waiting for a real build is not itself a defect. Inspect the long
root cell's source before proposing compiler changes.

## Concrete next improvements to evaluate

1. Repair the demonstrated first-turn activation gap with a regression covering
   an actor-launched reviewer, once the failing boundary is identified.
2. Make the context-specific test helper accept candidate identity explicitly;
   expose a concrete callable effect type in its seed/example.
3. Compose child-local pinned web preparation with browser checks. Retain
   prerequisite failures separately from expected-red product assertions.
4. Supply canonical focused target syntax and run formatting before recording
   the candidate. Both caused avoidable source/check rounds here.
5. Investigate repeated exit 137 before choosing job/memory defaults. The
   interviews report recovery with fewer Cargo workers; exit 137 alone does
   not prove OOM.

No production code changed during this audit; no resources retired or shared
daemons restarted. Preserve the failed-reviewer evidence until diagnosis.

## Follow-up: cleanup compilation and retained resources

The 63,095 ms cell bound two cleanup receipts and displayed their tuple.
Request `4175a1bfbab08989` alone took 57,219 ms at the client (57,119 ms
compiler total). The compiler had just rotated at 12:30:24.831 after 329
requests: RSS 7,194 MiB exceeded the configured 7,168 MiB ceiling. The next
request was marked `followed_rotation=true`, `served=0`. Its principal phases
were GHC load 12,152 ms, lowering 23,845 ms and module interfaces 16,540 ms.
These are cold reconstruction costs after rotation, not evidence of seven
identical compilations. No cache or rotation-policy change follows from this
single incident. No shared daemon was restarted for this investigation.

The cancelled reviewer's supervisor checkpoint still says `process_stopped`,
no pending operation and no error. Its host-tools/input socket paths remain.
This confirms process stop only; it does not supersede the host's retained
ToolService/BuildResource/WorktreeBinding receipt. Resources were preserved
for the lifecycle follow-up rather than manually deleting them.

The full follow-up root interview is retained in `wave16-helper-interview.md`.
