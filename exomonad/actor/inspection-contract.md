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
