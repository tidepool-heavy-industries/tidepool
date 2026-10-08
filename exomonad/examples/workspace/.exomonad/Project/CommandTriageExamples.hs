{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}

module Project.CommandTriageExamples
  ( TriagePacket, triagePacket, triageState, diagnoseCheck, packageFetchCriteria, diagnoseFetch ) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Jev.Operators as J
import Jev.Operators (Packet ((:=), (:&)))
import Tidepool.Effects.Core (Jev)
import Project.CommandTriagePattern

type TriagePacket = ("fault" J.::= J.Choice FaultOptions)
  J.:& ("transient" J.::= J.Optional J.Noul)

-- The original result is retained by the caller. The reference here locates
-- that result; the excerpt is deliberately supplied, not recovered or rerun.
-- This example returns the full response alongside its interpretation.
diagnoseCheck
  :: Member Jev effects
  => RepeatAllowance -> Text -> Text
  -> Eff effects (Either (J.JevError J.JevCallError)
       (J.Response (J.Packet TriagePacket J.Answers), Either J.Doubt (J.Settled J.Careful Followup)))
diagnoseCheck = diagnose testFailureCriteria

-- A second authored client changes the policy, not the request/decoder code.
packageFetchCriteria :: TriageCriteria
packageFetchCriteria = testFailureCriteria
  { failureContext = "A package artifact fetch failed. Classify the supplied fetch diagnostic; classify only failures established by the excerpt."
  , describeFailure = \kind -> case kind of
      AssertionFailure -> "A downloaded artifact failed an explicit checksum or integrity assertion"
      InputFailure -> "The requested package/version, arguments, credentials or local configuration must be corrected"
      ToolFailure -> "The remote service or transport refused or failed the fetch, including an exhausted account credit balance"
      UnknownFailure -> "The supplied excerpt does not establish a fetch failure cause"
  , transientEvidence = "The service explicitly reports temporary overload or a transient transport interruption."
  , repeatExclusions = "Authentication, payment/quota exhaustion, an unknown package/version, and persistent TLS or proxy configuration are not transient. A retry-after delay must be respected by the caller before acting."
  }

diagnoseFetch
  :: Member Jev effects
  => Text -> Text
  -> Eff effects (Either (J.JevError J.JevCallError)
       (J.Response (J.Packet TriagePacket J.Answers), Either J.Doubt (J.Settled J.Careful Followup)))
diagnoseFetch = diagnose packageFetchCriteria NoRepeat

triageState reference excerpt = J.state (#retainedResult := (reference :: Text) :& #diagnostic := (excerpt :: Text))

triagePacket :: TriageCriteria -> RepeatAllowance -> J.Packet TriagePacket J.Questions
triagePacket criteria allowance = #fault := faultQuestion criteria
  :& #transient := J.optional (case allowance of
    NoRepeat -> Nothing
    OneRepeatAllowed -> Just (transientQuestion criteria))

diagnose
  :: Member Jev effects
  => TriageCriteria -> RepeatAllowance -> Text -> Text
  -> Eff effects (Either (J.JevError J.JevCallError)
       (J.Response (J.Packet TriagePacket J.Answers), Either J.Doubt (J.Settled J.Careful Followup)))
diagnose criteria allowance reference excerpt = do
  result <- J.ask (triageState reference excerpt) (triagePacket criteria allowance)
  pure $ fmap (\response ->
    let a = J.answers response
    in (response, chooseFollowup J.careful allowance a.fault a.transient)) result
