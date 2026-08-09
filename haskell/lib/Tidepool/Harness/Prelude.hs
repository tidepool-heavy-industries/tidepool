{-# LANGUAGE NoImplicitPrelude #-}
-- | Curated re-export surface for harness AUTHOR modules — the file an
-- author writes to drive the self-iterating harness
-- (@examples\/harness\/Harness.hs@ is the reference contract). ONE import in
-- place of the @import Tidepool.Prelude@ \/
-- @import Tidepool.Harness (Harness, runLLMTurn)@ pair every harness
-- fixture writes today.
--
-- Curated, not a dump
-- (@plans\/self-iterating-harness\/15-generic-surface-wave.md@): every name
-- here is one an author actually reaches for while writing
-- @loop@\/@State@\/@render@. One collision is resolved by construction here
-- rather than left for an author to discover: bare @from@\/@to@ from
-- "GHC.Generics" are an AMBIGUOUS OCCURRENCE against
-- @Control.Lens.Iso.from@\/@Control.Lens.Getter.to@ (both re-exported
-- wholesale by 'Tidepool.Prelude') — hit live through the real extractor by
-- the generic-codec spike
-- (@plans\/self-iterating-harness\/16-generic-spike-receipts.md@, finding
-- 1). 'gFrom'\/'gTo' below are the SAFE names: an author who needs the
-- generic round trip reaches for these, never the bare (ambiguous) names
-- and never a per-file @import qualified GHC.Generics as G@ of their own.
--
-- (An author-contract @render@ used to collide with a generic @render@
-- 'Tidepool.Prelude' re-exported from @Tidepool.Render@ — every existing
-- harness fixture wrote @import Tidepool.Prelude hiding (render)@ to dodge
-- it. That re-export is gone from 'Tidepool.Prelude' now (approved surface
-- decision, generic-surface wave item 4, 2026-08-08: bare @render@ was never
-- an advertised verb), so there is nothing left to hide here.)
module Tidepool.Harness.Prelude
  ( -- * Tidepool.Prelude
    module Tidepool.Prelude
    -- * The harness verbs (Tidepool.Harness)
  , Harness
  , runLLMTurn
    -- * Generic machinery, under known-safe names
  , gFrom
  , gTo
  ) where

import Tidepool.Prelude
import Tidepool.Harness (Harness, runLLMTurn)
import qualified GHC.Generics as G

-- | Safe-named wrapper for @GHC.Generics.from@. Reach for this — never the
-- bare, ambiguous @from@, never a per-file qualified import — to get a
-- derived-'Generic' type's generic representation.
gFrom :: G.Generic a => a -> G.Rep a x
gFrom = G.from
{-# INLINE gFrom #-}

-- | Safe-named wrapper for @GHC.Generics.to@ — the inverse of 'gFrom'.
gTo :: G.Generic a => G.Rep a x -> a
gTo = G.to
{-# INLINE gTo #-}
