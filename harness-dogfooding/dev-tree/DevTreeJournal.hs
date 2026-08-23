{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}

-- | dev-tree's durable journal vocabulary, typed.
--
-- "Tidepool.Journal" and "Tidepool.Resume" are deliberately generic: a
-- journal entry is a @(kind, key, payload :: Value)@ triple end to end, and
-- neither module knows dev-tree's own @split@\/@outcome@\/@replan@\/
-- @rebase@\/@escalation@ vocabulary. That genericity is correct at THAT
-- layer (the driver and the wire format must not know any one harness's
-- schema) — but it means nothing connects what "Harness" WRITES to what it
-- READS BACK: a kind string typo'd at one call site and correct at another
-- compiles clean, and only breaks crash recovery, in production, during an
-- actual crash.
--
-- This module is the one layer up: the five kinds dev-tree actually
-- journals, as a Haskell sum, with exactly one function that turns a value
-- of it into a @record@ call ('recordEvent') and exactly one that turns a
-- folded @(kind, key, payload)@ triple back into one ('decodeEvent'). Every
-- kind string in this file appears in exactly one place — 'kindText' — so a
-- kind rename is a single edit, and a mismatched kind between the write side
-- and a read side is impossible to express: 'Harness' never again spells a
-- kind as a bare 'Text' literal, only as a 'JournalKind' constructor.
--
-- == Wire compatibility is the whole point
--
-- 'recordEvent' emits exactly the kind string, key, and payload shape
-- "Harness"'s @emitSplit@\/@journalOutcome@\/@onChildFailure@\/
-- @mechanicalRebase@\/@awaitResolutions@\/@escalate@ wrote before this
-- module existed, and 'decodeEvent' reads exactly what its
-- pre-refactor @decodeSplit@\/@decodeOutcome@ read — including their
-- defaulting behaviour for a field an older or foreign entry might omit.
-- A journal written by the pre-refactor code folds identically under this
-- reader; @dogfood_harness_typecheck.rs@'s round-trip tests pin this against
-- literal captured wire samples.
module DevTreeJournal
  ( JournalKey (..)
  , JournalKind (..)
  , JournalEvent (..)
  , kindOf
  , keyOf
  , payloadOf
  , recordEvent
  , decodeEvent
  , lookupEvent
  , eventsOfKind
  ) where

import HarnessTypes (DevPlan (..), FoldReceipt (..), Outcome (..), RebaseNote (..), ReplanDecision)
import Tidepool.Aeson (Value, object, toJSON, (.=))
import Tidepool.Harness (Harness)
import Tidepool.Journal (record)
import Tidepool.Prelude
import Tidepool.Resume (ResumeEntry (..), ResumeFold, lookupResumeEntry, resumeOfKind)

-- | The durable identity a journal entry is keyed under. Usually a branch
-- name, sometimes a plain plan node name (see 'JournalEvent's Outcome case,
-- which dev-tree keys by node name for a receiptless failure or a skip) —
-- never a git sha or anything rendered from a receipt. A newtype, not bare
-- 'Text', so a call site cannot pass a scaffold head or a rendered summary
-- where a key was meant.
newtype JournalKey = JournalKey Text
  deriving (Show, Eq, Ord)

-- | The five kinds dev-tree actually journals, and nothing else. Every kind
-- STRING lives in 'kindText' alone; everywhere else in this file, and
-- everywhere in "Harness", a kind is one of these four-ish constructors.
data JournalKind
  = SplitKind
  | OutcomeKind
  | ReplanKind
  | RebaseKind
  | EscalationKind
  deriving (Show, Eq)

kindText :: JournalKind -> Text
kindText SplitKind = "split"
kindText OutcomeKind = "outcome"
kindText ReplanKind = "replan"
kindText RebaseKind = "rebase"
kindText EscalationKind = "escalation"

