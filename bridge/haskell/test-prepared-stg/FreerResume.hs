{-# LANGUAGE DataKinds #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE MagicHash #-}
{-# LANGUAGE TypeOperators #-}

module FreerResume where

import Control.Monad.Freer (send)
import Control.Monad.Freer.Internal (Arrs, Eff (..), qApp)
import GHC.Exts (Int (I#), Int#)

-- `!Int`, not `Int`: the prepared-machine observation boundary
-- (`tidepool/codegen/src/prepared_program/observe.rs`'s `inspect_outer`,
-- shared by the plain observation path too) reads exactly one already-WHNF
-- constructor layer and never forces a field -- by design, per that module's
-- doc comment ("never grants permission to evaluate a constructor's
-- descendants"). A lazy `Ask`'s argument field is a live `Thunk` object
-- until something forces it; `inspect_outer` on that field then fails
-- (`ObservationFailure::Unobservable(Thunk)`), confirmed empirically while
-- building the E2 resume-loop test. Marking the field strict makes this
-- constructor's own argument force (and, since it is a plain `Int`,
-- unbox to `Int#`) at the point `Ask` is applied, so `E2`'s resume loop
-- can read it as a `Scalar` the moment `Ask` itself inspects as a real
-- constructor -- no change to `program`'s two-dependent-`send`s shape or
-- its opacity to the simplifier.
data Req result where
  Ask :: !Int -> Req Int

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

-- Reads one suspended `E`'s request payload down to its raw `Int#`.
-- `Union`'s own payload field (`Data.OpenUnion.Internal`, a library type
-- E2 does not own) is an ordinary lazy field regardless of `Ask`'s own
-- strictness, so the retained payload value is still a `Thunk` when E2
-- reaches it by `inspect_outer` alone. This is the plain-Haskell way to
-- force it: an ordinary pattern match, compiled and run exactly like every
-- other top here (not a Rust-side freer walker, and not a second decoder of
-- the freer envelope -- E2 still finds `Union`'s tag/payload shape only
-- through `inspect_outer`; this top only resolves the one field
-- `inspect_outer` cannot).
askArgument :: Req Int -> Int#
askArgument (Ask (I# n)) = n
{-# NOINLINE askArgument #-}

-- Reads a settled `Val`'s boxed `Int` down to its raw `Int#`, for the same
-- reason as `askArgument`: `Val`'s field is an ordinary lazy field (`pure
-- (a + b)` is not forced by anything on the path back to Rust), so
-- `inspect_outer` alone cannot reach it. Never applied to `E`: E2 only
-- calls this top once `inspect_outer` on the outer `Eff` value has already
-- identified it as `Val`.
valResult :: Eff '[Req] Int -> Int#
valResult (Val (I# n)) = n
valResult (E _ _) = error "valResult: Eff value is E, not Val"
{-# NOINLINE valResult #-}

-- `program`, `resumeInt`, `askArgument` and `valResult` do not reference
-- each other's bindings, so the corpus projection's per-entry reachability
-- closure (`selectPreparedTarget` in `Tidepool.ExecutionProjection`) would
-- keep them apart under separate targets. Referencing all four from one top
-- keeps them in a single reachable closure so one projection run admits
-- each of the other three as a genuine additional entry of the same
-- artifact, each addressable by its own `ValueId` alongside `program`.
freerResumeEntries ::
  ( Eff '[Req] Int,
    Arrs '[Req] Int Int -> Int# -> Eff '[Req] Int,
    Req Int -> Int#,
    Eff '[Req] Int -> Int#
  )
freerResumeEntries = (program, resumeInt, askArgument, valResult)
{-# NOINLINE freerResumeEntries #-}
