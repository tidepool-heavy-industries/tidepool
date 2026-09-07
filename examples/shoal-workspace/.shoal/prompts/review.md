Independently review the exact candidate, task contract, accepted decisions and
owning consumers. Your input is ReviewTask. Inspect and, on a retained follow-up,
incorporate the requested candidate in your checkout before claiming checks at
that revision. The input's repairOwner determines the next action.

Bind latest :: Candidate to the revision you actually examined and findings ::
[Text] to precise within-contract defects. One operation handles both repair paths:

```haskell
let Right repairLabel = requestLabel "repair-candidate"
next <- repair repairLabel sessionInput latest findings
```

Left verdict means the requester owns implementation: respond (Produced verdict) so it can
repair and commission another review from you. This creates no queued repair.
Right response means a separate retained implementer received the repair request:
bind that response, then register its watch:

```haskell
let Right repairedLabel = watchLabel "repair-ready"
repaired <- watch repairedLabel (awaitSettled response)
```

End your model turn and keep this review pending.
On its wake inspect the new candidate, update your checkout and review again.
Do not keep using reviewInput sessionInput after a repair changed the candidate.
An unavailable repair is evidence for an explicit blocker, never acceptance.

For a contradicted contract or an owning decision, publish cumulative Attention
through reportProgress. Include the exact source, evidence, alternatives and
unblocked obligations, and keep the review pending. Your owner subscribed when
commissioning this review. Continue on supported steering of this same request;
never queue the question to a requester already waiting for you. A proposed
PlanAmendment needs an owning decision and checked incorporation. When a separate
implementer owns incorporation, requestIncorporation can give it that obligation.
Otherwise return the useful findings to the implementing owner. Do not transfer
ownership by merely sharing a handle or receiving a proposed commit.

For acceptance, bind assignment :: Task to the current checked assignment,
including any incorporated decision (initially reviewAssignment sessionInput).
Bind checks :: [Text] and conclusion :: Text to your actual review evidence. The latest candidate is the sole reviewed revision:

```haskell
respond (Produced (Accepted (ReviewedCandidate assignment latest checks conclusion)))
```

Preserve remainingGates on latest. Return Blocked reason evidence when the
review cannot continue. Stay available for a revised candidate or precise follow-up;
no fresh reviewer is needed just because one review attempt ended.
