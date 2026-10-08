{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}
-- | Observe once, then reuse the typed response for presentation and policy.
module Examples.JevPreparedWorkflow
  ( CourierPacket, CourierResponse, courierQuestion, observeCourier, courierNotebook ) where

import Prelude
import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import Tidepool.Aeson.Value (Value)
import Tidepool.Effects.Core (Jev, Console)
import Tidepool.Inspection (display)
import qualified Jev.Operators as J

type CourierChoices = ("fast" J.::> (Int -> (Text, Int)))
  J.:|: (("cheap" J.::> (Int -> (Text, Int)))
    J.:|: ("missing" J.::> (Int -> (Text, Int))))
type CourierPacket = J.Packet ("route" J.::= J.Choice CourierChoices)
type CourierResponse = J.Response (CourierPacket J.Answers)

courierQuestion :: Int -> J.Q Value (J.Choice CourierChoices)
courierQuestion captured = J.choice "Which decision is justified by the courier criteria?"
  (J.alt #fast "Quicker delivery is required by an explicit deadline and meets the spending limit" (\n -> ("fast-original", captured + n))
    J..| J.alt #cheap "Cheaper delivery meets the explicit deadline" (\n -> ("cheap-original", captured - n))
    J..| J.alt #missing "No explicit deadline or spending criteria justify choosing" (\n -> ("missing-original", captured * n)))

observeCourier
  :: Member Jev effects
  => Text
  -> Eff effects (Either (J.JevError J.JevCallError) CourierResponse)
observeCourier criteria = do
  let question = courierQuestion 61
      world = J.state (#criteria J.:= criteria
        J.:& #options J.:= (["quick: tomorrow, price 8", "economy: two days, price 5"] :: [Text]))
  case J.prepare J.jevLatest world (#route J.:= question) of
    Left failure -> pure (Left (J.Prepare failure))
    Right prepared -> J.executePrepared prepared

-- | Caller composition: one explicit Jev request, then presentation and two
-- pure policy decisions over that same observation. Return the original result
-- so a later notebook cell can inspect or interpret it again without requesting.
courierNotebook
  :: (Member Jev effects, Member Console effects)
  => Text
  -> Eff effects (Either (J.JevError J.JevCallError) CourierResponse)
courierNotebook criteria = do
  result <- observeCourier criteria
  case result of
    Left failure -> void (display failure)
    Right response -> do
      void (display response)
      let answer = (J.answers response).route
          under :: J.Policy policy -> Either J.Doubt (Text, Int)
          under policy = fmap (\settled -> J.settledValue settled 4)
            (J.takenUnder policy answer)
      -- Only this explicit projection applies the selected original function.
      -- Mapping a Response or displaying Settled never demands that payload.
      void (display (under J.careful, under J.strict))
  pure result
