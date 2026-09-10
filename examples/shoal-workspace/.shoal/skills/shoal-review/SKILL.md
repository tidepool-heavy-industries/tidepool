---
name: shoal-review
description: Commission independent review and repair of exact Shoal candidates using Project.Work, and return typed review decisions without copying full task histories.
---

Use review when independent judgment helps the owning integration decision.
Given the current `sessionInput :: Task` and your committed `candidate :: Candidate`:

```haskell
(reviewer, reviewProgress) <- reviewCandidate sessionInput OwnerRepairs candidate
```

This returns a retained reviewer plus progress. `OwnerRepairs` means you repair
findings; it avoids queuing a repair behind your own pending delivery. Follow the
reviewer with the ordinary routing skill, using `reviewSummary` for its result.

Inside the reviewer, the assignment is `sessionInput :: ReviewTask`. Its candidate
accessor is `reviewInput`, not `candidate`. After executing the relevant review,
with `checks :: [Text]` naming the actual checks and `scope :: Text` describing what
those checks establish:

```haskell
respond (Produced (Accepted (ReviewedCandidate (reviewAssignment sessionInput) (reviewInput sessionInput) checks scope)))
```

For defects, return `Produced (Repair (reviewInput sessionInput) findings)` instead.
Keep findings actionable: exact source, defect, consequence and required repair.
Reference durable evidence rather than reproducing the plan or unaffected constraints.
The tool's reply-submission result is sufficient; don't add an acknowledgment turn.

After repair, reuse the retained specialist with `reviewAgain` at the revised
candidate; consult its signature only when needed. Integrate the reviewed source,
then verify the changed integration boundary. A review decision covers its stated
scope and does not turn partial work into product completion.
