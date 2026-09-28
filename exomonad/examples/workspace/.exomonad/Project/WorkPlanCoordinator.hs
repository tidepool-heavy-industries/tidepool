{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeFamilies #-}
{-# LANGUAGE TypeOperators #-}

-- | Event-driven interpreter for an authored WorkPlan. The actor owns each
-- request it submits. Routing, ReviewFlow, CheckResults, and Merge continue to
-- own their respective histories and checks.
module Project.WorkPlanCoordinator
  ( Coordinator (beginPlan, planView, answerQuestion, correctQuestion, observeCorrection, reportIncorporation, closePlan)
  , PlanState
  , PlanView (..), ActiveDevelopment (..), CoordinatorStart (..), QuestionReply (..)
  , PlanClose (..), CorrectionReply (..), CorrectionState (..)
  , coordinator
  ) where

import Control.Monad.Freer (Eff, Member)
import Control.Monad (void)
import Data.Text (Text)
import qualified Data.Text as Text
import qualified Tidepool.Command as Cmd
import Tidepool.Actors.Worktree (boundWorktree)
import GHC.Generics (Generic)
import qualified Tidepool.Actor as Actor
import qualified Tidepool.Actor.Record as R
import Tidepool.Actors.Exomonad
import Tidepool.Effects.Core (Actor, WorktreeHandle (..))
import Tidepool.Effects.Row (knownEffects)
import Tidepool.Worktree (WorktreeReceipt (..), renderGitOid, renderWorktreeId)
import Project.CheckResults (CheckState, finishChecks)
import Project.BaselineIncorporation
  ( BaselineChange (..), Affected (..), validateBaselineFor, incorporationUpdate )
import Project.FocusedGateExample
  ( PlanReport (..), planPassed, planSummary )
import qualified Project.FocusedGateExample as Focused
import qualified Project.Merge as Merge
import Project.ReviewFlow
  ( ReviewFlow, ReviewCompletion (..), ReviewFlowState (..), ReviewFlowPolicy (..)
  , checkedReviewFlow, firstCandidate, reviewSnapshot, semanticReviewChoice )
import Project.Routing
  ( WorkActor, WorkEvent (..), WorkState (..), WorkSource (..)
  , followWork, readWork )
import Project.Types hiding (reviewChecks)
import Project.Work (candidateAtSubmission, decisionContext, projectPrompt)
import Project.WorkPlan
import Project.WorkPlanVerification
  ( VerificationRunner, startExactVerification )

data CoordinatorStart = PlanStarted | PlanAlreadyStarted | PlanUnauthorized
  deriving (Show, Eq)

data QuestionReply
  = QuestionUnauthorized
  | QuestionRefused Text
  | QuestionSent (Either NotificationError NotificationReceipt)
  deriving (Show)

data PlanClose
  = PlanCloseUnauthorized
  | PlanClosePending
  | PlanClosed [Either CheckpointRefusal ()]
  deriving (Show)

data CorrectionReply
  = CorrectionUnauthorized
  | CorrectionRefused Text
  | CorrectionAccepted RequestUpdate
  deriving (Show)

data CorrectionState = CorrectionState
  { correctionRequest :: RequestId
  , correctionChange :: BaselineChange
  , correctionUpdate :: Either ReplyError RequestUpdate
  , correctionDelivery :: Maybe (Either ReplyError RequestUpdateState)
  , correctionReported :: Maybe Incorporation
  , correctionReportRefusal :: Maybe Text
  , correctionExpectedChecks :: [Text]
  } deriving (Show)

data ActiveDevelopment = ActiveDevelopment
  { activeResponse :: Response (Outcome Candidate)
  , activeProgress :: Progress WorkProgress
  , activeRouter :: R.ActorHandle (WorkActor (Outcome Candidate))
  , activeTask :: Task
  , activeScopes :: [ComponentScope]
  }

data ActiveReview root where
  ActiveReview
    :: Int
    -> R.ActorHandle ReviewFlow
    -> R.ActorHandle (CompletionRelay ReviewCompletion)
    -> Developed
    -> (Reviewed -> WorkPlan result)
    -> (Either PlanFailure result -> PlanHandler root ())
    -> ActiveReview root

data ActiveVerification root where
  ActiveVerification
    :: Int
    -> R.ActorHandle (CompletionRelay CheckState)
    -> R.ActorHandle VerificationRunner
    -> Verification value
    -> CheckedSource
    -> Focused.PlanStart
    -> (AcceptedSource value -> WorkPlan result)
    -> (Either PlanFailure result -> PlanHandler root ())
    -> ActiveVerification root

data PlanState root = PlanState
  { planOwner :: AgentRef
  , planNamespace :: Text
  , planInitial :: WorkPlan root
  , planStarted :: Bool
  , planOutcome :: Maybe (Either PlanFailure root)
  , planDevelopments :: [ActiveDevelopment]
  , planReviews :: [ActiveReview root]
  , planHandledDevelopments :: [RequestId]
  , planHandledReviews :: [Int]
  , planVerifications :: [ActiveVerification root]
  , planNextTicket :: Int
  , planQuestions :: [(RequestId, QuestionReply)]
  , planCompletionRoute :: Maybe (R.Send (Either PlanFailure root))
  , planCompletionAdmission :: Maybe (Either Text ())
  , planJoins :: [SomeJoin root]
  , planCheckpoints :: [ContextCheckpoint]
  , planCheckpointReleases :: Maybe [Either CheckpointRefusal ()]
  , planCorrection :: Maybe CorrectionState
  , planAuxClosed :: Bool
  }

data PlanView root = PlanView
  { viewedOutcome :: Maybe (Either PlanFailure root)
  , viewedDevelopments :: [ActiveDevelopment]
  , viewedReviewCount :: Int
  , viewedQuestionReplies :: [(RequestId, QuestionReply)]
  , viewedCompletionAdmission :: Maybe (Either Text ())
  , viewedReviews :: [R.ActorHandle ReviewFlow]
  , viewedCheckpointReleases :: Maybe [Either CheckpointRefusal ()]
  , viewedCorrection :: Maybe CorrectionState
  }

data DevelopmentStarted root where
  DevelopmentStarted
    :: [ComponentScope] -> Task -> Response (Outcome Candidate) -> Progress WorkProgress
    -> (Developed -> WorkPlan result)
    -> (Either PlanFailure result -> PlanHandler root ())
    -> DevelopmentStarted root

data DevelopmentFinished root where
  DevelopmentFinished
    :: Task -> Response (Outcome Candidate) -> Progress WorkProgress
    -> Either ResponseFailure (ResponseResult (Outcome Candidate))
    -> (Developed -> WorkPlan result)
    -> (Either PlanFailure result -> PlanHandler root ())
    -> DevelopmentFinished root

data PlanResume root where
  PlanResume :: Either PlanFailure (WorkPlan result)
    -> (Either PlanFailure result -> PlanHandler root ())
    -> PlanResume root

data JoinState left right = JoinState
  { joinedLeft :: Maybe (Either PlanFailure left)
  , joinedRight :: Maybe (Either PlanFailure right)
  , joinAdmission :: Maybe (Either Text ())
  }

data Join root left right result mode = Join
  { joinState :: mode :- State (JoinState left right)
  , joinLeft :: mode :- Call (Either PlanFailure left) NoReply
  , joinRight :: mode :- Call (Either PlanFailure right) NoReply
  , joinView :: mode :- Call () (R.Reply (JoinState left right))
  } deriving Generic

type JoinEffects root left right result =
  R.LocalEffects (Join root left right result) '[Actor]

data SomeJoin root where
  SomeJoin :: R.ActorHandle (Join root left right result) -> SomeJoin root

data CompletionRelay payload mode = CompletionRelay
  { relayState :: mode :- State (Int, R.Send (Int, payload), Maybe (Either Text ()))
  , relayForward :: mode :- Call payload NoReply
  } deriving Generic

type RelayEffects payload = R.LocalEffects (CompletionRelay payload) '[Actor]

completionRelay :: Int -> R.Send (Int, payload) -> R.ActorSpec (CompletionRelay payload) (RelayEffects payload)
completionRelay ticket destination =
  R.definition "work-plan-completion-relay" (Actor.Selected knownEffects) CompletionRelay
    { relayState = (ticket, destination, Nothing)
    , relayForward = \payload -> do
        admission <- R.trySend destination (ticket, payload)
        R.modify' (\(key, route, _) -> (key, route, Just admission))
    }

joinDefinition
  :: forall root left right result.
     R.Send (PlanResume root)
  -> ((left, right) -> WorkPlan result)
  -> (Either PlanFailure result -> PlanHandler root ())
  -> ActorSpec (Join root left right result) (JoinEffects root left right result)
joinDefinition destination next complete =
  R.definition "work-plan-join" (Actor.Selected knownEffects) Join
    { joinState = JoinState Nothing Nothing Nothing
    , joinLeft = \answer -> do
        R.modify' (\state -> state { joinedLeft = Just answer })
        publishPair
    , joinRight = \answer -> do
        R.modify' (\state -> state { joinedRight = Just answer })
        publishPair
    , joinView = \() -> R.get
    }
  where
    publishPair = do
      state <- R.get
      case (joinAdmission state, joinedLeft state, joinedRight state) of
        (Nothing, Just left, Just right) -> do
          let resolved = case (left, right) of
                (Right a, Right b) -> Right (next (a, b))
                (Left a, Left b) -> Left (ParallelStopped [a, b])
                (Left a, _) -> Left (ParallelStopped [a])
                (_, Left b) -> Left (ParallelStopped [b])
          admission <- R.trySend destination (PlanResume resolved complete)
          R.modify' (\current -> current { joinAdmission = Just admission })
        _ -> pure ()

data Coordinator root mode = Coordinator
  { coordinatorState :: mode :- State (PlanState root)
  , beginPlan :: mode :- Call () (R.Reply CoordinatorStart)
  , planView :: mode :- Call () (R.Reply (Maybe (PlanView root)))
  , answerQuestion :: mode :- Call (Response (Outcome Candidate), Question, AcceptedDecision) (R.Reply QuestionReply)
  , correctQuestion :: mode :- Call (Response (Outcome Candidate), Question, BaselineChange, [Text]) (R.Reply CorrectionReply)
  , observeCorrection :: mode :- Call () (R.Reply (Maybe CorrectionState))
  , reportIncorporation :: mode :- Call Incorporation NoReply
  , closePlan :: mode :- Call () (R.Reply PlanClose)
  , developmentStarted :: mode :- Call (DevelopmentStarted root) NoReply
  , developmentFinished :: mode :- Call (DevelopmentFinished root) NoReply
  , reviewFinished :: mode :- Call (Int, ReviewCompletion) NoReply
  , verificationFinished :: mode :- Call (Int, CheckState) NoReply
  , resumePlan :: mode :- Call (PlanResume root) NoReply
  } deriving Generic

type PlanEffects root = R.LocalEffects (Coordinator root) CodingEffects
type PlanHandler root value = Handler (PlanState root) (PlanEffects root) value

-- | The supplied checkout makes this a coding actor, so it can admit nested
-- coding workers. Parallel branches execute in this same actor; they do not
-- create a second checkout owner.
coordinator
  :: forall effects result. Member Actor effects
  => WorktreeId -> AgentRef -> [ContextCheckpoint] -> WorkPlan result
  -> Maybe (R.Send (Either PlanFailure result))
  -> Eff effects (R.ActorHandle (Coordinator result))
coordinator checkout owner checkpoints plan route = R.start $ R.withWorktree checkout $
  R.definition "work-plan" (Actor.Selected knownEffects) Coordinator
    { coordinatorState = PlanState owner (renderWorktreeId checkout) plan False Nothing [] [] [] [] [] 0 [] route Nothing [] checkpoints Nothing Nothing False
    , beginPlan = \() -> do
        state <- R.get
        authorized <- isOwner @result state
        if not authorized then pure PlanUnauthorized
        else if planStarted state then pure PlanAlreadyStarted
        else do
          R.modify' (\current -> current { planStarted = True })
          own <- R.self @(Coordinator result)
          drive own (planInitial state) (finishPlan own)
          pure PlanStarted
    , planView = \() -> do
        state <- R.get
        authorized <- isOwner @result state
        pure $ if authorized then Just PlanView
          { viewedOutcome = planOutcome state
          , viewedDevelopments = planDevelopments state
          , viewedReviewCount = length (planReviews state)
          , viewedQuestionReplies = planQuestions state
          , viewedCompletionAdmission = planCompletionAdmission state
          , viewedReviews = [flow | ActiveReview _ flow _ _ _ _ <- planReviews state]
          , viewedCheckpointReleases = planCheckpointReleases state
          , viewedCorrection = planCorrection state
          } else Nothing
    , closePlan = \() -> do
        state <- R.get
        authorized <- isOwner @result state
        if not authorized then pure PlanCloseUnauthorized
        else case planOutcome state of
          Nothing -> pure PlanClosePending
          Just _ -> case planCheckpointReleases state of
            Just released -> do
              if planAuxClosed state then pure () else do
                mapM_ (void . R.finish . activeRouter) (planDevelopments state)
                mapM_ (\(SomeJoin join) -> void (R.finish join)) (planJoins state)
                mapM_ (\(ActiveReview _ _ relay _ _ _) -> void (R.finish relay)) (planReviews state)
                R.modify' (\current -> current { planAuxClosed = True })
              pure (PlanClosed released)
            Nothing -> pure PlanClosePending
    , answerQuestion = \(response, question, decision) -> do
        state <- R.get
        authorized <- isOwner @result state
        if not authorized then pure QuestionUnauthorized else do
          let matching = [active | active <- planDevelopments state,
                requestId (activeResponse active) == requestId response]
          case matching of
            [active] -> do
              work <- readWork (activeRouter active)
              let current = [item | source <- collectedWork work,
                    item <- workQuestions (sourceProgress source)]
              observed <- pollResponse response
              if question `notElem` current then pure (QuestionRefused "question is not current")
              else if decisionQuestion decision /= question then pure (QuestionRefused "answer names another question")
              else if decisionSource decision /= taskSource (activeTask active) then
                pure (QuestionRefused "answer source differs from the active task")
              else case observed of
                ResponsePending _ -> deliverDecision response decision
                ResponseStarting _ -> deliverDecision response decision
                _ -> pure (QuestionRefused "development request settled")
            _ -> pure (QuestionRefused "request is not an active development")
    , correctQuestion = \(response, question, change, checks) -> do
        state <- R.get
        authorized <- isOwner @result state
        if not authorized then pure CorrectionUnauthorized
        else if maybe False (const True) (planCorrection state) then
          pure (CorrectionRefused "one correction episode is already recorded")
        else case [active | active <- planDevelopments state,
          requestId (activeResponse active) == requestId response] of
          [active] -> do
            work <- readWork (activeRouter active)
            let current = [item | source <- collectedWork work,
                  item <- workQuestions (sourceProgress source)]
                valid = do
                  if question `elem` current then Right ()
                    else Left "question is not current"
                  validateBaselineFor change (activeTask active) question checks
                  mapM_ (\scope -> case checkComponentAmendment scope
                    (baselineAmendment change) of
                      Left failure -> Left (Text.pack (show failure))
                      Right () -> Right ()) (activeScopes active)
                admitCorrection = do
                  let target = Affected "coordinator" (activeResponse active)
                        (responseActor (activeResponse active)) (activeTask active) question checks
                  updated <- updateRequest response (incorporationUpdate change target)
                  case updated of
                    Left refused -> do
                      R.modify' (\current -> current { planCorrection = Just
                        (CorrectionState (requestId response) change (Left refused)
                          Nothing Nothing Nothing checks) })
                      pure (CorrectionRefused (Text.pack (show refused)))
                    Right update -> do
                      R.modify' (\current -> current { planCorrection = Just
                        (CorrectionState (requestId response) change (Right update)
                          Nothing Nothing Nothing checks) })
                      pure (CorrectionAccepted update)
            observed <- pollResponse response
            case (valid, observed) of
              (Left reason, _) -> pure (CorrectionRefused reason)
              (_, ResponsePending _) -> admitCorrection
              (_, ResponseStarting _) -> admitCorrection
              _ -> pure (CorrectionRefused "development request settled")
          _ -> pure (CorrectionRefused "request is not an active development")
    , observeCorrection = \() -> do
        state <- R.get
        authorized <- isOwner @result state
        if not authorized then pure Nothing
        else case planCorrection state of
          Nothing -> pure Nothing
          Just correction -> do
            case correctionUpdate correction of
              Left _ -> pure (Just correction)
              Right update -> do
                observed <- pollRequestUpdate update
                let refreshed = correction { correctionDelivery = Just observed }
                R.modify' (\current -> current { planCorrection = Just refreshed })
                pure (Just refreshed)
    , reportIncorporation = \report -> do
        state <- R.get
        origin <- R.sender @(Coordinator result)
        case planCorrection state of
          Nothing -> pure ()
          Just correction | Left _ <- correctionUpdate correction -> pure ()
          Just correction -> do
            let matching = [active | active <- planDevelopments state,
                  requestId (activeResponse active) == correctionRequest correction]
            case matching of
              [active] | correctionRequest correction `notElem` planHandledDevelopments state
                && origin == ActorMessageFrom
                (agentIdentity (responseActor (activeResponse active))) ->
                  case report of
                    Incorporated amendment source checks
                      | amendment == baselineAmendment (correctionChange correction)
                        && source == baselineAfter (correctionChange correction)
                        && all (`elem` checks) (correctionExpectedChecks correction) -> do
                          R.modify' (\current -> current { planCorrection = Just
                            (correction { correctionReported = Just report }) })
                          pure ()
                    IncorporationBlocked amendment _ _
                      | amendment == baselineAmendment (correctionChange correction) -> do
                          R.modify' (\current -> current { planCorrection = Just
                            (correction { correctionReported = Just report
                              , correctionReportRefusal = Just "worker reported blocked incorporation" }) })
                          pure ()
                    _ -> R.modify' (\current -> current { planCorrection = Just
                      (correction { correctionReportRefusal = Just
                        "incorporation names another amendment, baseline, or checks" }) })
              _ -> pure ()
    , developmentStarted = \(DevelopmentStarted scopes task response progress next complete) -> do
        own <- R.self @(Coordinator result)
        router <- followWork [("worker", response, progress)] $ \event -> case event of
          WorkFinished _ receipt -> do
            accepted <- R.trySend (developmentFinished own)
              (DevelopmentFinished task response progress receipt next complete)
            case accepted of
              Left reason -> Just <$> sendMessage owner
                ("work-plan result route refused for " <> Text.pack (show (requestId response)) <> ": " <> reason)
              Right () -> pure Nothing
          _ -> pure Nothing
        R.modify' (\state -> state { planDevelopments = planDevelopments state
          ++ [ActiveDevelopment response progress router task scopes] })
    , developmentFinished = \(DevelopmentFinished task response progress result next complete) -> do
        active <- R.gets planDevelopments
        handled <- R.gets planHandledDevelopments
        if requestId response `notElem` handled
          && any ((== requestId response) . requestId . activeResponse) active then do
          R.modify' (\state -> state
            { planHandledDevelopments = planHandledDevelopments state ++ [requestId response] })
          developed <- case result of
            Left failure -> pure (Left (AdmissionRefused DevelopmentNode (Text.pack (show failure))))
            Right receipt
              | executionRequest (responseExecution receipt) /= requestId response ->
                  pure (Left (SourceRefused DevelopmentNode "response request identity differs"))
              | Blocked reason evidence <- responseValue receipt -> pure (Left (WorkerBlocked reason evidence))
              | Produced candidate <- responseValue receipt -> case candidateAtSubmission candidate (responseWorktree receipt) of
                  Left reason -> pure (Left (SourceRefused DevelopmentNode reason))
                  Right exact -> do
                    accepted <- acceptCorrection response exact
                    pure (Developed task exact response progress receipt <$ accepted)
          settled <- R.gets planOutcome
          case settled of
            Just _ -> releaseIfQuiescent
            Nothing -> case developed of
              Left failure -> complete (Left failure)
              Right value -> do
                own <- R.self @(Coordinator result)
                drive own (next value) complete
        else pure ()
    , reviewFinished = \completion -> do
        own <- R.self @(Coordinator result)
        continueReview own completion
    , verificationFinished = \checks -> do
        own <- R.self @(Coordinator result)
        continueVerification own checks
    , resumePlan = \(PlanResume resumed complete) -> do
        settled <- R.gets planOutcome
        case settled of
          Just _ -> releaseIfQuiescent
          Nothing -> case resumed of
            Left failure -> complete (Left failure)
            Right plan -> do
              own <- R.self @(Coordinator result)
              drive own plan complete
    }
  where
    deliverDecision response decision = do
      sent <- sendMessage (responseActor response) (decisionContext decision)
      R.modify' (\state -> state { planQuestions = planQuestions state
        ++ [(requestId response, QuestionSent sent)] })
      pure (QuestionSent sent)

