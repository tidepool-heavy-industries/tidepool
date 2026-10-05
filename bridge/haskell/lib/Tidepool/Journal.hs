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
-- APPEND-ONLY, FOREVER: there is no rewrite, truncate, compaction, or read
-- verb. A later record for the same 'key' does not replace an earlier one in
-- the file; it is simply appended after it. 'record' is write-only from the
-- authored program's point of view.
--
-- A write failure aborts the cycle: 'record' returns unit, not @Either@,
-- so the authored program cannot continue after a disk or permission error.
module Tidepool.Journal
  ( record
  , trace
  ) where

import Tidepool.Effects.Authored (record, trace)
