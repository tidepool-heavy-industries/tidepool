{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeFamilies #-}
{-# LANGUAGE TypeOperators #-}

-- One worktree-holding actor serializes merge and check for a parent. A green
-- check advances an optional named publication branch; red evidence remains in
-- the integration checkout. ReviewFlow calls this owner. Local
-- component integration uses this mechanism at every implementation depth.
module Exomonad.Contrib.Merge
  ( -- The merge target: supplied by whoever starts a review
    MergeTarget (..)
    -- The merge actor: one per merge target
  , Merge (..)
  , MergeEffects
  , MergeState (..)
  , PublishRequest (..)
  , MergeResult (..)
  , mergeInto
  , IntegrationCheck (..)
  , integrationPassed
  , MergeHistory (..)
  , MergeEvent (..)
  ) where

import Data.Text (Text)
import qualified Data.Text as Text
import GHC.Generics (Generic)

import qualified Tidepool.Actor as Actor
import qualified Tidepool.Actor.Record as R
import qualified Tidepool.Command as Cmd
import Tidepool.Actors.Exomonad
import Tidepool.Effects.Core (BranchName (..), Commands, WorktreeHandle (..), WorktreeReceipt (..))
import Tidepool.Worktree (SubmissionObservation (..), HeadState (..), renderGitOid, renderWorktreeError)
import Exomonad.Contrib.Types (cleanReviewCheckout)

-- One serialized actor owns the integration checkout and optional publication
-- branch. The project supplies the command; successful execution alone does
-- not prove that a test was selected or executed.
newtype MergeTarget = MergeTarget
  { mergeActor :: R.ActorHandle Merge
  } deriving (Show)

data PublishRequest = PublishRequest
  { publishTask :: Text
  , publishSource :: WorktreeId
  , publishCandidate :: GitOid
  , publishMessage :: Text
  } deriving (Show, Eq)

-- The original command receipt remains inspectable independently of its short
-- diagnostic display. The head is captured before execution and checked again
-- before publication, since a project command may change the checkout.
data IntegrationCheck = IntegrationCheck
  { integrationHead :: GitOid
  , integrationArgv :: [Text]
  , integrationReceipt :: Cmd.RunResult
  , integrationSubmission :: Maybe SubmissionObservation
  , integrationDetail :: Text
  } deriving (Eq)

instance Show IntegrationCheck where
  show checked = "IntegrationCheck " ++ show (integrationHead checked)
    ++ " argv=" ++ show (integrationArgv checked)
    ++ " " ++ show (Cmd.commandResult (integrationReceipt checked))
    ++ " source=" ++ show (fmap (cleanReviewCheckout . workingState) (integrationSubmission checked))
    ++ ": " ++ Text.unpack (integrationDetail checked)

integrationPassed :: IntegrationCheck -> Bool
integrationPassed = cleanSuccess . integrationReceipt

data MergeResult
  = Published GitOid GitOid IntegrationCheck
    -- ^ checked head, previous head, original successful command receipt
  | RedPreserved GitOid GitOid IntegrationCheck
    -- ^ previous head, checked head left in the checkout, original red receipt
  | Conflict Text [Text]
  | MergeBlocked Text
    -- ^ requests remain refused until the parent reconciles the checkout
  | MergeFailed Text
  deriving (Show, Eq)

data MergeEvent
  = PublishRefused Text
  | PublishFailed Text
  | MergeConflict Text [Text]
  | PublicationBlocked Text
  | IntegrationPublished GitOid IntegrationCheck
  | IntegrationRed GitOid IntegrationCheck
  | IntegrationBlocked IntegrationCheck Text
  | Reconciled Text
  deriving (Show, Eq)

data MergeHistory = MergeHistory
  { historyTask :: Text
  , historyCandidate :: Maybe GitOid
  , historyEvent :: MergeEvent
  } deriving (Show, Eq)

data Merge mode = Merge
  { mergeState :: mode :- State MergeState
  , publish :: mode :- Call PublishRequest (R.Reply MergeResult)
  , reconcile :: mode :- Call Text NoReply
  , mergeView :: mode :- Call () (R.Reply MergeState)
  } deriving Generic

data MergeState = MergeState
  { mergeAdvanceBranch :: Maybe BranchName
  , mergeCheckCommand :: [Text]
  , mergeTree :: Maybe WorktreeHandle
  , mergeBlocked :: Maybe Text
  , mergeHistory :: [MergeHistory]
  }

instance Show MergeState where
  show state = unlines $
    ( "merge tree=" ++ maybe "unbound" (Text.unpack . cwd . handleReceipt) (mergeTree state)
      ++ " advance=" ++ maybe "-" (\(BranchName branch) -> Text.unpack branch) (mergeAdvanceBranch state)
      ++ " check=" ++ show (mergeCheckCommand state)
      ++ " blocked=" ++ maybe "no" Text.unpack (mergeBlocked state)
    ) : map show (mergeHistory state)

type MergeEffects = R.LocalEffects Merge
  '[Replies, BoundWorktree, WorktreeIntegration, Commands, Actor]

-- | Bind the integration checkout and run the supplied argv after each merge.
-- Only a clean exit 0 can advance the optional publication branch.
mergeInto :: WorktreeId -> Maybe BranchName -> [Text] -> ActorSpec Merge MergeEffects
mergeInto tree advance check =
  R.withWorktree tree $ R.definition "integrator" (Actor.Selected knownEffects) Merge
    { mergeState = MergeState advance check Nothing Nothing []
    , mergeView = \() -> R.get
    , publish = runPublish
    , reconcile = \note -> R.modify' (\state -> state
        { mergeBlocked = Nothing
        , mergeHistory = mergeHistory state ++ [MergeHistory "integrate" Nothing (Reconciled note)] })
    }

runPublish :: PublishRequest -> Handler MergeState MergeEffects MergeResult
runPublish request = do
  blocked <- R.gets mergeBlocked
  case blocked of
    Just reason -> do
      history (PublishRefused reason)
      pure (MergeBlocked reason)
    Nothing -> do
      bound <- ownTree
      case bound of
        Left failure -> failed ("the merge actor holds no worktree: " <> Text.pack (show failure))
        Right handle -> do
          let path = cwd (handleReceipt handle)
          advance <- R.gets mergeAdvanceBranch
          beforeResult <- gitIn path ["rev-parse", "HEAD"]
          case beforeResult of
            Left failure -> failed failure
            Right before -> do
              drift <- publicationDrift path advance before
              case drift of
                Left failure -> failed failure
                Right (Just reason) -> block (PublicationBlocked reason) reason
                Right Nothing -> do
                  outcome <- tryMerge MergeRequest
                    { mergeSourceHead = publishCandidate request
                    , mergeSourceWorktree = publishSource request
                    , mergeSourceBranch = Nothing
                    , mergeTargetWorktree = worktreeId handle
                    , mergeMessage = publishMessage request
                    , mergeAdvance = Nothing
                    }
                  case outcome of
                    Left failure -> failed (Text.pack (show failure))
                    Right (ManualGitRequired _ _ reason paths) -> do
                      history (MergeConflict reason paths)
                      pure (Conflict reason paths)
                    Right _ -> do
                      checkedResult <- gitIn path ["rev-parse", "HEAD"]
                      case checkedResult of
                        Left failure -> block (PublicationBlocked failure) failure
                        Right checked -> do
                          evidence <- checkedOutput path (GitOid checked)
                          case evidence of
                            Left failure -> block (PublicationBlocked failure) failure
                            Right original -> do
                              after <- observeSubmission (worktreeId handle)
                              case after of
                                Left issue ->
                                  let failure = "integration source observation unavailable: " <> renderWorktreeError issue
                                  in block (IntegrationBlocked original failure) failure
                                Right submission -> do
                                  let check = original { integrationSubmission = Just submission }
                                      headAfter = renderGitOid (headOid (submittedHead submission))
                                  if headAfter /= checked then
                                      let reason = "integration command changed HEAD from " <> checked <> " to " <> headAfter
                                      in block (IntegrationBlocked check reason) reason
                                  else if not (integrationPassed check) then do
                                    history (IntegrationRed (GitOid before) check)
                                    pure (RedPreserved (GitOid before) (GitOid checked) check)
                                  else if not (cleanReviewCheckout (workingState submission)) then
                                    let reason = "integration command left the checked source dirty or in progress: "
                                          <> Text.pack (show (workingState submission))
                                    in block (IntegrationBlocked check reason) reason
                                  else do
                                    published <- case advance of
                                      Nothing -> pure (Right ())
                                      Just (BranchName branch) -> do
                                        result <- runIn path ["git", "update-ref", "refs/heads/" <> branch, checked, before]
                                        pure $ case result of
                                          Left failure -> Left failure
                                          Right receipt -> maybe (Right ()) Left (commandProblem receipt)
                                    case published of
                                      Left detail ->
                                        let reason = "the checked head was not published to " <> branchText advance <> ": " <> detail
                                        in block (IntegrationBlocked check reason) reason
                                      Right () -> do
                                        history (IntegrationPublished (GitOid before) check)
                                        pure (Published (GitOid checked) (GitOid before) check)
  where
    history :: MergeEvent -> Handler MergeState MergeEffects ()
    history event = R.modify' (\state -> state
      { mergeHistory = mergeHistory state
          ++ [MergeHistory (publishTask request) (Just (publishCandidate request)) event] })
    failed :: Text -> Handler MergeState MergeEffects MergeResult
    failed reason = do
      history (PublishFailed reason)
      pure (MergeFailed reason)
    block :: MergeEvent -> Text -> Handler MergeState MergeEffects MergeResult
    block event reason = do
      R.modify' (\state -> state { mergeBlocked = Just reason })
      history event
      pure (MergeBlocked reason)

