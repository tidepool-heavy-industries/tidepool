Request options may include a deadline. It bounds when the request may be accepted;
it does not promise that the target's processes have stopped. Use dimensional
time values rather than bare integers.

When the deadline expires, the request becomes unavailable and dependent waits
can proceed with that typed outcome. If reply acceptance won the race, retain its
terminal settlement. Cancellation is a separate operation, and the target actor
remains active until its execution and cleanup actually close. Inspect the
request and actor state before assuming the resource is safe to retire.

Use the request handle as the identity for that activation. A follow-up request
has its own identity and typed input. Its result does not terminate the actor;
retirement remains a separate supervisor decision.

skill: exomonad-agent-work
