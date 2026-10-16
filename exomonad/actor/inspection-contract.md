# Actor inspection contract

Status views (`status_for` and `watches_overview`) report retained request and
watch state without polling, releasing, or changing it. A settled value still
requires `pollResponse` or `pollWatch` to retrieve it; a status view does not
expose values. Request inspection distinguishes pending, ready, unavailable,
and stale/released handles. A watch can settle as ready while reporting failed
dependencies, or become unavailable when a required dependency was released.

Forest graph nodes carry one optional lifecycle snapshot from `RetainedActorExit`.
`Live` does not establish application readiness; `Paused` retains its cause;
`Exited` retains the terminal outcome. If neither the lifecycle owner nor a
recorded exit is available, the lifecycle is unknown. The graph's `terminal()`
accessor derives from that same snapshot, without waiting for an actor turn.

`observe_watch` has one deliberate acknowledgment effect: after returning a
settled ready or unavailable state to its owner, it records that the owner has
seen the transition so a queued duplicate notice can be suppressed. Foreign
actors may read a shared watch, but that read cannot acknowledge the owner's
notice. Status and overview reads never change this acknowledgment.

Command jobs retain their identity, starting source, live status, terminal outcome
and cleanup independently of presentation or waiter lifetime. Any observer can
await terminal completion and read retained cleanup; only the exact owner can
probe backend cleanup or control the command. Stream output is
paged from the bytes still retained by the job. `observe` and its completion-notifying
variants do not detach; invocation cleanup is an ownership operation, not a transfer
or revocation of borrowed observation authority. Pages identify gaps,
evicted bytes, lossy decoding, and partial boundaries; readers must preserve
those omissions when describing output. There is no separate history store,
and a released request or forgotten watch is stale rather than an implicit
copy of its former result.

Exact operation observation joins existing native, scheduler and Store evidence;
it does not issue a new job or result record. Native `status` with `view=operation`
uses the caller's actual conversation. Authenticated GUI inspection uses the
same reader, with its requested current HostIdentity checked by Store. Both
select the full original origin, request and call identity. A reused call ID,
matching actor path or stale incarnation cannot substitute for that identity.

Execution, native finalization, durable settlement and delivery remain separate.
Delivery means membership in the observing conversation's retained input
history, never model attention or acceptance. Absence from the current history
is not proof that delivery never occurred or remains queued. Store facts share one transaction;
live scheduler and native observations are independently identified samples.
Unknown or unavailable owners do not imply completion. Inspection never waits
for work, acknowledges completion, cancels, consumes a watch or grants retry.
Full output retrieval remains the existing exact result-reader operation.

Without an operation selector, the same native status view discovers original
references from the existing execution journal. It prioritizes unfinalized or
unknown finalization, but keeps finalized entries because finalization can
precede durable delivery. Pages contain at most 32 references and an explicit
full-identity continuation. This is a live listing: settlement can move an entry
between priority stages, and new earlier references require starting again at
the first page. Listing does not certify execution state or receipt delivery.

`status revisions` explicitly refreshes source drift through the installed source
owner outside the notebook compiler. The sample distinguishes the inspecting
request's issued notebook generation, the active generation and observed disk
revision. Publication checks both the latest refresh token and the exact active
generation under the installation lock. A superseded or failed capture remains
unavailable with explicitly last-known evidence, never a clean current sample.
No filesystem polling or change event certifies source equality.
