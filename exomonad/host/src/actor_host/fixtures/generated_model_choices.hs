{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
module GeneratedModelChecks where

import Prelude
import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Exomonad.Models as Models
import qualified Tidepool.Effects.Core as Core
import qualified Tidepool.Internal.ModelControl as Control

selections :: [(Text, Text, Core.Reasoning)]
selections = map (unpack . Models.modelSelection)
  [ Models.Argon (Models.ArgonOptions Models.EffortMedium)
  , Models.Flash (Models.DeepSeekOptions Models.DeepSeekMax)
  , Models.Sol (Models.OpenAIOptions Models.OpenAILow)
  ]
  where
    unpack (Control.ModelSelectionEnvelope identity key reasoning) = (identity, key, reasoning)

checkCatalog :: Text -> Bool
checkCatalog identity = selections ==
  [ (identity, "argon", Core.ArgonReasoning Core.EffortMedium)
  , (identity, "flash", Core.DeepSeekReasoning Core.DeepSeekMax)
  , (identity, "sol", Core.OpenAIReasoning Core.OpenAILow)
  ]

primaryMatches :: Text -> Bool
primaryMatches expected = case Models.modelSelection Models.primaryModel of
  Control.ModelSelectionEnvelope _ key _ -> key == expected

-- The test appends stagePrimary with the selected primary's exact effort type.
