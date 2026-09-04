module Tidepool.EffectSchema
  ( VerbSpec(..)
  , SiteAnswerSource(..)
  , NominalHead(..)
  , SiteType(..)
  , YieldSite(..)
  , sitedVerbs
  ) where

import Data.Text (Text)
import Data.Word (Word64)
import Tidepool.TypePolicy (NominalHead(..))

-- | One GHC-rendered, monomorphic type crossing a suspension boundary.
data SiteType = SiteType
  { stType :: Text
  , stModules :: [Text]
  , stHeads :: [NominalHead]
  }
  deriving (Eq, Show)

-- | Compile-time type metadata for one suspension call site. The answer is
-- always present; @ysInputs@ contains only live inputs an interpreter must
-- mount back into a typed workbench.
data YieldSite = YieldSite
  { ysSite :: Word64
  , ysOrigin :: Text
  , ysOrdinal :: Word64
  , ysAnswer :: SiteType
  , ysInputs :: [SiteType]
  }
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
  , vsListAnswer :: Bool
  , vsMisShapeIsError :: Bool
  , vsInputTypeArgs :: [Int]
  , vsAnswerSource :: SiteAnswerSource
  }
  deriving (Eq, Show)

-- | Where a surface verb exposes the value an eventual suspension resumes.
-- Most primitive verbs name it as their first visible type argument. A
-- higher-level action combinator can instead return that value directly.
data SiteAnswerSource
  = FirstTypeArgument
  | TypeArgument Int
  | AppliedResultType
  deriving (Eq, Show)

-- | The complete typed-suspension vocabulary understood by the extractor.
-- Adding a verb is one row here; recognition and sibling resolution both
-- consume this table.
sitedVerbs :: [VerbSpec]
sitedVerbs =
  [ verb "runLLMTurn" "Tidepool.Effects.Core"
      "runLLMTurnSited" "Tidepool.Effects.Core" 1 1 False False []
  , verb "runLLMTurnFork" "Tidepool.Effects.Core"
      "runLLMTurnForkSited" "Tidepool.Effects.Core" 1 1 False False []
  , verb "runLLMTurnFanout" "Tidepool.Effects.Core"
      "runLLMTurnFanoutSited" "Tidepool.Effects.Core" 1 1 True False []
  , verb "finalize" "Tidepool.Effects.Core"
      "finalizeSited" "Tidepool.Effects.Core" 2 1 False False []
  , verb "fork" "Tidepool.Answerer.Fork"
      "forkSited" "Tidepool.Effects.Core" 1 1 False False []
  , verb "forkAll" "Tidepool.Answerer.Fork"
      "forkAllSited" "Tidepool.Effects.Core" 1 1 True False []
  , verb "forkMap" "Tidepool.Answerer.Fork"
      "forkMapSited" "Tidepool.Answerer.Fork" 2 2 True True []
  , verb "forkCata" "Tidepool.Answerer.Fork"
      "forkCataSited" "Tidepool.Answerer.Fork" 2 2 True True []
  , verb "request" "Tidepool.Actors.Internal.Agent"
      "requestSited" "Tidepool.Actors.Internal.Agent" 2 3 False True [1]
  , verb "requestWith" "Tidepool.Actors.Internal.Agent"
      "requestWithSited" "Tidepool.Actors.Internal.Agent" 2 2 False True [1]
  , VerbSpec
      { vsName = "child"
      , vsModule = "Tidepool.Actors.Unfold"
      , vsSitedName = "childSited"
      , vsSitedModule = "Tidepool.Actors.Unfold"
      , vsTypeArgs = 4
      , vsValueArity = 1
      , vsListAnswer = False
      , vsMisShapeIsError = True
      , vsInputTypeArgs = [2]
      , vsAnswerSource = TypeArgument 0
      }
  , verb "receive" "Tidepool.Actor"
      "receiveSited" "Tidepool.Actor" 1 1 False True []
  , verb "serve" "Tidepool.Actor"
      "serveSited" "Tidepool.Actor" 1 2 False True []
  ]
  where
    verb name source sibling siblingSource typeArgs valueArity listAnswer shapeError inputs =
      VerbSpec name source sibling siblingSource typeArgs valueArity listAnswer shapeError inputs FirstTypeArgument
