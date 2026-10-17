{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}
import Data.Text (Text)
import qualified Tidepool.Actors.Exomonad as Agent
import qualified Data.Text as Text
import Tidepool.Actors.Exomonad
import Tidepool.Inspection (display)
import qualified Project.Researcher as Researcher

Right checkedResearcher <- validateAgentSpec Researcher.researcherSpec
researcher <- do
  spawned <- spawnSubagent
    (FreshCtx "Use search and open for source evidence. Complete each request with its active reply tool; preserve returned source URLs and references.")
    SameDir
    (defaultValidatedSpawnOptions checkedResearcher)
  case spawned of
    Left failure -> error (Text.pack (show failure))
    Right ready -> pure ready
reportRequest <- do
  requested <- requestWithReplyTool @Researcher.Report researcher
    ("Which plants retain water? Search for moss and open a source; return a sourced report." :: Text)
    defaultRequestOptions
  case requested of
    Left failure -> error (Text.pack (show failure))
    Right ready -> pure ready
report <- either (error . Text.pack . show) pure =<< await (Agent.result reportRequest)
display report
Right followUpRequest <- requestWithReplyTool @Researcher.FollowUp researcher
  ("Explain the same source in one sentence and cite its URL." :: Text)
  defaultRequestOptions
followUp <- either (error . Text.pack . show) pure =<< await (Agent.result followUpRequest)
display followUp
