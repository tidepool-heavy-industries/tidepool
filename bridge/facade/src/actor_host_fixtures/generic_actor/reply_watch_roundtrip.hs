import qualified Tidepool.Agent.Contract as A
import Tidepool.Effects.Core (Commands, Lookup)

data ReplyReport = ReplyReport Int deriving (Show, Eq)
-- TIDEPOOL-ITEM --
data EchoReport = EchoReport Text deriving (Show, Eq)
-- TIDEPOOL-ITEM --
data ScaffoldReport = ScaffoldReport Text deriving (Show, Eq)
-- TIDEPOOL-ITEM --
first3 (value, _, _) = value
second3 (_, value, _) = value
third3 (_, _, value) = value
-- TIDEPOOL-ITEM --
Right replyTypesContext <- checkpoint "nominal reply types and helpers"
Right workerAgent <- spawnSubagent
  (ForkCtx replyTypesContext)
  (ForkWorktree projectHead)
  ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies, Commands, Lookup, BoundWorktree]))
    { spawnLabel = Just "worker"
    , spawnLifetime = ActorOwned
    , spawnInstructions = Just "Complete the worker assignment and report its typed result."
    })
Right witnessAgent <- spawnSubagent
  (ForkCtx replyTypesContext)
  (ForkWorktree projectHead)
  ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies, Commands, Lookup, BoundWorktree]))
    { spawnLabel = Just "witness"
    , spawnLifetime = ActorOwned
    , spawnInstructions = Just "Complete the witness assignment and report its typed result."
    })
Right scaffoldAgent <- spawnSubagent
  (ForkCtx replyTypesContext)
  (ForkWorktree projectHead)
  ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies, Commands, Lookup, BoundWorktree]))
    { spawnLabel = Just "scaffold"
    , spawnLifetime = ActorOwned
    , spawnInstructions = Just "Complete the scaffold assignment and report its typed result."
    })
-- TIDEPOOL-ITEM --
let workerOptions = defaultRequestOptions
      { requestLabel = Just "worker"
      , requestDeadline = Just (minutes 5)
      }
Right workerRequest <- request @ReplyReport workerAgent (41 :: Int) workerOptions
Right witnessRequest <- request @EchoReport witnessAgent ("cache" :: Text)
  (defaultRequestOptions { requestLabel = Just "witness" })
Right scaffoldRequest <- request @ScaffoldReport scaffoldAgent ("recursive" :: Text)
  (defaultRequestOptions { requestLabel = Just "scaffold" })
-- TIDEPOOL-ITEM --
let workers = (workerRequest, witnessRequest, scaffoldRequest)
-- TIDEPOOL-ITEM --
readiness <- watch (Just "both-ready")
  ((,) <$> settlement (first3 workers) <*> settlement (second3 workers))
-- TIDEPOOL-ITEM --
initially <- pollWatch readiness
