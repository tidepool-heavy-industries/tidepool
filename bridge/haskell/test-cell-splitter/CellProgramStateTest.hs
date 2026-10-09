module CellProgramStateTest (cellProgramStateChecks) where

import Control.Monad (unless)
import qualified Data.ByteString as BS
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import Tidepool.Binders
import Tidepool.TurnSource (emptyCompilerDefaultRecipe)
import Tidepool.CellProgramState
import Tidepool.CheckedCell (CheckedSignature(..))
import Tidepool.ExactScope (ExactScope(..), emptyScopeInputs, ExactScopePurpose(..))
import Tidepool.TypedSegment
  ( TypedItemPlan(..), TypedItemBody(..), typedSegmentPlan )

-- Component accumulation only: these inert signature bytes are never given
-- to a compiler decoder or used to issue capture/execution authority.
cellProgramStateChecks :: IO ()
cellProgramStateChecks = do
  let prologue = SourcePrologue [] [] emptyCompilerDefaultRecipe
      exact = ExactScope "" "" "" "" emptyScopeInputs [] [] [] []
        NoCheckedPurpose Nothing Set.empty Set.empty
      initial = initialProgramCellState prologue exact Map.empty
      item index kind names form = CellAnalysisItem (CellSourceSpan index 1 index 2) (show index)
        (StmtBinders kind names []) [] False form
      plan items = CellSourcePlan prologue items [] "" "" [] ""
      firstPlan = plan [item 1 KBind ["value"] (Just ActionBinding), item 2 KExpr [] Nothing]
      declaration = plan [item 3 KDecl [] Nothing, item 4 KDecl [] Nothing]
      lastPlan = plan [item 5 KExpr [] Nothing]
      expression key ty lift = CellExpressionPlan key lift ty []
      signature key ty = CheckedSignature key ty (BS.singleton 0) []
      signatures1 = [signature "entry0:value" "Int", signature "entry1:observation1" "() -> Bool"]
      signatures2 = [signature "entry4:observation4" "() -> Char"]
      observations1 = [expression "entry1" "Bool" ExpressionPure]
      observations2 = [expression "entry4" "Char" ExpressionEffectful]
      assert label ok = unless ok (fail ("cell accumulation: " ++ label))
  typedFirst <- either (fail . show) pure $ typedSegmentPlan (replicate 64 'a') "rootFirst"
    [ TypedItemPlan 0 "entry0" 1 (ActionItem "step0" "probe0" "marker0" ["value"])
    , TypedItemPlan 1 "entry1" 2 (ObservationItem "probe1" "observation1") ]
  typedLast <- either (fail . show) pure $ typedSegmentPlan (replicate 64 'b') "rootLast"
    [TypedItemPlan 4 "entry4" 3 (ObservationItem "probe4" "observation4")]
  let first = recordTypedSegment firstPlan typedFirst observations1 signatures1 "typed-1" initial
      middle = recordDeclarationSegment declaration "declared" "receipt-2" first
      final = recordTypedSegment lastPlan typedLast observations2 signatures2 "typed-3" middle
  assert "initial histories are empty"
    (null (programTypedPlans initial) && null (programObservations initial)
      && null (programCaptureSignatures initial))
  assert "item offsets count lexical items rather than segments"
    (map programItemOffset [initial,first,middle,final] == [0,2,4,5])
  assert "declaration receipt uses its starting ordinal"
    (programDeclarations final == [(2,"receipt-2")])
  assert "source plans and rendered source retain chronology"
    (programPlans final == [firstPlan,declaration,lastPlan]
      && concat (programSources final) == "typed-1declaredtyped-3")
  assert "typed reservations survive the declaration boundary in original order"
    (programTypedPlans first == [typedFirst] && programTypedPlans middle == [typedFirst]
      && programTypedPlans final == [typedFirst,typedLast])
  assert "actual capture-signature keys and observation chunks retain chronology"
    (programCaptureSignatures final == signatures1 ++ signatures2
      && programObservations final == observations1 ++ observations2)
  assert "adding a later segment leaves prior snapshots unchanged"
    (programCaptureSignatures first == signatures1 && programCaptureSignatures middle == signatures1
      && programObservations first == observations1 && programObservations middle == observations1)
  assert "accumulation does not modify native authority"
    (programExact final == exact && programPrologue final == prologue
      && null (programValues final) && null (programOriginals final)
      && programOriginal final == Nothing && Map.null (programRetained final)
      && programSourceImports final == Nothing)
  putStrLn "cell accumulation: 8 checks passed"
