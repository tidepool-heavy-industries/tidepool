{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}

module Project.WorkPlanChecks (structural, correction, regressions, complete) where

import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import qualified Data.Text as Text
import Prelude hiding (readFile)
import qualified Project.WorkPlanRegressionChecks as Regression
import Tidepool.Check

complete :: Member RecipeCheck effects => Eff effects ()
complete = do
  correction
  regressions

regressions :: Member RecipeCheck effects => Eff effects ()
regressions = do
  Regression.duplicateReview
  Regression.parallelVerification
  Regression.missingIncorporation

-- The real record interpreter executes a nested component and a typed
-- parallel join. No model response is needed for these pure leaves.
structural :: Member RecipeCheck effects => Eff effects ()
structural = do
  owner <- root
  source <- git owner ["rev-parse", "HEAD"]
  started <- turn owner (Text.unlines
    [ "(plan, begun) <- do"
    , "  let sourceHead = " <> gitOidLiteral source
    , "  let scopeTask = task [label|work-plan-structure|] \"Coordinate two leaves\" [\"plans/work-plan.md\"] \"join typed results\" sourceHead"
    , "  let scope = ComponentScope scopeTask me 2"
    , "  let graph = do { prefix <- Pure (\"prefix:\" :: Text); (left, right) <- parallel (Component scope (Pure (\"left\" :: Text))) (Component scope (Pure (\"right\" :: Text))); Pure (prefix <> left <> \":\" <> right) } :: WorkPlan Text"
    , "  Right tree <- createWorktree (fromCurrentRepository \"work-plan-structural\")"
    , "  plan <- coordinator (worktreeId tree) me [] graph Nothing"
    , "  begun <- R.call (beginPlan (R.client plan)) ()"
    , "  pure (plan, begun)"
    , "inspectFull begun"
    ])
  check "authorized owner starts the coordinator" ("PlanStarted" `Text.isInfixOf` lastOutput started)
  joined <- awaitOutput owner
    "do { view <- R.call (planView (R.client plan)) (); pure (inspectFull (viewedOutcome <$> view)) }"
    (Text.isInfixOf "prefix:left:right")
  check "sequential continuation receives the typed parallel result"
    ("Right \"prefix:left:right\"" `Text.isInfixOf` joined)
  closed <- turn owner "R.call (closePlan (R.client plan)) ()"
  check "terminal custody closes with no checkpoint lease" ("PlanClosed []" `Text.isInfixOf` lastOutput closed)
  void $ turn owner "R.finish plan >> pure ()"

