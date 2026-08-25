{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE RankNTypes #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeOperators #-}

-- | Minimal standalone repro for a JIT row-changing @reinterpret@ gap: a
-- private, row-changing effect 'Ping', reinterpreted with freer-simple's
-- @reinterpret@ (built on @replaceRelay@ / @Data.OpenUnion@'s
-- @decomp@\/@weaken@) onto a real Rust-serviced effect (@AskUser@'s
-- @NoteWith@ constructor), whose handler body performs exactly one @send@
-- that must reach the driver as a real suspension.
--
-- Deliberately the smallest shape that reproduces the gap: one private
-- GADT, one @reinterpret@ (not @reinterpret2@ — the survey already ruled
-- out @reinterpret2@'s extra @Weakens@\/@replaceRelayN@ machinery as the
-- specific culprit, since plain @reinterpret@ fails identically), one
-- handler body that performs exactly one @send@. Modeled directly on
-- @Tidepool.Agent.Delegate@'s shape (private GADT, ordinary polymorphic
-- stdlib-shaped interpreter, @send@ of a real effect constructor from the
-- handler body) minus everything specific to Subagent\/Worktree.
module ReinterpretRepro (Ping (..), ping, runPing) where

import Control.Monad.Freer (Eff, Member, reinterpret, send)
import Tidepool.Effects (AskUser (..))

-- | The private, row-changing effect: never a @RowArgs@\/@EffectDecl@
-- entry, exactly like @Tidepool.Agent.Delegate.Delegate@.
data Ping a where
  PingReq :: Int -> Ping Int

ping :: Member Ping effs => Int -> Eff effs Int
ping = send . PingReq

-- | Lower 'Ping' onto the real @AskUser@ machinery. An ordinary, fully
-- polymorphic stdlib-shaped function — @effs@ is never named concretely,
-- exactly like @Tidepool.Agent.Delegate.runDelegate@ (no per-turn code
-- generation).
runPing :: forall effs a. Eff (Ping ': effs) a -> Eff (AskUser ': effs) a
runPing = reinterpret handlePing
  where
    handlePing :: forall x. Ping x -> Eff (AskUser ': effs) x
    handlePing (PingReq n) = do
      send (NoteWith "ping")
      pure n
