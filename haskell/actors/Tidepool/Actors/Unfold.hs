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
  , ActorPath
  , GitBranchPrefix
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
  , batch
  , subgroup
  , Branch
  , withInstructions
  , withLifetime
  , WorkerLifetime (..)
  , ForkEffort (..)
  , Model (..)
  , withEffort
  , withModel
  , WorkerContext
  , inherited
  , selected
  , withContext
  , RolePolicy
  , inspectionPolicy
  , codingPolicy
  , narrowed
  , ForkRole (..)
  , ForkWorkspaceAccess (..)
  , ForkBudget (..)
  , ForkAllowance (..)
  , WorkerLaunchPreview (..)
  , ForkContext (..)
  , BranchPreview (..)
  , DelegationAuthority (..)
  , withForkBudget
  , previewBranch
  , researching
  , researchingLeaf
  , coding
  , scaffolding
  , integrating
  , Unfold
  , child
  , childSited
  , childWithProgress
  , childWithProgressSited
  , BranchReceipt (..)
  , ForkGroupHandle
  , forkGroupHandle
  , forkGroupGitBranchPrefix
  , ForkGroupSnapshot (..)
  , observeForkGroup
  , CleanupPlan (..)
  , CleanupActorPlan (..)
  , CleanupActorState (..)
  , CleanupReceipt (..)
  , CleanupStepReceipt (..)
  , planCleanup
  , executeCleanup
  , UnfoldError (..)
  , attemptUnfold
  , unfold
  ) where

import Control.Monad.Freer (Eff, Member, send)
import Data.Char (isAsciiLower, isDigit)
import Data.Kind (Type)
import Data.String (IsString (fromString))
import Data.Text (Text)
import qualified Data.Text as Text
import Prelude

import qualified Tidepool.Actor as Actor
import Tidepool.Agent.Reply (Replies, Response)
import Tidepool.Agent.Reply.Internal (Progress (..), responseRequestId, responseLaunch, withResponseLaunch)
import Tidepool.Agent.Assignment (Assignment (..), Label, NameError (..), labelText)
import Tidepool.Agent.Launch
  ( ActorPath (..), GitBranchPrefix (..), ForkRole (..)
  , ForkWorkspaceAccess (..), BranchReceipt (..)
  )
import Tidepool.Actors.Internal.Agent
  ( AgentRef
  , agentIdentity
  , lookupAgent
  , requestWithSited
  , startForkedAgent
  , roleCode
  )
import Tidepool.Actors.Role
  ( CodingEffects
  , Effects
  , IntegrationEffects
  , KnownEffects
  , ResearchEffects
  , ResearchLeafEffects
  , Subset
  , effectKeys
  , knownEffects
  )
import Tidepool.Effects.Core
  ( WorkerLaunchPreview (..)
  , ActorEffectKey (..)
  , AgentControl (..)
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
  , ForkContext (..)
  , WorkerLifetime (..)
  , ForkEffort (..)
  , Model (..)
  , AgentInspection (..)
  , WorktreeSpec (..)
  )
import Tidepool.Worktree (worktreeId)

data CampaignLabel = CampaignLabel Text
data ForkGroupLabel = ForkGroupLabel Text
data ForkGroupPath = ForkGroupPath Bool Text
  deriving (Show, Eq)

actorGitBranchPrefix :: ActorPath -> GitBranchPrefix
actorGitBranchPrefix (ActorPath path) = GitBranchPrefix ("shoal/" <> path)

instance IsString CampaignLabel where
  fromString = validatedLiteral campaignLabel

instance IsString ForkGroupLabel where
  fromString = validatedLiteral forkGroupLabel


validatedLiteral :: (Text -> Either NameError label) -> String -> label
validatedLiteral validate = either (error . show) id . validate . Text.pack

campaignLabel :: Text -> Either NameError CampaignLabel
campaignLabel = fmap CampaignLabel . validateSegment

forkGroupLabel :: Text -> Either NameError ForkGroupLabel
forkGroupLabel = fmap ForkGroupLabel . validateSegment

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
  deriving (Show, Eq)

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