-- One real hosted child keeps its original development request open through a
-- source correction and a typed incorporation report, then settles normally.
correction :: Member RecipeCheck effects => Eff effects ()
correction = do
  structural
  owner <- root
  before <- git owner ["rev-parse", "HEAD"]
  started <- turn owner (Text.unlines
    [ "(plan, beforeOid, begun) <- do"
    , "  let beforeOid = " <> gitOidLiteral before
    , "  let work = task [label|work-plan-correction|] \"Implement component\" [\"work-plan.txt\"] \"report exact source\" beforeOid"
    , "  let graph = develop (ForkWorker work (lunaWorker [label|work-plan-child|] Medium currentCheckout))"
    , "  Right tree <- createWorktree (fromCurrentRepository \"work-plan-correction\")"
    , "  plan <- coordinator (worktreeId tree) me [] graph Nothing"
    , "  begun <- R.call (beginPlan (R.client plan)) ()"
    , "  pure (plan, beforeOid, begun)"
    , "inspectFull begun"
    ])
  check "coordinator admits a real Luna development node" ("PlanStarted" `Text.isInfixOf` lastOutput started)
  worker <- activation
  check "the development worker runs on Luna" (checkModel worker == Just "gpt-6-luna")
  void $ awaitOutput owner
    "do { Just planState <- R.call (planView (R.client plan)) (); pure (inspectFull (length (viewedDevelopments planState))) }"
    (Text.isInfixOf "1")
  void $ turn owner
    "response <- do { Just planState <- R.call (planView (R.client plan)) (); pure (activeResponse (head (viewedDevelopments planState))) }"
  void $ turn (checkActor worker) (Text.unlines
    [ "do"
    , "  let before = " <> gitOidLiteral before
    , "  let question = Question \"accepted-baseline\" (DesignQuestion \"plans/work-plan.md\" before \"Which accepted source applies?\" [] [] [\"incorporation\"])"
    , "  reportProgress (WorkProgress [] [question])"
    ])
  void $ awaitOutput owner
    "do { Just planState <- R.call (planView (R.client plan)) (); workState <- readWork (activeRouter (head (viewedDevelopments planState))); pure (inspectFull (collectedWork workState)) }"
    (Text.isInfixOf "accepted-baseline")
  after <- checkpoint owner "plans/work-plan.md" "Accepted baseline\n" "accept work-plan baseline"
  refused <- turn owner (Text.unlines
    [ "(afterOid, question, amendment, change, staleResult) <- do"
    , "  let afterOid = " <> gitOidLiteral after
    , "  let question = Question \"accepted-baseline\" (DesignQuestion \"plans/work-plan.md\" beforeOid \"Which accepted source applies?\" [] [] [\"incorporation\"])"
    , "  let amendment = PlanAmendment beforeOid afterOid [\"plans/work-plan.md\"] \"accept baseline\" [\"incorporation\"] [\"owner check\"]"
    , "  let decision = AcceptedDecision question afterOid \"use accepted baseline\" [\"owner check\"]"
    , "  let change = BaselineChange beforeOid afterOid amendment decision"
    , "  staleResult <- R.call (correctQuestion (R.client plan)) (response, Question \"stale\" (questionDetails question), change, [\"read baseline\"])"
    , "  pure (afterOid, question, amendment, change, staleResult)"
    , "inspectFull staleResult"
    ])
  check "stale question is refused before request update"
    ("question is not current" `Text.isInfixOf` lastOutput refused)
  accepted <- turn owner
    "R.call (correctQuestion (R.client plan)) (response, question, change, [\"read baseline\"])"
  check "one correction targets the original pending response"
    ("CorrectionAccepted" `Text.isInfixOf` lastOutput accepted)
  delivered <- present
  check "worker receives accepted baseline in its pending request"
    (after `Text.isInfixOf` delivered)
  observed <- turn owner
    "R.call (observeCorrection (R.client plan)) ()"
  check "presentation is observed separately from incorporation"
    ("UpdatePresented" `Text.isInfixOf` lastOutput observed
      && "correctionReported = Nothing" `Text.isInfixOf` lastOutput observed)
  void $ git (checkActor worker) ["merge", "--ff-only", after]
  incorporatedSource <- readFile (checkActor worker) "plans/work-plan.md"
  check "worker checkout contains the accepted baseline before reporting"
    (incorporatedSource == "Accepted baseline\n")
  void $ turn (checkActor worker)
    (Text.unlines
      [ "do"
      , "  let assigned = sessionInput :: WorkerAssignment"
      , "  let before = " <> gitOidLiteral before
      , "  let after = " <> gitOidLiteral after
      , "  let amendment = PlanAmendment before after [\"plans/work-plan.md\"] \"accept baseline\" [\"incorporation\"] [\"owner check\"]"
      , "  R.send (incorporationRoute assigned) (Incorporated amendment after [\"read baseline\"])"
      ])
  reported <- awaitOutput owner
    "do { Just planState <- R.call (planView (R.client plan)) (); pending <- pollResponse response; pure (inspectFull (pending, viewedCorrection planState)) }"
    (\observed -> "ResponsePending" `Text.isInfixOf` observed
      && "correctionReported = Just" `Text.isInfixOf` observed)
  check "worker report arrives while the original request remains pending"
    ("ResponsePending" `Text.isInfixOf` reported
      && "correctionReported = Just" `Text.isInfixOf` reported)
  candidate <- checkpoint (checkActor worker) "work-plan.txt" "component complete\n" "work-plan candidate"
  void $ turn (checkActor worker)
    ("respond (Produced (Candidate " <> gitOidLiteral candidate <> " [] [\"review remains\"]))")
  settled <- awaitOutput owner
    "do { Just planState <- R.call (planView (R.client plan)) (); pure (inspectFull (fmap (either (const \"failed\") (renderGitOid . candidateCommit . developedCandidate)) (viewedOutcome planState))) }"
    (Text.isInfixOf candidate)
  check "normal settlement preserves the exact candidate after correction"
    (candidate `Text.isInfixOf` settled)
  closed <- turn owner "R.call (closePlan (R.client plan)) ()"
  check "terminal correction releases the coordinator's collectors"
    ("PlanClosed []" `Text.isInfixOf` lastOutput closed)
  void $ turn owner "R.finish plan >> pure ()"
