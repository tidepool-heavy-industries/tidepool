{-# LANGUAGE NoImplicitPrelude #-}

-- | Fixture for the mount spike
-- (@tidepool-harness\/tests\/companion_mount_spike.rs@, PRD 21 lane C1): the
-- minimal, C0-independent record the spec calls for — single-constructor,
-- one function field, deliberately no serialization instances (same
-- reasoning as @examples\/harness\/fn-record-spike@'s @Edits@: its whole
-- point is that it crosses in-heap, never through JSON).
module HarnessTypes (Mounted (..)) where

import Tidepool.Prelude

data Mounted = Mounted {applyMounted :: Int -> Int}