data Branch (childEffects :: [Type -> Type]) input result where
  Branch
    :: ForkRole
    -> WorktreeSeed
    -> Effects childEffects
    -> BranchOptions
    -> Assignment input
    -> Branch childEffects input result

data BranchOptions = BranchOptions
  { branchInstructions :: Maybe Text
  , branchEffort :: Maybe ForkEffort
  , branchModel :: Maybe Model
  , branchContext :: ForkContext
  , branchLifetime :: WorkerLifetime
  , branchBudget :: Maybe ForkBudget
  }

defaultBranchOptions :: BranchOptions
defaultBranchOptions = BranchOptions Nothing Nothing Nothing InheritedContext ParentOwned Nothing

-- | Requested descendant generations and active descendants across the subtree.
-- The runtime clamps these to configured ceilings and remaining parent authority.
data ForkBudget = ForkBudget { forkDepth :: Int, forkWidth :: Int }
  deriving (Show, Eq)

-- | Effective authority. Nothing width means no concurrency ceiling.
data ForkAllowance = ForkAllowance { allowanceDepth :: Int, allowanceWidth :: Maybe Int }
  deriving (Show, Eq)

data DelegationAuthority = ForksOmitted | BudgetExhausted | CanFork
  deriving (Show, Eq)

data BranchPreview = BranchPreview
  { previewRole :: ForkRole
  , previewWorkspace :: ForkWorkspaceAccess
  , previewSource :: WorktreeSeed
  , previewRequestedBudget :: Maybe ForkBudget
  , previewEffectiveBudget :: ForkAllowance
  , previewEffects :: Text
  , previewContext :: ForkContext
  , previewLifetime :: WorkerLifetime
  , previewGuidance :: Maybe Text
  , previewLaunch :: Maybe WorkerLaunchPreview
  , previewDelegation :: DelegationAuthority
  } deriving (Show, Eq)

withForkBudget :: ForkBudget -> Branch child input result -> Branch child input result
withForkBudget budget (Branch role seed effects options assigned) =
  Branch role seed effects (options { branchBudget = Just budget }) assigned

budgetPair :: ForkBudget -> (Int, Int)
budgetPair (ForkBudget depth width) = (depth, width)

-- | Resolve policy and static host settings without allocating a worktree or
-- starting a provider. Nothing launch means no resolver in this embedding.
-- Admission still checks capacity, worktree seeds and provider provenance.
previewBranch
  :: Member Forks effects
  => Branch child input result
  -> Eff effects (Either Text BranchPreview)
previewBranch (Branch role seed effects options assigned) = do
  let keys = effectKeys effects
  answer <- send (ForksPreviewWith (roleCode (launchRoleFor role)) keys (budgetPair <$> branchBudget options) (branchModel options) (branchEffort options) (branchContext options) (branchInstructions options) (branchLifetime options))
  pure $ case answer of
    Left failure -> Left failure
    Right ((row, depth, width), launch) -> Right BranchPreview
      { previewRole = role
      , previewWorkspace = accessFor role
      , previewSource = seed
      , previewRequestedBudget = branchBudget options
      , previewEffectiveBudget = ForkAllowance depth width
      , previewEffects = row
      , previewContext = branchContext options
      , previewLifetime = branchLifetime options
      , previewGuidance = guidance assigned
      , previewLaunch = launch
      , previewDelegation = if not (includesForks keys) then ForksOmitted
          else if depth == 0 || width == Just 0 then BudgetExhausted else CanFork
      }
  where
    includesForks [] = False
    includesForks (EffectForks : _) = True
    includesForks (_ : rest) = includesForks rest

withEffort :: ForkEffort -> Branch child input result -> Branch child input result
withEffort effort (Branch role seed effects options assigned) =
  Branch role seed effects (options { branchEffort = Just effort }) assigned

-- | Provider model selection is independent of transcript ancestry.
withModel :: Model -> Branch child input result -> Branch child input result
withModel model (Branch role seed effects options assigned) =
  Branch role seed effects (options { branchModel = Just model }) assigned

