-- | Immutable provenance for one launched request.
module Tidepool.Agent.Launch
  ( ActorPath (..)
  , GitBranchPrefix (..)
  , ForkRole (..)
  , ForkWorkspaceAccess (..)
  , BranchReceipt (..)
  ) where

import Data.Text (Text)
import Prelude

import Tidepool.Effects.Core (WorktreeReceipt)

newtype ActorPath = ActorPath Text
  deriving (Show, Eq, Ord)

newtype GitBranchPrefix = GitBranchPrefix Text
  deriving (Show, Eq, Ord)

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
  , launchedHaskellScope :: Maybe Int
  }
  deriving (Show, Eq)
