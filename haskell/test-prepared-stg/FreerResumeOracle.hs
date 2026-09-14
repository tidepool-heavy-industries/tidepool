{-# LANGUAGE GADTs #-}

-- | GHC-native oracle for the `E2` resume loop test
-- (`tidepool-runtime/tests/prepared_execution.rs`). Interprets `program`
-- (`FreerResume.hs`) with `Control.Monad.Freer`'s own `handleRelay`,
-- answering each `Ask n` with `n` itself — the same "answer = the request's
-- own argument" policy the Rust test drives through `resumeInt`, so this is
-- the real probe evaluated purely, not a hand-derived arithmetic restatement
-- of it. Run under the pinned GHC (`bash scripts/dev-shell.sh` puts the nix
-- `ghcEnv`, which carries `freer-simple`, on `PATH`):
--
--   cd haskell/test-prepared-stg && runghc -i. FreerResumeOracle.hs
--
-- `FreerResumeExpectations.json`'s value is transcribed from this program's
-- stdout, per the `BignumContractOracle.hs` precedent ("no value is
-- hand-derived").
module Main where

import Control.Monad.Freer (run)
import Control.Monad.Freer.Internal (handleRelay)
import FreerResume (Req (..), program)

main :: IO ()
main = print (run (handleRelay pure (\(Ask n) k -> k n) program))
