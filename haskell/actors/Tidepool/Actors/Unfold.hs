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
  , awaitFork
  , awaitSettledFork
  , unfold
  ) where

import Control.Monad.Freer (Eff, Member, send)
import Data.Char (isAsciiLower, isDigit)
import Data.Kind (Type)
import Data.Text (Text)
import qualified Data.Text as Text
import Prelude

import Tidepool.Agent.Reply (Replies, Response, ResponseResult)
import Tidepool.Agent.Reply.Internal (RequestLabel (..))
import Tidepool.Agent.Watch (Await, Settlement, awaitResponse, awaitSettled)
import Tidepool.Actors.Internal.Agent
  ( AgentRef
  , AgentSpec
  , codingAgent
  , scaffoldingAgent
  , integrationAgent
  , readonlyWorktreeAgent
  , requestSited
  , startForkedAgent
  , agentIdentity
  )
import Tidepool.Actors.Role
  ( CodingEffects
  , IntegrationEffects
  , KnownEffects
  , ResearchEffects
  , ScaffoldEffects
  , Subset
  )
import Tidepool.Effects.Core
  ( DirtyPolicy (..)
  , GitRef
  , Worktree (WorktreeCreateForActorPath, WorktreeCreateFromBoundForActorPath)
  , WorktreeError
  , WorktreeHandle (..)
  , WorktreeReceipt
  , WorktreeSource (..)
  , Forks (..)
  , WorktreeSpec (..)
  )
import Tidepool.Worktree (renderWorktreeError, worktreeId)

newtype CampaignLabel = CampaignLabel Text
newtype ForkGroupLabel = ForkGroupLabel Text
newtype BranchLabel = BranchLabel Text
data ForkGroupPath = ForkGroupPath Bool Text

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
    -> input
    -> Branch childEffects input result

researching
  :: forall result input
   . BranchLabel
  -> WorktreeSeed
  -> input
  -> Branch ResearchEffects input result
researching label seed input = Branch label ResearchFork seed input

coding
  :: forall result input
   . BranchLabel
  -> WorktreeSeed
  -> input
  -> Branch CodingEffects input result
coding label seed input = Branch label CodingFork seed input

scaffolding
  :: forall result input
   . BranchLabel
  -> WorktreeSeed
  -> input
  -> Branch ScaffoldEffects input result
scaffolding label seed input =
  Branch label ScaffoldingFork seed input

integrating
  :: forall result input
   . BranchLabel
  -> WorktreeSeed
  -> input
  -> Branch IntegrationEffects input result
integrating label seed input =
  Branch label IntegrationFork seed input

