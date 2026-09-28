{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}

module Project.WorkPlanRegressionChecks
  ( duplicateReview, parallelVerification, missingIncorporation ) where

import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import qualified Data.Text as Text
import Prelude hiding (readFile, writeFile)
import Exomonad.Workspace (workspaceRoot)
import Tidepool.Check

-- The checked review and product checks run the same real command/evidence
-- transport against a deterministic source fixture.
prepareRunner :: Member RecipeCheck effects => CheckActor -> Eff effects ()
prepareRunner owner = do
  runner <- readFile owner (Text.pack workspaceRoot <> "/checks/checked-review-fixture.sh")
  writeFile owner "scripts/cargo-focused-test" runner
  void $ git owner ["add", "--", "scripts/cargo-focused-test"]
  void $ git owner ["update-index", "--chmod=+x", "--", "scripts/cargo-focused-test"]
  void $ git owner ["checkout-index", "-f", "--", "scripts/cargo-focused-test"]
  void $ git owner ["commit", "-m", "install work-plan check fixture"]
  clean <- git owner ["status", "--porcelain"]
  check "fixture runner is committed before worktree forks" (Text.null clean)

-- Two Review nodes consume the same Developed value. Both ReviewFlow actors
-- must settle even though their implementer request identity is identical.
duplicateReview :: Member RecipeCheck effects => Eff effects ()
duplicateReview = do
  owner <- root
  source <- git owner ["rev-parse", "HEAD"]
  started <- turn owner (Text.unlines
    [ "import qualified Project.WorkPlan as WP"
    , "(plan, begun) <- do"
    , "  let sourceHead = " <> gitOidLiteral source
    , "  let work = task [label|review-correlation|] \"Review one candidate twice\" [\"review-flow.txt\"] \"settle both reviews\" sourceHead"
    , "  Right firstTree <- createWorktree (fromCurrentRepository \"review-correlate-a\")"
    , "  Right secondTree <- createWorktree (fromCurrentRepository \"review-correlate-b\")"
    , "  Right coordinatorTree <- createWorktree (fromCurrentRepository \"review-correlate-plan\")"
    , "  let firstSpec = ReviewSpec work (worktreeId firstTree) defaultReviewFlowPolicy []"
    , "  let secondSpec = ReviewSpec work (worktreeId secondTree) defaultReviewFlowPolicy []"
    , "  let graph = do { developed <- develop (ForkWorker work (lunaWorker [label|review-correlate-child|] Medium currentCheckout)); parallel (WP.review firstSpec developed) (WP.review secondSpec developed) }"
    , "  plan <- coordinator (worktreeId coordinatorTree) me [] graph Nothing"
    , "  begun <- R.call (beginPlan (R.client plan)) ()"
    , "  pure (plan, begun)"
    , "inspectFull begun"
    ])
  check "duplicate review plan starts" ("PlanStarted" `Text.isInfixOf` lastOutput started)
  worker <- activation
  candidate <- checkpoint (checkActor worker) "review-flow.txt" "pass\n" "review correlation candidate"
  void $ turn (checkActor worker)
    ("respond (Produced (Candidate " <> gitOidLiteral candidate <> " [] []))")
  settled <- awaitOutput owner
    "do { Just state <- R.call (planView (R.client plan)) (); pure (inspectFull (length (viewedReviews state) == 2 && case viewedOutcome state of { Just (Left (ParallelStopped [_, _])) -> True; _ -> False })) }"
    (Text.isInfixOf "True")
  check "both review admissions independently return terminal refusals"
    ("True" `Text.isInfixOf` settled)
  closed <- turn owner "R.call (closePlan (R.client plan)) ()"
  check "duplicate review plan reaches terminal closure" ("PlanClosed []" `Text.isInfixOf` lastOutput closed)
  void $ turn owner (Text.unlines
    [ "do"
    , "  Just state <- R.call (planView (R.client plan)) ()"
    , "  mapM_ (\\flow -> R.call (reviewCleanup (R.client flow)) ReviewCleanupOnce >> R.finish flow) (viewedReviews state)"
    , "  R.finish plan"
    ])

