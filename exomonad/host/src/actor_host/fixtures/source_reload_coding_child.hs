import qualified Tidepool.Agent.Contract as A
import Tidepool.Actors.Exomonad
import Tidepool.Effects.Core (Commands, Console, Lookup)
import Tidepool.Effects.Core (Source)

Right seed <- checkpoint "source-reload child context"
Right child <- spawnSubagent (ForkCtx seed) (ForkWorktree projectHead)
  ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies, Commands, Console, Lookup, BoundWorktree, Source]))
    { spawnLabel = Just "editor", spawnLifetime = ActorOwned
    , spawnInstructions = Just "Use the configured workspace tools for this request and return the typed result." })
Right worker <- request @Text child () (defaultRequestOptions { requestLabel = Just "editor" })
