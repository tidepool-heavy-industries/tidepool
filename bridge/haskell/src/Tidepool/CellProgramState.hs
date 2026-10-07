module Tidepool.CellProgramState
  ( ProgramCellState(programPrologue, programExact, programValues, programOriginal, programOriginals, programRetained, programSourceImports)
  , initialProgramCellState, programItemOffset
  , recordDeclarationSegment, recordTypedSegment
  , programPlans, programObservations, programCaptureSignatures
  , programSources, programDeclarations, programTypedPlans
  ) where

import Data.Foldable (toList)
import qualified Data.Map.Strict as Map
import qualified Data.Sequence as Seq
import Data.Word (Word64)
import Tidepool.Binders
  ( SourcePrologue, CellSourcePlan(..), CellExpressionPlan )
import Tidepool.CheckedCell (CheckedSignature)
import Tidepool.CheckedPrefixImports (CompletedValueImport)
import Tidepool.ExactScope (ExactScope)
import Tidepool.ExecutionSchema (SymbolIdentity)
import Tidepool.GhcPipeline (ProgramSourceImports)
import Tidepool.TypedSegment (TypedSegmentPlan)

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
  , expressionChunks :: Seq.Seq [CellExpressionPlan]
  , signatureChunks :: Seq.Seq [CheckedSignature]
  , sourceHistory :: Seq.Seq String
  , declarationHistory :: Seq.Seq (Int,String)
  , typedPlanHistory :: Seq.Seq TypedSegmentPlan
  }

initialProgramCellState :: SourcePrologue -> ExactScope -> Map.Map SymbolIdentity Word64 -> ProgramCellState
initialProgramCellState prologue exact retained = ProgramCellState
  prologue exact [] Nothing [] retained Nothing 0
  Seq.empty Seq.empty Seq.empty Seq.empty Seq.empty Seq.empty

recordDeclarationSegment :: CellSourcePlan -> String -> String -> ProgramCellState -> ProgramCellState
recordDeclarationSegment plan source digest state = (recordPlan plan source state)
  { declarationHistory = declarationHistory state Seq.|> (programItemOffset state,digest) }

recordTypedSegment :: CellSourcePlan -> TypedSegmentPlan -> [CellExpressionPlan]
  -> [CheckedSignature] -> String -> ProgramCellState -> ProgramCellState
recordTypedSegment plan typed observations signatures source state = (recordPlan plan source state)
  { typedPlanHistory = typedPlanHistory state Seq.|> typed
  , expressionChunks = expressionChunks state Seq.|> observations
  , signatureChunks = signatureChunks state Seq.|> signatures }

programTypedPlans :: ProgramCellState -> [TypedSegmentPlan]
programTypedPlans = toList . typedPlanHistory

recordPlan :: CellSourcePlan -> String -> ProgramCellState -> ProgramCellState
recordPlan plan source state = state
  { programItemOffset = programItemOffset state + length (cellPlanItems plan)
  , planHistory = planHistory state Seq.|> plan
  , sourceHistory = sourceHistory state Seq.|> source }

programPlans :: ProgramCellState -> [CellSourcePlan]
programPlans = toList . planHistory

programObservations :: ProgramCellState -> [CellExpressionPlan]
programObservations = concat . toList . expressionChunks

programCaptureSignatures :: ProgramCellState -> [CheckedSignature]
programCaptureSignatures = concat . toList . signatureChunks

programSources :: ProgramCellState -> [String]
programSources = toList . sourceHistory

programDeclarations :: ProgramCellState -> [(Int,String)]
programDeclarations = toList . declarationHistory
