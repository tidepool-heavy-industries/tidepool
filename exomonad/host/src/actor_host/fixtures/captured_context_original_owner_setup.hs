import qualified Tidepool.Agent.Contract as A

data OriginalInput = OriginalInput Int deriving Show
data OriginalReply = OriginalReply Int deriving Show

Right ownerCheckpoint <- checkpoint currentModel "captured nominal owner"
Right originalOwner <- spawnSubagent (ForkCtx ownerCheckpoint) SameDir
  (defaultSpawnOptions currentModel (A.defaultWorkbenchSpec @'[Replies]))
