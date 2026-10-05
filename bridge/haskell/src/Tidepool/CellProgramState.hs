module Tidepool.CellProgramState
  ( ProgramCellState(programPrologue, programExact, programValues, programOriginal, programOriginals, programRetained, programSourceImports)
  , initialProgramCellState, programItemOffset
  , recordDeclarationSegment, recordCheckedSegment
  , programPlans, programPins, programExpressions, programCheckedSignatures
  , programSources, programDeclarations, signaturesFor, expressionsFor
  ) where

import Data.Foldable (toList)
import qualified Data.Map.Strict as Map
import qualified Data.Sequence as Seq
import Data.Word (Word64)
import Tidepool.Binders
  ( SourcePrologue, CellSourcePlan(..), CheckedBinderPin, CellExpressionPlan(expressionPlanKey) )
import Tidepool.CheckedCell (CheckedSignature(signatureKey))
import Tidepool.CheckedPrefixImports (CompletedValueImport)
import Tidepool.ExactScope (ExactScope)
import Tidepool.ExecutionSchema (SymbolIdentity)
import Tidepool.GhcPipeline (ProgramSourceImports)

-- Active authority is updated by compilation; observations accumulate in order
-- without rebuilding the already checked prefix on every segment.
data ProgramCellState = ProgramCellState
  { programPrologue :: SourcePrologue
  , programExact :: ExactScope
  , programValues :: [CompletedValueImport]
  , programOriginal :: Maybe ((String,String),String)
  , programOriginals :: [((String,String),String)]
  , programRetained :: Map.Map SymbolIdentity Word64
  , programSourceImports :: Maybe ProgramSourceImports
  , programItemOffset :: !Int
  , planHistory :: Seq.Seq CellSourcePlan
  , pinChunks :: Seq.Seq [CheckedBinderPin]
  , expressionChunks :: Seq.Seq [CellExpressionPlan]
  , signatureChunks :: Seq.Seq [CheckedSignature]
  , sourceHistory :: Seq.Seq String
  , declarationHistory :: Seq.Seq (Int,String)
  }

initialProgramCellState :: SourcePrologue -> ExactScope -> Map.Map SymbolIdentity Word64 -> ProgramCellState
initialProgramCellState prologue exact retained = ProgramCellState
  prologue exact [] Nothing [] retained Nothing 0
  Seq.empty Seq.empty Seq.empty Seq.empty Seq.empty Seq.empty

recordDeclarationSegment :: CellSourcePlan -> String -> String -> ProgramCellState -> ProgramCellState
recordDeclarationSegment plan source digest state = (recordPlan plan source state)
  { declarationHistory = declarationHistory state Seq.|> (programItemOffset state,digest) }

-- Evidence is visible before native items in this segment are compiled. Keep
-- duplicate keys and signature order: admission filtering owns their meaning.
recordCheckedSegment :: CellSourcePlan -> [CheckedBinderPin] -> [CellExpressionPlan]
  -> [CheckedSignature] -> String -> ProgramCellState -> ProgramCellState
recordCheckedSegment plan pins expressions signatures source state = (recordPlan plan source state)
  { pinChunks = pinChunks state Seq.|> pins
  , expressionChunks = expressionChunks state Seq.|> expressions
  , signatureChunks = signatureChunks state Seq.|> signatures }

recordPlan :: CellSourcePlan -> String -> ProgramCellState -> ProgramCellState
recordPlan plan source state = state
  { programItemOffset = programItemOffset state + length (cellPlanItems plan)
  , planHistory = planHistory state Seq.|> plan
  , sourceHistory = sourceHistory state Seq.|> source }

programPlans :: ProgramCellState -> [CellSourcePlan]
programPlans = toList . planHistory

programPins :: ProgramCellState -> [CheckedBinderPin]
programPins = concat . toList . pinChunks

programExpressions :: ProgramCellState -> [CellExpressionPlan]
programExpressions = concat . toList . expressionChunks

programCheckedSignatures :: ProgramCellState -> [CheckedSignature]
programCheckedSignatures = concat . toList . signatureChunks

programSources :: ProgramCellState -> [String]
programSources = toList . sourceHistory

programDeclarations :: ProgramCellState -> [(Int,String)]
programDeclarations = toList . declarationHistory

-- Signature requests follow binder-key order, including repeated requested keys.
-- Expression evidence instead follows checked chronology, even for ambiguous keys.
signaturesFor :: [String] -> ProgramCellState -> [CheckedSignature]
signaturesFor keys state = concatMap
  (\key -> matchingChunks ((== key) . signatureKey) (signatureChunks state)) keys

expressionsFor :: [String] -> ProgramCellState -> [CellExpressionPlan]
expressionsFor keys state = matchingChunks
  ((`elem` keys) . expressionPlanKey) (expressionChunks state)

-- Traverse the chunk owner directly, allocating only the matching result spine.
matchingChunks :: (a -> Bool) -> Seq.Seq [a] -> [a]
matchingChunks predicate = foldr
  (\chunk rest -> foldr (\value values -> if predicate value then value : values else values) rest chunk) []
