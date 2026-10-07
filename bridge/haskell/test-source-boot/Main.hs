module Main (main, tests) where

import Tidepool.Test.Runner (TestTree, runTests, testCase, testGroup, requiredInput)
import Test.Tasty (withResource)
import SourceBootCases
import BoundedReadTest (boundedReadChecks)
import SourceBootFixtureSupport (withTiming, withScratch)
import CandidateExecutionSourcesTest (candidateExecutionSourcesTest)
import ExecutionSourceDecodeTest (executionSourceDecodeChecks, executionSourceResolutionBudgetChecks)
import FinalizedCoreTest (finalizedCoreChecks, postloadProviderFrontendOnce, memoIngressSelectionHistory)
import ProgressBoundaryTest (progressBoundaryChecks, watchReplyEvidenceChecks, watchReplyWarmAuthorityChecks)
import PhysicalExecutableEpochTest (physicalExecutableEpoch)

main :: IO ()
main = runTests tests

tests :: TestTree
tests = testGroup "source-boot"
  [ testCase "bounded artifact reads" boundedReadChecks
  , testCase "physical native executable epoch" physicalExecutableEpoch
  , testCase "finalized Core" $ finalizedCoreChecks
  , testCase "finalized frontend once" $ finalizedFrontendOnce
  , testCase "post-load provider frontend once" postloadProviderFrontendOnce
  , testCase "memo ingress selection history" memoIngressSelectionHistory
  , testCase "execution source decode" $ executionSourceDecodeChecks
  , testCase "execution source resolution budget" $ executionSourceResolutionBudgetChecks
  , testCase "exact scope binders" $ exactScopeBinders
  , testCase "original constructor metadata closure" $ withEffects originalConstructorMetadataClosure
  , testCase "original package projection" $ originalPackageProjection
  , testCase "original product projection" $ originalProjectionProducts
  , testCase "native graph scheduling equality" nativeGraphSchedulingEquality
  , testCase "candidate compact inventory" $ candidateCompactInventory
  , testCase "candidate GHC load" $ candidateGhcLoad
  , testCase "SOURCE boot reuse" $ withTiming (withScratch sourceBootReuseAt)
  , testCase "candidate request site siblings" $ candidateRequestSitedSiblings
  , testCase "canonical current source" $ canonicalCurrentSource
  , testCase "completed program source imports" $ completedProgramSourceImports
  , testCase "completed program import pairing" $ completedProgramSourceImportPairing
  , testCase "canonical source obligations" $ canonicalSourceObligations
  , testCase "generated scaffold imports" $ generatedScaffoldImports
  , testCase "activation preview original orphan scope" $ activationPreviewOriginalOrphanScope
  , testCase "hydrated site siblings" $ hydratedSiteSiblings
  , testCase "fresh execution recipe" $ freshExecutionRecipeTest
  , testCase "candidate graph descriptors" $ withTiming (withScratch candidateGraphDescriptorsAt)
  , testCase "native checked signatures" $ nativeCheckedSignaturesTest
  , testCase "original home thin interface" originalHomeThinInterfaceTest
  , testCase "package-only thin interface" packageOnlyThinInterfaceTest
  , testCase "wired-in package thin interface" wiredInPackageThinInterfaceTest
  , testCase "host activation purpose" $ hostActivationPurposeTest Nothing
  , testCase "candidate execution sources" $ candidateExecutionSourcesTest
  , testCase "retained exact quoter" $ exactRetainedQuoter
  , testCase "retained execution publication" $ retainedExecutionPublication
  , testCase "retained execution TH counter" $ retainedExecutionThCounter
  , testCase "exact reexport quoter" $ exactReexportQuoter
  , withResource acquireExecutionInstanceFixture releaseExecutionInstanceFixture $ \fixture ->
      testGroup "exact instance execution"
        [ testCase "sealed and qualified controls" $ exactExecutionSealedInstance fixture
        , testCase "transitively imported orphan" $ exactExecutionTransitiveInstance fixture
        , testCase "unrelated provider isolation and explicit import" $ exactExecutionUnrelatedInstance fixture
        , testCase "class parent import and hiding" $ exactExecutionClassInstance fixture
        ]
  , testCase "exact checked value execution" $ exactExecutionValues
  , testCase "exact to ordinary transition" $ exactToOrdinary
  , testCase "checked value imports" $ checkedValueImports
  , testCase "loaded exact metadata" $ exactLoadedMetadata
  , testCase "exact transaction reuse" $ exactTransactionReuse
  , testCase "exact transaction cancellation" $ exactTransactionCancellation
  , testCase "package finder history isolation" packageFinderHistoryIsolation
  , testCase "lazy original home package instances survive context forks" lazyHomePackageInstances
  , testCase "exact transaction candidate reuse" $ exactTransactionCandidateReuse
  , testCase "exact legacy value isolation" $ exactLegacyValueIsolation
  , testCase "quasiquote codegen transition" $ quasiQuoteCodegenTransition
  , testCase "package inputs" $ withTiming packageInputs
  , testCase "session native body demand" sessionNativeBodyDemand
  , testCase "selected home instance edges" $ selectedHomeInstanceEdges
  , testCase "ordered resolution paths" $ resolutionPaths
  , testCase "original package cohort" $ withEffects $ \effects -> withScratch (originalPackageCohort effects)
  , testCase "checked value type closure" $ withEffects checkedValueTypeClosure
  , testCase "progress boundary" $ withEffects progressBoundaryChecks
  , testCase "watch reply evidence" $ withEffects $ \effects -> withScratch (watchReplyEvidenceChecks effects)
  , testCase "watch reply warm authority" $ withEffects $ \effects -> withScratch (watchReplyWarmAuthorityChecks effects)
  , testCase "exact bash metadata" $ withEffects exactBashMetadata
  , testCase "mixed independent 1" (withTiming (mixedGraph False 1))
  , testCase "mixed independent 10" (withTiming (mixedGraph False 10))
  , testCase "mixed independent 100" (withTiming (mixedGraph False 100))
  , testCase "mixed required independent 10" (withTiming (mixedGraph True 10))
  ]

withEffects :: (FilePath -> IO a) -> IO a
withEffects action = requiredInput "TIDEPOOL_TEST_EFFECTS_DIR" >>= action
