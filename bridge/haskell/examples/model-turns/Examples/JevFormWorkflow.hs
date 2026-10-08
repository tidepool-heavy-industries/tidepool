{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}
-- | An authored courier dialogue. Observation, policy, human choice and
-- continuation execution are separate ordinary Haskell operations.
module Examples.JevFormWorkflow
  ( Route(..), RouteMeaning(..), RouteResult(..), RouteChoice(..)
  , RoutePacket, RouteResponse, routeChoices, routeQuestion, humanRouteForm
  , observeRoutes, chooseRouteUnder, routeResponseView
  , SelectionOrigin(..), QuestionStage(..), DialogueOutcome(..), DialogueResult(..)
  , dialogue, dialogueUnder
  ) where

import Prelude
import Control.Monad.Freer (Eff, Member)
import Data.List.NonEmpty (NonEmpty(..))
import qualified Data.List.NonEmpty as NE
import Data.Text (Text)
import Tidepool.Aeson.Value (Value, object, (.=))
import Tidepool.Effects.Core (AskUser, Console, Jev, JevCallError)
import qualified Tidepool.Form as F
import Tidepool.Inspection (display)
import qualified Tidepool.View as V
import qualified Jev.Operators as J

data Route = Express | Economy deriving (Eq, Show)
data RouteMeaning = Delivery Route | MissingDeliveryCriteria deriving (Eq, Show)
data RouteResult = DeliveryPrepared Route Text Text | CriteriaSupplied Text Text
  deriving (Eq, Show)

-- | Semantic conditions and human presentation describe the same original
-- payload. Form assigns occurrence identities; semantic keys never dispatch
-- actions in this program.
data RouteChoice effects = RouteChoice
  { semanticKey :: Text
  , semanticCondition :: Text
  , humanView :: V.View
  , meaning :: RouteMeaning
  , originalContinuation :: Eff effects (F.FormResult RouteResult)
  }

type RoutePacket effects = J.Packet
  ("route" J.::= J.Choice ("routes" J.::* RouteChoice effects))
type RouteResponse effects = J.Response (RoutePacket effects J.Answers)

-- | These closures capture the original subject. Their forms are built and
-- their effects run only when a caller sequences the selected continuation.
routeChoices :: Member AskUser effects => Text -> NonEmpty (RouteChoice effects)
routeChoices subject =
  delivery Express "express" "An explicit deadline requires next-day delivery and permits a price of 8" "Tomorrow · 8"
  :| [ delivery Economy "economy" "Two-day delivery meets the explicit deadline and costs 5" "Two days · 5"
     , RouteChoice
         { semanticKey = "missing"
         , semanticCondition = "The supplied criteria do not establish a deadline or spending limit sufficient to select a delivery route"
         , humanView = V.column
             [V.text "Clarify delivery criteria", V.markdown "Supply the deadline and spending limit before choosing a route."]
         , meaning = MissingDeliveryCriteria
         , originalContinuation = F.askUser
             (F.present (V.column [V.text subject, V.text "Which delivery constraints are missing?"])
               *> (CriteriaSupplied subject <$> F.textInput "Delivery criteria" Nothing))
         }
     ]
  where
    delivery route key condition estimate = RouteChoice
      { semanticKey = key
      , semanticCondition = condition
      , humanView = V.column [V.text (routeName route), V.text estimate]
      , meaning = Delivery route
      , originalContinuation = F.askUser
          (F.present (deliveryView route subject)
            *> (DeliveryPrepared route subject <$> F.textInput "Delivery note" Nothing))
      }

routeQuestion :: NonEmpty (RouteChoice effects)
  -> J.Q Value (J.Choice ("routes" J.::* RouteChoice effects))