-- A real reviewed and published source feeds two independent product checks.
-- Their watcher callbacks must each resume the matching Verify node.
parallelVerification :: Member RecipeCheck effects => Eff effects ()
parallelVerification = do
  owner <- root
  prepareRunner owner
  source <- git owner ["rev-parse", "HEAD"]
  started <- turn owner (Text.unlines
    [ "import qualified Project.Merge as M"
    , "import qualified Project.WorkPlan as WP"
    , "(plan, merger, begun) <- do"
    , "  let sourceHead = " <> gitOidLiteral source
    , "  let work = task [label|verify-correlation|] \"Review and verify one candidate\" [\"review-flow.txt\"] \"retain both check outcomes\" sourceHead"
    , "  let checkFor name = PlanCheck name (\\headOid -> FocusedSpec name (renderGitOid headOid) \"fixture\" \"lib\" \"fixture::one\" 1) (Cmd.MiB 256) WithoutPreparation"
    , "  Right reviewTree <- createWorktree (fromCurrentRepository \"verify-correlate-review\")"
    , "  Right integrationTree <- createWorktree (fromCurrentRepository \"verify-correlate-integration\")"
    , "  Right coordinatorTree <- createWorktree (fromCurrentRepository \"verify-correlate-plan\")"
    , "  merger <- R.start (M.mergeInto (worktreeId integrationTree) Nothing [\"true\"])"
    , "  let reviewSpec = ReviewSpec work (worktreeId reviewTree) defaultReviewFlowPolicy [checkFor \"review\"]"
    , "  let integrateSpec = IntegrationSpec (M.MergeTarget merger) \"verify candidate\" \"publish checked candidate\""
    , "  let leftSpec = Verification [checkFor \"product-left\"] (\\_ _ -> Right (\"left\" :: Text))"
    , "  let rightSpec = Verification [checkFor \"product-right\"] (\\_ _ -> Right (\"right\" :: Text))"
    , "  let graph = do { developed <- develop (ForkWorker work (lunaWorker [label|verify-correlate-child|] Medium currentCheckout)); reviewed <- WP.review reviewSpec developed; checked <- integrate integrateSpec reviewed; (left, right) <- parallel (verify leftSpec checked) (verify rightSpec checked); Pure (acceptedValue left, acceptedValue right) }"
    , "  plan <- coordinator (worktreeId coordinatorTree) me [] graph Nothing"
    , "  begun <- R.call (beginPlan (R.client plan)) ()"
    , "  pure (plan, merger, begun)"
    , "inspectFull begun"
    ])
  check "parallel verification plan starts" ("PlanStarted" `Text.isInfixOf` lastOutput started)
  worker <- activation
  candidate <- checkpoint (checkActor worker) "review-flow.txt" "pass\n" "verified candidate"
  void $ turn (checkActor worker)
    ("respond (Produced (Candidate " <> gitOidLiteral candidate <> " [] []))")
  reviewer <- activation
  reviewedHead <- git (checkActor reviewer) ["rev-parse", "HEAD"]
  check "reviewer sees exact candidate before product checks" (reviewedHead == candidate)
  void $ turn (checkActor reviewer)
    "respond (Produced (Accepted (ReviewedCandidate (reviewBasis sessionInput) (reviewInput sessionInput) [\"fixture passed\"] \"accepted\")))"
  settled <- awaitOutput owner
    "do { Just state <- R.call (planView (R.client plan)) (); pure (inspectFull (case viewedOutcome state of { Just (Right (left, right)) -> left == \"left\" && right == \"right\"; _ -> False })) }"
    (Text.isInfixOf "True")
  check "both distinct verification callbacks preserve their own result"
    ("True" `Text.isInfixOf` settled)
  closed <- turn owner "R.call (closePlan (R.client plan)) ()"
  check "parallel verification plan closes" ("PlanClosed []" `Text.isInfixOf` lastOutput closed)
  void $ turn owner (Text.unlines
    [ "do"
    , "  Just state <- R.call (planView (R.client plan)) ()"
    , "  mapM_ (\\flow -> R.call (reviewCleanup (R.client flow)) ReviewCleanupOnce >> R.finish flow) (viewedReviews state)"
    , "  R.finish merger"
    , "  R.finish plan"
    ])

