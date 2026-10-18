import qualified Tidepool.Agent.Contract as A
import Tidepool.Actors.Exomonad
import qualified Tidepool.Actors.Exomonad as Actors
import Tidepool.Effects.Core (Commands, Lookup)
import qualified AgentSpec as Spec
import qualified Project.Tools as Tools
Right seed <- checkpoint currentModel "agent-spec child context"
Right child <- spawnSubagent (ForkCtx seed) (ForkWorktree projectHead) ((defaultSpawnOptions currentModel (Spec.agentSpec :: A.AgentSpec (Tools.SpecTools '[Replies, Commands, Lookup, BoundWorktree]) '[Replies, Commands, Lookup, BoundWorktree])) { spawnLabel = Just "editor", spawnLifetime = ActorOwned, spawnInstructions = Just "Use the configured workspace tools for this request and return the typed result." })
Right worker <- Actors.request @Text child () (defaultRequestOptions { requestLabel = Just "editor" })