-- | One durable fact dev-tree records. Every constructor carries the key it
-- is filed under ('evKey') alongside its own typed payload — 'recordEvent'
-- and 'decodeEvent' are what make that pairing, and the wire's
-- @kind@\/@key@\/@payload@ triple, agree by construction.
data JournalEvent
  = -- | The coalgebra's split decision. Journaled TWICE under the same key
    -- (see "Harness"'s @emitSplit@) — once before children are allocated
    -- ('evChildTrees' @=@ 'Nothing', which is why "children" appears on the
    -- wire but "childTrees" does not) and once after ('evChildTrees' @=@
    -- @'Just' trees@, possibly @[]@ if every child was denied). The fold
    -- keeps only the later append, which is the point: it is the only
    -- durable record of which retained worktree belongs to which child.
    SplitEvent
      { evKey          :: JournalKey
      , evSplitPlan    :: DevPlan
      , evScaffoldHead :: Text
      , evChildTrees   :: Maybe [(Text, Text)]
      }
  | -- | The algebra's fold result — 'Done', 'Failed', or 'Skipped'. The wire
    -- shape depends on the constructor (a bare receipt for 'Done'; an
    -- object for the other two), decided by 'payloadOf', not stored
    -- alongside the 'Outcome' redundantly.
    OutcomeEvent
      { evKey     :: JournalKey
      , evOutcome :: Outcome
      }
  | -- | An 'onChildFailure'\/'applyPolicy' replan agent session's answer.
    ReplanEvent
      { evKey      :: JournalKey
      , evDecision :: ReplanDecision
      }
  | -- | One step of the eager rebase cascade that actually moved a tip (a
    -- 'RebaseTier.RebaseCurrent' no-op is never journaled — nothing moved,
    -- nothing to replay).
    RebaseEvent
      { evKey  :: JournalKey
      , evNote :: RebaseNote
      }
  | -- | A tier-3 escalation handed to the parent's failure policy.
    EscalationEvent
      { evKey       :: JournalKey
      , evEscNode   :: Text
      , evEscDetail :: Text
      }
  deriving (Show, Eq)

kindOf :: JournalEvent -> JournalKind
kindOf SplitEvent {} = SplitKind
kindOf OutcomeEvent {} = OutcomeKind
kindOf ReplanEvent {} = ReplanKind
kindOf RebaseEvent {} = RebaseKind
kindOf EscalationEvent {} = EscalationKind

keyOf :: JournalEvent -> Text
keyOf ev = case ev.evKey of JournalKey k -> k

-- | The exact payload shape 'recordEvent' writes for each kind — split out
-- so a round-trip test can call it directly, without performing the
-- 'Harness' effect.
payloadOf :: JournalEvent -> Value
payloadOf SplitEvent {evSplitPlan = p, evScaffoldHead = h, evChildTrees = childTrees} =
  object
    ( [ "node" .= nodeName p
      , "scaffoldHead" .= h
      , "children" .= map nodeName (childPlans p)
      , "plan" .= toJSON p
      ]
        <> maybe [] (\trees -> ["childTrees" .= map childTreeJson trees]) childTrees
    )
  where
    childTreeJson (n, b) = object ["name" .= n, "branch" .= b]
payloadOf OutcomeEvent {evOutcome = o} = case o of
  Done {doneReceipt = r} -> toJSON r
  Failed {outcomeNode = n, outcomeFailure = f, partialReceipt = Just r} ->
    object ["node" .= n, "failure" .= toJSON f, "receipt" .= toJSON r]
  Failed {outcomeNode = n, outcomeFailure = f, partialReceipt = Nothing} ->
    object ["node" .= n, "failure" .= toJSON f]
  Skipped {outcomeNode = n, skipReason = why} ->
    object ["node" .= n, "skipped" .= why]
payloadOf ReplanEvent {evDecision = d} = toJSON d
payloadOf RebaseEvent {evNote = n} = toJSON n
payloadOf EscalationEvent {evEscNode = n, evEscDetail = why} =
  object ["node" .= n, "detail" .= why]

-- | The ONE boundary onto "Tidepool.Journal"'s generic @record@. Every
-- 'JournalEvent' this module can construct records at exactly the kind
-- 'kindOf' names, under exactly the key it carries, with exactly the
-- payload 'payloadOf' builds.
recordEvent :: JournalEvent -> Harness ()
recordEvent ev = record (kindText (kindOf ev)) (keyOf ev) (payloadOf ev)

-- | The ONE boundary onto "Tidepool.Resume"'s generic fold. Total and
-- degrading: a kind this sum does not recognise, or a payload this reader
-- cannot parse, decodes to 'Nothing' rather than raising — exactly the
-- pre-refactor @decodeSplit@\/@decodeOutcome@ contract ("a payload this run
-- cannot make sense of is Nothing, which degrades to 'do the work', never to
-- a wrong skip"), now enforced at one call site instead of scattered ones.
decodeEvent :: Text -> Text -> Value -> Maybe JournalEvent
decodeEvent kind k payload
  | kind == kindText SplitKind = decodeSplitEvent k payload
  | kind == kindText OutcomeKind = OutcomeEvent (JournalKey k) <$> decodeOutcomeValue k payload
  | kind == kindText ReplanKind = ReplanEvent (JournalKey k) <$> decodeJson payload
  | kind == kindText RebaseKind = RebaseEvent (JournalKey k) <$> decodeJson payload
  | kind == kindText EscalationKind =
      Just
        ( EscalationEvent
            (JournalKey k)
            (fromMaybe k (payload ^? key "node" . _String))
            (fromMaybe "escalated" (payload ^? key "detail" . _String))
        )
  | otherwise = Nothing

decodeSplitEvent :: Text -> Value -> Maybe JournalEvent
decodeSplitEvent k v = do
  h <- v ^? key "scaffoldHead" . _String
  p <- v ^? key "plan" >>= decodeJson
  pure
    SplitEvent
      { evKey = JournalKey k
      , evSplitPlan = p
      , evScaffoldHead = h
      , evChildTrees = v ^? key "childTrees" . _Array >>= traverse decodeChildTree
      }
  where
    decodeChildTree cv = (,) <$> (cv ^? key "name" . _String) <*> (cv ^? key "branch" . _String)

-- | 'k' is the fallback for a payload with no "node" field of its own.
-- Sound because every current caller looks this up under the same key the
-- entry was recorded under (a branch, or — for a receiptless 'Failed'\/a
-- 'Skipped' — the plan node name that also became the key), so the fallback
-- and the field agree whenever the field is present, and the writer always
-- writes it.
decodeOutcomeValue :: Text -> Value -> Maybe Outcome
decodeOutcomeValue k v = case v ^? key "skipped" . _String of
  Just why -> Just Skipped {outcomeNode = named, outcomeTrail = [], skipReason = why}
  Nothing -> case v ^? key "failure" >>= decodeJson of
    Just f ->
      Just
        Failed
          { outcomeNode = named
          , outcomeTrail = []
          , outcomeFailure = f
          , partialReceipt = v ^? key "receipt" >>= decodeJson
          }
    Nothing -> case decodeJson v of
      Just r -> Just Done {outcomeNode = r.receiptNode, outcomeTrail = [], doneReceipt = r}
      Nothing -> Nothing
  where
    named = fromMaybe k (v ^? key "node" . _String)

decodeJson :: FromJSON a => Value -> Maybe a
decodeJson v = case fromJSON v of
  Success a -> Just a
  Error _ -> Nothing

-- | The last event folded under one @(kind, key)@ pair, alongside the
-- sequence number "Harness"'s @amendmentIsNewest@ compares. Total: a kind
-- this sum does not recognise, or a payload it cannot parse, is 'Nothing' —
-- the same "do the work" degradation 'decodeEvent' documents.
lookupEvent :: JournalKind -> Text -> ResumeFold -> Maybe (Int, JournalEvent)
lookupEvent kind key fold = do
  e <- lookupResumeEntry (kindText kind) key fold
  ev <- decodeEvent e.resumeKind e.resumeKey e.resumePayload
  pure (e.resumeSeq, ev)

-- | Every decodable entry of one kind, key and sequence number alongside it,
-- in the fold's own @(kind, key)@ order.
eventsOfKind :: JournalKind -> ResumeFold -> [(Text, Int, JournalEvent)]
eventsOfKind kind fold =
  [ (e.resumeKey, e.resumeSeq, ev)
  | e <- resumeOfKind (kindText kind) fold
  , Just ev <- [decodeEvent e.resumeKind e.resumeKey e.resumePayload]
  ]
