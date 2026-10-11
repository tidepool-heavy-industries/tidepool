import qualified Tidepool.Actor as Actor
import qualified Tidepool.Actors.Spawn as Spawn
import qualified Tidepool.Agent.Contract as Contract
import qualified Tidepool.Agent.Watch as Await
import Tidepool.Agent.Reply (Replies)
import Tidepool.Effects.Core (Actor, AgentLaunch, Commands)
import Tidepool.Effects.Row (knownEffects)
let selectedLeaf :: Actor.ActorDefinition () Maybe Int
    selectedLeaf = Actor.ActorDefinition
      { Actor.label = "selected-start-leaf"
      , Actor.effectProfile = Actor.Selected (knownEffects @'[Replies])
      , Actor.initialization = pure
      , Actor.behavior = \() () -> pure 42
      , Actor.onShutdown = \_ -> pure () }
    selectedParent :: Actor.ActorDefinition () Maybe Bool
    selectedParent = Actor.ActorDefinition
      { Actor.label = "selected-launch-parent"
      , Actor.effectProfile = Actor.Selected (knownEffects @'[Actor, AgentLaunch, Replies, Await.Watches])
      , Actor.initialization = \() -> do
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
          pure (leafExit, wider, duplicate, worker)
      , Actor.behavior = \() (leafExit, wider, duplicate, worker) -> do
          admission <- request @Int worker () defaultRequestOptions
          pending <- case admission of
            Left failure -> error (tshow failure)
            Right admitted -> pure admitted
          reply <- Await.await (Await.result pending)
          pure ((case leafExit of { Actor.Completed 42 -> True; _ -> False })
            && (case wider of { Left (Spawn.SpawnRefused _) -> True; _ -> False })
            && (case duplicate of { Left (Spawn.SpawnRefused _) -> True; _ -> False })
            && reply == Right 42)
      , Actor.onShutdown = \_ -> pure () }
selectedOwner <- Actor.startActor selectedParent ()
