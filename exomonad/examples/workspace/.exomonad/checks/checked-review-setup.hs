{-# LANGUAGE QuasiQuotes #-}
import qualified Tidepool.Actor.Record as R
import Tidepool.Effects.Core (GitRef (..))
import qualified Project.Merge as Merge
Right campaign <- pure (campaignLabel ("checked-review-" <> scenario))
let task = Task (batch campaign "component") "plans/component.md" sourceHead
      "Repair the recovery fixture" "Exercise a counted check before exact review"
      ["review-flow.txt"] "One recovery test passes; semantic ownership stays with parent" []
(worker, _) <- unfold (taskGroup task)
  (childWithProgress @WorkProgress @(Outcome Candidate)
    (coding (atRef (GitRef (renderGitOid sourceHead))) (assignment [label|implement|] task)))
let check = PlanCheck "recovery" (\oid -> FocusedSpec "recovery fixture" (renderGitOid oid) "fixture" "lib" "fixture::one" 1) (Cmd.MiB 256) WithoutPreparation
integration <- if scenario == "publish" then do
  Right tree <- createWorktree (fromRef (GitRef (renderGitOid sourceHead)) "checked-publish")
  merger <- R.start (Merge.mergeInto (worktreeId tree) Nothing ["sh", "-c", "test \"$(cat review-flow.txt)\" = pass"])
  pure (Just (Merge.MergeTarget merger))
  else pure Nothing
let policy = defaultReviewFlowPolicy { flowRepairLimit = 1, flowIntegration = integration }
Right reviewRun <- startReviewFlowWith
  (\_ -> pure (ReviewRouteResult
    (if scenario == "escalate" then EscalateReview "owner scope decision" else HonorReview)
    DeterministicRoute)) me task policy worker [check]
let flow = reviewRunFlow reviewRun
let initialRoute = reviewRunRoute reviewRun
