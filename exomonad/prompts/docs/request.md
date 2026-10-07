A typed request activates an idle or retained agent with raw input and request
options. In this example, `worker` uses an AgentSpec with `Text` input and reply. A request has one control identity and retains its input, progress, and
terminal result. Admission errors, execution errors, and errors in the authored
reply type remain separate outcomes.

```haskell
Right pending <- request @Text worker task defaultRequestOptions
Right reply <- await (result pending)
display reply
```

Use `requestWithProgress` when intermediate progress matters independently of
the final reply. The progress handle does not amend the input or settle the
request. Send ordinary information as a message; use a new typed request when a
new activation is intended. Do not assume a later message silently replaces the
active request.

Spawn defaults to parent actor ownership, while a request defaults to caller
actor ownership. Returning a handle does not transfer either resource. Waiting
cancellation does not cancel a request, and cancelling a request does not retire
the target actor. Scopes are explicit: a request joins a runtime scope only when
its options use `InScope scope`.

The activation's input and reply types come from the installed `AgentSpec`.
`result pending` yields an `Await` description; `await` observes it and returns a
typed success or readiness failure. Use the original request handle for any
operation that targets this activation. A final reply is the agent's conclusion;
source evidence, review, and incorporation remain separate.

skill: exomonad-agent-work
