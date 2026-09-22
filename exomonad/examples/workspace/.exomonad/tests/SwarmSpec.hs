{-# LANGUAGE OverloadedStrings #-}

module Main (main) where

import Control.Monad (unless)
import Data.List.NonEmpty (NonEmpty ((:|)))
import Data.Text (Text)
import Project.Swarm

type TestBatch = Batch Text Text

type TestRequest = Request Text Text

main :: IO ()
main = do
  testBeginAndValidation
  testSerializedMerges
  testOptionalFrontier
  testEscalation
  testRepairTicketsAndLimit
  testRebaseLimit
  testNestedSubtree
  testMergeUncertainty
  testIntegrationFailure
  putStrLn "SwarmSpec: all tests passed"

assertEqual :: (Eq a, Show a) => String -> a -> a -> IO ()
assertEqual label expected actual =
  unless (expected == actual) $
    error (label <> ": expected " <> show expected <> ", got " <> show actual)

assertTrue :: String -> Bool -> IO ()
assertTrue label condition = unless condition (error (label <> ": assertion failed"))

mustBegin
  :: String
  -> Limits
  -> Frontier
  -> Text
  -> [(Child, Text)]
  -> IO (TestBatch, [TestRequest])
mustBegin label policy requested base tasks =
  case begin policy requested base tasks of
    Left problem -> error (label <> ": begin failed with " <> show problem)
    Right result -> pure result

mustAdvance
  :: String
  -> Child
  -> Ticket
  -> Reply Text
  -> TestBatch
  -> IO (TestBatch, [TestRequest])
mustAdvance label child ticket reply batch =
  case advance child ticket reply batch of
    Left problem -> error (label <> ": advance failed with " <> show problem)
    Right result -> pure result

runTicket :: String -> Child -> Action Text Text -> [TestRequest] -> IO Ticket
runTicket label child action requests =
  case [ticket | Run owner ticket actual <- requests, owner == child, actual == action] of
    [ticket] -> pure ticket
    matches -> error (label <> ": expected one matching Run, got " <> show (length matches)
      <> " in " <> show requests)

assertNoWake :: String -> [TestRequest] -> IO ()
assertNoWake label requests =
  assertTrue label (null [() | WakeParent _ <- requests])

assertNoMerge :: String -> [TestRequest] -> IO ()
assertNoMerge label requests =
  assertTrue label (null [() | Run _ _ (Merge _ _) <- requests])

standardLimits :: Limits
standardLimits = Limits {repairLimit = 2, rebaseLimit = 2}

testBeginAndValidation :: IO ()
testBeginAndValidation = do
  (batch, requests) <- mustBegin "common scaffold" standardLimits AllChildren "scaffold"
    [("alpha", "task-a"), ("beta", "task-b"), ("gamma", "task-c")]
  alpha <- runTicket "alpha launch" "alpha" (Work "task-a" "scaffold") requests
  beta <- runTicket "beta launch" "beta" (Work "task-b" "scaffold") requests
  gamma <- runTicket "gamma launch" "gamma" (Work "task-c" "scaffold") requests
  assertTrue "launch tickets are distinct" (alpha /= beta && beta /= gamma && alpha /= gamma)
  assertEqual "initial view" (View "scaffold"
    [("alpha", InFlight), ("beta", InFlight), ("gamma", InFlight)] [] False) (view batch)
  assertEqual "empty batch" (Left EmptyBatch :: Either Problem (TestBatch, [TestRequest]))
    (begin standardLimits AllChildren "base" [])
  assertEqual "duplicate children" (Left DuplicateChildren :: Either Problem (TestBatch, [TestRequest]))
    (begin standardLimits AllChildren "base" [("alpha", "one"), ("alpha", "two")])
  assertEqual "unknown frontier child" (Left (UnknownFrontierChild "ghost")
      :: Either Problem (TestBatch, [TestRequest]))
    (begin standardLimits (After ("alpha" :| ["ghost"])) "base" [("alpha", "one")])

testSerializedMerges :: IO ()
testSerializedMerges = do
  (batch0, launches) <- mustBegin "serialized merges" standardLimits AllChildren "base"
    [("alpha", "task-a"), ("beta", "task-b")]
  alphaWork <- runTicket "alpha work" "alpha" (Work "task-a" "base") launches
  betaWork <- runTicket "beta work" "beta" (Work "task-b" "base") launches
  (batch1, alphaChecks) <- mustAdvance "alpha produced" "alpha" alphaWork (Produced "candidate-a") batch0
  alphaReview <- runTicket "alpha review" "alpha"
    (CheckAndReview "task-a" "candidate-a") alphaChecks
  (batch2, betaChecks) <- mustAdvance "beta produced" "beta" betaWork (Produced "candidate-b") batch1
  betaReview <- runTicket "beta review" "beta"
    (CheckAndReview "task-b" "candidate-b") betaChecks
  (batch3, alphaMergeRequests) <- mustAdvance "alpha accepted" "alpha" alphaReview
    (Reviewed Accept) batch2
  alphaMerge <- runTicket "alpha merge" "alpha" (Merge "base" "candidate-a") alphaMergeRequests
  (batch4, betaAcceptedRequests) <- mustAdvance "beta accepted" "beta" betaReview
    (Reviewed Accept) batch3
  assertNoMerge "second merge is serialized" betaAcceptedRequests
  (batch5, alphaIntegratedRequests) <- mustAdvance "alpha integrated" "alpha" alphaMerge
    (Integrated (Applied "head-a")) batch4
  let alphaNotice = ChildMerged "alpha" "candidate-a" "head-a"
  assertTrue "alpha merge queues notice" (QueueNotice alphaNotice `elem` alphaIntegratedRequests)
  assertTrue "open sibling learns new base" (BaseAdvanced "beta" "head-a" `elem` alphaIntegratedRequests)
  assertNoWake "default frontier waits for all children" alphaIntegratedRequests
  betaMerge <- runTicket "beta merge uses new head" "beta"
    (Merge "head-a" "candidate-b") alphaIntegratedRequests
  (batch6, betaIntegratedRequests) <- mustAdvance "beta integrated" "beta" betaMerge
    (Integrated (Applied "head-b")) batch5
  let betaNotice = ChildMerged "beta" "candidate-b" "head-b"
  assertTrue "beta merge queues notice" (QueueNotice betaNotice `elem` betaIntegratedRequests)
  assertTrue "settled batch wakes parent"
    (WakeParent [BatchSettled] `elem` betaIntegratedRequests)
  assertEqual "final serialized view" (View "head-b"
    [("alpha", Landed "candidate-a" "head-a"), ("beta", Landed "candidate-b" "head-b")]
    [alphaNotice, betaNotice] False) (view batch6)
  assertTrue "serialized batch complete" (complete batch6)

testOptionalFrontier :: IO ()
testOptionalFrontier = do
  (batch0, launches) <- mustBegin "optional frontier" standardLimits
    (After ("alpha" :| [])) "base" [("alpha", "task-a"), ("beta", "task-b")]
  alphaWork <- runTicket "frontier alpha work" "alpha" (Work "task-a" "base") launches
  betaWork <- runTicket "frontier beta work" "beta" (Work "task-b" "base") launches
  (batch1, checks) <- mustAdvance "frontier alpha produced" "alpha" alphaWork
    (Produced "candidate-a") batch0
  review <- runTicket "frontier alpha review" "alpha"
    (CheckAndReview "task-a" "candidate-a") checks
  (batch2, merges) <- mustAdvance "frontier alpha accepted" "alpha" review (Reviewed Accept) batch1
  merge <- runTicket "frontier alpha merge" "alpha" (Merge "base" "candidate-a") merges
  (batch3, earlyWake) <- mustAdvance "frontier alpha integrated" "alpha" merge
    (Integrated (Applied "head-a")) batch2
  assertTrue "selected frontier wakes early" (WakeParent [FrontierReady] `elem` earlyWake)
  (batch4, betaChecks) <- mustAdvance "frontier beta produced" "beta" betaWork
    (Produced "candidate-b") batch3
  betaReview <- runTicket "frontier beta review" "beta"
    (CheckAndReview "task-b" "candidate-b") betaChecks
  (batch5, betaMerges) <- mustAdvance "frontier beta accepted" "beta" betaReview
    (Reviewed Accept) batch4
  betaMerge <- runTicket "frontier beta merge" "beta" (Merge "head-a" "candidate-b") betaMerges
  (_, finalWake) <- mustAdvance "frontier beta integrated" "beta" betaMerge
    (Integrated (Applied "head-b")) batch5
  assertTrue "frontier wakes again when fully settled"
    (WakeParent [BatchSettled] `elem` finalWake)

testEscalation :: IO ()
testEscalation = do
  (batch0, launches) <- mustBegin "escalation" standardLimits AllChildren "base"
    [("alpha", "task-a"), ("beta", "task-b")]
  alphaWork <- runTicket "escalation alpha work" "alpha" (Work "task-a" "base") launches
  betaWork <- runTicket "escalation beta work" "beta" (Work "task-b" "base") launches
  (batch1, alphaChecks) <- mustAdvance "escalation alpha produced" "alpha" alphaWork
    (Produced "candidate-a") batch0
  alphaReview <- runTicket "escalation alpha review" "alpha"
    (CheckAndReview "task-a" "candidate-a") alphaChecks
  (batch2, escalation) <- mustAdvance "contract question" "alpha" alphaReview
    (Reviewed (ContractQuestion "which contract?")) batch1
  assertTrue "contract question wakes immediately"
    (WakeParent [ChildNeedsDecision "alpha" "which contract?"] `elem` escalation)
  assertEqual "escalated child stops while sibling remains open" (View "base"
    [("alpha", NeedsDecision "which contract?"), ("beta", InFlight)] [] False) (view batch2)
  (batch3, betaChecks) <- mustAdvance "sibling still produces" "beta" betaWork
    (Produced "candidate-b") batch2
  betaReview <- runTicket "sibling review" "beta"
    (CheckAndReview "task-b" "candidate-b") betaChecks
  (batch4, betaMerges) <- mustAdvance "sibling accepted" "beta" betaReview
    (Reviewed Accept) batch3
  betaMerge <- runTicket "sibling merge" "beta" (Merge "base" "candidate-b") betaMerges
  (batch5, settled) <- mustAdvance "sibling integrated" "beta" betaMerge
    (Integrated (Applied "head-b")) batch4
  assertTrue "siblings continue through settlement" (complete batch5)
  assertTrue "escalated batch settles after siblings"
    (WakeParent [BatchSettled] `elem` settled)

testRepairTicketsAndLimit :: IO ()
testRepairTicketsAndLimit = do
  let policy = Limits {repairLimit = 1, rebaseLimit = 1}
  (batch0, launches) <- mustBegin "repair" policy AllChildren "base" [("alpha", "task-a")]
  work <- runTicket "repair work" "alpha" (Work "task-a" "base") launches
  (batch1, checks1) <- mustAdvance "repair initial produced" "alpha" work
    (Produced "candidate-1") batch0
  review1 <- runTicket "repair initial review" "alpha"
    (CheckAndReview "task-a" "candidate-1") checks1
  let findings1 = "fix one" :| []
  (batch2, repairs) <- mustAdvance "repair requested" "alpha" review1
    (Reviewed (Fix findings1)) batch1
  repair <- runTicket "repair run" "alpha"
    (RepairWork "task-a" "candidate-1" findings1) repairs
  assertEqual "old review cannot approve repair" (Left (StaleReply "alpha" review1))
    (advance "alpha" review1 (Reviewed Accept) batch2)
  (batch3, checks2) <- mustAdvance "repair produced" "alpha" repair
    (Produced "candidate-2") batch2
  review2 <- runTicket "repaired revision is reviewed" "alpha"
    (CheckAndReview "task-a" "candidate-2") checks2
  assertTrue "repair review has a fresh ticket" (review2 /= review1 && review2 /= repair)
  let findings2 = "still broken" :| ["also flaky"]
  (batch4, exhausted) <- mustAdvance "repair budget exhausted" "alpha" review2
    (Reviewed (Fix findings2)) batch3
  let reason = "Repair budget exhausted: still broken; also flaky"
  assertTrue "repair exhaustion wakes decision"
    (WakeParent [ChildNeedsDecision "alpha" reason, BatchSettled] `elem` exhausted)
  assertEqual "repair exhaustion state" (View "base" [("alpha", NeedsDecision reason)] [] False)
    (view batch4)

testRebaseLimit :: IO ()
testRebaseLimit = do
  let policy = Limits {repairLimit = 1, rebaseLimit = 1}
  (batch0, launches) <- mustBegin "rebase" policy AllChildren "base-0"
    [("alpha", "task-a"), ("beta", "task-b"), ("gamma", "task-g")]
  workA <- runTicket "rebase alpha work" "alpha" (Work "task-a" "base-0") launches
  workB <- runTicket "rebase beta work" "beta" (Work "task-b" "base-0") launches
  workG <- runTicket "rebase gamma work" "gamma" (Work "task-g" "base-0") launches
  (batch1, checksA) <- mustAdvance "rebase alpha produced" "alpha" workA
    (Produced "candidate-a-1") batch0
  reviewA <- runTicket "rebase alpha review" "alpha"
    (CheckAndReview "task-a" "candidate-a-1") checksA
  (batch2, checksB) <- mustAdvance "rebase beta produced" "beta" workB
    (Produced "candidate-b") batch1
  reviewB <- runTicket "rebase beta review" "beta"
    (CheckAndReview "task-b" "candidate-b") checksB
  (batch3, checksG) <- mustAdvance "rebase gamma produced" "gamma" workG
    (Produced "candidate-g") batch2
  reviewG <- runTicket "rebase gamma review" "gamma"
    (CheckAndReview "task-g" "candidate-g") checksG
  (batch4, merges1) <- mustAdvance "rebase alpha accepted" "alpha" reviewA
    (Reviewed Accept) batch3
  merge1 <- runTicket "first merge" "alpha" (Merge "base-0" "candidate-a-1") merges1
  (batch5, betaReady) <- mustAdvance "rebase beta accepted" "beta" reviewB
    (Reviewed Accept) batch4
  assertNoMerge "ready sibling waits during first merge" betaReady
  (batch6, gammaReady) <- mustAdvance "rebase gamma accepted" "gamma" reviewG
    (Reviewed Accept) batch5
  assertNoMerge "second ready sibling also waits" gammaReady
  (batch7, rebases) <- mustAdvance "first rebase required" "alpha" merge1
    (Integrated (RebaseRequired "base-1")) batch6
  assertTrue "retry rebase advances both open sibling bases"
    (BaseAdvanced "beta" "base-1" `elem` rebases
      && BaseAdvanced "gamma" "base-1" `elem` rebases)
  rebase <- runTicket "bounded rebase work" "alpha"
    (RebaseWork "task-a" "candidate-a-1" "base-1") rebases
  betaMerge <- runTicket "retry releases slot on observed base" "beta"
    (Merge "base-1" "candidate-b") rebases
  (batch8, checks2) <- mustAdvance "rebased work produced" "alpha" rebase
    (Produced "candidate-a-2") batch7
  review2 <- runTicket "rebased revision reviewed" "alpha"
    (CheckAndReview "task-a" "candidate-a-2") checks2
  (batch9, alphaReady) <- mustAdvance "rebased revision accepted" "alpha" review2
    (Reviewed Accept) batch8
  assertNoMerge "rebased candidate waits for active sibling merge" alphaReady
  (batch10, afterBeta) <- mustAdvance "ready sibling integrated" "beta" betaMerge
    (Integrated (Applied "head-beta")) batch9
  merge2 <- runTicket "rebased candidate uses latest integrated head" "alpha"
    (Merge "head-beta" "candidate-a-2") afterBeta
  (batch11, exhausted) <- mustAdvance "second rebase required" "alpha" merge2
    (Integrated (RebaseRequired "base-2")) batch10
  let reason = "Rebase budget exhausted"
  assertTrue "exhausted rebase advances open sibling base"
    (BaseAdvanced "gamma" "base-2" `elem` exhausted)
  assertTrue "rebase exhaustion wakes decision"
    (WakeParent [ChildNeedsDecision "alpha" reason] `elem` exhausted)
  _ <- runTicket "remaining sibling uses exhausted observed base" "gamma"
    (Merge "base-2" "candidate-g") exhausted
  assertEqual "rebase exhaustion keeps observed base" (View "base-2"
    [ ("alpha", NeedsDecision reason)
    , ("beta", Landed "candidate-b" "head-beta")
    , ("gamma", InFlight)
    ]
    [ChildMerged "beta" "candidate-b" "head-beta"] False) (view batch11)

testNestedSubtree :: IO ()
testNestedSubtree = do
  (parent0, parentLaunches) <- mustBegin "parent batch" standardLimits AllChildren "shared-scaffold"
    [("subtree", "parent-child-obligation")]
  parentWork <- runTicket "parent launches subtree" "subtree"
    (Work "parent-child-obligation" "shared-scaffold") parentLaunches

  (child0, childLaunches) <- mustBegin "child batch" standardLimits AllChildren "shared-scaffold"
    [("grandchild-a", "leaf-a"), ("grandchild-b", "leaf-b")]
  workA <- runTicket "grandchild a shared scaffold" "grandchild-a"
    (Work "leaf-a" "shared-scaffold") childLaunches
  workB <- runTicket "grandchild b shared scaffold" "grandchild-b"
    (Work "leaf-b" "shared-scaffold") childLaunches
  (child1, checksA) <- mustAdvance "grandchild a produced" "grandchild-a" workA
    (Produced "leaf-a-candidate") child0
  reviewA <- runTicket "grandchild a reviewed" "grandchild-a"
    (CheckAndReview "leaf-a" "leaf-a-candidate") checksA
  (child2, checksB) <- mustAdvance "grandchild b produced" "grandchild-b" workB
    (Produced "leaf-b-candidate") child1
  reviewB <- runTicket "grandchild b reviewed" "grandchild-b"
    (CheckAndReview "leaf-b" "leaf-b-candidate") checksB
  (child3, mergesA) <- mustAdvance "grandchild a accepted" "grandchild-a" reviewA
    (Reviewed Accept) child2
  mergeA <- runTicket "grandchild a merge" "grandchild-a"
    (Merge "shared-scaffold" "leaf-a-candidate") mergesA
  (child4, noSecondMerge) <- mustAdvance "grandchild b accepted" "grandchild-b" reviewB
    (Reviewed Accept) child3
  assertNoMerge "nested subtree serializes merges" noSecondMerge
  (child5, afterMergeA) <- mustAdvance "grandchild a integrated" "grandchild-a" mergeA
    (Integrated (Applied "subtree-head-1")) child4
  mergeB <- runTicket "grandchild b merges on folded head" "grandchild-b"
    (Merge "subtree-head-1" "leaf-b-candidate") afterMergeA
  (child6, childSettled) <- mustAdvance "grandchild b integrated" "grandchild-b" mergeB
    (Integrated (Applied "subtree-head-2")) child5
  assertTrue "nested child batch settles"
    (complete child6 && WakeParent [BatchSettled] `elem` childSettled)
  assertEqual "nested child fold head" "subtree-head-2" (currentHead (view child6))
  assertEqual "duplicate final merge receipt is stale"
    (Left (StaleReply "grandchild-b" mergeB))
    (advance "grandchild-b" mergeB (Integrated (Applied "duplicate-head")) child6)
  assertEqual "duplicate receipt cannot add a notice" 2 (length (mergeNotices (view child6)))

  (parent1, parentChecks) <- mustAdvance "subtree fold produced to parent" "subtree" parentWork
    (Produced (currentHead (view child6))) parent0
  parentReview <- runTicket "parent reviews folded subtree against its obligation" "subtree"
    (CheckAndReview "parent-child-obligation" "subtree-head-2") parentChecks
  (parent2, parentMerges) <- mustAdvance "folded subtree accepted" "subtree" parentReview
    (Reviewed Accept) parent1
  parentMerge <- runTicket "parent merges folded subtree" "subtree"
    (Merge "shared-scaffold" "subtree-head-2") parentMerges
  (parent3, parentSettled) <- mustAdvance "parent integrated" "subtree" parentMerge
    (Integrated (Applied "parent-head")) parent2
  assertTrue "parent batch settles after folded subtree"
    (complete parent3 && WakeParent [BatchSettled] `elem` parentSettled)
  assertEqual "parent fold view" (View "parent-head"
    [("subtree", Landed "subtree-head-2" "parent-head")]
    [ChildMerged "subtree" "subtree-head-2" "parent-head"] False) (view parent3)

readyPair :: String -> IO (TestBatch, Ticket)
readyPair label = do
  (batch0, launches) <- mustBegin label standardLimits AllChildren "known-head"
    [("alpha", "task-a"), ("beta", "task-b")]
  alphaWork <- runTicket (label <> " alpha work") "alpha" (Work "task-a" "known-head") launches
  betaWork <- runTicket (label <> " beta work") "beta" (Work "task-b" "known-head") launches
  (batch1, alphaChecks) <- mustAdvance (label <> " alpha produced") "alpha" alphaWork
    (Produced "candidate-a") batch0
  alphaReview <- runTicket (label <> " alpha review") "alpha"
    (CheckAndReview "task-a" "candidate-a") alphaChecks
  (batch2, betaChecks) <- mustAdvance (label <> " beta produced") "beta" betaWork
    (Produced "candidate-b") batch1
  betaReview <- runTicket (label <> " beta review") "beta"
    (CheckAndReview "task-b" "candidate-b") betaChecks
  (batch3, alphaMerges) <- mustAdvance (label <> " alpha accepted") "alpha" alphaReview
    (Reviewed Accept) batch2
  alphaMerge <- runTicket (label <> " alpha merge") "alpha"
    (Merge "known-head" "candidate-a") alphaMerges
  (batch4, betaAccepted) <- mustAdvance (label <> " beta accepted") "beta" betaReview
    (Reviewed Accept) batch3
  assertNoMerge (label <> " serial merge") betaAccepted
  pure (batch4, alphaMerge)

testMergeUncertainty :: IO ()
testMergeUncertainty = do
  (batch0, merge) <- readyPair "uncertain merge"
  (batch1, requests) <- mustAdvance "uncertain result" "alpha" merge
    (Integrated (MergeUncertain "integration result unknown")) batch0
  assertNoMerge "uncertainty pauses later merges" requests
  assertTrue "uncertainty wakes integration decision"
    (WakeParent [IntegrationNeedsDecision "alpha" "integration result unknown"] `elem` requests)
  assertEqual "uncertainty retains known head and ready sibling" (View "known-head"
    [("alpha", NeedsDecision "integration result unknown"), ("beta", InFlight)] [] True)
    (view batch1)

testIntegrationFailure :: IO ()
testIntegrationFailure = do
  (batch0, merge) <- readyPair "failed check"
  (batch1, repairRequests) <- mustAdvance "failed integrated check" "alpha" merge
    (Integrated (IntegrationFailed "observed-head" "integration tests failed")) batch0
  assertNoMerge "failed check pauses later merges" repairRequests
  repair <- runTicket "failed check starts bounded integration repair" "alpha"
    (RepairIntegration "observed-head" ("integration tests failed" :| [])) repairRequests
  assertEqual "failed check retains observed head while repairing" (View "observed-head"
    [("alpha", InFlight), ("beta", InFlight)] [] True) (view batch1)
  (batch2, checkRequests) <- mustAdvance "integration repair produced" "alpha" repair
    (Produced "repaired-head") batch1
  check <- runTicket "repaired integration head gets fresh check" "alpha"
    (CheckIntegration "repaired-head") checkRequests
  assertNoMerge "integration remains paused during fresh check" checkRequests
  (batch3, resumed) <- mustAdvance "integration repair accepted" "alpha" check
    (Reviewed Accept) batch2
  let notice = ChildMerged "alpha" "candidate-a" "repaired-head"
  assertTrue "successful repair queues original candidate with repaired head"
    (QueueNotice notice `elem` resumed)
  assertTrue "successful repair advances sibling base"
    (BaseAdvanced "beta" "repaired-head" `elem` resumed)
  betaMerge <- runTicket "successful repair resumes sibling merge" "beta"
    (Merge "repaired-head" "candidate-b") resumed
  assertEqual "successful repair resumes unpaused" (View "repaired-head"
    [("alpha", Landed "candidate-a" "repaired-head"), ("beta", InFlight)] [notice] False)
    (view batch3)
  (batch4, settled) <- mustAdvance "resumed sibling integrated" "beta" betaMerge
    (Integrated (Applied "final-head")) batch3
  assertTrue "repaired batch settles" (complete batch4 && WakeParent [BatchSettled] `elem` settled)

  let oneRepair = Limits {repairLimit = 1, rebaseLimit = 1}
  (failed0, failedLaunches) <- mustBegin "exhausted integration repair budget" oneRepair
    AllChildren "base" [("alpha", "task-a")]
  failedWork <- runTicket "exhaustion work" "alpha" (Work "task-a" "base") failedLaunches
  (failed1, failedChecks) <- mustAdvance "exhaustion produced" "alpha" failedWork
    (Produced "candidate") failed0
  failedReview <- runTicket "exhaustion review" "alpha"
    (CheckAndReview "task-a" "candidate") failedChecks
  (failed2, failedMerges) <- mustAdvance "exhaustion accepted" "alpha" failedReview
    (Reviewed Accept) failed1
  failedMerge <- runTicket "exhaustion merge" "alpha" (Merge "base" "candidate") failedMerges
  (failed3, failedRepairs) <- mustAdvance "repairable integration failure" "alpha" failedMerge
    (Integrated (IntegrationFailed "failed-head" "still broken")) failed2
  failedRepair <- runTicket "last integration repair" "alpha"
    (RepairIntegration "failed-head" ("still broken" :| [])) failedRepairs
  (failed4, failedRechecks) <- mustAdvance "last repair produced" "alpha" failedRepair
    (Produced "last-repaired-head") failed3
  failedRecheck <- runTicket "last repaired head checked" "alpha"
    (CheckIntegration "last-repaired-head") failedRechecks
  let repeated = "still failing" :| ["new regression"]
      reason = "Integration repair budget exhausted: still failing; new regression"
  (failed5, escalated) <- mustAdvance "integration repair exhausted" "alpha" failedRecheck
    (Reviewed (Fix repeated)) failed4
  assertTrue "exhausted integration repair escalates"
    (WakeParent [IntegrationNeedsDecision "alpha" reason] `elem` escalated)
  assertEqual "exhausted repair retains repaired head and pause" (View "last-repaired-head"
    [("alpha", NeedsDecision reason)] [] True) (view failed5)
  (questioned, questionWake) <- mustAdvance "integration contract question" "alpha" failedRecheck
    (Reviewed (ContractQuestion "contract unclear")) failed4
  assertTrue "integration contract question escalates"
    (WakeParent [IntegrationNeedsDecision "alpha" "contract unclear"] `elem` questionWake)
  assertEqual "contract escalation retains repaired head and pause" (View "last-repaired-head"
    [("alpha", NeedsDecision "contract unclear")] [] True) (view questioned)
  (checkFailed, failureWake) <- mustAdvance "integration check failed" "alpha" failedRecheck
    (Failed "review unavailable") failed4
  assertTrue "integration check failure escalates"
    (WakeParent [IntegrationNeedsDecision "alpha" "review unavailable"] `elem` failureWake)
  assertEqual "failed escalation retains repaired head and pause" (View "last-repaired-head"
    [("alpha", NeedsDecision "review unavailable")] [] True) (view checkFailed)
