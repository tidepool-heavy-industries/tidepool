{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}

-- | Typed completion of one model deliberation.
--
-- The result type is part of the effect row selected for the current goal, so
-- @complete value@ is checked by GHC before Rust ever sees the suspension.
-- The constructor remains private: authored code gets one completion action,
-- not a second raw request API.
module Tidepool.Deliberation
  ( Complete
  , complete
  ) where

import Control.Monad.Freer (Eff, Member, send)

data Complete result a where
  CompleteWith :: Int -> result -> Complete result a

-- | Settle the current typed goal with an in-heap value. The value may be a
-- closure or any other ordinary Haskell value; it is never serialized.
complete :: Member (Complete result) effs => result -> Eff effs a
complete value = send (CompleteWith 0 value)
