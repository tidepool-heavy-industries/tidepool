---
name: exomonad-review
description: Commission independent review and repair of exact Exomonad candidates using Project.Work, and return typed review decisions without copying full task histories.
---

Use review when independent judgment helps the owning integration decision.
The reviewer is seeded at the exact candidate commit, so it can run the
candidate's own tests. From any actor, root included, with a commit, its
acceptance and its owned paths:

```haskell
(reviewer, reviewProgress) <- reviewCommit commit "Round-trip tests for every item kind pass" ["src/parse.rs"] OwnerRepairs
```

Inside a request whose `sessionInput :: Task` describes the work, with your
committed `candidate :: Candidate`:

```haskell
(reviewer, reviewProgress) <- reviewCandidate sessionInput OwnerRepairs candidate
let reviewerRef = responseActor reviewer
```

Both return a retained reviewer plus progress; its settlement notice wakes you.
`OwnerRepairs` means you repair findings; it avoids queuing a repair behind
your own pending delivery.

Inside the reviewer, the assignment is `sessionInput :: ReviewTask`. Its candidate
accessor is `reviewInput`, not `candidate`. Read for structure before bugs: does the change add a second way to do
something that exists? Confirm `git rev-parse HEAD` is the
candidate commit before running checks; a test filter that matched zero tests is
"not run", never "passed". After executing the relevant review, with
`checks :: [Text]` naming the checks that actually ran (with matched counts) and
`scope :: Text` describing what those checks establish:

```haskell
let reviewed = ReviewedCandidate
      { acceptedAssignment = reviewAssignment sessionInput
      , reviewedCandidate = reviewInput sessionInput
      , reviewChecks = checks
      , reviewRationale = scope
      }
respond (Produced (Accepted reviewed))
```

For defects, return `Produced (Repair (reviewInput sessionInput) findings)` instead.
Keep findings actionable: exact source, defect, consequence and required repair.
Reference durable evidence rather than reproducing the plan or unaffected constraints.
The tool's reply-submission result is sufficient; don't add an acknowledgment turn.

After repair, reuse the retained specialist with `reviewAgain` at the revised
candidate; consult its signature only when needed. Integrate the reviewed source,
then verify the changed integration boundary. A review decision covers its stated
scope and does not turn partial work into product completion.
