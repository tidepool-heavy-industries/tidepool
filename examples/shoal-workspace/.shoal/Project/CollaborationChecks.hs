{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}
module Project.CollaborationChecks (collaboration) where

import Prelude hiding (readFile, writeFile)
import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import qualified Data.Text as Text
import Tidepool.Check

import Project.Checks (script, checkImprovement)

collaboration :: Member RecipeCheck effects => Eff effects ()
collaboration = do
  owner <- root
  source <- git owner ["rev-parse", "HEAD"]
  void $ turn owner ("let sourceHead = " <> literal source <> " :: Text")
  void $ turn owner "before <- snapshot"
  script owner "project_delivery_setup"
  implementer <- activation
  candidate <- checkpoint (checkActor implementer) "feature.txt" "candidate\n" "implement feature"
  void $ turn (checkActor implementer) ("respond (Produced (Candidate " <> literal candidate <> " [\"focused candidate check\"] [\"open product gate\"]))")
  script owner "project_review_start"
  reviewer <- activation
  script (checkActor reviewer) "project_review_repair"
  repair <- activation
  repairPacket <- turn (checkActor repair) "inspectFull (repairInput sessionInput, repairFindings sessionInput)"
  check "delegated repair reuses its available implementer with exact typed evidence" (checkActor repair == checkActor implementer && candidate `Text.isInfixOf` output repairPacket && "open product gate" `Text.isInfixOf` output repairPacket && "preserve the product gate" `Text.isInfixOf` output repairPacket)
  revised <- checkpoint (checkActor implementer) "feature.txt" "repaired\n" "repair feature"
  void $ turn (checkActor implementer) ("respond (Produced (Candidate " <> literal revised <> " [\"focused repair check\"] [\"open product gate\"]))")
  void $ turn (checkActor reviewer) "state <- pollWatch repaired"
  script (checkActor reviewer) "project_design_question"
  expert <- activation
  check "the tagged Astra receives the repaired source and actual uncertainty" (checkModel expert == Just "gpt-6-astra" && revised `Text.isInfixOf` checkContext expert && "Does preparation preserve the boundary?" `Text.isInfixOf` checkContext expert)
  amendment <- checkpoint (checkActor expert) "plans/feature.md" "Preparation retains the open product gate.\n" "clarify acceptance"
  void $ turn (checkActor expert) ("respond (AmendPlan (PlanAmendment " <> literal revised <> " " <> literal amendment <> " [\"plans/feature.md\"] \"retain the preparation gate\" [\"feature review\"] [\"boundary evidence\"]))")
  void $ turn (checkActor reviewer) "design <- pollWatch designReady"
  script (checkActor reviewer) "project_plan_incorporation"
  incorporation <- activation
  check "incorporation stays with its exact recipient" (checkActor incorporation == checkActor implementer)
  void $ git (checkActor implementer) ["merge", "--ff-only", amendment]
  plan <- readFile (checkActor implementer) "plans/feature.md"
  check "the accepted plan really reached the implementer's checkout" (plan == "Preparation retains the open product gate.\n")
  void $ turn (checkActor implementer) ("respond (Incorporated (incorporationAmendment sessionInput) " <> literal amendment <> " [\"read exact plan at resulting head\"])")
  void $ turn (checkActor reviewer) "incorporation <- pollWatch planReady"
  script (checkActor reviewer) "project_review_questions"
  attention <- turn owner "observedQuestions <- pollProgress reviewQuestions\ninspectFull observedQuestions"
  check "coalesced attention retains both unresolved questions" ("semantics" `Text.isInfixOf` output attention && "product-gate" `Text.isInfixOf` output attention)
  -- A separate component progresses while this review awaits its owning decision.
  void $ turn owner ("let Right otherLabel = branchLabel \"unrelated\"\nother <- unfold (taskGroup task) (child @Text (solTaskFrom otherLabel projectHead (task { taskSource = " <> literal source <> ", obligation = \"Inspect an independent consumer\" })))")
  other <- activation
  void $ turn (checkActor other) "respond (\"independent work finished\" :: Text)"
  independent <- turn owner "independent <- pollResponse (forkedResponse other)\ninspectFull independent"
  check "an unrelated obligation finishes during the pending question" ("independent work finished" `Text.isInfixOf` output independent)
  void $ turn owner ("let incorporatedHead = " <> literal amendment <> " :: Text")
  script owner "project_decision_return"
  void $ notPresented "controlled presentation failure"
  failed <- turn owner "pollRequestUpdate clarification\npollResponse (forkedResponse reviewer)"
  check "failed steering preserves the pending review and receipt" ("UpdateNotPresented" `Text.isInfixOf` output failed && "ResponsePending" `Text.isInfixOf` output failed)
  void $ turn owner "Right supported <- updateRequest (forkedResponse reviewer) (decisionContext acceptedDecision)"
  message <- present
  check "the owning return carries the exact question and incorporated source" (amendment `Text.isInfixOf` message && "semantics @" `Text.isInfixOf` message)
  void $ git (checkActor reviewer) ["merge", "--ff-only", amendment]
  propagation <- readFile (checkActor reviewer) ".shoal/checks/project_decision_consumer.hs" >>= turn (checkActor reviewer)
  check "an old answer cannot clear a changed question or rewind the task source" ("(True,True,True,True)" `Text.isInfixOf` output propagation)
  consumer <- activation
  check "the fresh consumer receives the accepted decision and rationale" (amendment `Text.isInfixOf` checkContext consumer && "Preparation retains the boundary" `Text.isInfixOf` checkContext consumer && "Why:" `Text.isInfixOf` checkContext consumer)
  consumerHead <- git (checkActor consumer) ["rev-parse", "HEAD"]
  check "the fresh consumer starts at the incorporated decision source" (consumerHead == amendment)
  remaining <- turn owner "remaining <- pollProgress reviewQuestions\ninspectFull remaining"
  check "answering one question leaves the other open" ("product-gate" `Text.isInfixOf` output remaining && not ("questionKey = \"semantics\"" `Text.isInfixOf` output remaining))
  original <- turn owner "original <- pollResponse (forkedResponse worker)\ninspectFull original"
  check "repair did not rewrite the original candidate receipt" (candidate `Text.isInfixOf` output original && not (revised `Text.isInfixOf` output original))
  void $ turn (checkActor reviewer) ("let latest = Candidate " <> literal amendment <> " [\"focused repair check\",\"incorporated plan check\"] [\"open product gate\"]\nrespond (Produced (Accepted (ReviewedCandidate assignment latest [\"reviewed revised source and plan\"] \"preparation only\")))")
  accepted <- turn owner "accepted <- pollResponse (forkedResponse reviewer)\ninspectFull accepted"
  check "independent acceptance retains the updated contract and latest source" (amendment `Text.isInfixOf` output accepted && "Preparation retains the boundary" `Text.isInfixOf` output accepted && "open product gate" `Text.isInfixOf` output accepted)
  void $ git owner ["merge", "--ff-only", amendment]
  feature <- readFile owner "feature.txt"
  combinedPlan <- readFile owner "plans/feature.md"
  check "combined source contains both repaired implementation and accepted semantics" (feature == "repaired\n" && combinedPlan == "Preparation retains the open product gate.\n")
  -- An uncertain presentation fences settlement instead of silently losing steering.
  void $ turn (checkActor reviewer) "Right uncertain <- updateRequest (forkedResponse consumer) \"Clarify the retained gate\""
  void $ unconfirmed "controlled lost presentation acknowledgement"
  uncertainty <- turn (checkActor reviewer) "pollRequestUpdate uncertain"
  check "unconfirmed presentation remains explicit" ("UpdateUnconfirmed" `Text.isInfixOf` output uncertainty)
  fenced <- turn (checkActor consumer) ("attemptReply sessionReply (Produced (Candidate " <> literal amendment <> " [] [\"open product gate\"]))")
  check "an uncertain update cannot silently settle the waiting obligation" ("ReplyUpdatePending" `Text.isInfixOf` output fenced)
  -- Feed this same repaired, accepted and incorporated work into the next-wave improvement.
  void $ turn owner "let ResponseReady reviewAnswer = accepted\nlet Produced (Accepted reviewed) = responseValue reviewAnswer\nlet delivered = Produced (Delivered reviewed (candidateCommit (reviewedCandidate reviewed)) [\"checked combined feature and plan\"]) :: Delivery\ninspectFull (deliverySummary delivered)"
  void $ turn owner "later <- snapshot\nlet packet = RsiInput (candidateCommit (reviewedCandidate reviewed)) \"Human requested: improve decision handoffs from this completed preparation.\" [] before later [deliverySummary delivered, \"Retained repair, accepted amendment, fresh consumer and failed/unconfirmed steering were exercised; no live usage measured.\"]\nlet Right improvementWave = forkGroupLabel \"requested-improvement\"\nlet Right improvementLabel = branchLabel \"workspace-style\"\nimprovement <- unfold (batch campaign improvementWave) (child (rsiBranch improvementLabel (atRef (GitRef (rsiSource packet))) packet))"
  checkImprovement owner
