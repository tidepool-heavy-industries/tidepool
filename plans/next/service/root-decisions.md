# Root decisions on service checkpoint

Reviewed source-only checkpoint 6ffd4ee819ac701d3d80f72d4a5e34e06d3f274f;
not accepted as service implementation. Custody repair remains the immediate
priority and can deliver independently of the external native dependency.

Approve reuse of native RemoteAppServerClient at the human-delivered matching
revision, including its currently large app-server/core/exec-server dependency
closure. Root directly inspected the pinned client manifest. Avoiding a second
protocol router is worth the build/dependency cost for this scoped wave; this
is not a claim the closure is cheap. No external packaging work is required as
a prerequisite. Root owns manifest/pin integration after actual delivery; verify
API, tool forwarding, controller ownership and build compatibility then. Keep
backend-specific dependencies confined to the existing backend owner. Escalate
unexpected runtime coupling, not merely the known compilation footprint.

Approve the proposed distinct Notifications effect, notify, opaque inbox-backed
receipt and pollNotification. Keep admission failure separate from observation
of accepted/presented/unconfirmed delivery. No duplicate registry or scheduler.
Active typed requests retain their bindings even when provider sampling is idle;
idle actors without an assignment have no respond binding. Notification delivery
never settles or fences an unrelated typed response. Exact incarnation authority,
no automatic retry after uncertainty, and durable completed-call boundaries remain
required. Service must scaffold and verify actual owning consumers before claiming
this API works.

Coordination UX finding: first-wave assignments lacked a root handle or watched
progress channel, forcing a terminal checkpoint for nonterminal findings. Continued
service work gets watched Text progress for cumulative findings and decisions.
Progress does not solve the separately broken active-update return route; genuine
blocking decisions may still require a terminal checkpoint until that is repaired.
Do not invent an AgentRef from roster IDs or imply a supplied handle grants authority.

## Approved resident admission / cleanup contract

Root reviewed checkpoint d1f1f52b (service898e7b81) and directly inspected current
local_actor::finish_actor/shutdown_children, resident_actor::shutdown, and
resident_workbench::with_host_machine. Approve bounded implementation under the
existing service lead, with actor/runtime ownership and fresh independent review.
This is contract approval, not acceptance of code or permission to deploy.

Ownership: service lead assigns one actor-lifecycle owner for the mailbox barrier,
retained exit evidence, resident shutdown and changed endpoint projection. The
retained host-tools lead owns consuming it with HTTP lifecycle; custody lead owns
combining exact evidence in existing addressable deployment storage. Coordinate
shared actor_host regions before edits. Extend existing runtime checkout/realm
owners only where required; no replacement scheduler, registry, counter or issuer.

Approved semantics and acceptance constraints:
- Exact-incarnation, privately constructed HostedWorkSeal from an acknowledged
  actor mailbox barrier. Irreversibly reject later Tool/Workbench at the owning
  handler, including previously HTTP-admitted late dispatch and client clones.
  Unsupported endpoints return explicit unavailable evidence, not successful
  defaults. Seal is an admission/order fact, not external-effect quiescence.
- Keep correlated completion possible during sealing. Before confirming cleanup,
  close its release-vs-abort boundary explicitly and include children released by
  completion in shutdown evidence; no child snapshot taken before that boundary
  can prove all children stopped. Reject reattachment/new sessions that reopen
  ownership. Repeated seal observation must not reopen admission or create a
  second retirement operation.
- Extend existing retained exit state with typed component cleanup outcomes.
  ActorTerminal alone remains insufficient. Confirmed requires positive evidence
  for every required actor/machine/child component. Timeout, forced kill, panic,
  unavailable child evidence and aborted unpublished groups cannot imply success.
  Never reinterpret old terminal-only evidence as confirmed cleanup.
- If a hook returns failure, preserve it and attempt remaining safe cleanup through
  owning checkout/realm paths rather than short-circuiting all cleanup. Do not
  destroy a realm while independently running work may still own it. Accumulate
  typed component failures; uncertain execution remains unconfirmed.
- Preserve existing linear checkout settlement under lost waiters. Use retained
  lifecycle evidence to observe outcomes, not cancellation/retry of uncertain
  retirement. Actor/machine confirmation excludes arbitrary external handler,
  process, socket, HTTP and build-resource cleanup, which remain separate gates.

Before acceptance, execute actual authored Haskell endpoint tests with a controlled
owning handler: active work plus queued late dispatch; lost first waiter; barrier
ordering and irreversible rejection; correlated completion release versus abort;
hook failure; child timeout/force; checkout settlement despite waiter loss; exact
actor identity/no sibling proof substitution. Compile changed consumers, retain
exact revisions and independent review. Unit-only HTTP mocks or GHC-free kernel
checks do not establish the complete authored contract. If a necessary component
cannot yet be confirmed, return typed uncertainty and leave settlement disabled.

Root consumer baseline9d64a066 (evidence descendants through main) must also be
incorporated, with resulting head/checks reported: BindingTable now denies both
mutations and authority reads after uncertain persistence. Previously recorded
root integration checks are complete and passed, not still running.

This committed decision is available through shared Git. It is not proof of
service receipt/incorporation. The live active-update transport is known broken;
root has not retried it or queued a hidden substitute behind the active request.
Service should acknowledge the contract and intended ownership in its existing
progress channel after inspection, escalating any incompatible design change.
