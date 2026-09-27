# Waiting and cancellation contract

`LocalActorRef::call` first admits one message to the actor mailbox, then waits
on a oneshot for its result. Dropping that waiter ends the caller's wait; it
does not withdraw work already admitted. If the actor later produces a
`MailboxValue` after its receiver was dropped, the value is dropped and its
custody is released.

`drain` closes mailbox admission under the same lock used to enqueue calls and
casts, then places a fence after already accepted messages. Work admitted
before the fence is handled before the actor records its terminal result;
later work is refused as `MailboxClosed`. Actor shutdown or retirement is a
separate operation: deferred work is released when the actor actually stops,
and observers read the actor's retained terminal and cleanup outcome. Losing a
waiter does not establish that the child, request, or process was cancelled.

For hosted Haskell work, native input delivery waits while the actor is running
a computing cell. The supported child-question flow is to admit children and
register their watch in a cell that returns, then let the child receive its
input and use later actor turns to inspect progress or settled responses. A
synchronous cell that waits for a child question prevents that same actor from
handling the input that would answer it. `awaitResponse` describes an
`Await`; `watch` registers it and returns a `Watch`; `pollWatch` observes its
state. Status or a wake does not itself prove successful child work.

The actor tests `local_actor::tests::drain_fence_finishes_accepted_calls_before_stopping`,
`local_actor::tests::dropping_a_mailbox_waiter_does_not_cancel_accepted_work`,
`local_actor::tests::shutdown_releases_mailbox_custody_deferred_behind_external_work`,
and `local_actor::tests::startup_admission_cancellation_is_retained_before_barrier`
cover admission, waiter loss, actor retirement, and cancelled startup. Existing
facade source coverage is in
`disconnected_foreground_caller_retries_the_same_handoff_without_reexecution`,
`cancelled_command_result_projects_and_later_cells_still_run`, and
`tracked_delivery_waits_for_a_computing_cell_to_end`. The recipe test
`attention_actor_recipe_retains_independent_sources_through_closure` uses
`checks/attention-sources-question.hs` and `checks/progress-route-questions.hs`
to exercise child turns and later parent calls across source updates and
closure.
