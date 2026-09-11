# Sleep owner

## Settled interruption UX

Human delegated this choice to the initial planner after source inspection.
Sleep leaves one logical tool invocation pending; ending a transport observation
does not cancel the evaluation, complete the invocation, or authorize re-execution.
Use retained exact invocation identity for transport reconciliation.

Any new message delivered to the sleeping LLM through its normal human/actor
steering or notification path interrupts the whole suspended evaluation.
Do not classify message prose as important/unimportant: even a delivered progress
notification interrupts. Data retained only in Haskell collectors, actor events,
and new requests queued behind an active request do not interrupt merely by
arriving. Haskell-only handlers retain sequential mailbox semantics.

Queue the message, obtain the exact evaluation's terminal cancellation outcome,
settle the interrupted tool invocation, then permit inference to handle the
message. Abort the continuation rather than returning unit from sleep. Completed
effects and independently owned jobs survive according to their existing owners;
only actually committed/installed workbench bindings are recoverable, not arbitrary
locals in an unfinished do block. Never resume abandoned code after steering.

Cancellation and expiry need one owning race decision. If expiry wins and suffix
effects already execute, report them honestly; acknowledged cancellation prevents
further suffix execution, not effects retroactively. If cancellation cannot be
confirmed, report uncertainty rather than claiming quiescence or admitting
conflicting workbench execution. No retained-program-across-inference mechanism.

Inspected launch-main `ResidentToolEndpoint` has dispatch/completion/reattachment/
seal but no explicit exact-evaluation cancellation operation. The native-to-resident
cancel-and-settle join therefore needs implementation ownership and consumer proof.
Reuse resident continuation abort and native correlated completion owners;
dropping an HTTP future or native response waiter is not cancellation proof.
The inspected selected-native host-tools call has no explicit whole-call deadline;
this is not proof of the entire outer fifteen-minute transport path.

Start on launch main plus planning commits, never R7 engine source.
Read the PRD, nearest contributor guidance, and actual suspension/lifetime and
tool-wait consumers. Keep verified observations separate from implementation proposals.

Lead retains API choice against existing duration vocabulary, timer ownership,
exact-once continuation/cancellation integration, and actor-handler semantics.
Deliver a minimal usable typed effect and Rust dispatch seam plus shared fixture
contract before related implementation forks. Reject invalid/overflow duration
before scheduling, monotonic not-before completion; no command reservation,
new scheduler/registry or redundant wake service.

Useful ready frontier after that shared context and a completed owning warm build:

* Timer/lifetime checks owner: controlled fifteen-minute time, zero/negative/
  overflow, cancellation versus expiry, retirement, sibling progress and handler
  sequentiality at actual owner. May implement isolated test hooks, not another
  timer implementation. Lead continues production lifetime wiring.
* Native-wait integration owner: actual scripted-provider TUI fixture, complete
  long-tool-wait path, normal interruption and post-interrupt usability. Coordinate
  shared backend/native edits through the applications owner. After a working
  paired seam, this child can split the long real-wait fixture from interrupt/
  failure coverage while retaining actual transport integration.

If minimal production timer work is independently separable from the transport,
lead may delegate its implementation rather than tests alone, after signatures
and cancellation ownership agree. Do not delay useful forks for a broad battery.

Join unlocks exact public sleep example + observable suffix in real Codex.
Lead proves same primitive works in Haskell handlers and resident blocks, adds
exact signature/import/example to shipped guidance, and reviews resulting source.
Coordinator owns the final combined-pair rerun and fifteen-minute smoke scheduling;
lead supplies runnable fixture and no-inference/completion-count assertions.
