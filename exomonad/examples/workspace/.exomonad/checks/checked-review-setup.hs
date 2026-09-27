{-# LANGUAGE QuasiQuotes #-}
import qualified Tidepool.Actor.Record as R
import Tidepool.Effects.Core (GitRef (..))
Right campaign <- pure (campaignLabel ("checked-review-" <> scenario))
let task = Task (batch campaign "component") "plans/component.md" sourceHead
      "Repair the recovery fixture" "Exercise a counted check before exact review"
      ["review-flow.txt"] "One recovery test passes; semantic ownership stays with parent" []
(worker, _) <- unfold (taskGroup task)
  (childWithProgress @WorkProgress @(Outcome Candidate)
    (coding (atRef (GitRef (renderGitOid sourceHead))) (assignment [label|implement|] task)))
Right coordinatorTree <- createWorktree (fromRef (GitRef (renderGitOid sourceHead)) ("checked-review-" <> scenario))
let check = PlanCheck "recovery" (\oid -> FocusedSpec "recovery fixture" (renderGitOid oid) "fixture" "lib" "fixture::one" 1) (Cmd.MiB 256) WithoutPreparation
let policy = defaultReviewFlowPolicy { flowRepairLimit = 1 }
flow <- R.start (R.withWorktree (worktreeId coordinatorTree)
  (checkedReviewFlow me task policy worker [check]
    (\_ -> pure (ReviewRouteResult
      (if scenario == "escalate" then EscalateReview "owner scope decision" else HonorReview)
      DeterministicRoute))))
initialRoute <- R.forwardResult worker (firstCandidate (R.client flow))
