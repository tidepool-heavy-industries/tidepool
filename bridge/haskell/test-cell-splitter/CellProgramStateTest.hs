module CellProgramStateTest (cellProgramStateChecks) where

import Control.Monad (unless)
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import Tidepool.Binders
import Tidepool.CellProgramState
import Tidepool.CheckedCell (CheckedSignature(..))
import Tidepool.CborEncode (encodeCellOut)
import Tidepool.ExactScope (ExactScope(..))

cellProgramStateChecks :: IO ()
cellProgramStateChecks = do
  let prologue = SourcePrologue [] []
      exact = ExactScope "" "" "" "" [] [] [] [] [] Nothing Nothing Nothing Nothing Nothing Set.empty
      initial = initialProgramCellState prologue exact Map.empty
      item index kind = CellAnalysisItem (CellSourceSpan index 1 index 2) (show index)
        (StmtBinders kind [] []) [] False
      plan items = CellSourcePlan prologue items [] "" "" [] ""
      firstPlan = plan [item 1 KBind,item 2 KExpr]
      declaration = plan [item 3 KDecl,item 4 KDecl]
      lastPlan = plan [item 5 KExpr]
      pin key = CheckedBinderPin key "Int" [] []
      expression key = CellExpressionPlan key ExpressionPure ExpressionRendered "Int" [] []
      signature key ty = CheckedSignature key ty []
      signatures1 = [signature "pin-0" "Int",signature "expr-1" "Bool"]
      signatures2 = [signature "expr-4" "Char",signature "pin-0" "Duplicate"]
      expressions1 = [expression "expr-1"]
      expressions2 = [expression "expr-4"]
      first = recordCheckedSegment firstPlan [pin "pin-0"] expressions1 signatures1 "checked-1" initial
      middle = recordDeclarationSegment declaration "declared" "receipt-2" first
      final = recordCheckedSegment lastPlan [pin "pin-4"] expressions2 signatures2 "checked-3" middle
      assert label ok = unless ok (fail ("cell accumulation: " ++ label))
  assert "item offsets count items rather than segments"
    (map programItemOffset [initial,first,middle,final] == [0,2,4,5])
  assert "declaration receipt uses its starting ordinal"
    (programDeclarations final == [(2,"receipt-2")])
  assert "plan and checked-source chronology"
    (programPlans final == [firstPlan,declaration,lastPlan]
      && concat (programSources final) == "checked-1declaredchecked-3")
  assert "native compilation sees current segment evidence and keeps duplicate keys"
    (programCheckedSignatures first == signatures1
      && programExpressions middle == expressions1
      && programCheckedSignatures final == signatures1 ++ signatures2
      && [signatureType value | value <- programCheckedSignatures final, signatureKey value == "pin-0"]
        == ["Int","Duplicate"])
  assert "observations preserve legacy list append bytes"
    (encodeCellOut (plan (cellPlanItems firstPlan ++ cellPlanItems declaration ++ cellPlanItems lastPlan))
      ([pin "pin-0"] ++ [pin "pin-4"]) (expressions1 ++ expressions2) "checked-1declaredchecked-3"
      == encodeCellOut (plan (concatMap cellPlanItems (programPlans final)))
        (programPins final) (programExpressions final) (concat (programSources final)))
  assert "native authority is unchanged by output accumulation"
    (programExact final == exact && programPrologue final == prologue
      && null (programValues final) && null (programOriginals final)
      && programOriginal final == Nothing && Map.null (programRetained final))
  assert "signature queries retain requested key order, duplicate evidence, and repeated keys"
    (signaturesFor ["expr-4","missing","pin-0","pin-0"] final
      == [signature "expr-4" "Char",signature "pin-0" "Int",signature "pin-0" "Duplicate"
         ,signature "pin-0" "Int",signature "pin-0" "Duplicate"]
      && null (signaturesFor [] final) && null (signaturesFor ["missing"] final))
  let duplicateExpression = (expression "expr-1") { expressionPlanType = "Bool" }
      ambiguous = recordCheckedSegment (plan []) [] [duplicateExpression] [] "" final
  assert "expression queries retain chronology and ambiguity regardless of requested key order"
    (expressionsFor ["expr-4","missing","expr-1","expr-1"] ambiguous
      == expressions1 ++ expressions2 ++ [duplicateExpression]
      && expressionsFor ["expr-1"] ambiguous == expressions1 ++ [duplicateExpression]
      && null (expressionsFor [] ambiguous) && null (expressionsFor ["missing"] ambiguous))
  putStrLn "cell accumulation: 8 checks passed"
