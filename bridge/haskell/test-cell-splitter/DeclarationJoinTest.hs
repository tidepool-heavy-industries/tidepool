module Main (main, tests) where

import Control.Monad (unless)
import Test.QuickCheck qualified as QC
import Tidepool.Test.Runner (TestTree, runTests, testCase, testGroup)
import DeclarationJoinCases
import Tidepool.ExecutionSchema qualified as Execution

main :: IO ()
main = runTests tests

tests :: TestTree
tests = testGroup "declaration-join"
  [ testCase "indexed declarations match generated histories" declarationIndexProperty
  , testCase "indexed declarations match exhaustive bounded histories" declarationIndexHistories
  , testCase "current typed declaration request roundtrip" wireRoundTripChecks
  , testCase "original package roots" originalProductRootsProof
  , testCase "evaluated original package roots control" $
      originalProductRootsProofWith (\global ->
        (Execution.globalIdentity global, Execution.globalRequiredEvaluated global))
  , testCase "original interfaces fresh consumers and retained family conflicts" declarationJoinScenario
  ]

-- Compile the original GHC identities once, then generate and shrink the
-- selection history independently from the indexed production implementation.
declarationIndexProperty :: IO ()
declarationIndexProperty = withDeclarationIndexOracle $ \classCount familyCount compareHistory -> do
  let generator = (,,)
        <$> QC.listOf (QC.chooseInt (0, classCount - 1))
        <*> QC.sublistOf [0 .. familyCount - 1]
        <*> QC.arbitrary
      shrink (operations, families, visible) =
        [(smaller, families, visible) | smaller <- QC.shrinkList (const []) operations]
        ++ [(operations, smaller, visible) | smaller <- QC.shrinkList (const []) families]
        ++ [(operations, families, False) | visible]
      property = QC.forAllShrink generator shrink $ \(operations, families, visible) ->
        case compareHistory operations families visible of
          Left diagnostic -> QC.counterexample diagnostic False
          Right categories -> QC.checkCoverage $
            QC.cover 10 ("accepted" `elem` categories) "accepted" $
            QC.cover 5 ("ClassInstanceConflict" `elem` categories) "class conflict" $
            QC.cover 5 ("FamilyInstanceConflict" `elem` categories) "family conflict" $
            QC.cover 20 (not visible) "hidden orphan instances" $
            QC.cover 20 visible "visible orphan instances" True
  result <- QC.quickCheckWithResult QC.stdArgs { QC.maxSuccess = 256, QC.maxSize = 18 } property
  unless (QC.isSuccess result) (fail "declaration-index history property failed")