publicationDrift :: Text -> Maybe BranchName -> Text -> Handler MergeState MergeEffects (Either Text (Maybe Text))
publicationDrift _ Nothing _ = pure (Right Nothing)
publicationDrift path (Just (BranchName branch)) before = do
  published <- gitIn path ["rev-parse", "refs/heads/" <> branch]
  case published of
    Left failure -> pure (Left failure)
    Right headPublished -> do
      checkouts <- gitIn path ["worktree", "list", "--porcelain"]
      pure $ do
        listing <- checkouts
        let checkedOut = "branch refs/heads/" <> branch `elem` Text.lines listing
        pure $ if headPublished /= before
          then Just ("publication branch " <> branch <> " is at " <> headPublished
                     <> " but the integration worktree is at " <> before)
          else if checkedOut
          then Just ("publication branch " <> branch <> " is checked out in another worktree; update-ref would desynchronise it")
          else Nothing

ownTree :: Handler MergeState MergeEffects (Either WorktreeError WorktreeHandle)
ownTree = do
  cached <- R.gets mergeTree
  case cached of
    Just handle -> pure (Right handle)
    Nothing -> do
      bound <- boundWorktree
      case bound of
        Right handle -> do
          R.modify' (\state -> state { mergeTree = Just handle })
          pure bound
        Left _ -> pure bound