isOwner :: forall root. PlanState root -> PlanHandler root Bool
isOwner state = do
  origin <- R.sender @(Coordinator root)
  pure $ origin == ActorMessageFrom (agentIdentity (planOwner state))

finishPlan :: Coordinator root R.Self -> Either PlanFailure root -> PlanHandler root ()
finishPlan _ result = do
  prior <- R.gets planOutcome
  case prior of
    Just _ -> pure ()
    Nothing -> do
      R.modify' (\state -> state { planOutcome = Just result })
      releaseIfQuiescent
      route <- R.gets planCompletionRoute
      case route of
        Nothing -> pure ()
        Just destination -> do
          admission <- R.trySend destination result
          R.modify' (\state -> state { planCompletionAdmission = Just admission })

releaseIfQuiescent :: PlanHandler root ()
releaseIfQuiescent = do
  state <- R.get
  let reviewsSettled = all (\(ActiveReview ticket _ _ _ _ _) ->
        ticket `elem` planHandledReviews state) (planReviews state)
      verificationSettled = null (planVerifications state)
  case (planOutcome state, planCheckpointReleases state) of
    (Just _, Nothing)
      | all (\active -> requestId (activeResponse active)
          `elem` planHandledDevelopments state) (planDevelopments state)
          && reviewsSettled
          && verificationSettled -> do
          released <- mapM releaseCheckpoint (planCheckpoints state)
          R.modify' (\current -> current { planCheckpointReleases = Just released })
    _ -> pure ()

