module QuantitiesContract (quantitiesTests) where

import Control.Monad (unless)
import Data.List (isInfixOf)
import System.Directory (createDirectoryIfMissing)
import System.Exit (ExitCode (..))
import System.Process (readProcessWithExitCode)
import Tidepool.Test.Runner

data Expectation = Executed | Rejected String

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
    Rejected effect -> unless
      (status /= ExitSuccess && effect `isInfixOf` (out ++ err)
       && "is not a member of the type-level list" `isInfixOf` (out ++ err))
      (error (fixture ++ " did not refuse at the effect boundary\n" ++ out ++ err))
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
  , testCase "pure installed profile refuses resource scopes" $ checkFixture "NarrowScopeRefusal" (Rejected "ResourceScopes")
  , testCase "pure installed profile refuses sleep" $ checkFixture "NarrowSleepRefusal" (Rejected "Sleep")
  ]
