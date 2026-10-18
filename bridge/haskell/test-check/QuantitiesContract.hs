module QuantitiesContract (quantitiesTests) where

import Control.Monad (unless)
import Data.List (isInfixOf)
import System.Directory (createDirectoryIfMissing)
import System.Exit (ExitCode (..))
import System.Process (readProcessWithExitCode)
import Tidepool.Test.Runner

data Expectation = Executed | Rejected [String]

checkFixture :: String -> Expectation -> IO ()
checkFixture fixture expectation = do
  support <- requiredInput "TIDEPOOL_TEST_EFFECTS_DIR"
  let output = "quantity-fixture-objects/" ++ fixture
      binary = output ++ "/fixture"
      mode = case expectation of Executed -> ["-O0", "-o", binary]; _ -> ["-fno-code"]
  createDirectoryIfMissing True output
  (status, out, err) <- readProcessWithExitCode "ghc"
    (mode ++ [ "-fforce-recomp", "-i" ++ support, "-ilib", "-iactors"
             , "-outputdir", output, "test-check/quantities/" ++ fixture ++ ".hs" ]) ""
  writeFile (output ++ "/compile.log") (out ++ err)
  case expectation of
    Rejected diagnostics -> unless
      (status /= ExitSuccess && all (`isInfixOf` (out ++ err)) diagnostics)
      (error (fixture ++ " did not refuse at the expected type boundary\n" ++ out ++ err))
    _ -> do
      unless (status == ExitSuccess) (error (fixture ++ " did not compile\n" ++ out ++ err))
      case expectation of
        Executed -> do
          (executed, stdout, stderr) <- readProcessWithExitCode binary [] ""
          writeFile (output ++ "/execution.log") (stdout ++ stderr)
          unless (executed == ExitSuccess) (error (fixture ++ " failed\n" ++ stdout ++ stderr))
        _ -> pure ()

quantitiesTests :: [TestTree]
quantitiesTests =
  [ testCase "spawn quantities and duration magnitude boundaries" $ checkFixture "Quantities" Executed
  , testCase "generated actor profile installs scope and sleep" $ checkFixture "DefaultScopeSleep" Executed
  , testCase "pure installed profile refuses resource scopes" $ checkFixture "NarrowScopeRefusal" (Rejected ["ResourceScopes", "is not a member of the type-level list"])
  , testCase "pure installed profile refuses sleep" $ checkFixture "NarrowSleepRefusal" (Rejected ["Sleep", "is not a member of the type-level list"])
  , testCase "command observation units cannot be swapped" $ checkFixture "SwappedCommandObservation" (Rejected ["Couldn't match expected type", "Cmd.Wait", "Int"])
  , testCase "same-family bounded choice captured launch and cleanup execute" $ checkFixture "FamilyOperations" Executed
  , testCase "model family cannot be changed by coercion" $ checkFixture "ModelFamilyCoerceRefusal" (Rejected ["OpenAI", "Argon", "coerce"])
  , testCase "checkpoint family cannot be changed by coercion" $ checkFixture "CheckpointFamilyCoerceRefusal" (Rejected ["OpenAI", "Argon", "coerce"])
  , testCase "bounded invocation requires its selected model family effect" $ checkFixture "ModelFamilyRowRefusal" (Rejected ["ModelCall", "OpenAI", "Argon"])
  , testCase "captured checkpoint cannot launch another model family" $ checkFixture "CapturedFamilyRefusal" (Rejected ["OpenAI", "Argon"])
  , testCase "fresh context requires spawn permission even with captured-context permission" $ checkFixture "FreshFamilyRowRefusal" (Rejected ["SpawnAgent", "OpenAI", "is not a member of the type-level list"])
  ]
