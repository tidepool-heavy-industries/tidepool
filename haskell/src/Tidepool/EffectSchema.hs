module Tidepool.EffectSchema
  ( VerbSpec(..)
  , sitedVerbs
  ) where

-- | Declarative description of a surface verb rewritten to a site-aware
-- sibling during Core lowering.
data VerbSpec = VerbSpec
  { vsName :: String
  , vsModule :: String
  , vsSitedName :: String
  , vsSitedModule :: String
  , vsTypeArgs :: Int
  , vsValueArity :: Int
  , vsListAnswer :: Bool
  , vsMisShapeIsError :: Bool
  }
  deriving (Eq, Show)

-- | The complete typed-suspension vocabulary understood by the extractor.
-- Adding a verb is one row here; recognition and sibling resolution both
-- consume this table.
sitedVerbs :: [VerbSpec]
sitedVerbs =
  [ verb "runLLMTurn" "Tidepool.Effects.Core"
      "runLLMTurnSited" "Tidepool.Effects.Core" 1 1 False False
  , verb "runLLMTurnFork" "Tidepool.Effects.Core"
      "runLLMTurnForkSited" "Tidepool.Effects.Core" 1 1 False False
  , verb "runLLMTurnFanout" "Tidepool.Effects.Core"
      "runLLMTurnFanoutSited" "Tidepool.Effects.Core" 1 1 True False
  , verb "finalize" "Tidepool.Effects.Core"
      "finalizeSited" "Tidepool.Effects.Core" 2 1 False False
  , verb "fork" "Tidepool.Fork"
      "forkSited" "Tidepool.Effects.Core" 1 1 False False
  , verb "forkAll" "Tidepool.Fork"
      "forkAllSited" "Tidepool.Effects.Core" 1 1 True False
  , verb "forkMap" "Tidepool.Fork"
      "forkMapSited" "Tidepool.Fork" 2 2 True True
  , verb "forkCata" "Tidepool.Fork"
      "forkCataSited" "Tidepool.Fork" 2 2 True True
  ]
  where
    verb = VerbSpec
