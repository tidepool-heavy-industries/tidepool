module Tidepool.CellProgramState
  ( ProgramCellState(programPrologue, programInput, programValues, programOriginal, programOriginals, programRetained, programSourceImports)
  , initialProgramCellState, programItemOffset, selectedProgramValues
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
import Tidepool.CheckedPrefixImports (CompletedValueImport(..))
import Tidepool.ExecutionSchema (SymbolIdentity)
import Tidepool.GhcPipeline (ProgramSourceImports)
import Tidepool.TypedSegment (TypedSegmentPlan)

-- The input basis stays with its actual compiler owner; inference histories
-- carry no ExactScope requirement. Observations accumulate in source order
-- without rebuilding the already checked prefix on every segment.
data ProgramCellState input = ProgramCellState
  { programPrologue :: SourcePrologue
  , programInput :: input
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

initialProgramCellState :: SourcePrologue -> input -> Map.Map SymbolIdentity Word64 -> ProgramCellState input
initialProgramCellState prologue input retained = ProgramCellState
  prologue input [] Nothing [] retained Nothing 0
  Seq.empty Seq.empty Seq.empty Seq.empty Seq.empty Seq.empty

recordDeclarationSegment :: CellSourcePlan -> String -> String -> ProgramCellState input -> ProgramCellState input
recordDeclarationSegment plan source digest state = (recordPlan plan source state)
  { declarationHistory = declarationHistory state Seq.|> (programItemOffset state,digest) }

recordTypedSegment :: CellSourcePlan -> TypedSegmentPlan -> [CellExpressionPlan]
  -> [CheckedSignature] -> String -> ProgramCellState input -> ProgramCellState input
recordTypedSegment plan typed observations signatures source state = (recordPlan plan source state)
  { typedPlanHistory = typedPlanHistory state Seq.|> typed
  , expressionChunks = expressionChunks state Seq.|> observations
  , signatureChunks = signatureChunks state Seq.|> signatures }

programTypedPlans :: ProgramCellState input -> [TypedSegmentPlan]
programTypedPlans = toList . typedPlanHistory

recordPlan :: CellSourcePlan -> String -> ProgramCellState input -> ProgramCellState input
recordPlan plan source state = state
  { programItemOffset = programItemOffset state + length (cellPlanItems plan)
  , planHistory = planHistory state Seq.|> plan
  , sourceHistory = sourceHistory state Seq.|> source }

programPlans :: ProgramCellState input -> [CellSourcePlan]
programPlans = toList . planHistory

programObservations :: ProgramCellState input -> [CellExpressionPlan]
programObservations = concat . toList . expressionChunks

programCaptureSignatures :: ProgramCellState input -> [CheckedSignature]
programCaptureSignatures = concat . toList . signatureChunks

programSources :: ProgramCellState input -> [String]
programSources = toList . sourceHistory

programDeclarations :: ProgramCellState input -> [(Int,String)]
programDeclarations = toList . declarationHistory

selectedProgramValues :: [CompletedValueImport] -> [CompletedValueImport]
selectedProgramValues values =
  [value { completedValueBinders = kept }
  | (index,value) <- zip [0::Int ..] values
  , let kept = [(name,identifier) | (name,identifier) <- completedValueBinders value
          , Map.lookup name winners == Just index]
  , not (null kept)]
  where winners = Map.fromList [(name,index) | (index,value) <- zip [0::Int ..] values
          , (name,_) <- completedValueBinders value]