data BranchReceipt = BranchReceipt
  { requestedPath :: Text
  , allocatedPath :: Text
  , forkGroupIdentity :: Int
  , launchedActorId :: Int
  , launchedActorIncarnation :: Int
  , launchedRole :: ForkRole
  , launchedWorkspaceAccess :: ForkWorkspaceAccess
  , launchedWorktree :: WorktreeReceipt
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

-- | Interpret one independent applicative layer. Actor construction is a
-- complete first pass and request publication is a complete second pass; the
-- runtime batch owner adds rollback and provider-readiness gating around this
-- same Haskell-owned result tree.
unfold
  :: forall parent result
   . (Member Forks parent, Member Replies parent, Member Worktree parent)
  => ForkGroupPath
  -> Unfold parent result
  -> Eff parent result
unfold (ForkGroupPath relative groupName) plan = do
  (groupId, resolvedGroup, allocated) <- send (ForksBeginWith relative groupName (branchNames plan))
  let group = ForkGroupPath False resolvedGroup
  (started, remaining) <- start groupId group plan allocated
  case remaining of
    [] -> do
      result <- activate started
      send (ForksCommitWith groupId)
      pure result
    _ -> error "unfold: runtime returned more branch paths than requested"
  where
    start
      :: Int
      -> ForkGroupPath
      -> Unfold parent a
      -> [Text]
      -> Eff parent (Started parent a, [Text])
    start _ _ (PureU value) paths = pure (StartedPure value, paths)
    start groupId path (BranchU site branchPlan) paths = case paths of
      [] -> error "unfold: runtime returned fewer branch paths than requested"
      allocated : rest -> do
        (actor, confirmed, tree) <- startBranch groupId allocated branchPlan
        pure (StartedBranch site groupId path branchPlan actor confirmed tree, rest)
    start groupId path (ApU functions arguments) paths = do
      (startedFunctions, afterFunctions) <- start groupId path functions paths
      (startedArguments, remaining) <- start groupId path arguments afterFunctions
      pure (StartedAp startedFunctions startedArguments, remaining)

    activate :: Started parent a -> Eff parent a
    activate (StartedPure value) = pure value
    activate (StartedBranch site groupId path branchPlan actor allocated tree) =
      requestBranch site groupId path branchPlan actor allocated tree
    activate (StartedAp functions arguments) =
      activate functions <*> activate arguments

    branchNames :: Unfold parent a -> [Text]
    branchNames (PureU _) = []
    branchNames (BranchU _ (Branch (BranchLabel leaf) _ _ _)) = [leaf]
    branchNames (ApU functions arguments) =
      branchNames functions <> branchNames arguments

startBranch
  :: forall effects child input result
   . (Member Forks effects, Member Worktree effects)
  => Int
  -> Text
  -> Branch child input result
  -> Eff effects (AgentRef, Text, WorktreeHandle)
startBranch groupId allocated (Branch _ role seed _) = do
  created <- createNamedWorktree allocated seed
  case created of
    Left failure -> do
      send (ForksAbortWith groupId)
      error (Text.unpack ("unfold worktree admission failed: " <> renderWorktreeError failure))
    Right tree ->
      do
        (actor, confirmed) <- startForkedAgent groupId allocated (agentFor role tree)
        pure (actor, confirmed, tree)

createNamedWorktree
  :: Member Worktree effects
  => Text
  -> WorktreeSeed
  -> Eff effects (Either WorktreeError WorktreeHandle)
createNamedWorktree allocated (WorktreeSeed source dirtyPolicy) =
  send (WorktreeCreateForActorPath (WorktreeSpec source allocated dirtyPolicy) allocated)
createNamedWorktree allocated (BoundHeadSeed dirtyPolicy) =
  send (WorktreeCreateFromBoundForActorPath dirtyPolicy allocated)

agentFor :: ForkRole -> WorktreeHandle -> AgentSpec
agentFor ResearchFork = readonlyWorktreeAgent
agentFor CodingFork = codingAgent
agentFor ScaffoldingFork = scaffoldingAgent
agentFor IntegrationFork = integrationAgent

requestBranch
  :: forall effects child input result
   . Member Replies effects
  => Int
  -> Int
  -> ForkGroupPath
  -> Branch child input result
  -> AgentRef
  -> Text
  -> WorktreeHandle
  -> Eff effects (Forked result)
requestBranch site groupId (ForkGroupPath _ group) (Branch (BranchLabel leaf) role _ input) actor allocated tree = do
  let requested = group <> "/" <> leaf
      (actorId, incarnation) = agentIdentity actor
  response <- requestSited @result @input site actor (RequestLabel leaf) input
  pure Forked
    { forkedActor = actor
    , forkedResponse = response
    , forkedLaunch = BranchReceipt
        { requestedPath = requested
        , allocatedPath = allocated
        , forkGroupIdentity = groupId
        , launchedActorId = actorId
        , launchedActorIncarnation = incarnation
        , launchedRole = role
        , launchedWorkspaceAccess = accessFor role
        , launchedWorktree = handleReceipt tree
        }
    }

accessFor :: ForkRole -> ForkWorkspaceAccess
accessFor ResearchFork = InspectForkWorktree
accessFor CodingFork = WriteForkWorktree
accessFor ScaffoldingFork = WriteForkWorktree
accessFor IntegrationFork = WriteForkWorktree