-- A corrected request cannot advance on the worker's HEAD alone. The worker
-- must have received the update, reported the exact incorporation, and
-- submitted a commit descending from the accepted baseline.
acceptCorrection
  :: Response (Outcome Candidate) -> Candidate
  -> PlanHandler root (Either PlanFailure ())
acceptCorrection response candidate = do
  correction <- R.gets planCorrection
  case correction of
    Just current | correctionRequest current == requestId response ->
      case correctionUpdate current of
        Left _ -> pure (Left (SourceRefused DevelopmentNode
          "accepted correction update was refused"))
        Right update -> do
          observed <- pollRequestUpdate update
          R.modify' (\state -> state { planCorrection = Just
            (current { correctionDelivery = Just observed }) })
          case observed of
            Right UpdatePresented -> case correctionReported current of
              Just (Incorporated amendment source checks)
                | correctionReportRefusal current == Nothing
                  && amendment == baselineAmendment (correctionChange current)
                  && source == baselineAfter (correctionChange current)
                  && all (`elem` checks) (correctionExpectedChecks current) ->
                    checkAcceptedAncestor (baselineAfter (correctionChange current))
                      (candidateCommit candidate)
              _ -> pure (Left (SourceRefused DevelopmentNode
                "accepted correction lacks an exact incorporation report"))
            _ -> pure (Left (SourceRefused DevelopmentNode
              "accepted correction was not presented to the worker"))
    _ -> pure (Right ())

