{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeOperators #-}

-- Remix these around your project's admitted evidence and messaging effects.
-- Callbacks are supplied by the caller: there is no implicit shell, checkout,
-- or recipient authority. Retain the returned receipt alongside your decision.
module Coordination where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import GHC.Generics (Generic)
import Tidepool.Aeson (FromJSON, ToJSON (..))
import Tidepool.Aeson.Value (encodeValue)
import Tidepool.Agent.Contract
import Tidepool.Model

-- Actual bounded diff evidence belongs in the input, not merely a commit ID.
data ChangePacket = ChangePacket
  { candidate :: Text
  , boundedDiff :: Text
  , consumerContracts :: Text
  , evidenceComplete :: Bool
  } deriving (Generic, FromJSON, ToJSON, JsonSchema)

data Route = Unaffected | NeedsReview | NeedMoreEvidence
  deriving (Generic, FromJSON, ToJSON, JsonSchema)
data RoutingDecision = RoutingDecision
  { route :: Route
  , reason :: Text
  , evidenceReferences :: [Text]
  } deriving (Generic, FromJSON, ToJSON, JsonSchema)

data EvidenceRef = EvidenceRef { reference :: Text }
  deriving (Generic, FromJSON, ToJSON, JsonSchema)

data EvidenceTools mode = EvidenceTools
  { readEvidence :: mode :- Call EvidenceRef Text
  } deriving (Generic)

-- For a small decision from a complete packet, prefer Jev. Use this turn when
-- deciding requires fetching particular evidence and explaining the boundary.
routeChange :: Member ModelCall effects
            => (Text -> Eff effects Text)
            -> ChangePacket -> Eff effects (ModelResult RoutingDecision)
routeChange readRef packet = invokeModel
  (typedTurn @RoutingDecision
    (defaultSpec { specTools = EvidenceTools (tool "Read a retained evidence reference; do not rerun work" (readRef . reference)) })
    "Route the supplied change using the actual diff and declared consumer contracts. Fetch referenced evidence if needed. Missing or truncated evidence means NeedMoreEvidence, not Unaffected.")
  (encodeValue (toJSON packet))

data Clarification = Clarification
  { recipient :: Text
  , question :: Text
  } deriving (Generic, FromJSON, ToJSON, JsonSchema)
data Delivery = Delivered | Refused
  deriving (Generic, FromJSON, ToJSON, JsonSchema)
data CoordinationTools mode = CoordinationTools
  { inspectDependency :: mode :- Call EvidenceRef Text
  , clarifyWithOwner :: mode :- Call Clarification Delivery
  } deriving (Generic)

-- A supplied sender enforces the admitted recipients. This turn can inspect,
-- ask the owner, and report the observed delivery; it cannot widen authority.
clarifyDependency :: Member ModelCall effects
                  => (Text -> Eff effects Text)
                  -> (Clarification -> Eff effects Delivery)
                  -> Text -> Eff effects (ModelResult Text)
clarifyDependency inspect sendQuestion packet = invokeModel
  (withLimits (defaultLimits { requestLimit = Just 4, toolLimit = Just 3 })
    (textTurn (defaultSpec { specTools = CoordinationTools
      (tool "Inspect an admitted dependency" (inspect . reference))
      (tool "Ask the named owner a concrete clarification; report delivery outcome" sendQuestion) })
      "Resolve one dependency ambiguity from this packet. Inspect only what is needed, ask at most one concrete question if unresolved, and return the observed state and next owner. Do not invent an acknowledgment."))
  packet

prepareHandoff :: Member ModelCall effects
               => (Text -> Eff effects Text)
               -> Text -> Eff effects (ModelResult Text)
prepareHandoff readRef packet = invokeModel
  (textTurn (defaultSpec { specTools = EvidenceTools (tool "Read retained evidence" (readRef . reference)) })
    "Prepare a concise handoff: exact candidate, changed contract, completed checks, unverified behavior, next owner. Preserve evidence references. Inspect missing referenced facts; never turn compiled-only into passed.")
  packet

investigateFailure :: Member ModelCall effects
                   => (Text -> Eff effects Text)
                   -> Text -> Eff effects (ModelResult Text)
investigateFailure readRef packet = invokeModel
  (withLimits (defaultLimits { requestLimit = Just 4, toolLimit = Just 3 })
    (textTurn (defaultSpec { specTools = EvidenceTools (tool "Read retained failure evidence; no execution" (readRef . reference)) })
      "Inspect this one failure. Separate observation from hypothesis, cite the relevant retained output, and return the next discriminating check. Do not rerun commands to retrieve existing output."))
  packet
