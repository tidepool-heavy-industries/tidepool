{-# LANGUAGE NoImplicitPrelude #-}

-- | Fixtures for the scope-tree acceptance suite
-- (@tidepool-harness\/tests\/companion_scope_trees.rs@, PRD 21 lane C2). The
-- C1 mount-spike fixture (@examples\/harness\/mount-spike@) is left alone: it
-- is the FLAT-session mount user, and its passing unmodified is a back-compat
-- proof obligation.
--
-- Two shapes, both called out by the C1 seam note's "what C2 should
-- generalize" as the untested corners of the mount seam:
--
-- * 'Toolkit' — a record with SEVERAL function-bearing fields, mixed with an
--   ordinary data field. The seam's closure detection is a TRANSITIVE walk, so
--   this is plausible-but-unexercised rather than known-good; the suite proves
--   every field survives the crossing individually.
-- * 'Focus' — function-typed at TOP level (a newtype over a function, so the
--   mounted value is a bare closure once the newtype erases). Expected to work
--   already, since it is the same shape the C1 spike's @Mounted@ field has one
--   level down; pinned so "a lens mounts" stops being an expectation.
--
-- Deliberately no serialization instances, for the same reason
-- @examples\/harness\/mount-spike@ and @examples\/harness\/fn-record-spike@
-- have none: the whole point is that these cross IN-HEAP, never through JSON.
module HarnessTypes (Toolkit (..), Focus (..)) where

import Tidepool.Prelude

-- | Several function fields, plus one ordinary one. Each field is called
-- separately by the acceptance suite: a sentinel substituted for any one of
-- them would case-trap at that call, not at the crossing.
data Toolkit = Toolkit
  { bumpBy :: Int -> Int
  , scaleBy :: Int -> Int
  , clampAt :: Int -> Int
  , toolkitTag :: Int
  }

-- | Lens-shaped: a modifier over an @Int@, function-typed at the top level
-- rather than inside a record field. The get\/modify encoding, not the van
-- Laarhoven one — the mount seam has no reason to carry a @Functor@, and the
-- property under test is the top-level closure shape, not lens ergonomics.
newtype Focus = Focus {runFocus :: (Int -> Int) -> Int -> Int}