-- A presented correction cannot be treated as incorporated when the child
-- settles from its old source and never uses its typed report route.
missingIncorporation :: Member RecipeCheck effects => Eff effects ()
missingIncorporation = do
  owner <- root
  before <- git owner ["rev-parse", "HEAD"]
  started <- turn owner (Text.unlines
    [ "(plan, beforeOid, begun) <- do"
    , "  let beforeOid = " <> gitOidLiteral before
    , "  let work = task [label|missing-incorporation|] \"Implement component\" [\"work-plan.txt\"] \"incorporate accepted source\" beforeOid"
    , "  let graph = develop (ForkWorker work (lunaWorker [label|missing-incorporation-child|] Medium currentCheckout))"
    , "  Right tree <- createWorktree (fromCurrentRepository \"missing-incorporation-plan\")"
    , "  plan <- coordinator (worktreeId tree) me [] graph Nothing"
    , "  begun <- R.call (beginPlan (R.client plan)) ()"
    , "  pure (plan, beforeOid, begun)"
    , "inspectFull begun"
    ])
  check "correction refusal plan starts" ("PlanStarted" `Text.isInfixOf` lastOutput started)
  worker <- activation
  void $ awaitOutput owner
    "do { Just state <- R.call (planView (R.client plan)) (); pure (inspectFull (length (viewedDevelopments state))) }"
    (Text.isInfixOf "1")
  void $ turn owner
    "response <- do { Just state <- R.call (planView (R.client plan)) (); pure (activeResponse (head (viewedDevelopments state))) }"
  void $ turn (checkActor worker) (Text.unlines
    [ "do"
    , "  let before = " <> gitOidLiteral before
    , "  reportProgress (WorkProgress [] [Question \"accepted-baseline\" (DesignQuestion \"plans/work-plan.md\" before \"Which source applies?\" [] [] [\"incorporation\"])])"
    ])
  void $ awaitOutput owner
    "do { Just state <- R.call (planView (R.client plan)) (); work <- readWork (activeRouter (head (viewedDevelopments state))); pure (inspectFull (collectedWork work)) }"
    (Text.isInfixOf "accepted-baseline")
  after <- checkpoint owner "plans/work-plan.md" "Accepted baseline for negative case\n" "accept baseline without child incorporation"
  accepted <- turn owner (Text.unlines
    [ "(change, accepted) <- do"
    , "  let afterOid = " <> gitOidLiteral after
    , "  let question = Question \"accepted-baseline\" (DesignQuestion \"plans/work-plan.md\" beforeOid \"Which source applies?\" [] [] [\"incorporation\"])"
    , "  let amendment = PlanAmendment beforeOid afterOid [\"plans/work-plan.md\"] \"accept baseline\" [\"incorporation\"] [\"owner check\"]"
    , "  let decision = AcceptedDecision question afterOid \"use accepted baseline\" [\"owner check\"]"
    , "  let change = BaselineChange beforeOid afterOid amendment decision"
    , "  accepted <- R.call (correctQuestion (R.client plan)) (response, question, change, [\"read baseline\"])"
    , "  pure (change, accepted)"
    , "inspectFull accepted"
    ])
  check "accepted correction updates the original pending request"
    ("CorrectionAccepted" `Text.isInfixOf` lastOutput accepted)
  delivered <- present
  check "child receives accepted baseline before stale settlement" (after `Text.isInfixOf` delivered)
  candidate <- checkpoint (checkActor worker) "work-plan.txt" "old source candidate\n" "candidate without accepted baseline"
  void $ turn (checkActor worker)
    ("respond (Produced (Candidate " <> gitOidLiteral candidate <> " [] []))")
  refused <- awaitOutput owner
    "do { Just state <- R.call (planView (R.client plan)) (); pure (inspectFull (case (viewedOutcome state, viewedCorrection state) of { (Just (Left (SourceRefused DevelopmentNode _)), Just correction) -> case correctionReported correction of { Nothing -> True; Just _ -> False }; _ -> False })) }"
    (Text.isInfixOf "True")
  check "missing incorporation refuses old-source candidate"
    ("True" `Text.isInfixOf` refused)
  closed <- turn owner "R.call (closePlan (R.client plan)) ()"
  check "failed correction plan can close" ("PlanClosed []" `Text.isInfixOf` lastOutput closed)
  void $ turn owner "R.finish plan >> pure ()"