data WorkerContext input = Inherited | Selected (input -> Text)

inherited :: WorkerContext input
inherited = Inherited

selected :: (input -> Text) -> WorkerContext input
selected = Selected

-- | Selected contexts receive the typed input and authored guidance in an
-- isolated Haskell scope and fresh TUI conversation. Imported project modules
-- remain available from the swarm's fixed source selection.
withContext :: WorkerContext input -> Branch child input result -> Branch child input result
withContext context (Branch role seed effects options assigned) = case context of
  Inherited -> Branch role seed effects (options { branchContext = InheritedContext }) assigned
  Selected render -> Branch role seed effects (options { branchContext = SelectedContext })
    (assigned { guidance = Just (render (input assigned)) })

-- | Persistent behavioral instructions, independent of task context and authority.
withInstructions :: Text -> Branch child input result -> Branch child input result
withInstructions body (Branch role seed effects options assigned) =
  Branch role seed effects (options { branchInstructions = Just body }) assigned

-- | Swarm-owned workers have selected contexts and outlive their creator.
-- The runtime admits them only from a top-level actor; authority is not widened.
withLifetime :: WorkerLifetime -> Branch child input result -> Branch child input result
withLifetime lifetime (Branch role seed effects options assigned) =
  Branch role seed effects (options { branchLifetime = lifetime }) assigned

data RolePolicy (childEffects :: [Type -> Type]) = RolePolicy ForkRole WorktreeSeed

inspectionPolicy :: WorktreeSeed -> RolePolicy childEffects
inspectionPolicy = RolePolicy ResearchFork

codingPolicy :: WorktreeSeed -> RolePolicy childEffects
codingPolicy = RolePolicy CodingFork

narrowed
  :: forall child result input
   . Effects child
  -> RolePolicy child
  -> Assignment input
  -> Branch child input result
narrowed effects (RolePolicy role seed) assigned =
  Branch role seed effects defaultBranchOptions assigned

researching
  :: forall result input
   . WorktreeSeed
  -> Assignment input
  -> Branch ResearchEffects input result
researching seed assigned = Branch ResearchFork seed knownEffects defaultBranchOptions assigned

-- | Inspection-only leaf, regardless of the host research recursion allowance.
researchingLeaf
  :: forall result input
   . WorktreeSeed
  -> Assignment input
  -> Branch ResearchLeafEffects input result
researchingLeaf seed assigned = Branch ResearchFork seed knownEffects defaultBranchOptions assigned

coding
  :: forall result input
   . WorktreeSeed
  -> Assignment input
  -> Branch CodingEffects input result
coding seed assigned = Branch CodingFork seed knownEffects defaultBranchOptions assigned

scaffolding
  :: forall result input
   . WorktreeSeed
  -> Assignment input
  -> Branch CodingEffects input result
scaffolding seed assigned =
  Branch ScaffoldingFork seed knownEffects defaultBranchOptions assigned

integrating
  :: forall result input
   . WorktreeSeed
  -> Assignment input
  -> Branch IntegrationEffects input result
integrating seed assigned =
  Branch IntegrationFork seed knownEffects defaultBranchOptions assigned

data ForkGroupHandle = ForkGroupHandle Int ActorPath
  deriving (Show, Eq)

data ForkGroupSnapshot = ForkGroupSnapshot
  { observedGroup :: ForkGroupHandle
  , groupRoster :: [AgentRosterEntry]
  }
  deriving (Show, Eq)

-- | Inspect exact admitted ancestry. Nothing means that the group or its
-- retained roster is unavailable to this actor; labels never select members.
observeForkGroup
  :: Member AgentInspection effs
  => ForkGroupHandle
  -> Eff effs (Maybe ForkGroupSnapshot)
observeForkGroup group@(ForkGroupHandle groupId _) = do
  roster <- send (AgentGroupListWith groupId)
  pure (ForkGroupSnapshot group <$> roster)

