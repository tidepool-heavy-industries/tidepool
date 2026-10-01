{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}

module Project.BaselineIncorporationChecks (episode) where

import Prelude hiding (readFile)
import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import qualified Data.Text as Text
import Tidepool.Check

-- The collector is allocated before the workers, then bound to one accepted
-- change only after those workers have active requests. The fixture exercises
-- the real owner-only update and a worker's typed, authenticated report while
-- its original request remains pending.
episode :: Member RecipeCheck effects => Eff effects ()
episode = do
  owner <- root
  before <- git owner ["rev-parse", "HEAD"]
  void $ turn owner (Text.unlines
    [ "let before = " <> gitOidLiteral before
    , "let ownerTask = task [label|baseline-change|] \"Incorporate the accepted baseline\" [\"plans/baseline.md\"] \"report exact source and named checks\" before"
    , "let question = Question \"baseline\" (DesignQuestion \"plans/baseline.md\" before \"The accepted source changed\" [] [] [\"incorporation\"])"
    , "opened <- openBaselineEpisode me"
    , "lateOpened <- openBaselineEpisode me"
    ])
  after <- checkpoint owner "plans/baseline.md" "Accepted baseline\n" "accept a baseline"
  void $ turn owner (Text.unlines
    [ "let after = " <> gitOidLiteral after
    , "let amendment = PlanAmendment before after [\"plans/baseline.md\"] \"accept baseline\" [\"incorporation\"] [\"plan check\"]"
    , "let decision = AcceptedDecision question after \"Adopt the accepted baseline\" [\"plan check\"]"
    , "let change = BaselineChange before after amendment decision"
    , "let workerBranch label ownerLabel collector = lunaBaselineTaskFrom label Medium (atRef (GitRef (renderGitOid before))) (BaselineAssignment ownerTask ownerLabel collector)"
    , "worker <- unfold (taskGroup ownerTask) (child @(Outcome Candidate) (withLifetime ActorOwned (workerBranch [label|incorporate|] \"worker\" opened)))"
    ])
  worker <- activation
  check "selected Luna receives the task without changing its role prompt"
    (checkModel worker == Just "gpt-6-luna" && "Plan:" `Text.isInfixOf` checkContext worker
      && "baselineCollector sessionInput" `Text.isInfixOf` checkContext worker)
  void $ turn owner "lateWorker <- unfold (batch (labelCampaign [label|late-baseline|]) \"work\") (child @(Outcome Candidate) (withLifetime ActorOwned (workerBranch [label|late|] \"late\" lateOpened)))"
  lateWorker <- activation
  void $ turn (checkActor lateWorker) "respond (Blocked \"already settled\" [] :: Outcome Candidate)"

  void $ turn owner (Text.unlines
    [ "let affected response label = Affected label response (responseActor response) ownerTask question [\"read exact baseline\"]"
    , "exactRoute <- routeQuestion me question [affected worker \"worker\", affected lateWorker \"late\"]"
    , "Right active <- beginBaselineEpisode opened change [affected worker \"worker\"]"
    , "Right late <- beginBaselineEpisode lateOpened change [affected lateWorker \"late\"]"
    ])
  void $ turn owner "inspectFull exactRoute"
  assertCell owner "explicit question ownership routes to both affected owners"
    "exactRoute == Right [\"worker\", \"late\"]"
  void $ turn owner "lateView <- episodeView late\ninspectFull (map ownerDelivery (collectorOwners lateView), episodeComplete lateView)"
  assertCell owner "settled request retains a late update without claiming delivery"
    "not (episodeComplete lateView) && case collectorOwners lateView of { [row] -> case ownerDelivery row of { UpdateRefused _ -> True; _ -> False }; _ -> False }"
  delivered <- present
  check "accepted source and named evidence were delivered to the active worker"
    (after `Text.isInfixOf` delivered && "read exact baseline" `Text.isInfixOf` delivered)
  void $ turn owner "refreshed <- refreshBaselineEpisode active\npure ()"
  assertCell owner "owner-side refresh accepts observed presentation"
    "refreshed == [(\"worker\", ReportAccepted)]"
  void $ turn (checkActor worker) (Text.unlines
    [ "let workerCollector = openedActor (baselineCollector (sessionInput :: BaselineAssignment))"
    , "let workerLabel = baselineOwnerLabel (sessionInput :: BaselineAssignment)"
    , "workerView <- R.call (snapshotEpisode (R.client workerCollector)) ()"
    , "let Just workerChange = collectorChange workerView"
    , "let workerAmendment = baselineAmendment workerChange"
    , "let workerAfter = baselineAfter workerChange"
    , "let workerBefore = baselineBefore workerChange"
    , "inspectFull (baselineTask (sessionInput :: BaselineAssignment), collectorBegun workerView)"
    ])
  assertCell (checkActor worker) "typed Luna assignment carries live handle into selected context"
    ("taskSource (baselineTask (sessionInput :: BaselineAssignment)) == workerBefore && collectorBegun workerView && workerBefore == " <> gitOidLiteral before <> "")
  void $ turn owner
    "forged <- R.call (submitIncorporation (R.client (openedActor opened))) (\"worker\", Incorporated amendment after [\"read exact baseline\"])\nforgedView <- episodeView active\npure ()"
  assertCell owner "another actor cannot claim worker incorporation"
    "case forged of { ReportRejected _ -> case collectorOwners forgedView of { [row] -> ownerReport row == NoReport; _ -> False }; _ -> False }"
  void $ turn (checkActor worker)
    "wrongHead <- R.call (submitIncorporation (R.client workerCollector)) (workerLabel, Incorporated workerAmendment workerBefore [\"read exact baseline\"])\nwrongHeadView <- R.call (snapshotEpisode (R.client workerCollector)) ()\npure ()"
  assertCell (checkActor worker) "old resulting head report is refused with exact report retained"
    "case wrongHead of { ReportRejected _ -> case collectorOwners wrongHeadView of { [row] -> case ownerReport row of { ReportRefused (Incorporated amendment head checks) _ -> amendment == workerAmendment && head == workerBefore && checks == [\"read exact baseline\"]; _ -> False }; _ -> False }; _ -> False }"
  void $ turn (checkActor worker)
    "blocked <- R.call (submitIncorporation (R.client workerCollector)) (workerLabel, IncorporationBlocked workerAmendment \"conflict\" [\"needs owner decision\"])\nblockedView <- R.call (snapshotEpisode (R.client workerCollector)) ()\npure ()"
  assertCell (checkActor worker) "exact worker reports semantic refusal without settling task"
    "case blocked of { ReportRejected _ -> case collectorOwners blockedView of { [row] -> case ownerReport row of { ReportBlocked (IncorporationBlocked amendment reason checks) -> amendment == workerAmendment && reason == \"conflict\" && checks == [\"needs owner decision\"]; _ -> False }; _ -> False }; _ -> False }"
  void $ turn (checkActor worker)
    "partial <- R.call (submitIncorporation (R.client workerCollector)) (workerLabel, Incorporated workerAmendment workerAfter [])\npartialView <- R.call (snapshotEpisode (R.client workerCollector)) ()\npure ()"
  assertCell (checkActor worker) "missing named checks retain rejected exact report"
    "case partial of { ReportRejected _ -> case collectorOwners partialView of { [row] -> case ownerReport row of { ReportRefused (Incorporated amendment head checks) _ -> amendment == workerAmendment && head == workerAfter && null checks; _ -> False }; _ -> False }; _ -> False }"
  void $ turn owner "view <- episodeView active\ninspectFull (ownerReport (head (collectorOwners view)), episodeComplete view)"
  assertCell owner "partial evidence remains separate from completed incorporation"
    "not (episodeComplete view) && case collectorOwners view of { [row] -> case ownerReport row of { ReportRefused (Incorporated amendment head checks) _ -> amendment == baselineAmendment change && head == after && null checks; _ -> False }; _ -> False }"
  void $ git (checkActor worker) ["merge", "--ff-only", after]
  plan <- readFile (checkActor worker) "plans/baseline.md"
  check "worker reads the accepted source in its own checkout" (plan == "Accepted baseline\n")
  void $ turn (checkActor worker)
    "accepted <- R.call (submitIncorporation (R.client workerCollector)) (workerLabel, Incorporated workerAmendment workerAfter [\"read exact baseline\"])\npure ()"
  assertCell (checkActor worker) "worker corrects evidence in same pending request"
    "accepted == ReportAccepted"
  void $ turn owner "pending <- pollResponse worker\npure ()"
  assertCell owner "reporting incorporation does not settle original task"
    "case pending of { ResponsePending _ -> True; _ -> False }"
  void $ turn owner "view <- episodeView active\ninspectFull (episodeComplete view, ownerReport (head (collectorOwners view)))"
  assertCell owner "completion needs presentation and exact worker reported checks"
    "episodeComplete view && case collectorOwners view of { [row] -> case (ownerDelivery row, ownerReport row) of { (UpdateTracked _ (Just (Right UpdatePresented)), Reported (Incorporated amendment head checks)) -> amendment == baselineAmendment change && head == after && checks == [\"read exact baseline\"]; _ -> False }; _ -> False }"
  void $ turn owner "invalid <- openBaselineEpisode me\ninvalidResult <- beginBaselineEpisode invalid (change { baselineBefore = after }) [affected worker \"worker\"]\ninvalidView <- R.call (snapshotEpisode (R.client (openedActor invalid))) ()\nR.finish (openedActor invalid)\ninspectFull (case invalidResult of { Left reason -> reason; Right _ -> \"unexpected acceptance\" })"
  assertCell owner "stale amendment base refuses before update"
    "case invalidResult of { Left _ -> not (collectorBegun invalidView) && null (collectorOwners invalidView) && case collectorChange invalidView of { Nothing -> True; _ -> False }; _ -> False }"
  void $ turn owner "invalid <- openBaselineEpisode me\ninvalidResult <- beginBaselineEpisode invalid (change { baselineBefore = after, baselineAmendment = amendment { amendmentBase = after } }) [affected worker \"worker\"]\ninvalidView <- R.call (snapshotEpisode (R.client (openedActor invalid))) ()\nR.finish (openedActor invalid)\ninspectFull (case invalidResult of { Left reason -> reason; Right _ -> \"unexpected acceptance\" })"
  assertCell owner "changed owner task baseline refuses before update"
    "case invalidResult of { Left _ -> not (collectorBegun invalidView) && null (collectorOwners invalidView) && case collectorChange invalidView of { Nothing -> True; _ -> False }; _ -> False }"
  void $ turn (checkActor worker) "respond (Blocked \"report retained; source acceptance remains with owner\" [] :: Outcome Candidate)"
  void $ turn owner "R.finish (openedActor opened)\nR.finish (openedActor lateOpened)"
