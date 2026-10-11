{-# LANGUAGE QuasiQuotes #-}
import qualified Tidepool.Actor.Record as R
let leftTask = task "left" "Investigate the left boundary" ["src/left.rs"] "Return findings with references; no code candidate required" sourceHead
let rightTask = task "right" "Investigate the right boundary" ["src/right.rs"] "Return findings with references; no code candidate required" sourceHead
-- Descendants use the explicit retained prefix; the root receives task guidance.
Right localCheckpoint <- Tidepool.Actors.Exomonad.checkpoint "recursive-requests"
let childContext work = if phaseName == "first" then FreshCtx (taskContext work) else ForkCtx localCheckpoint
let sink :: WorkSink (Outcome Text)
    sink = notifyWork me (workMessage (\result -> T.pack (show (result :: Outcome Text))))
Right leftAgent <- spawnSubagent (childContext leftTask) (ForkWorktree currentCheckout)
  ((defaultSpawnOptions workspaceAgentSpec)
    { spawnModel = Just (Alias "luna"), spawnEffort = Just Medium
    , spawnInstructions = Just (projectPrompt "task"), spawnLabel = Just (taskName leftTask) })
Right (leftRequest, leftProgress) <- requestWithProgress @WorkProgress @(Outcome Text) leftAgent leftTask
  (defaultRequestOptions { requestReporting = Silent })
Right rightAgent <- spawnSubagent (childContext rightTask) (ForkWorktree currentCheckout)
  ((defaultSpawnOptions workspaceAgentSpec)
    { spawnModel = Just (Alias "luna"), spawnEffort = Just Medium
    , spawnInstructions = Just (projectPrompt "task"), spawnLabel = Just (taskName rightTask) })
Right (rightRequest, rightProgress) <- requestWithProgress @WorkProgress @(Outcome Text) rightAgent rightTask
  (defaultRequestOptions { requestReporting = Silent })
(collection, answerer) <- if phaseName == "subcomponents" then do
  Right (answerSpec, workForAnswers) <- pure (answeredWorkDefinition me
    [("left", leftTask, leftAgent, R.settlement leftRequest, R.progress leftProgress),
     ("right", rightTask, rightAgent, R.settlement rightRequest, R.progress rightProgress)] sink)
  answers <- R.start answerSpec
  admitted <- R.start (workForAnswers answers)
  pure (admitted, Just answers)
  else do
    Right admitted <- traverse R.start (workDefinition [("left", R.settlement leftRequest, R.progress leftProgress), ("right", R.settlement rightRequest, R.progress rightProgress)] sink)
    pure (admitted, Nothing)
releaseCheckpoint localCheckpoint
