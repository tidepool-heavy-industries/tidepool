# Native delivery and service convergence checkpoint

User delivered Codex600be9df39096121f76745f7d7f73a96bae8c82e on
controlled-execution-observer. Root verified that commit in /home/inanna/dev/codex,
read its codex-rs/app-server/README.md controlled-execution section and, with
explicit user authorization, pushed the exact commit to fork remote branch
controlled-execution-observer. No native source edits, force push or live restart.

Contract: one fresh private launcher credential per service lifetime; launch
`codex app-server --listen unix:///absolute/path/actor.sock --controller-token-file /absolute/path/controller-token`.
Initialize experimentalApi, then control/acquire on the same native WebSocket
connection. Register destination host before thread/ready. Observer uses
`codex observe THREAD_UUID --remote unix:///absolute/path/actor.sock` on an already
loaded execution. Controller loss irreversibly fences this service instance.
No replacement controller, callback replay or automatic input resubmission.
Native sessionsStopped does not prove executor-process quiescence. Reconcile
recorded versus uncertain effects before replacement. Full examples/recovery are
in that exact native README; source/client reuse remains the approved boundary.

User-attributed evidence:13/13 focused tests including real TUI/PTY and public
protocol tests; stable/experimental schemas passed. Tests preceded final lint/
format cleanup. Broad five-crate run10173 passed,485 failed,1 timed out; not all
failures established baseline-only. Complete workspace and macOS/Windows untested.
Root has not independently run native acceptance. Treat as integration candidate,
not blanket native acceptance. Mounted Shoal controller/observer canary remains
mandatory; no native evidence substitutes for it.

## Root ownership and immediate status interview

Root is assigning native flake pin/build preparation independently. Service owns
persistent native client/controller, hosted-call bridge, destination registration,
assignment/notification semantics, and mounted canary using the reviewed lifetime
owners. External dependency is no longer waiting on a human delivery. Root retains
Cargo/manifests integration authority; send exact required client dependency names
and seams if pin-only preparation cannot establish them.

User asks whether the hours-long service work is stalled or converged. Publish a
concise interview checkpoint on existing progress after reading this file:
1. Exact currently deliverable revision; what is production behavior versus scaffold.
2. Remaining critical path and actual outstanding actors/tasks; any stalled work.
3. What was spent on cleanup prerequisites versus controller/bridge implementation
   (qualitative unless actual timings exist).
4. Smallest coherent root-integrable result NOW; what must wait for native canary.
5. Plan to converge without unrelated cleanup expansion; escalate new necessary
   architectural scope rather than silently extending the tail indefinitely.
Keep existing host cleanup work active. Root source inspection at2db8f2c6 found
RemoteAppServerClient only in plans, not a production controller implementation;
correct this if a newer candidate supersedes it. Root needs an honest remaining-
work estimate in obligations, not an unsupported ETA.

Root tried the explicit user-requested active interview/amendment; request18 update4
returned UpdateNotPresented with the same initialize-proxy disconnection. This
committed checkpoint is the explicit shared-source communication route, not a queued
assignment masquerading as delivered steering. Receipt/incorporation still require
service acknowledgment. A terminal checkpoint is acceptable if necessary to regain
a reliable request/reply return route; it must retain current child obligations.
