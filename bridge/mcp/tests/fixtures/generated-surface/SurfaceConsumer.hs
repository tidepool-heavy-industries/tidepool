{-# LANGUAGE DataKinds, GADTs, FlexibleContexts, RankNTypes, TypeOperators #-}
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
