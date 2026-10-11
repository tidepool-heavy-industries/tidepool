import qualified Tidepool.Actors.Spawn as Spawn
import qualified Tidepool.Agent.Contract as Contract
nativeAdmission <- Spawn.spawnSubagent
  (Spawn.FreshCtx "Native request receiver fixture") Spawn.SameDir
  ((Spawn.defaultSpawnOptions (Contract.defaultAsyncWorkbenchSpec @'[Replies]))
    { Spawn.spawnLabel = Just "terminal-transfer-target" })
nativeAgent <- case nativeAdmission of
  Left failure -> error (show failure)
  Right agent -> pure agent
pure True
