---
name: exomonad-review
description: Commission independent review and repair of exact Exomonad candidates using Project.Work, and return typed review decisions without copying full task histories.
---

In the recursive execution loop, substantive leaves receive independent review;
component review checks joins and combined acceptance using accepted leaf evidence.
For a separate implementer's candidate, `startReviewFlow` handles the counted
check/review/bounded-repair sequence. Use the direct requests below for an owner’s
own candidate or an independent exact-commit question; preserve the same evidence
and integration boundaries.
The reviewer is seeded at the exact candidate commit, so it can run the
candidate's own tests. From any actor, root included, with the cumulative base, candidate commit, its
acceptance and its owned paths, pass a label naming this review's own
campaign. Resolve both OIDs from Git; base precedes candidate in the call.
reviewCommit unfolds an absolute group path from it rather than
nesting under your own (root has no allocated actor path to nest under):

```haskell
(reviewer, reviewProgress) <- reviewCommit [label|parse-fix-review|] base commit "Round-trip tests for every item kind pass" ["src/parse.rs"]
reviewQuestions <- followWork [("review", reviewer, reviewProgress)] (notifyWork me workQuestionsMessage)
```

Inside a request whose `sessionInput :: Task` describes the work, with your
committed `candidate :: Candidate`:

```haskell
(reviewer, reviewProgress) <- reviewCandidate sessionInput OwnerRepairs candidate
let reviewerRef = responseActor reviewer
reviewQuestions <- followWork [("review", reviewer, reviewProgress)] (notifyWork me workQuestionsMessage)
```

Both retain the original reviewer response and a question-only collector.
The request owns its settlement notice; the collector surfaces questions while
review is still pending, without a duplicate settlement notice. Read
`readWork reviewQuestions` for full questions and delivery receipts. After the
result arrives, drain `finishWork reviewQuestions` and retain its exit before
retiring the reviewer or starting the next attempt.
`OwnerRepairs` means you repair findings; it avoids queuing a repair behind
your own pending delivery.

Inside the reviewer, the request is `sessionInput :: ReviewRequest`. Its basis
is `AssignedTask Task` or `ExactScope GitOid [Text] Text`, and its candidate
accessor is `reviewInput`, not `candidate`. Use `reviewBase`, `reviewOwnedPaths`
and `reviewAcceptance` on the basis to check the exact cumulative scope.
Read for structure before bugs: does the change add a second way to do
something that exists? Confirm `git rev-parse HEAD` is the
candidate commit before running checks; a test filter that matched zero tests is
"not run", never "passed". After executing the relevant review, with
`checks :: [Text]` naming the checks that actually ran (with matched counts) and
`scope :: Text` describing what those checks establish:

```haskell
let reviewed = ReviewedCandidate
      { reviewedBasis = reviewBasis sessionInput
      , reviewedCandidate = reviewInput sessionInput
      , reviewChecks = checks
      , reviewRationale = scope
      }
respond (Produced (Accepted reviewed))
```

The reviewer can submit that acceptance through `submit_review` after reading
the live `currentRequest :: Eff CodingEffects (RequestScope ReviewRequest
(Outcome ReviewDecision))`. Use `requestIdNumber` for `expectedRequestId`, the
candidate commit for `expectedCandidateOid`, and supply `submittedChecks` and
`submittedRationale`. The tool checks the active request and clean checkout,
then uses the same typed reply. Its refusal leaves the review open.

For defects, return `Produced (Repair (reviewInput sessionInput) findings)` instead.
Keep findings actionable: exact source, defect, consequence and required repair.
Reference durable evidence rather than reproducing the plan or unaffected constraints.
The tool's reply-submission result is sufficient; don't add an acknowledgment turn.

After repair use `requestReview retryLabel revisedRequest` and attach the same
question-only collector to its returned handles in that admission cell. It preserves the
review basis and admits a fresh reviewer at the new candidate. A retained reviewer's
checkout does not change just because the request names another commit.
For automatic counted check/review/repair, use `startReviewFlow` with a separate
completed implementer, focused PlanChecks and a bounded policy. Optional
`flowIntegration` publishes through an existing MergeTarget after acceptance.

## Owner map and repair policy from exact commits

Check ownership in code, not with Jev: an outside-owned-path change is a
`git diff` fact. Take `changed` from the cumulative diff between the
assignment base and the exact candidate tip. Require `git merge-base
--is-ancestor <base> <tip>` to succeed, then use `git diff <base>..<tip>
--name-only`, never from the tip commit alone: a tip that looks scoped can
carry an ancestor commit that edited an unowned path, and the same branch
can pass the tip check twice while still carrying it. Escalate an ownership violation to the owner. Counted checks and exact source are
authoritative facts; Jev must not invent a passing gate or authorize a merge.
`semanticReviewChoice` can classify concrete review findings against explicit
repair/escalation criteria. Unknown evidence returns to the owner. ReviewFlow
retains the exact reviewer response and performs these transitions under its
bounded repair policy.
