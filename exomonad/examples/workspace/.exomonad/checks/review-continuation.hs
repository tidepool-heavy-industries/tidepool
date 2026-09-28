import GHC.Generics (Generic)
import qualified Tidepool.Actor as Actor
type ReviewWave = ActorHandle (WorkActor (Outcome ReviewDecision))
data RetainedReviewState = RetainedReviewState { retainedReviewCollectors :: [(Response (Outcome ReviewDecision), ReviewWave)], retainedReviewEvents :: [WorkEvent (Outcome ReviewDecision)], retainedCompletedReviews :: [WorkState (Outcome ReviewDecision)], retainedStoppedCandidates :: [Settlement (Outcome Candidate)], retainedStoppedNotices :: [Either NotificationError NotificationReceipt], retainedRepairAttempts :: [(Response (Outcome Candidate), Forwarding (Outcome Candidate))], retainedCandidateReceipts :: [Either ResponseFailure (ResponseResult (Outcome Candidate))], retainedSourceProblems :: [(ExecutionReceipt, Text)] }
instance Show RetainedReviewState where show state = "RetainedReviewState " ++ show (length (retainedReviewCollectors state), length (retainedCompletedReviews state), length (retainedReviewEvents state), length (retainedStoppedCandidates state), length (retainedStoppedNotices state), length (retainedRepairAttempts state), length (retainedSourceProblems state))
data RetainedReviewFixture mode = RetainedReviewFixture { retainedReviewState :: mode :- State RetainedReviewState, retainedReviewStarted :: mode :- Call (Response (Outcome ReviewDecision), Progress WorkProgress) NoReply, retainedReviewEvent :: mode :- Call (Response (Outcome ReviewDecision), WorkEvent (Outcome ReviewDecision)) NoReply, retainedReviewView :: mode :- Call () (R.Reply RetainedReviewState), retainedRepairStarted :: mode :- Call (Response (Outcome Candidate), Progress WorkProgress) NoReply, retainedRepairDone :: mode :- Call (Either ResponseFailure (ResponseResult (Outcome Candidate))) NoReply, retainedCandidateDone :: mode :- Event (Either ResponseFailure (ResponseResult (Outcome Candidate))) } deriving Generic
let startReview = (\own result -> do
        modify' (\state -> state { retainedCandidateReceipts = retainedCandidateReceipts state ++ [result] })
        case result of
          Right receipt | Produced candidate <- responseValue receipt -> case candidateAtSubmission candidate (responseWorktree receipt) of
            Right selected -> do
              _ <- requestWithProgressInto @WorkProgress @(Outcome ReviewDecision)
                (responseActor reviewer)
                ((assignment reviewLabel (ReviewRequest (AssignedTask task) selected repairPolicy)) { guidance = Just (projectPrompt "review"), report = Silent }) $ R.send (retainedReviewStarted (own :: RetainedReviewFixture Self))
              pure ()
            Left reason -> do
              modify' (\state -> state { retainedSourceProblems = retainedSourceProblems state ++ [(responseExecution receipt, reason)] })
              sent <- sendMessage owner reason
              modify' (\state -> state { retainedStoppedNotices = retainedStoppedNotices state ++ [sent] })
          _ -> do
            let settled = either ReplyUnavailable ReplyAvailable result
            modify' (\state -> state { retainedStoppedCandidates = retainedStoppedCandidates state ++ [settled] })
            case onStopped settled of
              Nothing -> pure ()
              Just message -> do
                sent <- sendMessage owner message
                modify' (\state -> state { retainedStoppedNotices = retainedStoppedNotices state ++ [sent] })
      ) :: RetainedReviewFixture Self -> Either ResponseFailure (ResponseResult (Outcome Candidate)) -> Handler RetainedReviewState (CoordinationEffects RetainedReviewFixture) ()
let reviewBoxDefinition = coordinationActor "review-handoff" RetainedReviewFixture
      { retainedReviewState = RetainedReviewState { retainedReviewCollectors = [], retainedReviewEvents = [], retainedCompletedReviews = [], retainedStoppedCandidates = [], retainedStoppedNotices = [], retainedRepairAttempts = [], retainedCandidateReceipts = [], retainedSourceProblems = [] }
      , retainedReviewView = \() -> get
      , retainedReviewStarted = \(attempt, updates) -> do
          own <- R.self @RetainedReviewFixture
          collector <- followWork [("review", attempt, updates)] (WorkSink $ \event -> do { R.send (retainedReviewEvent own) (attempt, event); runWorkSink onReview event })
          modify' (\state -> state { retainedReviewCollectors = retainedReviewCollectors state ++ [(attempt, collector)] })
      , retainedReviewEvent = \(attempt, event) -> do
          modify' (\state -> state { retainedReviewEvents = retainedReviewEvents state ++ [event] })
          case event of
            WorkFinished _ result -> do
              active <- gets retainedReviewCollectors
              case [collector | (response, collector) <- active, requestId response == requestId attempt] of
                [collector] -> do
                  completed <- finishWork collector
                  modify' (\state -> state { retainedReviewCollectors = [(response, retained) | (response, retained) <- retainedReviewCollectors state, requestId response /= requestId attempt], retainedCompletedReviews = case completed of { Actor.Completed value -> retainedCompletedReviews state ++ [value]; _ -> retainedCompletedReviews state } })
                _ -> error "review result has no unique owned collector"
              case (repairPolicy, result) of
                (RetainedImplementer implementer, Right receipt) | Produced (Repair candidate findings) <- responseValue receipt -> do
                  own <- R.self @RetainedReviewFixture
                  _ <- requestWithProgressInto @WorkProgress @(Outcome Candidate) implementer
                    ((assignment repairLabel (RepairTask task candidate findings)) { guidance = Just (projectPrompt "repair"), report = Silent })
                    (R.send (retainedRepairStarted own))
                  pure ()
                _ -> pure ()
            _ -> pure ()
      , retainedRepairStarted = \(attempt, _) -> do
          own <- R.self @RetainedReviewFixture
          forwarding <- R.forwardResult attempt (retainedRepairDone own)
          modify' (\state -> state { retainedRepairAttempts = retainedRepairAttempts state ++ [(attempt, forwarding)] })
      , retainedRepairDone = \result -> do
          own <- R.self @RetainedReviewFixture
          startReview own result
      , retainedCandidateDone = R.on (R.settlement worker) $ \result -> do
          own <- R.self @RetainedReviewFixture
          startReview own result
      }
reviewBox <- R.start reviewBoxDefinition
