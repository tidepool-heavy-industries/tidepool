module Tidepool.EffectSchema
  ( VerbSpec(..)
  , SiteAnswerSource(..)
  , NominalHead(..)
  , SiteType(..)
  , YieldSite(..)
  , SiteTypePosition(..)
  , polymorphicSiteMessage
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
  -- Presentation captured from the concrete answer TyCon; no downstream lookup.
  , ysReplyDeclaration :: Maybe Text
  }
  deriving (Eq, Show)

-- | Which type of a suspension site failed the monomorphism requirement.
data SiteTypePosition = SiteInput | SiteResult
  deriving (Eq, Show)

-- | The source diagnostic for a polymorphic typed site. Both the Core
-- translator and the prepared elaborator render it through this one function.
polymorphicSiteMessage :: String -> SiteTypePosition -> String -> String -> String
polymorphicSiteMessage verb position siteDesc typeStr =
  "polymorphic " ++ what ++ " site in " ++ siteDesc ++ ": " ++ typeStr ++ "\n" ++ advice
  where
    (what, advice) = case position of
      SiteInput -> (verb ++ " input", "The input type is unresolved. Add a concrete type annotation to the input.")
      SiteResult -> (verb, "The result type is unresolved. Add a concrete result type annotation or visible type application, for example `"
        ++ verb ++ " @Finding ...` when Finding is your intended result type.")

-- | Declarative description of a surface verb rewritten to a site-aware
-- sibling during Core lowering.
data VerbSpec = VerbSpec
  { vsName :: String
  , vsModule :: String
  , vsSitedName :: String
  , vsSitedModule :: String
  , vsListAnswer :: Bool
  , vsInputTypeArgs :: [Int]
  , vsAnswerSource :: SiteAnswerSource
  }
  deriving (Eq, Show)

-- | Where a surface verb exposes the value an eventual suspension resumes.
-- Most verbs name it as their first type argument; progress-aware verbs
-- select a later argument so progress remains separate from the result.
data SiteAnswerSource
  = FirstTypeArgument
  | TypeArgument Int
  deriving (Eq, Show)

-- | The complete typed-suspension vocabulary understood by the extractor.
-- Adding a verb is one row here; recognition and sibling resolution both
-- consume this table.
sitedVerbs :: [VerbSpec]
sitedVerbs =
  [ verb "runLLMTurn" "Tidepool.Effects.Core"
      "runLLMTurnSited" "Tidepool.Effects.Core" False []
  , verb "runLLMTurnFork" "Tidepool.Effects.Core"
      "runLLMTurnForkSited" "Tidepool.Effects.Core" False []
  , verb "runLLMTurnFanout" "Tidepool.Effects.Core"
      "runLLMTurnFanoutSited" "Tidepool.Effects.Core" True []
  , verb "finalize" "Tidepool.Effects.Core"
      "finalizeSited" "Tidepool.Effects.Core" False []
  , verb "fork" "Tidepool.Answerer.Fork"
      "forkSited" "Tidepool.Effects.Core" False []
  , verb "forkAll" "Tidepool.Answerer.Fork"
      "forkAllSited" "Tidepool.Effects.Core" True []
  , verb "forkMap" "Tidepool.Answerer.Fork"
      "forkMapSited" "Tidepool.Answerer.Fork" True []
  , verb "forkCata" "Tidepool.Answerer.Fork"
      "forkCataSited" "Tidepool.Answerer.Fork" True []
  , verb "request" "Tidepool.Actors.Internal.Agent"
      "requestSited" "Tidepool.Actors.Internal.Agent" False [1]
  , verb "requestWith" "Tidepool.Actors.Internal.Agent"
      "requestWithSited" "Tidepool.Actors.Internal.Agent" False [1]
  , (verb "requestWithProgress" "Tidepool.Actors.Internal.Agent"
      "requestWithProgressSited" "Tidepool.Actors.Internal.Agent" False [2, 0])
      { vsAnswerSource = TypeArgument 1 }
  , (verb "requestWithProgressInto" "Tidepool.Actors.Internal.Agent"
      "requestWithProgressIntoSited" "Tidepool.Actors.Internal.Agent" False [2, 0])
      { vsAnswerSource = TypeArgument 1 }
  , VerbSpec
      { vsName = "child"
      , vsModule = "Tidepool.Actors.Unfold"
      , vsSitedName = "childSited"
      , vsSitedModule = "Tidepool.Actors.Unfold"
      , vsListAnswer = False
      , vsInputTypeArgs = [2]
      , vsAnswerSource = TypeArgument 0
      }
  , (verb "childWithProgress" "Tidepool.Actors.Unfold"
      "childWithProgressSited" "Tidepool.Actors.Unfold" False [3, 0])
      { vsAnswerSource = TypeArgument 1 }
  , verb "receive" "Tidepool.Actor"
      "receiveSited" "Tidepool.Actor" False []
  , verb "serve" "Tidepool.Actor"
      "serveSited" "Tidepool.Actor" False []
  ]
  where
    verb name source sibling siblingSource listAnswer inputs =
      VerbSpec name source sibling siblingSource listAnswer inputs FirstTypeArgument
