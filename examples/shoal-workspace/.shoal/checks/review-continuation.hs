import qualified Tidepool.Actor as Actor
type ReviewWave = Actor.ActorRef (WorkInput (Outcome ReviewDecision)) (WorkState (Outcome ReviewDecision))
data ReviewFlowState = ReviewFlowState { reviewCollectors :: [ReviewWave], reviewEvents :: [WorkEvent (Outcome ReviewDecision)], stoppedCandidates :: [Settlement (Outcome Candidate)], stoppedNotices :: [Notice (Outcome Candidate)] }
instance Show ReviewFlowState where show state = "ReviewFlowState " ++ show (length (reviewCollectors state), length (reviewEvents state), length (stoppedCandidates state), length (stoppedNotices state))
data ReviewFlow result = ReviewAttached ReviewWave result | ReviewEvent (WorkEvent (Outcome ReviewDecision)) result | CandidateStopped (Settlement (Outcome Candidate)) result | StoppedNotice (Notice (Outcome Candidate)) result | ReviewFlowSnapshot (ReviewFlowState -> result)
let reviewBoxDefinition = (Actor.stateful "review-handoff" Actor.ReadOnly (\state message -> case message of
      ReviewAttached collector answer -> pure (answer, state { reviewCollectors = reviewCollectors state ++ [collector] })
      ReviewEvent event answer -> pure (answer, state { reviewEvents = reviewEvents state ++ [event] })
      CandidateStopped outcome answer -> pure (answer, state { stoppedCandidates = stoppedCandidates state ++ [outcome] })
      StoppedNotice notice answer -> pure (answer, state { stoppedNotices = stoppedNotices state ++ [notice] })
      ReviewFlowSnapshot answer -> pure (answer state, state)) :: Actor.ActorDefinition ReviewFlowState ReviewFlow ReviewFlowState)
reviewBox <- Actor.startActor reviewBoxDefinition (ReviewFlowState [] [] [] [])
let forwardReview = (\event -> do { Actor.cast reviewBox (ReviewEvent event ()); onReview event }) :: WorkSink (Outcome ReviewDecision)
reviewDispatch <- route (awaitSettledFork worker) (\settled -> case settled of
      ReplyAvailable receipt | Produced candidate <- responseValue receipt -> do
        (attempt, progress) <- reviewAgain (forkedActor reviewer) reviewLabel (ReviewTask task candidate OwnerRepairs)
        collector <- followWork [("review", attempt, progress)] forwardReview
        Actor.cast reviewBox (ReviewAttached collector ())
      _ -> do
        Actor.cast reviewBox (CandidateStopped settled ())
        case onStopped settled of
          Nothing -> pure ()
          Just message -> do
            sent <- sendMessage owner message
            let event = WorkFinished "implementation" (case settled of { ReplyAvailable receipt -> Right receipt; ReplyUnavailable failure -> Left failure })
            Actor.cast reviewBox (StoppedNotice (Notice event sent) ()))
