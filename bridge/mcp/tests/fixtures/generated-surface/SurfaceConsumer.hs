{-# LANGUAGE DataKinds, GADTs, FlexibleContexts, RankNTypes, TypeApplications, TypeOperators #-}
module SurfaceConsumer where
import Prelude
import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import Tidepool.Internal.RequestSite (RequestSite)
import qualified Tidepool.Effects.Core as Core
import qualified Tidepool.Effects.Authored as Authored
import qualified Tidepool.Worktree as Worktree
import qualified Tidepool.Event as Event
import qualified Tidepool.Actor as Actor
import Tidepool.Agent.Contract (AgentSpec, NoTools, defaultSpec)
import qualified Tidepool.Model as Model
import qualified Tidepool.Actors.Spawn as Spawn
import qualified Tidepool.Actors.Internal.Agent as Agent
import Tidepool.Agent.Ref (AgentRef)
import Tidepool.Agent.Reply (Replies, Request, RequestOptions (..), RequestError, Progress, defaultRequestOptions)
import qualified Tidepool.Scope as Scope

coreReceive :: RequestSite '[] next -> (forall result. api result -> Eff handlerEffects ()) -> Core.ActorLocal api next
coreReceive = Core.ActorReceiveWith

authoredConsole :: Text -> Authored.Console ()
authoredConsole = Authored.Print

worktreeIdentity :: Core.WorktreeHandle -> Core.WorktreeId
worktreeIdentity = Worktree.worktreeId

worktreeHead :: Member Core.Worktree effects => Core.WorktreeHandle -> Eff effects Core.GitOid
worktreeHead = Worktree.worktreeHead

repositoryChoice :: Event.Event a -> Event.Event a -> Event.Event a
repositoryChoice = (Event.<|>)

modelSpec :: AgentSpec NoTools effects
modelSpec = defaultSpec

modelTurn :: Model.ModelTurn NoTools effects Text
modelTurn = Model.textTurn modelSpec "compiled model contract"

scopeBody :: Member Core.ResourceScopes effects => Eff effects (Scope.ScopeOutcome (Int -> Int))
scopeBody = Scope.withScope $ \_ -> pure (+ 1)

scopeLifetime :: Scope.Scope -> Core.WorkerLifetime
scopeLifetime = Core.InScope

-- The child's declared effects are independent of the helper's executed row.
idleChild :: Member Core.AgentLaunch effects => Eff effects (Either Core.SpawnError AgentRef)
idleChild = Spawn.spawnSubagent (Spawn.FreshCtx "Review the supplied value") Spawn.SameDir
  (Spawn.defaultSpawnOptions (defaultSpec :: AgentSpec NoTools '[]))

rawRequest :: Member Replies effects => AgentRef -> Eff effects (Either RequestError (Request Text))
rawRequest agent = Agent.request @Text agent ("question" :: Text) defaultRequestOptions

progressRequest :: Member Replies effects => AgentRef -> Eff effects (Either RequestError (Request Text, Progress Int))
progressRequest agent = Agent.requestWithProgress @Int @Text agent ("question" :: Text)
  (defaultRequestOptions {requestLabel = Just "ordinary label with spaces"})

sharedWorkspace :: Spawn.WorkspaceHandle -> Spawn.Workspace
sharedWorkspace = Spawn.ExistingWorkspace

-- The ordinary authored vocabulary keeps the runtime-issued workspace type
-- nameable while the safe issuer supplies its only public construction path.
issuedWorkspace :: Member Core.BoundWorktree effects => Eff effects (Either Authored.WorktreeError Spawn.Workspace)
issuedWorkspace = fmap (fmap Spawn.ExistingWorkspace) Spawn.currentWorkspace

abstractWorkspace :: Authored.WorkspaceHandle -> Spawn.Workspace
abstractWorkspace = Spawn.ExistingWorkspace
