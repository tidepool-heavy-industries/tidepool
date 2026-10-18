{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}
import qualified Data.Text as Text
import Tidepool.Actors.Exomonad
import Tidepool.Agent.Contract
import Tidepool.Inspection (display)
import qualified Persona.Spec as Persona
Right fileCompanion <- do
  checked <- validateAgentSpec (Persona.agentSpec @Persona.PersonaEffects)
  case checked of
    Left failure -> error (Text.pack (show failure))
    Right approved -> do
      display (case validationReceipt (validatedAgentSpecReport approved) of
        SpecValidationReceipt names effects _ SpecProviderChecked ->
          names == ["read_file", "write_file", "edit_file", "preview_edit", "haskell", "reload"]
          && "FsRead" `elem` effects && "FsWrite" `elem` effects
          && "Reload" `notElem` effects && "ContextReadWrite" `notElem` effects
        _ -> False)
      spawnSubagent (FreshCtx "Repair the draft with the installed file tools. Complete this request with the active typed reply tool.") SameDir
        (defaultValidatedSpawnOptions currentModel approved)
