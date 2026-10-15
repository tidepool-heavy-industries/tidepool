{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}
import Data.Text (Text)
import Tidepool.Actors.Exomonad
import Tidepool.Inspection (display)
import qualified Project.Researcher as Researcher

Right checkedResearcher <- validateAgentSpec Researcher.researcherSpec
researcher <- do
  spawned <- spawnSubagent
    (FreshCtx "Use search and open for source evidence. Complete each request with its active reply tool; preserve returned source URLs and references.")
    SameDir
    ((defaultValidatedSpawnOptions checkedResearcher) { spawnModel = Just Luna })
  case spawned of
    Left failure -> error (show failure)
    Right ready -> pure ready
Right reportRequest <- requestWithReplyTool @Researcher.Report researcher
  ("Which plants retain water? Search for moss and open a source; return a sourced report." :: Text)
  defaultRequestOptions
Right report <- await (result reportRequest)
display report
Right followUpRequest <- requestWithReplyTool @Researcher.FollowUp researcher
  ("Explain the same source in one sentence and cite its URL." :: Text)
  defaultRequestOptions
Right followUp <- await (result followUpRequest)
display followUp
