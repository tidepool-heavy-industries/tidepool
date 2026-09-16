{-# LANGUAGE ExistentialQuantification #-}

-- | Engine-private freer plumbing for the prepared-STG route.
--
-- The prepared machine never walks freer data itself. A turn's program
-- exposes its effect computation through 'settle', whose result is one
-- constructor layer the host reads without forcing: 'Done' carries the
-- completed value already in weak head normal form, 'Suspended' carries the
-- effect request and the retained continuation. 'resumeLifted' is the one
-- resume entry for every site: it applies a retained continuation to a lifted
-- answer and yields the computation for the host to settle again.
--
-- Authored programs never import this module; the turn templates reference
-- it so both tops are admitted into every prepared turn artifact.
module Tidepool.Internal.Resume
  ( Settled (..)
  , settle
  , resumeLifted
  ) where

import Control.Monad.Freer.Internal (Arrs, Eff (..), qApp)
import Data.OpenUnion (Union)
import Prelude

-- | One settled layer of an effect computation.
data Settled effs a
  = -- | The computation completed; the value is in weak head normal form.
    Done !a
  | -- | The computation requested an effect and retained its continuation.
    forall b. Suspended (Union effs b) (Arrs effs b a)

-- | Settle an effect computation to its first constructor layer.
settle :: Eff effs a -> Settled effs a
settle (Val a) = Done a
settle (E u q) = Suspended u q
{-# NOINLINE settle #-}

-- | Apply a retained continuation to a lifted answer.
resumeLifted :: Arrs effs b a -> b -> Eff effs a
resumeLifted = qApp
{-# NOINLINE resumeLifted #-}
