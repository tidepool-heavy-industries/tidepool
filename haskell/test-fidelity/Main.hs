-- | Extract-fidelity regressions: erasure symmetry, intrinsic-recognizer
-- qualification, and the unboxed-tuple result-arity contract, each driven
-- through the real 'runPipeline' + 'translateModuleClosed' path.
--
-- Run: @cabal run extract-fidelity-test@ (needs the nix with-packages GHC on
-- PATH; same toolchain as @session-c-test@). Exit code is non-zero unless
-- every check passes.
module Main (main) where

import Fidelity.Harness (Check)
import qualified Fidelity.Erasure as Erasure
import qualified Fidelity.PrimopArity as PrimopArity
import qualified Fidelity.Recognizers as Recognizers
import qualified Fidelity.D1Defense as D1Defense
import qualified Fidelity.TurnBatch as TurnBatch

import Control.Monad (forM_)
import System.Exit (exitFailure, exitSuccess)

groups :: [(String, IO [Check])]
groups =
  [ ("coercion-binder erasure", Erasure.checks)
  , ("intrinsic recognizers",   Recognizers.checks)
  , ("pipeline barriers + unboxed-tuple arity", PrimopArity.checks)
  , ("D1 hard-fail metadata-subset defense (mutation test)", D1Defense.checks)
  , ("turn-batch per-item error attribution (§8)", TurnBatch.checks)
  ]

main :: IO ()
main = do
  results <- mapM (\(name, run) -> (,) name <$> run) groups
  forM_ results $ \(name, cs) -> do
    putStrLn $ "\n== " ++ name ++ " =="
    if null cs
      then putStrLn "  EMPTY — group contributes no checks"
      else forM_ cs $ \(label, ok) ->
             putStrLn $ "  " ++ (if ok then "PASS  " else "FAIL  ") ++ label
  let flat = concatMap snd results
  putStrLn $ "\n" ++ show (length (filter snd flat)) ++ "/" ++ show (length flat)
             ++ " checks passed"
  if all (not . null . snd) results && all snd flat then exitSuccess else exitFailure
