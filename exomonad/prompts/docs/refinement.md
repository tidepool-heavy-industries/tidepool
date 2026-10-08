A follow-up request is a new activation on the retained agent. Its typed input
should carry the accepted candidate, corrections, and evidence needed for the
revision. Settlement of one request does not settle another request or retire
the target actor. A captured context is a snapshot; later findings and accepted
decisions reach the next request only when explicitly supplied.

For a reviewer and implementer, keep the implementer's request open while it
needs a decision, and have the reviewer retain its own request while independent
checks or repairs proceed. Avoid a circular wait: a request queued to an actor
that is waiting on that very request cannot unblock it. Use progress for
nonterminal findings and the final typed reply for the authored conclusion.

The reviewer checks an exact candidate and reports its source identity, owned
scope, acceptance evidence, and remaining uncertainty. The integration owner
checks the resulting revision after incorporation. A report being delivered does
not prove it was read or integrated. For Git delivery, load
`exomonad-project-work` when its review and integration method fits the task;
load `exomonad-review` when its exact-candidate review operations fit.
