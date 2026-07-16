{-# LANGUAGE GADTs, DataKinds, TypeOperators, FlexibleContexts #-}
module Effects (module Effects, module Control.Monad.Freer) where

import Control.Monad.Freer

-- Hand-written, not generated: `tidepool_mcp::ensure_effects_module` emits
-- the STANDARD MCP eval-server stack (`Tidepool.Effects`: Console/KV/Fs/
-- Http/Exec/Lsp/Llm/Git/Time/Ask) as one fixed bundle — there is no
-- mechanism to generate just this demo's Console-with-Prompt/AwaitInt and
-- Rng effects. Both are intentionally custom (see `src/main.rs`'s module
-- doc comment), so nothing here duplicates a standard declaration by hand.

-- Console: emit a line, print a prompt, await an integer from stdin
data Console a where
  Emit     :: String -> Console ()
  Prompt   :: String -> Console ()
  AwaitInt :: Console Int

emit :: Member Console effs => String -> Eff effs ()
emit = send . Emit

prompt :: Member Console effs => String -> Eff effs ()
prompt = send . Prompt

awaitInt :: Member Console effs => Eff effs Int
awaitInt = send AwaitInt

-- Rng: generate random int in [lo, hi]
data Rng a where
  RandInt :: Int -> Int -> Rng Int

randInt :: Member Rng effs => Int -> Int -> Eff effs Int
randInt lo hi = send (RandInt lo hi)