routeQuestion rows = J.choice "Which next step is justified by the delivery criteria?"
  (J.many #routes semanticKey semanticCondition (NE.toList rows))

humanRouteForm :: Text -> NonEmpty (RouteChoice effects) -> F.Form (RouteChoice effects)
humanRouteForm subject rows =
  F.present (V.column [V.text subject, V.text "Choose the next step for this delivery."])
    *> F.choice "Next step" (fmap (\row -> F.option (humanView row) row) rows)

-- | One explicit request. The original typed response remains available for
-- inspection and any number of pure policy judgments by later notebook cells.
observeRoutes :: Member Jev effects => Text -> NonEmpty (RouteChoice effects)
  -> Eff effects (Either (J.JevError JevCallError) (RouteResponse effects))
observeRoutes criteria rows =
  case J.prepare J.jevLatest (J.rawState (object ["criteria" .= criteria]))
      (#route J.:= routeQuestion rows) of
    Left failure -> pure (Left (J.Prepare failure))
    Right prepared -> J.executePrepared prepared

-- | Projects the selected original row without executing its continuation.
chooseRouteUnder :: J.Policy policy -> RouteResponse effects
  -> Either J.Doubt (RouteChoice effects)
chooseRouteUnder policy response =
  fmap J.settledValue (J.takenUnder policy (J.answers response).route)

-- | Both judgments and the response display are pure reads of one response.
-- Projecting keys deliberately leaves captured actions opaque and unexecuted.
routeResponseView :: RouteResponse effects -> V.View
routeResponseView response = V.column
  [ V.text "Retained delivery judgment"
  , V.inspect response
  , V.text "Careful policy"
  , V.inspect (fmap semanticKey (chooseRouteUnder J.careful response))
  , V.text "Strict policy"
  , V.inspect (fmap semanticKey (chooseRouteUnder J.strict response))
  ]

data SelectionOrigin
  = JevSelected
  | HumanAfterInferenceFailure (J.JevError JevCallError)
  | HumanAfterPolicyDoubt J.Doubt
  deriving (Show)
data QuestionStage = SubjectQuestion | RouteQuestion | FollowupQuestion
  deriving (Eq, Show)
data DialogueOutcome
  = Finished SelectionOrigin RouteMeaning RouteResult
  | FirstDismissed
  | SelectionDismissed SelectionOrigin
  | ContinuationDismissed SelectionOrigin RouteMeaning
  | FormFailed QuestionStage (Maybe SelectionOrigin) F.FormCause
  deriving (Show)

-- | The response is the Jev-owned value, including original evidence and
-- payloads, rather than a reconstructed summary of the selected route.
data DialogueResult effects = DialogueResult
  { outcome :: DialogueOutcome
  , retainedResponse :: Maybe (RouteResponse effects)
  }

dialogue :: (Member AskUser effects, Member Jev effects, Member Console effects)
  => Eff effects (DialogueResult effects)
dialogue = dialogueUnder J.careful

-- | This author's policy explicitly asks a human after inference failure or
-- doubt. A confident missing-information answer instead runs the original
-- clarification continuation. Neither case is treated as a delivery choice.
dialogueUnder :: (Member AskUser effects, Member Jev effects, Member Console effects)
  => J.Policy policy -> Eff effects (DialogueResult effects)
dialogueUnder policy = do
  first <- F.askUser (F.textInput "Subject and delivery criteria" Nothing)
  case first of
    F.Dismissed -> pure (DialogueResult FirstDismissed Nothing)
    F.FormUnavailable cause -> pure (DialogueResult (FormFailed SubjectQuestion Nothing cause) Nothing)
    F.Submitted subject -> do
      let rows = routeChoices subject
          finish retained origin selected = do
            -- This is the only sequencing of an original selected action.
            followup <- originalContinuation selected
            case followup of
              F.Submitted result -> do
                _ <- display (routeResultView result)
                pure (DialogueResult (Finished origin (meaning selected) result) retained)
              F.Dismissed -> pure (DialogueResult (ContinuationDismissed origin (meaning selected)) retained)
              F.FormUnavailable cause -> pure (DialogueResult (FormFailed FollowupQuestion (Just origin) cause) retained)
          askHuman retained origin = do
            selected <- F.askUser (humanRouteForm subject rows)
            case selected of
              F.Submitted row -> finish retained origin row
              F.Dismissed -> pure (DialogueResult (SelectionDismissed origin) retained)
              F.FormUnavailable cause -> pure (DialogueResult (FormFailed RouteQuestion (Just origin) cause) retained)
      inferred <- observeRoutes subject rows
      case inferred of
        Left failure -> askHuman Nothing (HumanAfterInferenceFailure failure)
        Right response -> do
          _ <- display (routeResponseView response)
          case chooseRouteUnder policy response of
            Left doubt -> askHuman (Just response) (HumanAfterPolicyDoubt doubt)
            Right selected -> finish (Just response) JevSelected selected

routeName :: Route -> Text
routeName Express = "Express delivery"
routeName Economy = "Economy delivery"

deliveryView :: Route -> Text -> V.View
deliveryView route subject = V.column
  [ V.text subject
  , V.caption (V.svg (V.SvgDocument
      "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 320 64\" role=\"img\" aria-label=\"Delivery from sender to recipient\"><path d=\"M40 32 H280\" stroke=\"#4779b8\" stroke-width=\"4\"/><circle cx=\"40\" cy=\"32\" r=\"12\" fill=\"#4779b8\"/><path d=\"M264 20 L280 32 L264 44\" fill=\"none\" stroke=\"#4779b8\" stroke-width=\"4\"/></svg>"))
      (routeName route)
  , V.text "Add the note to accompany this delivery."
  ]

routeResultView :: RouteResult -> V.View
routeResultView (DeliveryPrepared route subject note) = V.column
  [deliveryView route subject, V.text "Submitted delivery note", V.inspect note]
routeResultView (CriteriaSupplied subject criteria) = V.column
  [V.text subject, V.text "Submitted delivery criteria", V.inspect criteria]
