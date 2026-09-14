{-# LANGUAGE DataKinds #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE MagicHash #-}
{-# LANGUAGE TypeOperators #-}

module FreerResume where

import Control.Monad.Freer (Eff, send)
import Control.Monad.Freer.Internal (Arrs, qApp)
import GHC.Exts (Int (I#), Int#)

data Req result where
  Ask :: Int -> Req Int

-- Two dependent sends: the second request's argument is built from the first
-- response, so the second `send` is not independently observable from the
-- first (operands opaque to the simplifier, matching the cohort probes).
program :: Eff '[Req] Int
program = do
  a <- send (Ask 3)
  b <- send (Ask (a + 1))
  pure (a + b)
{-# NOINLINE program #-}

-- Compiled resume: `qApp` applied to the retained continuation `k` and the
-- boxed unboxed-Int response. Calling this top from Rust with a managed `k`
-- and a scalar `n#` is the compiled qApp resume (Wave 6B decision D2) with no
-- Rust-side freer walker and no second decoder of the freer envelope.
resumeInt :: Arrs '[Req] Int Int -> Int# -> Eff '[Req] Int
resumeInt k n = qApp k (I# n)
{-# NOINLINE resumeInt #-}

-- `program` and `resumeInt` do not reference each other's bindings, so the
-- corpus projection's per-entry reachability closure (`selectPreparedTarget`
-- in Tidepool.ExecutionProjection) would keep them apart under separate
-- targets. Referencing both from one top keeps them in a single reachable
-- closure so one projection run admits `resumeInt` as a genuine second entry
-- of the same artifact, addressable by its own `ValueId` alongside `program`.
freerResumeEntries :: (Eff '[Req] Int, Arrs '[Req] Int Int -> Int# -> Eff '[Req] Int)
freerResumeEntries = (program, resumeInt)
{-# NOINLINE freerResumeEntries #-}
