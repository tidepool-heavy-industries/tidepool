{-# LANGUAGE QuasiQuotes #-}
import Tidepool.Agent.Reply (pollReply)
let AssignedTask assignedTask = reviewBasis sessionInput
let ResponseReady incorporationResult = incorporation
let Incorporated amendment incorporatedHead incorporationChecks = responseValue incorporationResult
let acceptedDecision = AcceptedDecision semantics incorporatedHead "Preparation retains the boundary; visible UI acceptance remains separate." incorporationChecks
let assignedWork = withDecision acceptedDecision assignedTask
let assignment = assignedWork
let remainingQuestions = resolveQuestion acceptedDecision openQuestions
reportProgress (WorkProgress [] remainingQuestions)
let newer = semantics { questionDetails = (questionDetails semantics) { questionFinding = "A new owning consumer contradicts the earlier answer." } }
let changedQuestions = raiseQuestion newer openQuestions
inspectFull (map questionKey remainingQuestions == ["product-gate"], resolveQuestion acceptedDecision changedQuestions == changedQuestions, raiseQuestion semantics firstQuestions == firstQuestions, taskSource assignedWork == incorporatedHead)
Right consumerAgent <- spawnSubagent (FreshCtx (taskContext assignedWork)) (ForkWorktree projectHead)
  ((defaultSpawnOptions workspaceAgentSpec)
    { spawnModel = Just "executor", spawnEffort = Just Medium
    , spawnInstructions = Just (projectPrompt "task"), spawnLabel = Just (taskName assignedWork) })
Right (consumer, consumerQuestions) <- requestWithProgress @WorkProgress @(Outcome Candidate) consumerAgent assignedWork defaultRequestOptions
pollReply sessionReply
