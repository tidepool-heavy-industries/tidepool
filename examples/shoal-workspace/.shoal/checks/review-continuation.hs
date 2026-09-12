import GHC.Generics (Generic)
import qualified Tidepool.Actor as Actor
type ReviewWave = ActorHandle (WorkActor (Outcome ReviewDecision))
data ReviewFlowState = ReviewFlowState { reviewCollectors :: [(Response (Outcome ReviewDecision), ReviewWave)], reviewEvents :: [WorkEvent (Outcome ReviewDecision)], completedReviews :: [WorkState (Outcome ReviewDecision)], stoppedCandidates :: [Settlement (Outcome Candidate)], stoppedNotices :: [Either NotificationError NotificationReceipt], repairAttempts :: [(Response (Outcome Candidate), Forwarding (Outcome Candidate))], candidateReceipts :: [Either ResponseFailure (ResponseResult (Outcome Candidate))], sourceProblems :: [(ExecutionReceipt, Text)] }
instance Show ReviewFlowState where show state = "ReviewFlowState " ++ show (length (reviewCollectors state), length (completedReviews state), length (reviewEvents state), length (stoppedCandidates state), length (stoppedNotices state), length (repairAttempts state), length (sourceProblems state))
data ReviewFlow mode = ReviewFlow { reviewState :: mode :- State ReviewFlowState, reviewStarted :: mode :- Call (Response (Outcome ReviewDecision), Progress WorkProgress) NoReply, reviewEvent :: mode :- Call (Response (Outcome ReviewDecision), WorkEvent (Outcome ReviewDecision)) NoReply, reviewView :: mode :- Call () (R.Reply ReviewFlowState), repairStarted :: mode :- Call (Response (Outcome Candidate), Progress WorkProgress) NoReply, repairDone :: mode :- Call (Either ResponseFailure (ResponseResult (Outcome Candidate))) NoReply, candidateDone :: mode :- Event (Either ResponseFailure (ResponseResult (Outcome Candidate))) } deriving Generic
let startReview = (\own result -> do
        modify' (\state -> state { candidateReceipts = candidateReceipts state ++ [result] })
        case result of
          Right receipt | Produced candidate <- responseValue receipt -> case candidateAtSubmission candidate (responseWorktree receipt) of
            Right selected -> do
              _ <- requestWithProgressInto @WorkProgress @(Outcome ReviewDecision)
                (responseActor reviewer)
                ((assignment reviewLabel (ReviewTask task selected repairPolicy)) { guidance = Just (projectPrompt "review"), report = Silent }) $ R.send (reviewStarted (own :: ReviewFlow Self))
              pure ()
            Left reason -> do
              modify' (\state -> state { sourceProblems = sourceProblems state ++ [(responseExecution receipt, reason)] })
              sent <- sendMessage owner reason
              modify' (\state -> state { stoppedNotices = stoppedNotices state ++ [sent] })
          _ -> do
            let settled = either ReplyUnavailable ReplyAvailable result
            modify' (\state -> state { stoppedCandidates = stoppedCandidates state ++ [settled] })
            case onStopped settled of
              Nothing -> pure ()
              Just message -> do
                sent <- sendMessage owner message
                modify' (\state -> state { stoppedNotices = stoppedNotices state ++ [sent] })
      ) :: ReviewFlow Self -> Either ResponseFailure (ResponseResult (Outcome Candidate)) -> Handler ReviewFlowState (CoordinationEffects ReviewFlow) ()
let reviewBoxDefinition = coordinationActor "review-handoff" ReviewFlow
      { reviewState = ReviewFlowState { reviewCollectors = [], reviewEvents = [], completedReviews = [], stoppedCandidates = [], stoppedNotices = [], repairAttempts = [], candidateReceipts = [], sourceProblems = [] }
      , reviewView = \() -> get
      , reviewStarted = \(attempt, updates) -> do
          own <- R.self @ReviewFlow
          collector <- followWork [("review", attempt, updates)] (\event -> do { R.send (reviewEvent own) (attempt, event); onReview event })
          modify' (\state -> state { reviewCollectors = reviewCollectors state ++ [(attempt, collector)] })
      , reviewEvent = \(attempt, event) -> do
          modify' (\state -> state { reviewEvents = reviewEvents state ++ [event] })
          case event of
            WorkFinished _ result -> do
              active <- gets reviewCollectors
              case [collector | (response, collector) <- active, requestId response == requestId attempt] of
                [collector] -> do
                  completed <- finishWork collector
                  modify' (\state -> state { reviewCollectors = [(response, retained) | (response, retained) <- reviewCollectors state, requestId response /= requestId attempt], completedReviews = case completed of { Actor.Completed value -> completedReviews state ++ [value]; _ -> completedReviews state } })
                _ -> error "review result has no unique owned collector"
              case (repairPolicy, result) of
                (RetainedImplementer implementer, Right receipt) | Produced (Repair candidate findings) <- responseValue receipt -> do
                  own <- R.self @ReviewFlow
                  _ <- requestWithProgressInto @WorkProgress @(Outcome Candidate) implementer
                    ((assignment repairLabel (RepairTask task candidate findings)) { guidance = Just (projectPrompt "repair"), report = Silent })
                    (R.send (repairStarted own))
                  pure ()
                _ -> pure ()
            _ -> pure ()
      , repairStarted = \(attempt, _) -> do
          own <- R.self @ReviewFlow
          forwarding <- R.forwardResult attempt (repairDone own)
          modify' (\state -> state { repairAttempts = repairAttempts state ++ [(attempt, forwarding)] })
      , repairDone = \result -> do
          own <- R.self @ReviewFlow
          startReview own result
      , candidateDone = R.on (R.settlement worker) $ \result -> do
          own <- R.self @ReviewFlow
          startReview own result
      }
reviewBox <- R.start reviewBoxDefinition
