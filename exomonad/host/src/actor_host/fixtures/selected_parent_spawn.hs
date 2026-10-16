import qualified Tidepool.Actor as Actor
import qualified Tidepool.Actors.Spawn as Spawn
import qualified Tidepool.Agent.Contract as Contract
import qualified Tidepool.Agent.Watch as Await
import Tidepool.Agent.Reply (Replies, Request, retainRequest)
import Tidepool.Effects.Core (Actor, AgentLaunch, Commands, WorkerLifetime(..))
import Tidepool.Effects.Row (knownEffects)
let selectedLeaf :: Actor.ActorDefinition () Maybe Int
    selectedLeaf = Actor.ActorDefinition
      { Actor.label = "selected-start-leaf"
      , Actor.effectProfile = Actor.Selected (knownEffects @'[Replies])
      , Actor.initialization = pure
      , Actor.behavior = \() () -> pure 42
      , Actor.onShutdown = \_ -> pure () }
    selectedParent :: Actor.ActorDefinition () ((->) (Bool, Maybe (Request Int))) ()
    selectedParent = Actor.ActorDefinition
      { Actor.label = "selected-launch-parent"
      , Actor.effectProfile = Actor.Selected (knownEffects @'[Actor, AgentLaunch, Replies, Await.Watches])
      , Actor.initialization = \() -> pure (False, Nothing)
      , Actor.behavior = \() initial -> Actor.serve initial $ \(accepted, response) observe -> do
          next <- case response of
            Nothing -> do
              leaf <- Actor.startActor selectedLeaf ()
              leafExit <- Actor.awaitExit leaf
              wider <- Spawn.spawnSubagent (Spawn.FreshCtx "refused wider child") Spawn.SameDir
                (Spawn.defaultSpawnOptions (Contract.defaultAsyncWorkbenchSpec @'[Replies, Commands]))
              duplicate <- Spawn.spawnSubagent (Spawn.FreshCtx "refused duplicate child") Spawn.SameDir
                (Spawn.defaultSpawnOptions (Contract.defaultAsyncWorkbenchSpec @'[Replies, Replies]))
              native <- Spawn.spawnSubagent (Spawn.FreshCtx "accepted subset child") Spawn.SameDir
                (Spawn.defaultSpawnOptions (Contract.defaultAsyncWorkbenchSpec @'[Replies]))
              worker <- case native of
                Left failure -> error (tshow failure)
                Right admitted -> pure admitted
              admission <- request @Int worker () defaultRequestOptions
              pending <- case admission of
                Left failure -> error (tshow failure)
                Right admitted -> pure admitted
              retained <- retainRequest pending ActorOwned
              case retained of
                Left failure -> error (tshow failure)
                Right () -> pure ()
              pure ((case leafExit of { Actor.Completed 42 -> True; _ -> False })
                && (case wider of { Left (Spawn.SpawnRefused _) -> True; _ -> False })
                && (case duplicate of { Left (Spawn.SpawnRefused _) -> True; _ -> False }), Just pending)
            Just pending -> do
              settled <- Await.await (Await.result pending)
              pure (accepted && settled == Right 42, response)
          pure (observe next, next)
      , Actor.onShutdown = \_ -> pure () }
selectedOwner <- Actor.startActor selectedParent ()