checkAcceptedAncestor :: GitOid -> GitOid -> PlanHandler root (Either PlanFailure ())
checkAcceptedAncestor ancestor candidate = do
  bound <- boundWorktree
  case bound of
    Left failure -> pure (Left (SourceRefused DevelopmentNode
      ("coordinator checkout unavailable: " <> Text.pack (show failure))))
    Right handle -> do
      checked <- Cmd.run (Cmd.inDirectory (cwd (handleReceipt handle))
        (Cmd.argv ["git", "merge-base", "--is-ancestor",
          renderGitOid ancestor, renderGitOid candidate]))
      pure $ case (Cmd.commandOutcome (Cmd.commandResult checked),
                   Cmd.commandCleanup (Cmd.commandResult checked)) of
        (Cmd.CommandExited 0, Cmd.CommandClean) -> Right ()
        (Cmd.CommandExited 1, Cmd.CommandClean) -> Left (SourceRefused DevelopmentNode
          "candidate does not descend from the accepted baseline")
        _ -> Left (SourceRefused DevelopmentNode
          "accepted baseline ancestry could not be verified")

drive
  :: forall root value. Coordinator root R.Self -> WorkPlan value
  -> (Either PlanFailure value -> PlanHandler root ())
  -> PlanHandler root ()
drive own plan complete = case stepPlan plan of
  Finished value -> complete (Right value)
  NeedDevelopment scopes input next -> case checkDevelopment scopes input of
    Left failure -> complete (Left failure)
    Right task -> case input of
      RetainedWorker worker label _ -> do
        _ <- requestWithProgressInto @WorkProgress @(Outcome Candidate) worker
          ((assignment label task) { guidance = Just (projectPrompt "task"), report = Silent })
          (\(response, progress) ->
            R.send (developmentStarted own)
              (DevelopmentStarted scopes task response progress next complete))
        pure ()
      ForkWorker _ makeBranch -> do
        attempted <- attemptUnfold (taskGroup task) $
          childWithProgress @WorkProgress @(Outcome Candidate)
            (makeBranch (WorkerAssignment task (reportIncorporation own)))
        case attempted of
          Left reason -> complete (Left (AdmissionRefused DevelopmentNode (Text.pack (show reason))))
          Right (response, progress) ->
            R.send (developmentStarted own)
              (DevelopmentStarted scopes task response progress next complete)
  NeedReview scopes spec developed next -> case checkReviewScope scopes spec of
    Left failure -> complete (Left failure)
    Right policy -> do
      owner <- R.gets planOwner
      ticket <- freshTicket
      relay <- R.start (completionRelay ticket (reviewFinished own))
      let policy' = policy { flowCompleted = Just (relayForward (R.client relay)) }
      flow <- R.start (R.withWorktree (reviewCheckout spec)
        (checkedReviewFlow owner (reviewTask spec) policy'
          (developedRequest developed) (reviewChecks spec) semanticReviewChoice))
      route <- R.forwardResult (developedRequest developed)
        (firstCandidate (R.client flow))
      R.modify' (\state -> state { planReviews = planReviews state ++
        [ActiveReview ticket flow relay developed next complete] })
      route `seq` pure ()
  NeedIntegration _ spec reviewed next -> do
    let source = responseWorktree (developedReceipt (reviewedDevelopment reviewed))
    case source of
      WorktreeObserved receipt _ _ -> do
        let candidate = checkpointCandidate (reviewedProof reviewed)
            target = Merge.mergeActor (integrationTarget spec)
            request = Merge.PublishRequest
              (integrationTaskName spec) (treeId receipt)
              (candidateCommit candidate) (integrationMessage spec)
        published <- R.call (Merge.publish (R.client target)) request
        case published of
          Merge.Published headOid previous _ ->
            drive own (next (CheckedSource reviewed headOid previous published)) complete
          other -> complete (Left (IntegrationStopped other))
      _ -> complete (Left (SourceRefused IntegrationNode "candidate worktree evidence is unavailable"))
  NeedVerification _ spec checked next -> do
    if null (verificationChecks spec) then
      complete (Left (VerificationStopped "no product checks declared"))
    else do
      ticket <- freshTicket
      relay <- R.start (completionRelay ticket (verificationFinished own))
      namespace <- R.gets planNamespace
      started <- startExactVerification (checkedHead checked) namespace ticket
        (verificationChecks spec) (relayForward (R.client relay))
      case started of
        Left reason -> do
          void (R.finish relay)
          complete (Left (VerificationStopped reason))
        Right (planStart, running) -> R.modify' (\state -> state
          { planVerifications = planVerifications state ++
              [ActiveVerification ticket relay running spec checked planStart next complete] })
  NeedParallel _ left right next -> do
    join <- R.start (joinDefinition (resumePlan own) next complete)
    R.modify' (\state -> state { planJoins = planJoins state ++ [SomeJoin join] })
    drive own left $ \answer -> do
      admission <- R.trySend (joinLeft (R.client join)) answer
      case admission of
        Right () -> pure ()
        Left reason -> complete (Left (AdmissionRefused DevelopmentNode
          ("parallel left join refused: " <> reason)))
    drive own right $ \answer -> do
      admission <- R.trySend (joinRight (R.client join)) answer
      case admission of
        Right () -> pure ()
        Left reason -> complete (Left (AdmissionRefused DevelopmentNode
          ("parallel right join refused: " <> reason)))

