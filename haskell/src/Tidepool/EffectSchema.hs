module Tidepool.EffectSchema
  ( SiteTypePolicy(..)
  , VerbSpec(..)
  , sitedVerbs
  ) where

-- | How the value named by a typed suspension site crosses the boundary.
data SiteTypePolicy
  = CrossCompileAnswer
    -- ^ The value may be supplied by code compiled with a different effect
    -- row, so its type must not contain a concrete @Eff@ application.
  | InHeapAnswer
    -- ^ The value stays in the current machine heap; only monomorphism is
    -- required.
  deriving (Eq, Show)

-- | Declarative description of a surface verb rewritten to a site-aware
-- sibling during Core lowering.
data VerbSpec = VerbSpec
  { vsName :: String
  , vsModule :: String
  , vsSitedName :: String
  , vsSitedModule :: String
  , vsTypeArgs :: Int
  , vsValueArity :: Int
  , vsTypePolicy :: SiteTypePolicy
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
      "runLLMTurnSited" "Tidepool.Effects.Core" 1 1 CrossCompileAnswer False False
  , verb "runLLMTurnFork" "Tidepool.Effects.Core"
      "runLLMTurnForkSited" "Tidepool.Effects.Core" 1 1 CrossCompileAnswer False False
  , verb "runLLMTurnFanout" "Tidepool.Effects.Core"
      "runLLMTurnFanoutSited" "Tidepool.Effects.Core" 1 1 CrossCompileAnswer True False
  , verb "finalize" "Tidepool.Effects.Core"
      "finalizeSited" "Tidepool.Effects.Core" 2 1 InHeapAnswer False False
  , verb "fork" "Tidepool.Fork"
      "forkSited" "Tidepool.Effects.Core" 1 1 CrossCompileAnswer False False
  , verb "forkAll" "Tidepool.Fork"
      "forkAllSited" "Tidepool.Effects.Core" 1 1 CrossCompileAnswer True False
  , verb "forkMap" "Tidepool.Fork"
      "forkMapSited" "Tidepool.Fork" 2 2 CrossCompileAnswer True True
  , verb "forkCata" "Tidepool.Fork"
      "forkCataSited" "Tidepool.Fork" 2 2 CrossCompileAnswer True True
  ]
  where
    verb = VerbSpec
