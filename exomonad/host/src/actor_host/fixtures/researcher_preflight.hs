{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}
import qualified AgentSpec
import qualified Project.Researcher as Researcher
import qualified Data.Text as DT
import Tidepool.Agent.Contract
import Tidepool.Agent.Reply (Replies)
checked <- validateAgentSpec AgentSpec.invalidSpec
refused <- spawnSubagent (FreshCtx "Must not be launched.") SameDir
  (defaultSpawnOptions AgentSpec.invalidSpec)
plain <- validateAgentSpec (defaultWorkbenchSpec @'[Replies])
research <- validateAgentSpec Researcher.researcherSpec
display (DT.pack (case checked of Left failure -> show failure; Right _ -> "scalar unexpectedly accepted"))
display (DT.pack (case refused of Left failure -> show failure; Right _ -> "scalar unexpectedly launched"))
display (DT.pack (case plain of Left failure -> show failure; Right _ -> "ordinary spec accepted"))
display (DT.pack (case research of Left failure -> show failure; Right _ -> "researcher accepted"))
display (case (checked, refused) of
  (Left (SpecInputSchemaInvalid _), Left (SpawnInvalidSpec (SpecInputSchemaInvalid _))) -> True
  _ -> False)