checkDevelopment :: [ComponentScope] -> Development -> Either PlanFailure Task
checkDevelopment scopes input = do
  let task = case input of
        RetainedWorker _ _ current -> current
        ForkWorker current _ -> current
  mapM_ (`checkComponentTask` task) scopes
  pure task

checkReviewScope :: [ComponentScope] -> ReviewSpec -> Either PlanFailure ReviewFlowPolicy
checkReviewScope scopes spec = do
  mapM_ (`checkComponentTask` reviewTask spec) scopes
  limit <- effectiveRepairLimit scopes
  pure ((reviewPolicy spec) { flowRepairLimit = min limit (flowRepairLimit (reviewPolicy spec)) })

freshTicket :: PlanHandler root Int
freshTicket = do
  ticket <- R.gets planNextTicket
  R.modify' (\state -> state { planNextTicket = ticket + 1 })
  pure ticket

continueReview :: Coordinator root R.Self -> (Int, ReviewCompletion) -> PlanHandler root ()
continueReview own (ticket, completion) = do
  reviews <- R.gets planReviews
  handled <- R.gets planHandledReviews
  case [active | active@(ActiveReview key _ _ _ _ _) <- reviews, key == ticket] of
    [ActiveReview _ flow relay developed next complete] | ticket `notElem` handled -> do
      R.modify' (\state -> state
        { planHandledReviews = planHandledReviews state ++ [ticket] })
      void (R.finish relay)
      settled <- R.gets planOutcome
      case settled of
        Just _ -> releaseIfQuiescent
        Nothing -> case completion of
          ReviewRefused request reason
            | request == requestId (developedRequest developed) ->
                complete (Left (ReviewStopped (Text.pack (show reason))))
          ReviewApproved request proof
            | request == requestId (developedRequest developed) -> do
                state <- R.call (reviewSnapshot (R.client flow)) ()
                drive own (next (Reviewed developed proof state)) complete
          _ -> complete (Left (SourceRefused ReviewNode
            "review callback names another development request"))
    _ -> pure ()

continueVerification :: Coordinator root R.Self -> (Int, CheckState) -> PlanHandler root ()
continueVerification own (ticket, checks) = do
  active <- R.gets planVerifications
  case [item | item@(ActiveVerification key _ _ _ _ _ _ _) <- active, key == ticket] of
    [ActiveVerification _ relay running spec checked started next complete] -> do
      let report = PlanReport started (Just checks)
      R.modify' (\state -> state { planVerifications =
        [item | item@(ActiveVerification key _ _ _ _ _ _ _) <- planVerifications state,
          key /= ticket] })
      void (R.finish relay)
      void (R.finish running)
      settled <- R.gets planOutcome
      case Focused.planWatcher started of
        Just (Right watcher) -> do
          _ <- finishChecks watcher
          pure ()
        _ -> pure ()
      if maybe False (const True) settled then releaseIfQuiescent
      else if not (planPassed report) then
        complete (Left (VerificationStopped (planSummary report)))
      else case verificationAccept spec checked report of
        Left reason -> complete (Left (VerificationStopped reason))
        Right value -> drive own
          (next (AcceptedSource value checked report)) complete
    _ -> pure ()
