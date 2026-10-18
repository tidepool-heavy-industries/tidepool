{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
module GeneratedModelChecks where

import Prelude
import Data.Text (Text)
import qualified Exomonad.Models as Models
import qualified Tidepool.Effects.Core as Core
import qualified Tidepool.Internal.ModelControl as Control

selections :: Text -> [(Text, Text, Core.Reasoning)]
selections identity =
  [ unpack (Models.modelSelection (Models.argon identity "argon" (Models.ArgonOptions Models.EffortMedium)))
  , unpack (Models.modelSelection (Models.deepSeek identity "flash" (Models.DeepSeekOptions Models.DeepSeekMax)))
  , unpack (Models.modelSelection (Models.openAI identity "sol" (Models.OpenAIOptions Models.OpenAILow)))
  ]
  where
    unpack (Control.ModelSelectionEnvelope identity key reasoning) = (identity, key, reasoning)

checkCatalog :: Text -> Bool
checkCatalog identity = selections identity ==
  [ (identity, "argon", Core.ArgonReasoning Core.EffortMedium)
  , (identity, "flash", Core.DeepSeekReasoning Core.DeepSeekMax)
  , (identity, "sol", Core.OpenAIReasoning Core.OpenAILow)
  ]

primaryMatches :: Text -> Bool
primaryMatches expected = case Models.modelSelection Models.primaryModel of
  Control.ModelSelectionEnvelope _ key _ -> key == expected
