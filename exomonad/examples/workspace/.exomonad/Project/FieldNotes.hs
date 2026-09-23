{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DuplicateRecordFields #-}
{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}

-- | Record root-side observations about completed tool calls. The hook is
-- stateless: cadence comes from the runtime-issued result ordinal, and only a
-- theory that crosses its policy creates a journal entry.
module Project.FieldNotes
  ( Rubric (..)
  , Policy (..)
  , NoulTheory (..)
  , ScoreTheory (..)
  , ChoiceTheory (..)
  , Theories (..)
  , coreTheories
  , fieldNotes
  , shouldObserveOrdinal
  , noulTrips
  , scorePolicyTrips
  , choicePolicyTrips
  , scoreLevelNames
  ) where

import Control.Monad (forM_)
import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as Text
import qualified Jev.Operators as J
import Jev.Operators (Packet ((:=), (:&)))
import Tidepool.Agent.Contract (Annotation (..), ToolCall (..), ToolResult (..))
import Tidepool.Actors.Exomonad
  ( ActorContextInfo (..)
  , ActorContextRole (..)
  , ConversationRole (..)
  , ConversationTurn (..)
  , TurnItem (..)
  )
import Tidepool.Aeson.Value (Value (..), object, (.=), toJSON)
import Tidepool.Effects.Core (ActorContext, Jev, Journal, Reflect, actorContext, reflect)
import Tidepool.Journal (record)
import qualified Data.Map.Strict as Map

data Rubric = Rubric
  { noneDescription :: Text
  , slightDescription :: Text
  , clearDescription :: Text
  , strongDescription :: Text
  }

data Policy
  = AtLeast Double
  | AtLevel Text
  | WhenIn [Text]

data NoulTheory = NoulTheory
  { noulName :: Text
  , noulQuestion :: Text
  , noulPolicy :: Policy
  }

data ScoreTheory = ScoreTheory
  { scoreName :: Text
  , scoreQuestion :: Text
  , scoreRubric :: Rubric
  , scorePolicy :: Policy
  }

-- Choice questions use four static alternatives so Jev's answer remains a
-- closed typed choice. The tuple makes the limit impossible to violate.
data ChoiceTheory = ChoiceTheory
  { choiceName :: Text
  , choiceQuestion :: Text
  , choiceOptions :: (Text, Text, Text, Text)
  , choicePolicy :: Policy
  }

data Theories = Theories
  { noulTheories :: [NoulTheory]
  , scoreTheories :: [ScoreTheory]
  , choiceTheories :: [ChoiceTheory]
  }

type ScoreLevels = "none" J.:|: ("slight" J.:|: ("clear" J.:|: "strong"))

type ChoiceOptions =
  ("first" J.::> Int)
    J.:|: (("second" J.::> Int)
      J.:|: (("third" J.::> Int)
        J.:|: ("fourth" J.::> Int)))

fieldNotes
  :: (Member Jev effects, Member ActorContext effects, Member Reflect effects, Member Journal effects)
  => Int
  -> Theories
  -> ToolCall
  -> ToolResult
  -> Eff effects Annotation
fieldNotes cadence theories call result
  | cadence <= 0 = pure (Abstained "field-note cadence must be positive")
  | otherwise = do
      context <- actorContext
      if contextRole context /= ContextRoot
        then pure (Abstained "field notes are installed on the root only")
        else if not (shouldObserveOrdinal cadence (toolResultOrdinal result))
          then pure NoAnnotation
          else judgeTheories
  where
    judgeTheories = do
      reflected <- reflect 4
      let turns = either (const []) id reflected
          recent = takeLast 12 (recentCallFacts turns)
          promptState = object
            [ "current_call" .= object
                [ "tool" .= Text.take 120 (toolCallName call)
                , "arguments" .= boundedValue 4 1600 (toolCallArguments call)
                , "result_handle" .= Text.take 200 (toolResultHandle result)
                , "result_ordinal" .= toolResultOrdinal result
                ]
            , "current_result" .= Text.take 3000 (toolResultOutput result)
            , "recent_calls" .= map snd recent
            , "recent_call_identities" .= map (Text.take 160 . fst) recent
            , "recent_instructions" .= map (Text.take 1800) (takeLast 3 (recentInstructions turns))
            ]
          packet =
            #nouls := J.each noulName (J.noul . noulQuestion) (noulTheories theories)
              :& #scores := J.each scoreName scoreQuestionValue (scoreTheories theories)
              :& #choices := J.each choiceName choiceQuestionValue (choiceTheories theories)
      answer <- J.ask (J.rawState promptState) packet
      case answer of
        Left _ -> pure (Abstained "jev unavailable for field notes")
        Right judgments -> do
          forM_ judgments.nouls $ \(theory, judged) ->
            if noulTrips (noulPolicy theory) judged.yes
              then writeNote (noulName theory) (toJSON judged) recent
              else pure ()
          forM_ judgments.scores $ \(theory, judged) ->
            if scoreTrips (scorePolicy theory) judged
              then writeNote (scoreName theory) (toJSON judged) recent
              else pure ()
          forM_ judgments.choices $ \(theory, judged) ->
            if choiceTrips (choicePolicy theory) (choiceOptions theory) judged
              then writeNote (choiceName theory) (toJSON judged) recent
              else pure ()
          pure NoAnnotation

    writeNote name typedAnswer recent =
      record name (toolResultHandle result)
        (object
          [ "answer" .= typedAnswer
          , "tool" .= Text.take 120 (toolCallName call)
          , "tool_arguments" .= boundedValue 4 1600 (toolCallArguments call)
          , "tool_result_ordinal" .= toolResultOrdinal result
          , "observed_call_identities" .= map (Text.take 160 . fst) recent
          ])

-- | The Noul threshold is pure policy so its boundary can be checked without
-- asking Jev or exercising the host transport.
noulTrips :: Policy -> Double -> Bool
noulTrips (AtLeast floorValue) likelihood = likelihood >= floorValue
noulTrips _ _ = False

shouldObserveOrdinal :: Int -> Int -> Bool
shouldObserveOrdinal cadence ordinal = cadence > 0 && ordinal > 0 && ordinal `mod` cadence == 0

scoreTrips :: Policy -> (J.Answers J.:- J.Score Text ScoreLevels) -> Bool
scoreTrips policy judged = scorePolicyTrips policy judged.expectation judged.masses

choiceTrips
  :: Policy
  -> (Text, Text, Text, Text)
  -> (J.Answers J.:- J.Choice ChoiceOptions)
  -> Bool
choiceTrips policy options judged = choicePolicyTrips policy selected judged.mass
  where
    selected = optionAt (J.handle judged
      ( #first id
          J..| #second id
          J..| #third id
          J..| #fourth id
      )) (tupleOptions options)

scorePolicyTrips :: Policy -> Double -> [(Text, Double)] -> Bool
scorePolicyTrips policy expectation masses = case policy of
  AtLeast floorValue -> expectation >= floorValue
  AtLevel levelName -> massFor levelName masses >= 0.5
  WhenIn levelNames -> any (\levelName -> massFor levelName masses >= 0.5) levelNames

choicePolicyTrips :: Policy -> Maybe Text -> Double -> Bool
choicePolicyTrips policy selected mass = case policy of
  AtLeast floorValue -> mass >= floorValue
  AtLevel levelName -> selected == Just levelName
  WhenIn levelNames -> maybe False (`elem` levelNames) selected

scoreLevelNames :: [Text]
scoreLevelNames = ["none", "slight", "clear", "strong"]

takeLast :: Int -> [a] -> [a]
takeLast count values = drop (max 0 (length values - count)) values

-- Keep the Jev input finite while retaining structured argument shapes.
boundedValue :: Int -> Int -> Value -> Value
boundedValue depth budget value = case value of
  Object fields
    | depth > 0 -> object
        [ (Text.take 80 key, boundedValue (depth - 1) (budget `div` 8) nested)
        | (key, nested) <- take 8 (Map.toList fields)
        ]
    | otherwise -> String "<object truncated>"
  Array values
    | depth > 0 -> Array (map (boundedValue (depth - 1) (budget `div` 8)) (take 8 values))
    | otherwise -> String "<array truncated>"
  String text -> String (Text.take budget text)
  other -> other

scoreQuestionValue :: ScoreTheory -> J.Q Value (J.Score Text ScoreLevels)
scoreQuestionValue theory =
  J.score (scoreQuestion theory)
    ( J.level #none (noneDescription (scoreRubric theory)) "none"
        J..| J.level #slight (slightDescription (scoreRubric theory)) "slight"
        J..| J.level #clear (clearDescription (scoreRubric theory)) "clear"
        J..| J.level #strong (strongDescription (scoreRubric theory)) "strong"
    )

choiceQuestionValue :: ChoiceTheory -> J.Q Value (J.Choice ChoiceOptions)
choiceQuestionValue theory =
  J.choice (choiceQuestion theory)
    ( J.alt #first (firstOption options) 0
        J..| J.alt #second (secondOption options) 1
        J..| J.alt #third (thirdOption options) 2
        J..| J.alt #fourth (fourthOption options) 3
    )
  where
    options = choiceOptions theory

tupleOptions :: (a, a, a, a) -> [a]
tupleOptions (first, second, third, fourth) = [first, second, third, fourth]

firstOption, secondOption, thirdOption, fourthOption :: (Text, Text, Text, Text) -> Text
firstOption (value, _, _, _) = value
secondOption (_, value, _, _) = value
thirdOption (_, _, value, _) = value
fourthOption (_, _, _, value) = value

optionAt :: Int -> [a] -> Maybe a
optionAt index values = case drop index values of
  value : _ -> Just value
  [] -> Nothing

massFor :: Text -> [(Text, Double)] -> Double
massFor wanted masses = case [mass | (name, mass) <- masses, name == wanted] of
  mass : _ -> mass
  [] -> 0

recentCallFacts :: [ConversationTurn] -> [(Text, Value)]
recentCallFacts turns =
  [ (identity, object
      [ "call_id" .= Text.take 160 identity
      , "tool" .= Text.take 120 name
      , "arguments" .= String (Text.take 1200 arguments)
      , "result" .= fmap (Text.take 1600) (firstResult identity turns)
      ])
  | turn <- turns
  , TurnToolCall identity name arguments <- turnItems turn
  ]

firstResult :: Text -> [ConversationTurn] -> Maybe Text
firstResult identity turns = case
  [output | turn <- turns, TurnToolResult resultIdentity output <- turnItems turn, resultIdentity == identity] of
    output : _ -> Just output
    [] -> Nothing

recentInstructions :: [ConversationTurn] -> [Text]
recentInstructions turns =
  [ Text.take 4000 body
  | turn <- turns
  , TurnMessage role body <- turnItems turn
  , role == RoleUser || role == RoleDeveloper
  ]

coreTheories :: Theories
coreTheories = Theories
  { noulTheories =
      [ NoulTheory "near-identical-call" "Did this call repeat a near-identical call from the observed recent tool history?" (AtLeast 0.65)
      , NoulTheory "unchanged-retry" "Did this call retry a rejected or failed tool call without changing its relevant arguments?" (AtLeast 0.65)
      , NoulTheory "fetch-named-result" "Did this call immediately fetch or inspect the resource named by the preceding tool result?" (AtLeast 0.65)
      , NoulTheory "unread-child-reply" "Does the recent tool history show a child reply arriving without a later call reading or using that reply?" (AtLeast 0.65)
      , NoulTheory "composed-round" "Did one tool call compose work that would otherwise have required an extra round without losing needed evidence?" (AtLeast 0.65)
      , NoulTheory "used-child-evidence" "Does recent tool activity use a child's reply without re-reading the evidence it supplied?" (AtLeast 0.65)
      ]
  , scoreTheories =
      [ ScoreTheory "assignment-drift"
          "How much does the recent tool activity drift from the assignment in the recent user and developer messages?"
          (Rubric
            "The calls clearly advance the stated assignment."
            "The calls are mostly relevant, with a small detour."
            "The calls substantially delay or distract from the assignment."
            "The calls do not appear to serve the assignment at all.")
          (AtLevel "strong")
      ]
  , choiceTheories =
      [ ChoiceTheory "current-mode"
          "Which mode best describes the root's current tool activity?"
          ("stuck", "exploring", "finishing", "waiting on a child")
          (WhenIn ["stuck", "waiting on a child"])
      ]
  }
