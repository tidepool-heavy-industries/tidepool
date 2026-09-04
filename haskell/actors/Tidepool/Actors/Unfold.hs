{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE KindSignatures #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE RankNTypes #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}

-- | Cache-preserving context forks described as an applicative layer.
module Tidepool.Actors.Unfold
  ( CampaignLabel
  , ForkGroupLabel
  , BranchLabel
  , ActorPath
  , renderActorPath
  , GitBranchPrefix
  , renderGitBranchPrefix
  , actorGitBranchPrefix
  , ForkGroupPath
  , NameError (..)
  , WorktreeSeed
  , projectHead
  , boundHead
  , existingWorktree
  , atRef
  , snapshotDirty
  , campaignLabel
  , forkGroupLabel
  , branchLabel
  , batch
  , subgroup
  , Branch
  , withBranchGuidance
  , withBranchDeadline
  , RolePolicy
  , inspectionPolicy
  , codingPolicy
  , scaffoldPolicy
  , integrationPolicy
  , narrowed
  , ForkRole (..)
  , ForkWorkspaceAccess (..)
  , researching
  , coding
  , scaffolding
  , integrating
  , Unfold
  , child
  , childSited
  , Forked
  , forkedActor
  , forkedResponse
  , forkedLaunch
  , BranchReceipt (..)
  , ForkGroupHandle
  , forkGroupHandle
  , forkGroupGitBranchPrefix
  , ForkObservation (..)
  , observeFork
  , CampaignSnapshot (..)
  , observeCampaign
  , ForkGroupCleanupOutcome (..)
  , cleanupForkGroup
  , CleanupPlan (..)
  , CleanupActorPlan (..)
  , CleanupActorState (..)
  , CleanupReceipt (..)
  , CleanupStepReceipt (..)
  , planCleanup
  , executeCleanup
  , awaitFork
  , awaitSettledFork
  , UnfoldError (..)
  , attemptUnfold
  , unfold
  ) where

import Control.Monad.Freer (Eff, Member, send)
import Data.Char (isAsciiLower, isDigit)
import Data.Kind (Type)
import Data.Text (Text)
import qualified Data.Text as Text
import Prelude

import qualified Tidepool.Actor as Actor
import Tidepool.Agent.Reply (Replies, Response, ResponseResult)
import Tidepool.Agent.Reply.Internal (RequestLabel (..))
import Tidepool.Agent.Watch (Await, Settlement, awaitResponse, awaitSettled)
import Tidepool.Actors.Internal.Agent
  ( AgentRef
  , RequestDeadline
  , agentIdentity
  , listAgents
  , lookupAgent
  , requestOptions
  , requestWithSited
  , startForkedAgent
  , withRequestDeadline
  , withRequestGuidance
  )
import Tidepool.Actors.Role
  ( CodingEffects
  , Effects
  , IntegrationEffects
  , KnownEffects
  , ResearchEffects
  , ScaffoldEffects
  , Subset
  , effectKeys
  , knownEffects
  )
import Tidepool.Effects.Core
  ( AgentControl (..)
  , CleanupActorPlan (..)
  , CleanupActorState (..)
  , CleanupPlan (..)
  , CleanupReceipt (..)
  , CleanupStepReceipt (..)
  , DirtyPolicy (..)
  , AgentRosterEntry (..)
  , GitRef
  , WorktreeHandle (..)
  , WorktreeReceipt
  , WorktreeSource (..)
  , Forks (..)
  , ForkGroupCleanupOutcome (..)
  , AgentInspection
  , WorktreeSpec (..)
  )
import Tidepool.Worktree (worktreeId)

newtype CampaignLabel = CampaignLabel Text
newtype ForkGroupLabel = ForkGroupLabel Text
newtype BranchLabel = BranchLabel Text
newtype ActorPath = ActorPath Text
  deriving (Show, Eq, Ord)
newtype GitBranchPrefix = GitBranchPrefix Text
  deriving (Show, Eq, Ord)
data ForkGroupPath = ForkGroupPath Bool Text

renderActorPath :: ActorPath -> Text
renderActorPath (ActorPath path) = path

renderGitBranchPrefix :: GitBranchPrefix -> Text
renderGitBranchPrefix (GitBranchPrefix prefix) = prefix

actorGitBranchPrefix :: ActorPath -> GitBranchPrefix
actorGitBranchPrefix (ActorPath path) = GitBranchPrefix ("shoal/" <> path)

data NameError
  = EmptyName
  | InvalidKebabName Text
  | NameTooLong Text
  deriving (Show, Eq)

campaignLabel :: Text -> Either NameError CampaignLabel
campaignLabel = fmap CampaignLabel . validateSegment

forkGroupLabel :: Text -> Either NameError ForkGroupLabel
forkGroupLabel = fmap ForkGroupLabel . validateSegment

branchLabel :: Text -> Either NameError BranchLabel
branchLabel = fmap BranchLabel . validateSegment

batch :: CampaignLabel -> ForkGroupLabel -> ForkGroupPath
batch (CampaignLabel campaignName) (ForkGroupLabel groupName) =
  ForkGroupPath False (campaignName <> "/" <> groupName)

subgroup :: ForkGroupLabel -> ForkGroupPath
subgroup (ForkGroupLabel groupName) = ForkGroupPath True groupName

validateSegment :: Text -> Either NameError Text
validateSegment value
  | Text.null value = Left EmptyName
  | Text.length value > 48 = Left (NameTooLong value)
  | Text.head value == '-' || Text.last value == '-' = Left (InvalidKebabName value)
  | "--" `Text.isInfixOf` value = Left (InvalidKebabName value)
  | Text.all valid value = Right value
  | otherwise = Left (InvalidKebabName value)
  where
    valid character = isAsciiLower character || isDigit character || character == '-'

data WorktreeSeed
  = WorktreeSeed WorktreeSource DirtyPolicy
  | BoundHeadSeed DirtyPolicy

projectHead :: WorktreeSeed
projectHead = WorktreeSeed SourceCurrentRepository RequireClean

boundHead :: WorktreeSeed
boundHead = BoundHeadSeed RequireClean

existingWorktree :: WorktreeHandle -> WorktreeSeed
existingWorktree tree = WorktreeSeed (SourceWorktree (worktreeId tree)) RequireClean

atRef :: GitRef -> WorktreeSeed
atRef ref = WorktreeSeed (SourceRef ref) RequireClean

snapshotDirty :: WorktreeSeed -> WorktreeSeed
snapshotDirty (WorktreeSeed source _) = WorktreeSeed source AllowDirtySnapshot
snapshotDirty (BoundHeadSeed _) = BoundHeadSeed AllowDirtySnapshot

data ForkRole
  = ResearchFork
  | CodingFork
  | ScaffoldingFork
  | IntegrationFork
  deriving (Show, Eq)

data ForkWorkspaceAccess
  = InspectForkWorktree
  | WriteForkWorktree
  deriving (Show, Eq)

data Branch (childEffects :: [Type -> Type]) input result where
  Branch
    :: BranchLabel
    -> ForkRole
    -> WorktreeSeed
    -> Effects childEffects
    -> BranchOptions
    -> input
    -> Branch childEffects input result

data BranchOptions = BranchOptions
  { branchGuidance :: Maybe Text
  , branchDeadline :: Maybe RequestDeadline
  }

defaultBranchOptions :: BranchOptions
defaultBranchOptions = BranchOptions Nothing Nothing

withBranchGuidance
  :: Text
  -> Branch child input result
  -> Branch child input result
withBranchGuidance guidance (Branch label role seed effects options input) =
  Branch label role seed effects (options { branchGuidance = Just guidance }) input

withBranchDeadline
  :: RequestDeadline
  -> Branch child input result
  -> Branch child input result
withBranchDeadline deadline (Branch label role seed effects options input) =
  Branch label role seed effects (options { branchDeadline = Just deadline }) input

data RolePolicy (childEffects :: [Type -> Type]) = RolePolicy ForkRole WorktreeSeed

inspectionPolicy :: WorktreeSeed -> RolePolicy childEffects
inspectionPolicy = RolePolicy ResearchFork

codingPolicy :: WorktreeSeed -> RolePolicy childEffects
codingPolicy = RolePolicy CodingFork

scaffoldPolicy :: WorktreeSeed -> RolePolicy childEffects
scaffoldPolicy = RolePolicy ScaffoldingFork

integrationPolicy :: WorktreeSeed -> RolePolicy childEffects
integrationPolicy = RolePolicy IntegrationFork

narrowed
  :: forall child result input
   . Effects child
  -> RolePolicy child
  -> BranchLabel
  -> input
  -> Branch child input result
narrowed effects (RolePolicy role seed) label input =
  Branch label role seed effects defaultBranchOptions input

researching
  :: forall result input
   . BranchLabel
  -> WorktreeSeed
  -> input
  -> Branch ResearchEffects input result
researching label seed input = Branch label ResearchFork seed knownEffects defaultBranchOptions input

coding
  :: forall result input
   . BranchLabel
  -> WorktreeSeed
  -> input
  -> Branch CodingEffects input result
coding label seed input = Branch label CodingFork seed knownEffects defaultBranchOptions input

scaffolding
  :: forall result input
   . BranchLabel
  -> WorktreeSeed
  -> input
  -> Branch ScaffoldEffects input result
scaffolding label seed input =
  Branch label ScaffoldingFork seed knownEffects defaultBranchOptions input

integrating
  :: forall result input
   . BranchLabel
  -> WorktreeSeed
  -> input
  -> Branch IntegrationEffects input result
integrating label seed input =
  Branch label IntegrationFork seed knownEffects defaultBranchOptions input

data BranchReceipt = BranchReceipt
  { requestedPath :: ActorPath
  , allocatedPath :: ActorPath
  , allocatedForkGroupPath :: ActorPath
  , forkGroupIdentity :: Int
  , launchedActorId :: Int
  , launchedActorIncarnation :: Int
  , launchedRole :: ForkRole
  , launchedWorkspaceAccess :: ForkWorkspaceAccess
  , launchedWorktree :: WorktreeReceipt
  , launchedSupervisor :: Maybe (Int, Int)
  , launchedContextParent :: Maybe (Int, Int)
  , launchedProviderParent :: Maybe Text
  , launchedHaskellSnapshot :: Maybe Int
  }
  deriving (Show, Eq)

awaitFork :: Forked result -> Await (ResponseResult result)
awaitFork = awaitResponse . forkedResponse

awaitSettledFork :: Forked result -> Await (Settlement result)
awaitSettledFork = awaitSettled . forkedResponse

data Forked result = Forked
  { forkedActor :: AgentRef
  , forkedResponse :: Response result
  , forkedLaunch :: BranchReceipt
  }

data ForkGroupHandle = ForkGroupHandle Int ActorPath
  deriving (Show, Eq)

data ForkObservation result = ForkObservation
  { observedFork :: Forked result
  , observedLaunch :: BranchReceipt
  , observedActor :: Maybe AgentRosterEntry
  }

observeFork
  :: Member AgentInspection effs
  => Forked result
  -> Eff effs (ForkObservation result)
observeFork worker = do
  actor <- lookupAgent (forkedActor worker)
  pure ForkObservation
    { observedFork = worker
    , observedLaunch = forkedLaunch worker
    , observedActor = actor
    }

data CampaignSnapshot = CampaignSnapshot
  { campaignRoot :: ActorPath
  , campaignRoster :: [AgentRosterEntry]
  }
  deriving (Show, Eq)

observeCampaign
  :: Member AgentInspection effs
  => ForkGroupHandle
  -> Eff effs CampaignSnapshot
observeCampaign (ForkGroupHandle _ root@(ActorPath path)) = do
  roster <- listAgents
  let descendant entry =
        rosterLabel entry == path
          || (path <> "/") `Text.isPrefixOf` rosterLabel entry
  pure CampaignSnapshot
    { campaignRoot = root
    , campaignRoster = filter descendant roster
    }

forkGroupHandle :: Forked result -> ForkGroupHandle
forkGroupHandle worker =
  let receipt = forkedLaunch worker
  in ForkGroupHandle (forkGroupIdentity receipt) (allocatedForkGroupPath receipt)

forkGroupGitBranchPrefix :: ForkGroupHandle -> GitBranchPrefix
forkGroupGitBranchPrefix (ForkGroupHandle _ (ActorPath path)) =
  GitBranchPrefix ("shoal/" <> path <> "/")

cleanupForkGroup
  :: Member Forks effs
  => ForkGroupHandle
  -> Eff effs ForkGroupCleanupOutcome
cleanupForkGroup (ForkGroupHandle groupId _) = send (ForksCleanupWith groupId)

planCleanup
  :: Member AgentControl effs
  => ForkGroupHandle
  -> Eff effs CleanupPlan
planCleanup (ForkGroupHandle groupId _) = send (AgentControlPlanCleanupWith groupId)

executeCleanup
  :: Member AgentControl effs
  => CleanupPlan
  -> Eff effs CleanupReceipt
executeCleanup plan = send (AgentControlExecuteCleanupWith (cleanupPlanGroup plan))

data Unfold (parent :: [Type -> Type]) result where
  PureU :: result -> Unfold parent result
  BranchU :: Int -> Branch child input result -> Unfold parent (Forked result)
  ApU :: Unfold parent (a -> result) -> Unfold parent a -> Unfold parent result

-- The intermediate tree preserves the caller's heterogeneous applicative
-- shape while separating actor construction from request publication. No
-- branch can receive its assignment until every sibling actor exists.
data Started (parent :: [Type -> Type]) result where
  StartedPure :: result -> Started parent result
  StartedBranch
    :: Int
    -> Int
    -> ForkGroupPath
    -> Branch child input result
    -> AgentRef
    -> Text
    -> WorktreeHandle
    -> Started parent (Forked result)
  StartedAp
    :: Started parent (a -> result)
    -> Started parent a
    -> Started parent result

instance Functor (Unfold parent) where
  fmap function plan = PureU function <*> plan

instance Applicative (Unfold parent) where
  pure = PureU
  (<*>) = ApU

{-# OPAQUE child #-}
child
  :: forall result child input parent
   . (KnownEffects child, Subset child parent)
  => Branch child input result
  -> Unfold parent (Forked result)
child = childSited 0

{-# OPAQUE childSited #-}
childSited
  :: forall result child input parent
   . (KnownEffects child, Subset child parent)
  => Int
  -> Branch child input result
  -> Unfold parent (Forked result)
childSited = BranchU

data UnfoldError
  = UnfoldBeginRejected Text
  | UnfoldBranchRejected Text Text
  | UnfoldShapeMismatch Text
  | UnfoldCommitRejected Text
  deriving (Show, Eq)

-- | Interpret one independent applicative layer. Actor construction is a
-- complete first pass and request publication is a complete second pass; the
-- runtime batch owner adds rollback and provider-readiness gating around this
-- same Haskell-owned result tree.
attemptUnfold
  :: forall parent result
   . (Member Forks parent, Member Replies parent, Member AgentInspection parent)
  => ForkGroupPath
  -> Unfold parent result
  -> Eff parent (Either UnfoldError result)
attemptUnfold (ForkGroupPath relative groupName) plan = do
  begun <- send (ForksBeginWith relative groupName (branchNames plan))
  case begun of
    Left failure -> pure (Left (UnfoldBeginRejected failure))
    Right (groupId, resolvedGroup, allocated) -> do
      let group = ForkGroupPath False resolvedGroup
      started <- start groupId group plan allocated
      case started of
        Left failure -> do
          _ <- send (ForksAbortWith groupId)
          pure (Left failure)
        Right (tree, []) -> do
          result <- activate tree
          committed <- send (ForksCommitWith groupId)
          pure $ case committed of
            Left failure -> Left (UnfoldCommitRejected failure)
            Right () -> Right result
        Right (_, _) -> do
          _ <- send (ForksAbortWith groupId)
          pure (Left (UnfoldShapeMismatch "runtime returned more branch paths than requested"))
  where
    start
      :: Int
      -> ForkGroupPath
      -> Unfold parent a
      -> [Text]
      -> Eff parent (Either UnfoldError (Started parent a, [Text]))
    start _ _ (PureU value) paths = pure (Right (StartedPure value, paths))
    start groupId path (BranchU site branchPlan) paths = case paths of
      [] -> pure (Left (UnfoldShapeMismatch "runtime returned fewer branch paths than requested"))
      allocated : rest -> do
        branch <- startBranch groupId allocated branchPlan
        pure $ case branch of
          Left failure -> Left failure
          Right (actor, confirmed, tree) ->
            Right (StartedBranch site groupId path branchPlan actor confirmed tree, rest)
    start groupId path (ApU functions arguments) paths = do
      startedFunctions <- start groupId path functions paths
      case startedFunctions of
        Left failure -> pure (Left failure)
        Right (functionsTree, afterFunctions) -> do
          startedArguments <- start groupId path arguments afterFunctions
          pure $ case startedArguments of
            Left failure -> Left failure
            Right (argumentsTree, remaining) ->
              Right (StartedAp functionsTree argumentsTree, remaining)

    activate :: Started parent a -> Eff parent a
    activate (StartedPure value) = pure value
    activate (StartedBranch site groupId path branchPlan actor allocated tree) =
      requestBranch site groupId path branchPlan actor allocated tree
    activate (StartedAp functions arguments) =
      activate functions <*> activate arguments

    branchNames :: Unfold parent a -> [Text]
    branchNames (PureU _) = []
    branchNames (BranchU _ (Branch (BranchLabel leaf) _ _ _ _ _)) = [leaf]
    branchNames (ApU functions arguments) =
      branchNames functions <> branchNames arguments

unfold
  :: forall parent result
   . (Member Forks parent, Member Replies parent, Member AgentInspection parent)
  => ForkGroupPath
  -> Unfold parent result
  -> Eff parent result
unfold path plan = do
  attempted <- attemptUnfold path plan
  case attempted of
    Left failure -> error ("unfold admission failed: " <> show failure)
    Right result -> pure result

startBranch
  :: forall effects child input result
   . Member Forks effects
  => Int
  -> Text
  -> Branch child input result
  -> Eff effects (Either UnfoldError (AgentRef, Text, WorktreeHandle))
startBranch groupId allocated (Branch _ role seed effects _ _) = do
  let (worktreeSpec, dirtyPolicy) = seedRequest allocated seed
  launched <- startForkedAgent
    (launchRoleFor role)
    groupId
    allocated
    worktreeSpec
    dirtyPolicy
    (effectKeys effects)
  pure $ case launched of
    Left failure -> Left (UnfoldBranchRejected allocated failure)
    Right branch -> Right branch

seedRequest :: Text -> WorktreeSeed -> (Maybe WorktreeSpec, DirtyPolicy)
seedRequest allocated (WorktreeSeed source dirtyPolicy) =
  (Just (WorktreeSpec source allocated dirtyPolicy), dirtyPolicy)
seedRequest _ (BoundHeadSeed dirtyPolicy) = (Nothing, dirtyPolicy)

launchRoleFor :: ForkRole -> Actor.LaunchRole
launchRoleFor ResearchFork = Actor.ResearchRole
launchRoleFor CodingFork = Actor.CodingRole
launchRoleFor ScaffoldingFork = Actor.ScaffoldingRole
launchRoleFor IntegrationFork = Actor.IntegrationRole

requestBranch
  :: forall effects child input result
   . (Member Replies effects, Member AgentInspection effects)
  => Int
  -> Int
  -> ForkGroupPath
  -> Branch child input result
  -> AgentRef
  -> Text
  -> WorktreeHandle
  -> Eff effects (Forked result)
requestBranch site groupId (ForkGroupPath _ group) (Branch (BranchLabel leaf) role _ _ options input) actor allocated tree = do
  let requested = group <> "/" <> leaf
      (actorId, incarnation) = agentIdentity actor
  let baseOptions = requestOptions (RequestLabel leaf) input
      guidedOptions = case branchGuidance options of
        Nothing -> baseOptions
        Just guidance -> withRequestGuidance guidance baseOptions
      finalOptions = case branchDeadline options of
        Nothing -> guidedOptions
        Just deadline -> withRequestDeadline deadline guidedOptions
  response <- requestWithSited @result @input site actor finalOptions
  observed <- lookupAgent actor
  let pair maybeId maybeInc = (,) <$> maybeId <*> maybeInc
  pure Forked
    { forkedActor = actor
    , forkedResponse = response
    , forkedLaunch = BranchReceipt
        { requestedPath = ActorPath requested
        , allocatedPath = ActorPath allocated
        , allocatedForkGroupPath = ActorPath (allocatedGroupPath leaf allocated)
        , forkGroupIdentity = groupId
        , launchedActorId = actorId
        , launchedActorIncarnation = incarnation
        , launchedRole = role
        , launchedWorkspaceAccess = accessFor role
        , launchedWorktree = handleReceipt tree
        , launchedSupervisor = observed >>= \entry ->
            pair (rosterSupervisorId entry) (rosterSupervisorIncarnation entry)
        , launchedContextParent = observed >>= \entry ->
            pair (rosterContextParentId entry) (rosterContextParentIncarnation entry)
        , launchedProviderParent = observed >>= rosterProviderParentThread
        , launchedHaskellSnapshot = rosterHaskellSnapshot <$> observed
        }
    }

allocatedGroupPath :: Text -> Text -> Text
allocatedGroupPath leaf allocated =
  case Text.stripSuffix ("/" <> leaf) allocated of
    Just group -> group
    Nothing -> allocated

accessFor :: ForkRole -> ForkWorkspaceAccess
accessFor ResearchFork = InspectForkWorktree
accessFor CodingFork = WriteForkWorktree
accessFor ScaffoldingFork = WriteForkWorktree
accessFor IntegrationFork = WriteForkWorktree
