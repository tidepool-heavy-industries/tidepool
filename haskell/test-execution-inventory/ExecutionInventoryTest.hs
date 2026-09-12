module Main (main) where

import Data.List (isInfixOf)
import Data.Set qualified as Set
import Tidepool.ExecutionIR

main :: IO ()
main = do
  let local = ExactName "tidepool-test" "Fixture" "loop"
      text = ExactName "text-2.1" "Data.Text.Internal" "Text"
      integer = ExactName "ghc-bignum-1.3" "GHC.Num.Integer" "Integer"
      evidence = PreparedInventory
        { inventoryModule = ExactName "tidepool-test" "Fixture" "<module>"
        , inventoryDependencies = Set.fromList
            [ Dependency RecursiveDependency local
            , Dependency LibraryDependency text
            , Dependency LibraryDependency integer
            ]
        , inventoryLiterals = Set.singleton (StringLiteral [97, 0, 98] True)
        , inventoryRuntimeForms = Set.fromList
            [ JoinBinding, VoidArgument, UnboxedTuple, UnboxedSum
            , UpdateForm SingleEntryUpdate
            ]
        , inventoryFacts = Set.fromList
            [ ImportedValue text
            , ClosureCaptures local [text]
            , ConstructorLayout local ["LiftedRep", "IntRep"]
            , OperationSignature "foreign:test" ["AddrRep"] ["IntRep"]
            , TagInferenceCount 2
            ]
        }
      rendered = renderPreparedInventory evidence
      expectedFragments =
        [ "module tidepool-test:Fixture:<module>"
        , "RecursiveDependency(tidepool-test:Fixture:loop)"
        , "LibraryDependency(text-2.1:Data.Text.Internal:Text)"
        , "LibraryDependency(ghc-bignum-1.3:GHC.Num.Integer:Integer)"
        , "StringLiteral [97,0,98] True"
        , "JoinBinding", "VoidArgument", "UnboxedTuple", "UnboxedSum"
        , "ImportedValue (ExactName {exactUnit = \"text-2.1\""
        , "OperationSignature \"foreign:test\" [\"AddrRep\"] [\"IntRep\"]"
        ]
  mapM_ (assertContains rendered) expectedFragments
  if rendered == renderPreparedInventory evidence
    then pure ()
    else fail "inventory rendering was not deterministic"

assertContains :: String -> String -> IO ()
assertContains haystack needle
  | needle `isInfixOf` haystack = pure ()
  | otherwise = fail ("missing inventory evidence: " <> needle)