branchText :: Maybe BranchName -> Text
branchText = maybe "no publication branch" (\(BranchName name) -> name)

runIn :: Text -> [Text] -> Handler MergeState MergeEffects (Either Text Cmd.RunResult)
runIn path arguments = do
  started <- Cmd.tryStart (Cmd.inDirectory path (Cmd.argv arguments))
  case started of
    Left failure -> pure (Left (Text.unwords arguments <> ": " <> Cmd.renderCommandError failure))
    Right job -> Right <$> Cmd.await job

gitIn :: Text -> [Text] -> Handler MergeState MergeEffects (Either Text Text)
gitIn path arguments = do
  result <- runIn path ("git" : arguments)
  pure $ do
    receipt <- result
    case commandProblem receipt of
      Just failure -> Left ("git " <> Text.unwords arguments <> ": " <> failure)
      Nothing -> case Cmd.stdout receipt of
        Left issue -> Left ("git " <> Text.unwords arguments <> ": " <> Text.pack (show issue))
        Right output -> Right (Text.strip output)

cleanSuccess :: Cmd.RunResult -> Bool
cleanSuccess result =
  Cmd.commandOutcome (Cmd.commandResult result) == Cmd.CommandExited 0
    && Cmd.commandCleanup (Cmd.commandResult result) == Cmd.CommandClean

commandProblem :: Cmd.RunResult -> Maybe Text
commandProblem result
  | cleanSuccess result = Nothing
  | otherwise = Just (maybe (Text.pack (show (Cmd.commandResult result))) id (Cmd.failure result)
      <> "; cleanup=" <> Text.pack (show (Cmd.commandCleanup (Cmd.commandResult result))))

checkedOutput :: Text -> GitOid -> Handler MergeState MergeEffects (Either Text IntegrationCheck)
checkedOutput path headChecked = do
  check <- R.gets mergeCheckCommand
  result <- runIn path check
  pure $ fmap (\receipt -> IntegrationCheck headChecked check receipt Nothing (detail receipt)) result
  where
    detail receipt = case Cmd.capturedOutput receipt of
      Left failure -> "output unavailable: " <> Cmd.renderCommandError failure
      Right output -> "display tail: " <> Text.takeEnd 400 (Text.strip
        (Cmd.outputText (Cmd.commandStdout output) <> "\n"
          <> Cmd.outputText (Cmd.commandStderr output)))
