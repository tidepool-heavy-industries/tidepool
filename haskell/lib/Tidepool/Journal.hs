{-# LANGUAGE OverloadedStrings #-}

-- | The durable append-only run journal.
--
-- A resident harness records mid-loop progress as it happens, one durable
-- entry at a time, so a crash between two loop-boundary 'State' checkpoints
-- loses only in-flight work — never the record of what already finished.
--
-- @
-- 'record' "split" (renderBranchName branch) (toJSON plan)
-- 'record' "outcome" (renderBranchName branch) (toJSON receipt)
-- @
--
-- Every entry is a fixed triple: 'kind' and 'key' are caller-chosen labels
-- (e.g. a step kind and the branch or task it concerns), and 'payload' is one
-- opaque JSON value.
--
-- APPEND-ONLY, FOREVER: there is no rewrite, truncate, or compaction verb —
-- a later record for the same 'key' does not replace an earlier one in the
-- file, it is simply appended after it. Folding many records for one 'key'
-- down to "the current state of that key" (e.g. keeping only the LAST one)
-- is a reader's job, not this module's; reading the journal back and
-- injecting it at boot so a resumed run can skip completed steps is the
-- caller's job too — 'record' is a write-only effect from the authored
-- program's point of view.
--
-- A WRITE FAILURE ABORTS THE RUN, deliberately: 'record' returns unit, not
-- @Either@ — a journal write failure (disk full, unwritable path) is not an
-- error the authored program handles, the driver fails the whole cycle. A
-- run that cannot journal cannot honestly resume, so continuing would trade
-- durability for the appearance of progress.
module Tidepool.Journal
  ( record
  ) where

import Tidepool.Effects (record)
