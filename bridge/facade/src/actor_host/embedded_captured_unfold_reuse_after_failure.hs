{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

import qualified Tidepool.Agent.Contract as A
import Tidepool.Actors.Exomonad

do
  Just seed <- R.call (readSeed (R.client seedStore)) ()
  Right again <- spawnSubagent (ForkCtx seed) (ForkWorktree projectHead)
    ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies, Commands, Lookup, BoundWorktree]))
      { spawnLabel = Just "capture-still-usable", spawnLifetime = ActorOwned })
  Right againRequest <- request @Int again ("reply with the retained getter" :: Text)
    (defaultRequestOptions { requestLabel = Just "capture-still-usable" })
  replies <- watch (Just "reused-captured-reply") (settlement againRequest)
  result <- await (observed replies)
  released <- releaseCheckpoint seed
  refused <- spawnSubagent (ForkCtx seed) (ForkWorktree projectHead)
    (defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies, Commands, Lookup, BoundWorktree]))
  cleanup <- stopAgent again
  case (result, released, refused, cleanup) of
    (Right (Right 42), Right (), Left (SpawnRefused _), StoppedNow) -> display True
    (Right (Right 42), Right (), Left (SpawnRefused _), AlreadyStopped) -> display True
    _ -> display False
