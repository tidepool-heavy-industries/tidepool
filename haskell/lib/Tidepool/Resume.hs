{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}

-- | The read half of the durable run journal — PRD 20's ("exomonad v3")
-- "Persistence and resume" section, lane S1-L5
-- (@plans\/self-iterating-harness\/20-s1-l5-resume.md@).
--
-- "Tidepool.Journal" is the WRITE side: @record kind key payload@ appends one
-- durable entry per completed step. This module is what the authored program
-- receives on the other end of a crash — the driver's FOLD of that journal,
-- handed in at boot.
--
-- == Nothing here reads a file
--
-- A 'ResumeFold' is a plain value. Locating the run's journal, loading it,
-- folding it, and injecting the result is the DRIVER's job, exactly as
-- "Tidepool.Journal"'s module doc says; @record@ stays write-only from the
-- authored side and no verb in this module touches I\/O. What arrives here has
-- already been folded.
--
-- == How it arrives: @resumeLoop@
--
-- The driver's ordinary entry point is @loop :: State -> Harness State@. A
-- harness that wants to resume declares ONE more top-level binding:
--
-- @
-- resumeLoop :: 'ResumeFold' -> State -> Harness State
-- resumeLoop fold st = ...
-- @
--
-- A run with nothing journaled enters through @loop@ as it always has. A run
-- whose journal has entries enters through @resumeLoop@ with them folded —
-- and a harness that journals but declares no @resumeLoop@ is REFUSED at boot
-- rather than quietly redoing finished work. The injection is one-shot: the
-- first cycle after boot receives the fold, later cycles are ordinary @loop@
-- calls.
--
-- == The fold's shape
--
-- One entry per @(kind, key)@ pair — the LAST one recorded, by sequence
-- number. The journal is append-only forever, so several records for one key
-- accumulate in the file; keeping the newest is the reader's job (there is no
-- compaction, deliberately). Keying on the PAIR rather than the key alone is
-- what lets a harness record several facts about one branch — a @"split"@ and
-- an @"outcome"@ under the same branch name — without either erasing the
-- other.
--
-- @
-- case 'lookupResume' "outcome" branch fold of
--   Just payload -> ...   -- this branch already finished; do not redo it
--   Nothing      -> ...   -- unstarted, or crashed mid-flight
-- @
--
-- == Payloads stay the harness's business
--
-- 'resumePayload' is an opaque 'Value'. The kinds, the keys, and the shape
-- inside are the authoring harness's schema — @dev-tree@'s
-- @split@\/@outcome@\/@replan@\/@rebase@\/@escalation@ vocabulary is invisible
-- to both the driver and this module.
module Tidepool.Resume
  ( -- * The fold
    ResumeFold (..)
  , ResumeEntry (..)
  , emptyResume
  , isResumed
    -- * Querying it
  , lookupResume
  , lookupResumeEntry
  , resumeOfKind
  , resumeKeysOf
  ) where

import Tidepool.Aeson (FromJSON (..), Object, Result, Value, withObject, (.:))
import Tidepool.Prelude

-- | One folded journal entry: the LAST record the run made under this
-- @(kind, key)@ pair.
data ResumeEntry = ResumeEntry
  { resumeSeq     :: Int
    -- ^ The record's sequence number within the run. Monotonic across the
    -- whole run, including across the processes a crash split it into, so it
    -- orders entries the file's line order alone could not be trusted to.
  , resumeKind    :: Text
  , resumeKey     :: Text
  , resumePayload :: Value
    -- ^ Opaque to everything but the harness that recorded it.
  }

-- | Everything the driver folded out of this run's journal at boot.
--
-- An empty 'resumeEntries' means a fresh run — nothing was journaled, so
-- there is nothing to skip. 'resumeRunId' names the run the entries came
-- from; it is the same id across every process the run survived.
data ResumeFold = ResumeFold
  { resumeRunId   :: Text
  , resumeEntries :: [ResumeEntry]
  }

-- | The fold of a run that has recorded nothing — what a fresh run would see
-- if it looked, and the honest default for a harness constructing one itself
-- (a test, or a @resumeLoop@ delegating to @loop@).
emptyResume :: ResumeFold
emptyResume = ResumeFold {resumeRunId = "", resumeEntries = []}

-- | Whether this fold carries anything worth skipping. False for a fresh run.
isResumed :: ResumeFold -> Bool
isResumed f = not (null f.resumeEntries)

-- | The payload last recorded under @kind@ and @key@, if any. The workhorse:
-- @lookupResume "outcome" branch fold@ answers "did this branch already
-- finish".
lookupResume :: Text -> Text -> ResumeFold -> Maybe Value
lookupResume kind k f = (.resumePayload) <$> lookupResumeEntry kind k f

-- | 'lookupResume' keeping the whole entry — for a caller that needs the
-- sequence number, e.g. to decide whether a @"replan"@ amendment is NEWER
-- than the @"split"@ it amends.
lookupResumeEntry :: Text -> Text -> ResumeFold -> Maybe ResumeEntry
lookupResumeEntry kind k f =
  find (\e -> e.resumeKind == kind && e.resumeKey == k) f.resumeEntries

-- | Every entry of one kind, in the fold's own order (sorted by
-- @(kind, key)@ — deterministic, and never completion order).
resumeOfKind :: Text -> ResumeFold -> [ResumeEntry]
resumeOfKind kind f = filter (\e -> e.resumeKind == kind) f.resumeEntries

-- | The keys recorded under one kind — e.g. every branch that got as far as a
-- recorded split.
resumeKeysOf :: Text -> ResumeFold -> [Text]
resumeKeysOf kind f = map (.resumeKey) (resumeOfKind kind f)

-- ---------------------------------------------------------------------------
-- The wire
--
-- Hand-written rather than generic-derived, because this is a CONTRACT with
-- the driver's encoder (`selfharness::resume::ResumeFold::to_json`) and the
-- field names on the wire are part of it: @seq@\/@kind@\/@key@\/@payload@,
-- not the Haskell record's own selector names. Keeping the two spellings
-- separate is what lets either side rename a field without a silent decode
-- failure at boot.
-- ---------------------------------------------------------------------------

instance FromJSON ResumeEntry where
  parseJSON = withObject "ResumeEntry" parseEntry

parseEntry :: Object -> Result ResumeEntry
parseEntry o =
  ResumeEntry
    <$> o .: "seq"
    <*> o .: "kind"
    <*> o .: "key"
    <*> o .: "payload"

instance FromJSON ResumeFold where
  parseJSON = withObject "ResumeFold" parseFold

parseFold :: Object -> Result ResumeFold
parseFold o =
  ResumeFold
    <$> o .: "runId"
    <*> o .: "entries"
