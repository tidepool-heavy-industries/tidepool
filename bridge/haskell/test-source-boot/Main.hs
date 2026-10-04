module Main (main, tests) where

import Tidepool.Test.Runner (TestTree, runTests, testCase, testGroup, requiredInput)
import SourceBootCases
import SourceBootFixtureSupport (withTiming, withScratch)
import CandidateExecutionSourcesTest (candidateExecutionSourcesTest)
import ExecutionSourceDecodeTest (executionSourceDecodeChecks, executionSourceResolutionBudgetChecks)
import FinalizedCoreTest (finalizedCoreChecks)
import ProgressBoundaryTest (progressBoundaryChecks, watchReplyEvidenceChecks, watchReplyWarmAuthorityChecks)

main :: IO ()
main = runTests tests

tests :: TestTree
tests = testGroup "source-boot"
  [ testCase "finalized Core" $ finalizedCoreChecks
  , testCase "finalized frontend once" $ finalizedFrontendOnce
  , testCase "execution source decode" $ executionSourceDecodeChecks
  , testCase "execution source resolution budget" $ executionSourceResolutionBudgetChecks
  , testCase "exact scope binders" $ exactScopeBinders
  , testCase "original package projection" $ originalPackageProjection
  , testCase "original product projection" $ originalProjectionProducts
  , testCase "candidate compact inventory" $ candidateCompactInventory
  , testCase "candidate GHC load" $ candidateGhcLoad
  , testCase "SOURCE boot reuse" $ withTiming (withScratch sourceBootReuseAt)
  , testCase "candidate sited siblings" $ candidateSitedSiblings
  , testCase "canonical current source" $ canonicalCurrentSource
  , testCase "canonical source obligations" $ canonicalSourceObligations
  , testCase "generated scaffold imports" $ generatedScaffoldImports
  , testCase "hydrated site siblings" $ hydratedSiteSiblings
  , testCase "fresh execution recipe" $ freshExecutionRecipeTest
  , testCase "candidate graph descriptors" $ withTiming (withScratch candidateGraphDescriptorsAt)
  , testCase "native checked signatures" $ nativeCheckedSignaturesTest
  , testCase "host activation purpose" $ hostActivationPurposeTest Nothing
  , testCase "candidate execution sources" $ candidateExecutionSourcesTest
  , testCase "retained exact quoter" $ exactRetainedQuoter
  , testCase "retained execution publication" $ retainedExecutionPublication
  , testCase "retained execution TH counter" $ retainedExecutionThCounter
  , testCase "exact reexport quoter" $ exactReexportQuoter
  , testCase "exact hidden instance execution" $ exactExecutionHiddenInstance
  , testCase "exact checked value execution" $ exactExecutionValues
  , testCase "exact to ordinary transition" $ exactToOrdinary
  , testCase "checked value imports" $ checkedValueImports
  , testCase "loaded exact metadata" $ exactLoadedMetadata
  , testCase "quasiquote codegen transition" $ quasiQuoteCodegenTransition
  , testCase "package inputs" $ withTiming packageInputs
  , testCase "selected home instance edges" $ selectedHomeInstanceEdges
  , testCase "ordered resolution paths" $ resolutionPaths
  , testCase "original package cohort" $ withEffects $ \effects -> withScratch (originalPackageCohort effects)
  , testCase "checked value type closure" $ withEffects checkedValueTypeClosure
  , testCase "progress boundary" $ withEffects progressBoundaryChecks
  , testCase "watch reply evidence" $ withEffects $ \effects -> withScratch (watchReplyEvidenceChecks effects)
  , testCase "watch reply warm authority" $ withEffects $ \effects -> withScratch (watchReplyWarmAuthorityChecks effects)
  , testCase "exact bash metadata" $ withEffects exactBashMetadata
  , testCase "mixed independent 1" (mixedGraph False 1)
  , testCase "mixed independent 10" (mixedGraph False 10)
  , testCase "mixed independent 100" (mixedGraph False 100)
  , testCase "mixed required independent 10" (mixedGraph True 10)
  ]

withEffects :: (FilePath -> IO a) -> IO a
withEffects action = requiredInput "TIDEPOOL_TEST_EFFECTS_DIR" >>= action
