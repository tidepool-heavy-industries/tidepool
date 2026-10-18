{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}
import Data.Text (Text)
import qualified Tidepool.Actors.Exomonad as Agent
import qualified Data.Text as Text
import Tidepool.Actors.Exomonad
import Tidepool.Inspection (display)
import qualified Project.Researcher as Researcher
import Project.PositiveReport (PositiveReport)
import qualified Exomonad.Models as Models
import qualified Tidepool.Model.Selection as Selection

let fixtureOpenAI key options = Selection.openAI Models.catalogIdentity key options

Right checkedResearcher <- validateAgentSpec Researcher.researcherSpec
researcher <- do
  spawned <- spawnSubagent
    (FreshCtx "Use search and open for source evidence. Complete each request with its active reply tool; preserve returned source URLs and references.")
    SameDir
    ((defaultValidatedSpawnOptions currentModel checkedResearcher) { spawnModel = fixtureOpenAI "luna" (Selection.OpenAIOptions Selection.OpenAILow) })
  case spawned of
    Left failure -> error (Text.pack (show failure))
    Right ready -> pure ready
reportRequest <- do
  requested <- requestWithReplyTool @PositiveReport researcher
    ("Which plants retain water? Search for moss and open a source; return a sourced report." :: Text)
    defaultRequestOptions
  case requested of
    Left failure -> error (Text.pack (show failure))
    Right ready -> pure ready
report <- either (error . Text.pack . show) pure =<< await (Agent.result reportRequest)
display report
