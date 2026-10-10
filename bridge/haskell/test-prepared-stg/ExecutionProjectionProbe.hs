module Main (main, tests) where

import Tidepool.Test.Runner (TestTree, runTests, testCase, testGroup)

import CallerResultProjectionTest (verifyCallerResultProjection)
import DeferredFunctionProjectionTest (verifyDeferredFunctionProjection)
import ExecutionProjectionTest
  ( projectProjectionContract, verifyRetainedImportProjection, verifyUnboxedSumJoinProjection
  , verifyTimeEitherShadow, verifyPreparedTime )
import ModuleEvidenceProjectionTest (verifyModuleEvidenceProjection)
import Tidepool.GhcPipeline
  ( PipelineSelection(PreparedStg), pprModules, runPipelineSelected )
import System.Directory (getCurrentDirectory)
import System.FilePath ((</>))

main :: IO ()
main = runTests tests

tests :: TestTree
tests = testGroup "execution-schema-projection"
  [ testCase "Time exact installed dependency owners" verifyPreparedTime
  , testCase "Time rejects home Either behind authenticated source" verifyTimeEitherShadow
  , testCase "unarised sum joins preserve literal signedness" verifyUnboxedSumJoinProjection
  , testCase "caller chosen result" verifyCallerResultProjection
  , testCase "module evidence" verifyModuleEvidenceProjection
  , testCase "deferred function" verifyDeferredFunctionProjection
  , testCase "retained import" verifyRetainedImportProjection
  , testCase "compiled M3 projection contract" compiledProjectionContract
  ]

compiledProjectionContract :: IO ()
compiledProjectionContract = do
  root <- getCurrentDirectory
  let fixtureDir = root </> "test-prepared-stg"
  result <- runPipelineSelected PreparedStg
    (fixtureDir </> "M3Vertical.hs") [fixtureDir]
  _ <- projectProjectionContract (pprModules result)
  pure ()