forkGroupHandle :: Response result -> Maybe ForkGroupHandle
forkGroupHandle response = do
  receipt <- responseLaunch response
  pure (ForkGroupHandle (forkGroupIdentity receipt) (allocatedForkGroupPath receipt))

forkGroupGitBranchPrefix :: ForkGroupHandle -> GitBranchPrefix
forkGroupGitBranchPrefix (ForkGroupHandle _ (ActorPath path)) =
  GitBranchPrefix ("shoal/" <> path <> "/")

planCleanup
  :: Member AgentInspection effs
  => ForkGroupHandle
  -> Eff effs CleanupPlan
planCleanup (ForkGroupHandle groupId _) = send (AgentInspectCleanupWith groupId)

executeCleanup
  :: Member AgentControl effs
  => CleanupPlan
  -> Eff effs CleanupReceipt
executeCleanup plan = send (AgentControlExecuteCleanupWith
  (cleanupPlanGroup plan)
  [ (cleanupActorId actor, cleanupActorIncarnation actor, cleanupActorRevision actor)
  | actor <- cleanupPlanActors plan
  ])

data Unfold (parent :: [Type -> Type]) result where
  PureU :: result -> Unfold parent result
  BranchU :: Int -> Branch child input result -> Unfold parent (Response result)
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
    -> Started parent (Response result)
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
  -> Unfold parent (Response result)
child = childSited 0

{-# OPAQUE childSited #-}
childSited
  :: forall result child input parent
   . (KnownEffects child, Subset child parent)
  => Int
  -> Branch child input result
  -> Unfold parent (Response result)
childSited = BranchU

{-# OPAQUE childWithProgress #-}
childWithProgress
  :: forall progress result child input parent
   . (KnownEffects child, Subset child parent)
  => Branch child input result
  -> Unfold parent (Response result, Progress progress)
childWithProgress = childWithProgressSited @progress @result @child @input @parent 0

{-# OPAQUE childWithProgressSited #-}
childWithProgressSited
  :: forall progress result child input parent
   . (KnownEffects child, Subset child parent)
  => Int
  -> Branch child input result
  -> Unfold parent (Response result, Progress progress)
childWithProgressSited site branch =
  (\response -> (response, Progress (responseRequestId response)))
    <$> childSited @result @child @input @parent site branch

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
  let names = branchNames plan
  -- Validated literals can otherwise remain thunks across the effect bridge.
  -- Force the whole launch shape before admission so an invalid later branch
  -- cannot launch an earlier child. Bridge-level strictness may replace this.
  begun <- forceTextList names `seq` send (ForksBeginWith relative groupName names)
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
    branchNames (BranchU _ (Branch _ _ _ _ assigned)) = [labelText (label assigned)]
    branchNames (ApU functions arguments) =
      branchNames functions <> branchNames arguments

    forceTextList :: [Text] -> ()
    forceTextList [] = ()
    forceTextList (value : values) = value `seq` forceTextList values

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
startBranch groupId allocated (Branch role seed effects options _) = do
  let (worktreeSpec, dirtyPolicy) = seedRequest allocated seed
  launched <- startForkedAgent
    (launchRoleFor role)
    groupId
    allocated
    worktreeSpec
    dirtyPolicy
    (effectKeys effects)
    (branchEffort options)
    (budgetPair <$> branchBudget options)
    (branchModel options)
    (branchContext options)
    (branchInstructions options)
    (branchLifetime options)
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
  -> Eff effects (Response result)
requestBranch site groupId (ForkGroupPath _ group) (Branch role _ _ options assigned) actor allocated tree = do
  let leaf = labelText (label assigned)
  let requested = group <> "/" <> leaf
      (actorId, incarnation) = agentIdentity actor
  response <- requestWithSited @result @input site actor assigned
  observed <- lookupAgent actor
  let pair maybeId maybeInc = (,) <$> maybeId <*> maybeInc
  pure (withResponseLaunch
    (BranchReceipt
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
        , launchedHaskellScope = rosterHaskellScope <$> observed
        }) response)

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
