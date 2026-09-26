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
    , "worker <- unfold (taskGroup ownerTask) (child @(Outcome Candidate) (workerBranch [label|incorporate|] \"worker\" opened))"
    ])
  worker <- activation
  check "selected Luna receives the task without changing its role prompt"
    (checkModel worker == Just "gpt-6-luna" && "Plan:" `Text.isInfixOf` checkContext worker
      && "baselineCollector sessionInput" `Text.isInfixOf` checkContext worker)
  void $ turn owner "lateWorker <- unfold (batch (labelCampaign [label|late-baseline|]) \"work\") (child @(Outcome Candidate) (workerBranch [label|late|] \"late\" lateOpened))"
  lateWorker <- activation
  void $ turn (checkActor lateWorker) "respond (Blocked \"already settled\" [] :: Outcome Candidate)"

  void $ turn owner (Text.unlines
    [ "let affected response label = Affected label response (responseActor response) ownerTask question [\"read exact baseline\"]"
    , "exactRoute <- routeQuestion me question [affected worker \"worker\", affected lateWorker \"late\"]"
    , "Right active <- beginBaselineEpisode opened change [affected worker \"worker\"]"
    , "Right late <- beginBaselineEpisode lateOpened change [affected lateWorker \"late\"]"
    ])
  route <- turn owner "inspectFull exactRoute"
  check "explicit question ownership routes to both affected owners"
    ("worker" `Text.isInfixOf` output route && "late" `Text.isInfixOf` output route)
  late <- turn owner "lateView <- episodeView late\ninspectFull (map ownerDelivery (collectorOwners lateView), episodeComplete lateView)"
  check "a settled request is retained as a late update, never treated as delivered"
    ("UpdateRefused" `Text.isInfixOf` output late && "False" `Text.isInfixOf` output late)

  -- The pending worker receives the owner's update at its next boundary.
  delivered <- present
  check "accepted source and named evidence were delivered to the active worker"
    (after `Text.isInfixOf` delivered && "read exact baseline" `Text.isInfixOf` delivered)
  refreshed <- turn owner "refreshBaselineEpisode active"
  check "owner-side refresh accepts the observed presentation" ("ReportAccepted" `Text.isInfixOf` output refreshed)

  handle <- turn (checkActor worker) (Text.unlines
    [ "let workerCollector = openedActor (baselineCollector (sessionInput :: BaselineAssignment))"
    , "let workerLabel = baselineOwnerLabel (sessionInput :: BaselineAssignment)"
    , "workerView <- R.call (snapshotEpisode (R.client workerCollector)) ()"
    , "let Just workerChange = collectorChange workerView"
    , "let workerAmendment = baselineAmendment workerChange"
    , "let workerAfter = baselineAfter workerChange"
    , "let workerBefore = baselineBefore workerChange"
    , "inspectFull (baselineTask (sessionInput :: BaselineAssignment), collectorBegun workerView)"
    ])
  check "typed Luna assignment carries the live handle into selected context"
    (before `Text.isInfixOf` output handle && "True" `Text.isInfixOf` output handle)

  forged <- turn owner
    "R.call (submitIncorporation (R.client (openedActor opened))) (\"worker\", Incorporated amendment after [\"read exact baseline\"])"
  check "another actor cannot claim the worker's incorporation"
    ("incorporation came from another actor" `Text.isInfixOf` output forged)
  wrongHead <- turn (checkActor worker)
    "R.call (submitIncorporation (R.client workerCollector)) (workerLabel, Incorporated workerAmendment workerBefore [\"read exact baseline\"])"
  check "a report naming an old resulting head is refused"
    ("changed baseline" `Text.isInfixOf` output wrongHead)

  blocked <- turn (checkActor worker)
    "R.call (submitIncorporation (R.client workerCollector)) (workerLabel, IncorporationBlocked workerAmendment \"conflict\" [\"needs owner decision\"])"
  check "the exact worker can report a semantic refusal without settling its task"
    ("ReportRejected" `Text.isInfixOf` output blocked)
  partial <- turn (checkActor worker)
    "R.call (submitIncorporation (R.client workerCollector)) (workerLabel, Incorporated workerAmendment workerAfter [])"
  check "missing named checks remain a rejected report" ("reported checks missing" `Text.isInfixOf` output partial)
  beforeCheck <- turn owner "view <- episodeView active\ninspectFull (ownerReport (head (collectorOwners view)), episodeComplete view)"
  check "partial evidence is retained separately from completed incorporation"
    ("ReportRefused" `Text.isInfixOf` output beforeCheck && "False" `Text.isInfixOf` output beforeCheck)

  void $ git (checkActor worker) ["merge", "--ff-only", after]
  plan <- readFile (checkActor worker) "plans/baseline.md"
  check "worker reads the accepted source in its own checkout" (plan == "Accepted baseline\n")
  accepted <- turn (checkActor worker)
    "R.call (submitIncorporation (R.client workerCollector)) (workerLabel, Incorporated workerAmendment workerAfter [\"read exact baseline\"])"
  check "worker can correct its reported evidence in the same pending request"
    ("ReportAccepted" `Text.isInfixOf` output accepted)
  pending <- turn owner "pollResponse worker"
  check "reporting incorporation does not settle the original task"
    ("ResponsePending" `Text.isInfixOf` output pending)
  final <- turn owner "view <- episodeView active\ninspectFull (episodeComplete view, ownerReport (head (collectorOwners view)))"
  check ("completion means presented plus exact worker report, with checks still only reported: " <> output final)
    ("True" `Text.isInfixOf` output final && "Reported" `Text.isInfixOf` output final)

  stale <- turn owner "invalid <- openBaselineEpisode me\ninvalidResult <- beginBaselineEpisode invalid (change { baselineBefore = after }) [affected worker \"worker\"]\nR.finish (openedActor invalid)\ninspectFull (case invalidResult of { Left reason -> reason; Right _ -> \"unexpected acceptance\" })"
  check "a stale amendment base is refused before any update" ("amendment base differs" `Text.isInfixOf` output stale)
  changed <- turn owner "invalid <- openBaselineEpisode me\ninvalidResult <- beginBaselineEpisode invalid (change { baselineBefore = after, baselineAmendment = amendment { amendmentBase = after } }) [affected worker \"worker\"]\nR.finish (openedActor invalid)\ninspectFull (case invalidResult of { Left reason -> reason; Right _ -> \"unexpected acceptance\" })"
  check "a changed owner task baseline is refused before any update" ("owner's task names a changed baseline" `Text.isInfixOf` output changed)

  void $ turn (checkActor worker) "respond (Blocked \"report retained; source acceptance remains with owner\" [] :: Outcome Candidate)"
  void $ turn owner "R.finish (openedActor opened)\nR.finish (openedActor lateOpened)"
