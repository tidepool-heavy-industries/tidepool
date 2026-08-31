{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}

-- | Typed completion of one model deliberation.
--
-- The result type is part of the effect row selected for the current goal, so
-- @complete value@ is checked by GHC before Rust ever sees the suspension.
-- The constructor remains private: authored code gets one completion action,
-- not a second raw request API.
module Tidepool.Deliberation
  ( Deliberation
  , deliberation
  , deliberate
  , Complete
  , complete
  ) where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)

import Tidepool.Effects.Core (Deliberate (..))

-- | One typed task for the actor's resident model. The constructor is hidden;
-- GHC records the concrete input and output types at each 'deliberate' call
-- site, so authored strings never participate in the type membrane.
data Deliberation input output = Deliberation Text

-- | Describe a typed model decision. The task is presentation; @input@ and
-- @output@ are the actual compile-time contract.
deliberation :: Text -> Deliberation input output
deliberation = Deliberation

-- | Suspend the installed actor program while its resident model produces a
-- value of the deliberation's output type.
{-# OPAQUE deliberate #-}
deliberate
  :: forall output input effs
   . Member Deliberate effs
  => Deliberation input output
  -> input
  -> Eff effs output
deliberate goal input = deliberateSited @output @input 0 goal input

-- Extractor substrate. Every fully-applied public call is rewritten here with
-- a fresh site id whose GHC-derived answer/input types travel in compiler
-- metadata; the placeholder zero is never authoritative.
{-# OPAQUE deliberateSited #-}
deliberateSited
  :: forall output input effs
   . Member Deliberate effs
  => Int
  -> Deliberation input output
  -> input
  -> Eff effs output
deliberateSited site (Deliberation task) input =
  send (DeliberateWith site input task)

data Complete result a where
  CompleteWith :: Int -> result -> Complete result a

-- | Settle the current typed goal with an in-heap value. The value may be a
-- closure or any other ordinary Haskell value; it is never serialized.
complete :: Member (Complete result) effs => result -> Eff effs a
complete value = send (CompleteWith 0 value)
