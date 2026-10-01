# Waiting and cancellation contract

`LocalActorRef::call` first admits one message to the actor mailbox, then waits
on a oneshot for its result. Dropping that mailbox waiter does not withdraw
already admitted work. If the actor later produces a `MailboxValue` after its
receiver was dropped, the value is dropped and its custody is released. This
mailbox rule does not override hosted-invocation ownership: scope exit cancels
unfinished owned commands, provider workers and requests while retaining cleanup.
Borrowed waiters cannot cancel another owner's resources; explicit detachment
transfers an owned resource to actor lifetime.

`drain` closes mailbox admission under the same lock used to enqueue calls and
casts, then places a fence after already accepted messages. Work admitted
before the fence is handled before the actor records its terminal result;
later work is refused as `MailboxClosed`. Actor shutdown or retirement is a
separate operation: deferred work is released when the actor actually stops,
and observers read the actor's retained terminal and cleanup outcome. Losing a
waiter does not establish that the child, request, or process was cancelled.

Ordinary hosted Haskell `Cmd.run` and `Cmd.await` suspend until command terminal
completion and resume the same continuation. `waitFor` suspends directly on an
applicative `Await`, returning typed `WatchFailure`; a named `watch` remains an
inspectable subscription. Bounded command observation returns live status normally
and never detaches. Status or a wake does not prove successful work or cleanup.

Immediate `unfold` publishes from captured or selected context and permits waiting
within the same invocation. Invocation-owned children must settle before return
or be cancelled by scope cleanup. For child questions needing another model turn,
use explicit actor-owned work, register its watch or collector and return. Deferred
unfold requires persistent lifetime and publication after the actual enclosing
result; awaiting its children before returning prevents that publication.

At the native input-control boundary, delivery waits while the same actor is
running a computing cell. A computation needing that input must return control
before it can be presented. Record-actor services created with `R.start` are
persistent; their handlers remain serialized while suspended. A handler must not
wait for an event requiring another handler on its own mailbox to run. Handlers
without a hosted invocation use actor ownership for their work.

The actor tests `local_actor::tests::drain_fence_finishes_accepted_calls_before_stopping`,
`local_actor::tests::dropping_a_mailbox_waiter_does_not_cancel_accepted_work`,
`local_actor::tests::shutdown_releases_mailbox_custody_deferred_behind_external_work`,
and `local_actor::tests::startup_admission_cancellation_is_retained_before_barrier`
cover admission, waiter loss, actor retirement, and cancelled startup. Existing
facade source coverage is in
`disconnected_command_wait_retries_the_same_invocation_without_reexecution`,
`command_wait_preserves_the_exact_continuation_until_terminal_completion`,
`cancelled_command_result_projects_and_later_cells_still_run`, and
`tracked_delivery_waits_for_a_computing_cell_to_end`. The recipe test
`attention_actor_recipe_retains_independent_sources_through_closure` uses
`checks/attention-sources-question.hs` and `checks/progress-route-questions.hs`
to exercise child turns and later parent calls across source updates and
closure.
