{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}
module Project.CollaborationChecks (collaboration) where

import Prelude hiding (readFile, writeFile)
import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import qualified Data.Text as Text
import Tidepool.Check
import Project.Work (workspaceAgentSpec)

import Project.Checks (script, checkSource, checkImprovementSelection)

collaboration :: Member RecipeCheck effects => Eff effects ()
collaboration = do
  owner <- root
  source <- git owner ["rev-parse", "HEAD"]
  void $ turn owner ("let sourceHead = " <> gitOidLiteral source)
  void $ turn owner "before <- snapshot"
  script owner "project_delivery_setup"
  implementer <- activation
  candidate <- checkpoint (checkActor implementer) "feature.txt" "candidate\n" "implement feature"
  void $ turn (checkActor implementer) ("respond (Produced (Candidate " <> gitOidLiteral candidate <> " [\"focused candidate check\"] [\"open product gate\"]))")
  script owner "project_review_start"
  reviewer <- activation
  script (checkActor reviewer) "project_review_repair"
  repair <- activation
  check "delegated repair reuses its available implementer" (checkActor repair == checkActor implementer)
  assertCell (checkActor repair) "delegated repair retains exact typed evidence"
    ("candidateCommit (repairInput sessionInput) == " <> gitOidLiteral candidate <> " && remainingGates (repairInput sessionInput) == [\"open product gate\"] && repairFindings sessionInput == [\"preserve the product gate\"]")
  revised <- checkpoint (checkActor implementer) "feature.txt" "repaired\n" "repair feature"
  void $ turn (checkActor implementer) ("respond (Produced (Candidate " <> gitOidLiteral revised <> " [\"focused repair check\"] [\"open product gate\"]))")
  void $ turn (checkActor reviewer) "state <- pollResponse revision"
  script (checkActor reviewer) "project_design_question"
  expert <- activation
  check "the tagged Astra receives the repaired source and actual uncertainty" (checkModel expert == Just "gpt-6-astra" && revised `Text.isInfixOf` checkContext expert && "Does preparation preserve the boundary?" `Text.isInfixOf` checkContext expert)
  amendment <- checkpoint (checkActor expert) "plans/feature.md" "Preparation retains the open product gate.\n" "clarify acceptance"
  void $ turn (checkActor expert) ("respond (AmendPlan (PlanAmendment " <> gitOidLiteral revised <> " " <> gitOidLiteral amendment <> " [\"plans/feature.md\"] \"retain the preparation gate\" [\"feature review\"] [\"boundary evidence\"]))")
  void $ turn (checkActor reviewer) "design <- pollResponse expert"
  script (checkActor reviewer) "project_plan_incorporation"
  incorporation <- activation
  check "incorporation stays with its exact recipient" (checkActor incorporation == checkActor implementer)
  void $ git (checkActor implementer) ["merge", "--ff-only", amendment]
  plan <- readFile (checkActor implementer) "plans/feature.md"
  check "the accepted plan really reached the implementer's checkout" (plan == "Preparation retains the open product gate.\n")
  void $ turn (checkActor implementer) ("respond (Incorporated (incorporationAmendment sessionInput) " <> gitOidLiteral amendment <> " [\"read exact plan at resulting head\"])")
  void $ turn (checkActor reviewer) "incorporation <- pollResponse planResponse"
  script (checkActor reviewer) "project_review_questions"
  void $ turn owner "observedQuestions <- pollProgress reviewQuestions"
  assertCell owner "coalesced attention retains both unresolved questions" "case observedQuestions of { ProgressUpdate _ progress -> map questionKey (workQuestions progress) == [\"semantics\",\"product-gate\"]; _ -> False }"
  -- A separate component progresses while this review awaits its owning decision.
  void $ turn owner ("let otherTask = task \"independent-consumer\" \"Inspect an independent consumer\" [\"consumer source\"] \"return exact findings\" " <> gitOidLiteral source
    <> "\nRight otherAgent <- spawnSubagent (FreshCtx (taskContext otherTask)) (ForkWorktree projectHead) ((defaultSpawnOptions workspaceAgentSpec) { spawnModel = Just (Alias \"luna\"), spawnEffort = Just Medium, spawnInstructions = Just (projectPrompt \"task\"), spawnLabel = Just (taskName otherTask) })\nRight otherRequest <- request @Text otherAgent (taskContext otherTask) defaultRequestOptions")
  otherActor <- activation
  void $ turn (checkActor otherActor) "respond (\"independent work finished\" :: Text)"
  void $ turn owner "independent <- pollResponse otherRequest"
  assertCell owner "an unrelated obligation finishes during the pending question" "case independent of { ResponseReady receipt -> responseValue receipt == \"independent work finished\"; _ -> False }"
  void $ turn owner ("let incorporatedHead = " <> gitOidLiteral amendment)
  script owner "project_decision_return"
  void $ notPresented "controlled presentation failure"
  void $ turn owner "failedUpdate <- pollRequestUpdate clarification\nfailedResponse <- pollResponse reviewer"
  assertCell owner "failed steering preserves the pending review and receipt" "(case failedUpdate of { Right (UpdateNotPresented _) -> True; _ -> False }) && (case failedResponse of { ResponsePending _ -> True; _ -> False })"
  void $ turn owner "Right supported <- updateRequest reviewer (decisionContext acceptedDecision)"
  message <- present
  check "the owning return carries the exact question and incorporated source" (amendment `Text.isInfixOf` message && "semantics @" `Text.isInfixOf` message)
  void $ git (checkActor reviewer) ["merge", "--ff-only", amendment]
  script (checkActor reviewer) "project_decision_consumer"
  assertCell (checkActor reviewer) "an old answer cannot clear a changed question or rewind the task source" "map questionKey remainingQuestions == [\"product-gate\"] && resolveQuestion acceptedDecision changedQuestions == changedQuestions && raiseQuestion semantics firstQuestions == firstQuestions && taskSource assignment == incorporatedHead"
  consumer <- activation
  check "the fresh consumer receives the accepted decision and rationale" (amendment `Text.isInfixOf` checkContext consumer && "Preparation retains the boundary" `Text.isInfixOf` checkContext consumer && "Why:" `Text.isInfixOf` checkContext consumer)
  consumerHead <- git (checkActor consumer) ["rev-parse", "HEAD"]
  check "the fresh consumer starts at the incorporated decision source" (consumerHead == amendment)
  void $ turn owner "remaining <- pollProgress reviewQuestions"
  assertCell owner "answering one question leaves the other open" "case remaining of { ProgressUpdate _ progress -> map questionKey (workQuestions progress) == [\"product-gate\"]; _ -> False }"
  void $ turn owner "original <- pollResponse worker"
  assertCell owner "repair did not rewrite the original candidate receipt"
    ("case original of { ResponseReady receipt -> case responseValue receipt of { Produced originalCandidate -> candidateCommit originalCandidate == " <> gitOidLiteral candidate <> "; _ -> False }; _ -> False }")
  void $ turn (checkActor reviewer) ("let latest = Candidate " <> gitOidLiteral amendment <> " [\"focused repair check\",\"incorporated plan check\"] [\"open product gate\"]\nrespond (Produced (Accepted (ReviewedCandidate (AssignedTask assignment) latest [\"reviewed revised source and plan\"] \"preparation only\")))")
  void $ turn owner "accepted <- pollResponse reviewer"
  assertCell owner "independent acceptance retains the updated contract and latest source"
    ("case accepted of { ResponseReady receipt -> case responseValue receipt of { Produced (Accepted reviewed) -> candidateCommit (reviewedCandidate reviewed) == " <> gitOidLiteral amendment <> " && remainingGates (reviewedCandidate reviewed) == [\"open product gate\"] && case reviewedBasis reviewed of { AssignedTask assigned -> any (T.isInfixOf \"Preparation retains the boundary\" . decisionSummary) (acceptedDecisions assigned); _ -> False }; _ -> False }; _ -> False }")
  void $ git owner ["merge", "--ff-only", amendment]
  feature <- readFile owner "feature.txt"
  combinedPlan <- readFile owner "plans/feature.md"
  check "combined source contains both repaired implementation and accepted semantics" (feature == "repaired\n" && combinedPlan == "Preparation retains the open product gate.\n")
  -- An uncertain presentation fences settlement instead of silently losing steering.
  void $ turn (checkActor reviewer) "Right uncertain <- updateRequest consumer \"Clarify the retained gate\""
  void $ unconfirmed "controlled lost presentation acknowledgement"
  void $ turn (checkActor reviewer) "uncertainty <- pollRequestUpdate uncertain"
  assertCell (checkActor reviewer) "unconfirmed presentation remains explicit" "case uncertainty of { Right (UpdateUnconfirmed _) -> True; _ -> False }"
  void $ turn (checkActor consumer) ("import Tidepool.Agent.Reply (attemptReply)\nfenced <- attemptReply sessionReply (Produced (Candidate " <> gitOidLiteral amendment <> " [] [\"open product gate\"]))")
  assertCell (checkActor consumer) "an uncertain update cannot silently settle the waiting obligation" "case fenced of { Left ReplyUpdatePending -> True; _ -> False }"
  -- Feed this same repaired, accepted and incorporated work into the next-wave improvement.
  void $ turn owner "let ResponseReady reviewAnswer = accepted\nlet Produced (Accepted reviewed) = responseValue reviewAnswer\nlet delivered = Produced (Delivered reviewed (candidateCommit (reviewedCandidate reviewed)) [\"checked combined feature and plan\"]) :: Delivery\ninspectFull (deliverySummary delivered)"
  void $ turn owner "later <- snapshot\nlet packet = RsiInput (candidateCommit (reviewedCandidate reviewed)) \"Human requested: improve decision handoffs from this completed preparation.\" [] before later [deliverySummary delivered, \"Retained repair, accepted amendment, fresh consumer and failed/unconfirmed steering were exercised; no live usage measured.\"]\nRight improvementAgent <- spawnSubagent (FreshCtx (rsiContext packet)) (ForkWorktree (atRef (GitRef (renderGitOid (rsiSource packet))))) ((defaultSpawnOptions workspaceAgentSpec) { spawnInstructions = Just (projectPrompt \"rsi\"), spawnLabel = Just \"workspace-style\" })\nRight (improvement, _) <- requestWithProgress @WorkProgress @(Outcome Candidate) improvementAgent packet defaultRequestOptions"
  checkImprovementSelection
