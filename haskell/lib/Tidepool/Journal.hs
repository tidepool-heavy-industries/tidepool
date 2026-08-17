{-# LANGUAGE OverloadedStrings #-}

-- | The durable append-only run journal — the authored surface of PRD 20's
-- ("exomonad v3") "Persistence and resume" section
-- (@plans\/self-iterating-harness\/20-exomonad-v3-prd.md@).
--
-- A resident harness records mid-loop progress as it happens, one durable
-- entry at a time, so a crash between two loop-boundary 'State' checkpoints
-- loses only in-flight work — never the record of what already finished.
-- Resume folds the journal back into a map instead of re-asking a planner or
-- redoing completed work.
--
-- @
-- 'record' "split" (renderBranchName branch) (toJSON plan)
-- 'record' "outcome" (renderBranchName branch) (toJSON receipt)
-- @
--
-- == Shape
--
-- Every entry is a fixed triple: 'kind' and 'key' are caller-chosen labels
-- (e.g. a step kind and the branch or task it concerns), and 'payload' is one
-- opaque JSON value — the PRD's locked lean is a fixed step shape with one
-- payload field, not a harness-extensible entry ADT.
--
-- == Append-only, forever
--
-- There is no rewrite, truncate, or compaction verb, deliberately: a later
-- record for the same 'key' does not replace an earlier one in the file, it
-- is simply appended after it. Folding many records for one 'key' down to
-- "the current state of that key" — e.g. keeping only the LAST one — is a
-- reader's job, not this module's.
--
-- == What this module does NOT do
--
-- Reading the journal back, folding it into a map, and injecting that map at
-- boot so a resumed run can skip already-completed steps is the swarm
-- driver's job (PRD 20 lane S1-L5), not this module's: 'record' is a
-- write-only effect from the authored program's point of view.
module Tidepool.Journal
  ( record
  ) where

import Tidepool.Effects (record)
