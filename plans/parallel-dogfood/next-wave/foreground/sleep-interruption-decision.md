# Sleep interruption and transport decision

Source baseline: `1c6f8816320e987cd61b031a90718662674c890f`.

## User-visible contract

A sleep remains one logically pending Haskell invocation. Losing observation of
its transport does not cancel or complete the evaluation and never permits
replay.

Any new message actually delivered to the sleeping LLM through the normal human
or actor steering/notification path cancels the whole suspended evaluation.
This includes delivered progress notifications. A queued request that has not
activated, or data retained only in a Haskell collector or mailbox, does not
interrupt the evaluation. Message content is irrelevant.

Haskell actor handlers retain sequential mailbox semantics while sleeping.
There is no retained-program-across-inference behavior.

## Ordering and ownership

The native path queues an incoming message, obtains the exact evaluation's
cancellation or terminal outcome, settles the original tool invocation, and
only then lets inference observe the message.

Cancellation preserves effects that already completed and independently owned
jobs. It does not synthesize local binding recovery. If expiry wins the race,
already-executed suffix effects are reported honestly. Once cancellation is
acknowledged, no further suffix may execute.

Implementation extends the existing invocation, continuation, lifetime, and
native completion owners. Inspection found that `ResidentToolEndpoint` lacks the
required exact-evaluation cancellation-and-settlement join; dropping an HTTP
future or a native pending response is not that join.

The selected native baseline
`80e36633f515b03e11189e8516be21065e73335e` has no explicit whole-call deadline
in the inspected host-tools path. Acceptance must prove the complete outer path,
including one real fifteen-minute wait.

## Required checks

- delivered human steering, actor notification, and progress notification each
  cancel the exact sleeping evaluation and order settlement before inference;
- queued-but-inactive requests and collector/mailbox-only data do not interrupt;
- transport observation loss neither resumes nor replays the evaluation;
- cancellation/expiry races produce one terminal outcome and never run a suffix
  after acknowledged cancellation;
- completed effects and independent jobs survive cancellation without invented
  bindings;
- sleeping handlers remain sequential while sibling actors progress;
- the matched native/Tidepool pair completes one real fifteen-minute sleep with
  no intermediate inference or tool result.
