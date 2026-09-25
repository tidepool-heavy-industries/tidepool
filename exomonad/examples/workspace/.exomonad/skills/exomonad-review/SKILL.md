---
name: exomonad-review
description: Commission independent review and repair of exact Exomonad candidates using Project.Work, and return typed review decisions without copying full task histories.
---

Use review when independent judgment helps the owning integration decision.
The reviewer is seeded at the exact candidate commit, so it can run the
candidate's own tests. From any actor, root included, with the cumulative base, candidate commit, its
acceptance and its owned paths, pass a label naming this review's own
campaign. Resolve both OIDs from Git; base precedes candidate in the call.
reviewCommit unfolds an absolute group path from it rather than
nesting under your own (root has no allocated actor path to nest under):

```haskell
(reviewer, reviewProgress) <- reviewCommit [label|parse-fix-review|] base commit "Round-trip tests for every item kind pass" ["src/parse.rs"] OwnerRepairs
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

## Owner map and repair policy from exact commits

Check ownership in code, not with Jev: an outside-owned-path change is a
`git diff` fact. Take `changed` from the cumulative diff between the
assignment base and the exact candidate tip. Require `git merge-base
--is-ancestor <base> <tip>` to succeed, then use `git diff <base>..<tip>
--name-only`, never from the tip commit alone: a tip that looks scoped can
carry an ancestor commit that edited an unowned path, and the same branch
can pass the tip check twice while still carrying it. Route the verdict
with a `J.choice`, naming the base and
candidate commits it ran at; `insufficient_evidence` means the state was
incomplete, not that the candidate is a defect — name the missing field
instead of merging or repairing on a guess:

```haskell
let outside = [p | p <- changed, p `notElem` owned]
let gate = J.choice "Which statement describes the candidate?"
      (J.alt #all_present "Every changed file is inside the owned paths and the required tests pass" ("merge" :: Text)
        J..| J.alt #item_missing "A changed file is outside the owned paths, or a required test is missing or failing" "repair"
        J..| J.alt #insufficient_evidence "The state does not carry what the checklist needs" "ask again")
answer <- J.ask1 (J.state (#owned_paths := owned :& #base := ("abc1230" :: Text) :& #candidate := ("def4560" :: Text) :& #outside := outside)) gate
```
