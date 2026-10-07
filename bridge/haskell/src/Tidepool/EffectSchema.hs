module Tidepool.EffectSchema
  ( VerbSpec(..)
  , SiteAnswerSource(..)
  , SiteDelivery(..)
  , SiteWireSource(..)
  , NominalHead(..)
  , SiteType(..)
  , YieldSite(..)
  , mergeYieldSites
  , SiteTypePosition(..)
  , polymorphicSiteMessage
  , sitedVerbs
  ) where

import Data.Text (Text)
import Data.Word (Word64)
import Control.Monad (foldM)
import Data.Map.Strict qualified as Map
import Tidepool.CheckedCell (CheckedTypeWitness, RequestTypeSignatures)
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
  , ysInputTypeWitnesses :: [Maybe CheckedTypeWitness]
  -- Presentation captured from the concrete answer TyCon; no downstream lookup.
  , ysReplyDeclaration :: Maybe Text
  , ysRequestTypeSignatures :: Maybe RequestTypeSignatures
  }
  deriving (Eq, Show)

-- | One site identity owns its complete compiler metadata. Repeated exact
-- evidence is shared; conflicting inputs, signatures or presentation refuse.
mergeYieldSites :: [YieldSite] -> Either Word64 [YieldSite]
mergeYieldSites sites = Map.elems <$> foldM admit Map.empty sites
  where
    admit known site = case Map.lookup (ysSite site) known of
      Just previous | previous /= site -> Left (ysSite site)
      _ -> Right (Map.insert (ysSite site) site known)

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
  , vsDerivedInput :: Maybe (Int, SiteWireSource)
  , vsAnswerSource :: SiteAnswerSource
  , vsDelivery :: SiteDelivery
  , vsWireSource :: SiteWireSource
  }
  deriving (Eq, Show)

-- | Where a surface verb exposes the value an eventual suspension resumes.
-- Most verbs name it as their first type argument; progress-aware verbs
-- select a later argument so progress remains separate from the result.
data SiteAnswerSource
  = FirstTypeArgument
  | TypeArgument Int
  | EffectResult
  deriving (Eq, Show)

data SiteDelivery
  = DeliverHostAnswer
  | DeliverLiveReentry
  | DeliverExitCellFill
  | DeliverTerminalCapture
  deriving (Eq, Ord, Show)

data SiteWireSource
  = SelectedAnswer
  | ListAnswer
  | ResponseResultEvidence
  | ProgressStateEvidence
  deriving (Eq, Ord, Show)

-- | The complete typed-suspension vocabulary understood by the extractor.
-- Adding a verb is one row here; recognition and sibling resolution both
-- consume this table.
sitedVerbs :: [VerbSpec]
sitedVerbs =
  [ verb "request" "Tidepool.Actors.Internal.Agent"
      "requestSited" "Tidepool.Actors.Internal.Agent" False [1]
      DeliverExitCellFill ResponseResultEvidence
  , (verb "requestWithProgress" "Tidepool.Actors.Internal.Agent"
      "requestWithProgressSited" "Tidepool.Actors.Internal.Agent" False [2, 0]
      DeliverExitCellFill ResponseResultEvidence)
      { vsAnswerSource = TypeArgument 1 }
  , (verb "requestWithProgressInto" "Tidepool.Actors.Internal.Agent"
      "requestWithProgressIntoSited" "Tidepool.Actors.Internal.Agent" False [2, 0]
      DeliverExitCellFill ResponseResultEvidence)
      { vsAnswerSource = TypeArgument 1 }
  , (verb "currentRequest" "Tidepool.Agent.Reply.Internal"
      "currentRequestSited" "Tidepool.Agent.Reply.Internal" False [0, 1]
      DeliverHostAnswer SelectedAnswer)
      { vsAnswerSource = EffectResult
      , vsDerivedInput = Just (1, ResponseResultEvidence)
      }
  , (verb "reportRequestProgress" "Tidepool.Agent.Reply.Internal"
      "reportRequestProgressSited" "Tidepool.Agent.Reply.Internal" False [0]
      DeliverHostAnswer SelectedAnswer) { vsAnswerSource = EffectResult }
  , (verb "pollProgress" "Tidepool.Agent.Reply.Internal"
      "pollProgressSited" "Tidepool.Agent.Reply.Internal" False [0]
      DeliverHostAnswer SelectedAnswer) { vsAnswerSource = EffectResult }
  , verb "after" "Tidepool.Agent.Watch.Internal"
      "afterSited" "Tidepool.Agent.Watch.Internal" False [0]
      DeliverHostAnswer ProgressStateEvidence
  , verb "progressSource" "Tidepool.Actor.Source"
      "progressSourceSited" "Tidepool.Actor.Source" False [0]
      DeliverHostAnswer ProgressStateEvidence
  , verb "receive" "Tidepool.Actor"
      "receiveSited" "Tidepool.Actor" False [] DeliverLiveReentry SelectedAnswer
  , verb "serve" "Tidepool.Actor"
      "serveSited" "Tidepool.Actor" False [] DeliverLiveReentry SelectedAnswer
  , (verb "runScope" "Tidepool.Scope.Internal"
      "runScopeSited" "Tidepool.Scope.Internal" False [] DeliverHostAnswer SelectedAnswer)
      { vsAnswerSource = EffectResult }
  ]
  where
    verb name source sibling siblingSource listAnswer inputs delivery wireSource =
      VerbSpec name source sibling siblingSource listAnswer inputs Nothing FirstTypeArgument
        delivery wireSource
