import qualified Project.DecisionAnswers as Answers
import qualified Data.Text as Text
Right producer <- spawnSubagent (FreshCtx "Wait for a short work request.") (ForkWorktree projectHead)
  ((defaultSpawnOptions workspaceAgentSpec) { spawnInstructions = Just (projectPrompt "task"), spawnLabel = Just "refused-worker" })
Right (producerRequest, updates) <- requestWithProgress @WorkProgress @Text producer "No allocation" defaultRequestOptions
let refusalTask = task "refused" "Validate relay configuration" [] "No relay allocation on refusal" refusalHead
let issuedResults = R.settlement producerRequest
let issuedProgress = R.progress updates
let pairedSource name = (name, refusalTask, producer, issuedResults, issuedProgress)
beforeRefusal <- length . snapshotActors <$> snapshot
emptyRefusal <- traverse R.start (workDefinition [] (keepWork :: WorkSink Text))
blankRefusal <- traverse R.start (workDefinition [("  ", R.settlement producerRequest, R.progress updates)] (keepWork :: WorkSink Text))
duplicateRefusal <- traverse R.start (workDefinition [("same", R.settlement producerRequest, R.progress updates), ("same", R.settlement producerRequest, R.progress updates)] (keepWork :: WorkSink Text))
pairedRefusals <- mapM (traverse (\(answerSpec, workForAnswers) -> do { answers <- R.start answerSpec; routed <- R.start (workForAnswers answers); pure (answers, routed) }))
  [ Answers.answeredWorkDefinition me [] (keepWork :: WorkSink Text)
  , Answers.answeredWorkDefinition me [pairedSource "  "] (keepWork :: WorkSink Text)
  , Answers.answeredWorkDefinition me [pairedSource "same", pairedSource "same"] (keepWork :: WorkSink Text)
  , Answers.answeredWorkDefinition me [pairedSource (Text.pack (show index)) | index <- [1 .. 17 :: Int]] (keepWork :: WorkSink Text)
  ]
let pairedDefinitionsRefused = map (either Just (const Nothing)) pairedRefusals == [Just Answers.InvalidAnswerCount, Just Answers.InvalidAnswerNames, Just Answers.InvalidAnswerNames, Just Answers.InvalidAnswerCount]
let pairedValidDefinition = Answers.answeredWorkDefinition me [pairedSource "producer"] (keepWork :: WorkSink Text)
afterRefusal <- length . snapshotActors <$> snapshot
producerState <- pollResponse producerRequest
(case emptyRefusal of { Left NoSources -> True; _ -> False }) && (case blankRefusal of { Left BlankSourceName -> True; _ -> False }) && (case duplicateRefusal of { Left (DuplicateSourceName "same") -> True; _ -> False }) && beforeRefusal == afterRefusal && (case producerState of { ResponsePending _ -> True; _ -> False }) && pairedDefinitionsRefused && (case pairedValidDefinition of { Right _ -> True; _ -> False })
