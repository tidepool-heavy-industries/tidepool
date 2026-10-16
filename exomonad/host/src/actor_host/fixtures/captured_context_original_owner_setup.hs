import qualified Tidepool.Agent.Contract as A

data OriginalInput = OriginalInput Int deriving Show
data OriginalReply = OriginalReply Int deriving Show

Right ownerCheckpoint <- checkpoint "captured nominal owner"
Right originalOwner <- spawnSubagent (ForkCtx ownerCheckpoint) SameDir
  (defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies]))
