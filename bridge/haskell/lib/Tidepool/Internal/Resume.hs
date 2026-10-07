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
  , segmentPure
  , segmentBind
  ) where

import Control.Monad.Freer.Internal (Arrs, Eff (..), qApp)
import Data.OpenUnion (Union)
import Prelude

-- | One settled layer of an effect computation. Both request and value are
-- strict so the host reads a constructor in each position: @send@ builds the
-- request as an unevaluated @inj x@, and a lazy field would hand the host a
-- thunk it must not force itself. The request's payload stays lazy; the host
-- forces it through the machine's observation path. The continuation is
-- never forced by the host.
data Settled effs a
  = -- | The computation completed; the value is in weak head normal form.
    Done !a
  | -- | The computation requested an effect and retained its continuation.
    forall b. Suspended {-# NOUNPACK #-} !(Union effs b) (Arrs effs b a)

-- | Settle an effect computation to its first constructor layer.
settle :: Eff effs a -> Settled effs a
settle (Val a) = Done a
settle (E u q) = Suspended u q
{-# NOINLINE settle #-}

-- | Apply a retained continuation to a lifted answer.
resumeLifted :: Arrs effs b a -> b -> Eff effs a
resumeLifted = qApp
{-# NOINLINE resumeLifted #-}

-- | Construct the pure node used by compiler-owned effect segments.
-- Authored programs do not import this helper.
segmentPure :: a -> Eff effs a
segmentPure = Val
{-# NOINLINE segmentPure #-}

-- | Bind compiler-owned effect segments using freer-simple's representation.
-- A completed value enters the next segment immediately; a suspended effect
-- retains its request and appends the next segment to its continuation.
segmentBind :: Eff effs a -> (a -> Eff effs b) -> Eff effs b
segmentBind = (>>=)
{-# NOINLINE segmentBind #-}
