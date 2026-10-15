import qualified AgentSpec
import Tidepool.Agent.Contract
checked <- validateAgentSpec AgentSpec.invalidSpec
refused <- spawnSubagent (FreshCtx "Must not be launched.") SameDir
  (defaultSpawnOptions AgentSpec.invalidSpec)
display (case (checked, refused) of
  (Left (SpecInputSchemaInvalid _), Left (SpawnInvalidSpec (SpecInputSchemaInvalid _))) -> True
  _ -> False)
