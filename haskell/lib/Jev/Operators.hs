{-# LANGUAGE ConstraintKinds #-}
{-# LANGUAGE DataKinds #-}
{-# LANGUAGE ExplicitNamespaces #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE FlexibleInstances #-}
{-# LANGUAGE KindSignatures #-}
{-# LANGUAGE PolyKinds #-}
{-# LANGUAGE RankNTypes #-}
{-# LANGUAGE TypeFamilies #-}
{-# LANGUAGE TypeOperators #-}
{-# LANGUAGE DuplicateRecordFields #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE UndecidableInstances #-}
-- The ToJSON instances for the answer types belong beside the monomorphic
-- front, not beside the polymorphic core that must not depend on a JSON type.
{-# OPTIONS_GHC -Wno-orphans #-}

-- | FOR AGENT USE AND REVIEW. The type-operator front over Tidepool's own
-- 'Tidepool.Aeson.Value.Value' (there is no aeson package here). Ported
-- verbatim from jev-dsl's @Jev.Operators@, monomorphized to Tidepool's
-- 'Value' instead of aeson's, via the 'Jev.Tidepool' instance.
--
-- A packet is written once from its questions and its type is inferred:
--
-- > Right resp <- ask world
-- >    ( #next    := choice "Most useful next step?"
-- >                    (alt #rerun "Rerun the focused check" c .| alt #ask_model "Needs judgment" h .| many edges)
-- >   :& #enough  := noul "Do the diagnostics establish the mechanism?"
-- >   :& #breadth := score "How broadly would the fix alter behavior?"
-- >                    (level #localized "…" .| level #adjacent "…" .| level #contract "…")
-- >   :& Nil )
--
-- Answers come back under the same labels and are plain records:
--
-- > a = answers resp
-- > a.next.key          -- the chosen alternative's wire key
-- > a.next.margin       -- how far ahead of the runner-up it is
-- > a.enough.yes        -- the provider's probability
-- > a.breadth.nearest   -- the level nearest the expectation
-- > handle (chosen a.next) (#rerun (\c -> …) .| #ask_model (\h -> …) .| onMany (\k e -> …))
-- > massAtOrAbove #adjacent a.breadth
--
-- Labels are wire ids verbatim. Duplicate labels, a missing label on
-- access, a pool cell whose label is not its name, and a handler list that
-- does not match its alternatives are compile errors in these words.
-- Wording, runtime candidates, level counts, and pool correspondence are
-- checked when the request is built.
--
-- No network: hand 'request' to any transport and give the body back to
-- 'decode', or use 'roundTrip'\/'jev1' with your own transport. Actor code
-- should not build a transport at all — 'ask'\/'ask1'\/'askWith' are the
-- documented path: they speak to the Shoal host's own @Jev@ effect
-- directly, so a packet or question is the only thing the LLM-visible
-- surface ever passes.
module Jev.Operators
  ( -- * Packets
    Cell ((:=)), Packet ((:&), Nil), (++.), jev1
    -- * Alternatives
  , alt, many, manyFrom, (.|), onMany
    -- * Rubrics
  , level, massAtOrAbove
    -- * Questions
  , noul, choice, score, each, pool, eachIn, askAbout, given, about, refKey, refPayload
    -- * Answers, as fields: @a.next.key@, @a.enough.yes@
    -- ('A' carries them: @yes@, @chosen@, @key@, @mass@, @margin@,
    -- @confidence@, @masses@, @expectation@, @nearest@.)
  , A (..)
  , contenders, selectedKey, handle, accept, explain, Doubt (..), Policy (..)
  , routing, spawning, merging
    -- * The operation
  , jevLatest, request, decode, roundTrip, answers, usage, Usage (..), resolvedModel, JevError (..)
    -- * The Shoal host operation — the documented path for actor code
  , ask, askWith, ask1
    -- * Types, for signatures only
  , type (::=), type (::>), type (:|:), Many, Offers, Handlers, Rubric
  , Noul, Choice, Score, Each, Group, PoolDecl, Ref, Selected
  , Q, Questions, Answers, type (:-), State, state, Model, Response, PrepError, DecodeError
  , Schema, Alternatives
  ) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import GHC.TypeLits (KnownNat, KnownSymbol)
import Tidepool.Aeson.Value (ToJSON (..), Value)
import Jev.Tidepool ()
import Jev.Host (jevTransport)
import qualified Jev.Core as Core
import Jev.Core
  ( A (..), Alternatives, Choice, DecodeError, Doubt (..), Each, Group, JevError (..), Label, Many, Model, Noul
  , Packet (..), Cell (..), PoolDecl, PrepError, Q, Score, Selected, type (:-), type (::=), type (::>), type (:|:), (++.), Policy (..)
  )
import Tidepool.Effects.Core (Jev)

type Questions = Core.Questions Value
type Answers = Core.Answers Value
type State = Core.State Value
type Ref n = Core.Ref Value n
type Response = Core.Response Value

-- | Offers for a disjunction: @alt #k wording payload .| many […]@.
type Offers alts = Core.Alts (Core.Offer Value) alts
-- | Handlers for a disjunction, in declaration order, each taking its
-- alternative's payload: @#k (\p -> …) .| onMany (\key p -> …)@.
type Handlers r alts = Core.Alts (Core.Handler Value r) alts
-- | Levels of a rubric, in order: @level #low "…" .| level #high "…"@.
type Rubric levels = Core.Alts (Core.Level Value) levels
type Schema s = Core.Schema Value s

(.|) :: Core.Single x => Core.Alts f x -> Core.Alts f rest -> Core.Alts f (x :|: rest)
(.|) = (Core..|)
infixr 4 .|

alt :: KnownSymbol k => Label k -> Value -> p -> Offers (k ::> p)
alt = Core.alt

many :: [(Text, Value, p)] -> Offers (Many p)
many = Core.many

manyFrom :: KnownSymbol n => Q Value (PoolDecl n a) -> Offers (Many a)
manyFrom = Core.manyFrom

onMany :: (Text -> p -> r) -> Handlers r (Many p)
onMany = Core.onMany

level :: KnownSymbol l => Label l -> Value -> Rubric l
level = Core.level

-- Questions
noul :: Text -> Q Value Noul
noul = Core.noul

choice :: Core.AltsOk alts => Text -> Offers alts -> Q Value (Choice alts)
choice = Core.choice

score :: Core.RubricOk levels => Text -> Rubric levels -> Q Value (Score levels)
score = Core.score

each :: [(Text, s Questions)] -> Q Value (Each s)
each = Core.each

pool :: Label n -> [(Text, Value, a)] -> Q Value (PoolDecl n a)
pool = Core.pool

eachIn :: KnownSymbol n => Q Value (PoolDecl n a) -> (Ref n a -> s Questions) -> Q Value (Each s)
eachIn = Core.eachIn

askAbout :: KnownSymbol n => Ref n a -> Text -> Q Value Noul
askAbout = Core.askAbout

refKey :: Ref n a -> Text
refKey = Core.refKey

refPayload :: Ref n a -> a
refPayload = Core.refPayload

given :: Core.Worded e => Text -> Q Value e -> Q Value e
given = Core.given

about :: Core.Worded e => [(Text, Value)] -> Q Value e -> Q Value e
about = Core.about

state :: Value -> State
state = Core.state

-- Answers
--
-- 'yes', 'chosen', 'key', 'mass', 'margin', 'confidence', 'masses',
-- 'expectation' and 'nearest' are the fields of the answer records
-- themselves, re-exported here. Read them with record dot.

contenders :: Double -> A Value (Choice alts) -> [(Double, Selected Value alts)]
contenders = Core.contenders

selectedKey :: Alternatives alts => Selected Value alts -> Text
selectedKey = Core.selectedKey

handle :: (Alternatives alts, Core.Match hs alts, hs ~ alts) => Selected Value alts -> Handlers r hs -> r
handle = Core.handle

accept :: Alternatives alts => Policy -> A Value (Choice alts) -> Either Doubt (Selected Value alts)
accept = Core.accept

-- | One line explaining why 'accept' returned what it did.
explain :: Alternatives alts => Policy -> A Value (Choice alts) -> Text
explain = Core.explain

-- | Read-only choices: which file, which skill.
routing :: Policy
routing = Policy 0.40 0.08 0.50

-- | Starting a worker, or choosing an approach.
spawning :: Policy
spawning = Policy 0.55 0.20 0.70

-- | Merging, stopping, anything with a receipt.
merging :: Policy
merging = Policy 0.70 0.40 0.85

massAtOrAbove :: KnownNat (Core.Index l levels) => Label l -> A Value (Score levels) -> Double
massAtOrAbove = Core.massAtOrAbove

-- | An answer is a ledger row: @toJSON a.next@.
instance ToJSON (A Value Noul) where toJSON = Core.previewAnswer
instance Alternatives alts => ToJSON (A Value (Choice alts)) where toJSON = Core.previewAnswer
instance Core.Rubric levels => ToJSON (A Value (Score levels)) where toJSON = Core.previewAnswer

-- | A whole answers packet is a ledger row too: @toJSON (answers resp)@.
instance (Core.Unique fs, Core.PacketSchema Value fs) => ToJSON (Packet fs Answers) where
  toJSON = Core.previewSchema

-- The operation
jevLatest :: Model
jevLatest = Core.jevLatest

request :: Schema s => Model -> State -> s Questions -> Either JevError Value
request = Core.request

decode :: Schema s => s Questions -> Value -> Either JevError (Response s)
decode = Core.decode

roundTrip :: (Monad m, Schema s) => (Value -> m (Either Text Value)) -> Model -> State -> s Questions -> m (Either JevError (Response s))
roundTrip = Core.roundTrip

jev1 :: (Monad m, Core.Endpoint Value e, Core.CellOk "value" e) => (Value -> m (Either Text Value)) -> Model -> State -> Q Value e -> m (Either JevError (Answers :- e))
jev1 = Core.jev1

answers :: Response s -> s Answers
answers = Core.answers

-- | Token counts for one call. Missing or non-numeric fields read as 0.
data Usage = Usage { inputTokens :: Int, outputTokens :: Int } deriving (Show, Eq)

usage :: Response s -> Usage
usage r = Usage (tokens "input_tokens") (tokens "output_tokens")
  where
    tokens :: Text -> Int
    tokens k = maybe 0 round (Core.lookupKey k (Core.usage r) >>= Core.viewNumber)

-- | The model the request resolved to, as reported by the response envelope.
resolvedModel :: Response s -> Text
resolvedModel = Core.responseModel

-- The Shoal host operation. Actor code reaches Jev only through these:
-- a packet or a question in, the host's Jev effect handles the transport.

-- | Send a whole packet to the host with the default model
-- ('jevLatest') and read back its typed 'Response'.
ask :: (Member Jev effs, Schema s) => State -> s Questions -> Eff effs (Either JevError (Response s))
ask = roundTrip jevTransport jevLatest

-- | 'ask', naming the model explicitly.
askWith :: (Member Jev effs, Schema s) => Model -> State -> s Questions -> Eff effs (Either JevError (Response s))
askWith = roundTrip jevTransport

-- | Send one question (a whole packet under the label @value@) to the host
-- with the default model and read back its typed answer.
ask1 :: (Member Jev effs, Core.Endpoint Value e, Core.CellOk "value" e) => State -> Q Value e -> Eff effs (Either JevError (Answers :- e))
ask1 = jev1 jevTransport jevLatest
