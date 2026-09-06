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
