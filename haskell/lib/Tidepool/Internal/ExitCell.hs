{-# LANGUAGE MagicHash #-}
{-# LANGUAGE UnboxedTuples #-}

-- | Engine-private, single-assignment storage for a live Haskell result.
--
-- The cell is deliberately a managed Haskell heap object rather than a
-- runtime value handle.  Copies share the same cell, so an arbitrary result
-- (including a closure) stays live through ordinary Haskell reachability.
-- Mutation is kept behind the actor/async libraries; authored programs never
-- receive a general shared-reference API.
module Tidepool.Internal.ExitCell
  ( ExitCell
  , newExitCell
  , fillExitCell
  , readExitCell
  ) where

import GHC.Exts
  ( RealWorld
  , SmallMutableArray#
  , newSmallArray#
  , readSmallArray#
  , runRW#
  , writeSmallArray#
  )
import Prelude

data CellState pending value
  = Pending pending
  | Filled value

-- | A pending token is retained until the value is published.  Besides being
-- useful ownership metadata, it makes allocation depend on the computation
-- that owns this particular cell rather than on a floatable nullary CAF.
data ExitCell pending value = ExitCell
  (SmallMutableArray# RealWorld (CellState pending value))

-- | Allocate one fresh cell for @pending@.
--
-- This is engine-internal mutable state, sequenced by the single-threaded
-- actor machine.  The public abstraction remains a single-assignment value.
{-# NOINLINE newExitCell #-}
newExitCell :: pending -> ExitCell pending value
newExitCell pending =
  runRW# $ \s0 ->
    case newSmallArray# 1# (Pending pending) s0 of
      (# _, cell #) -> ExitCell cell

-- | Publish the result.  The actor/async entry wrapper is the sole writer.
{-# NOINLINE fillExitCell #-}
fillExitCell :: ExitCell pending value -> value -> ()
fillExitCell (ExitCell cell) value =
  runRW# $ \s0 ->
    case readSmallArray# cell 0# s0 of
      (# s1, state #) ->
        case state of
          Pending _ ->
            case writeSmallArray# cell 0# (Filled value) s1 of
              _ -> ()
          Filled _ -> error "Tidepool.Internal.ExitCell: filled twice"

-- | Observe the published result without transferring it out of the managed
-- heap. The first argument is the authoritative lifecycle observation that
-- sequences this read after publication. @Nothing@ means the producing
-- computation has not completed.
{-# NOINLINE readExitCell #-}
readExitCell :: observation -> ExitCell pending value -> Maybe value
readExitCell observed (ExitCell cell) =
  observed `seq`
    runRW# (\s0 ->
      case readSmallArray# cell 0# s0 of
        (# _, state #) ->
          case state of
            Pending _ -> Nothing
            Filled value -> Just value)
