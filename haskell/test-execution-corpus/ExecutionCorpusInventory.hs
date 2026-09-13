{-# LANGUAGE LambdaCase #-}

-- | Non-wire diagnostics for the prepared execution corpus.  This inventory
-- observes recovered STG; it neither projects operations nor defines which
-- identities an execution backend should support.
module ExecutionCorpusInventory
  ( TargetInventory
  , inventoryRecoveredTarget
  , unavailableTargetInventory
  , renderTargetInventories
  , renderPreparedFactsForTest
  ) where

import Data.List (intercalate)
import Data.Map.Strict (Map)
import Data.Map.Strict qualified as Map
import Data.Set (Set)
import Data.Set qualified as Set
import Data.Text qualified as Text
import GHC.Builtin.PrimOps (primOpOcc)
import GHC.Data.FastString (unpackFS)
import GHC.Stg.Syntax
import GHC.Stg.Pipeline (StgCgInfos)
import GHC.Types.Literal (Literal(..))
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.Unique (Unique)
import GHC.Types.Unique.FM (UniqFM, listToUFM, lookupUFM)
import GHC.Types.Unique.Set
  ( UniqSet, mkUniqSet, nonDetEltsUniqSet )
import GHC.Types.Var (Id, varUnique)
import GHC.Unit.Module (Module, moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Types (unitString)
import GHC.Utils.Outputable (ppr, showSDocUnsafe)

import Tidepool.ExecutionIR (topBindingReferences)
import Tidepool.ExecutionProjection
  ( ProjectionContext, preparedTopIdentities )
import Tidepool.ExecutionSchema (SymbolIdentity(..))
import Tidepool.Json (jsonString)
import Tidepool.PreparedFacts
  ( PreparedFacts(..), PreparedOperation(..), PreparedRepresentation(..)
  , extractPreparedFacts )
import Tidepool.PreparedRecovery
  ( RecoveredClosure(..), RecoveryFailure )
import Tidepool.PreparedStg (PreparedModule(..))

data TargetInventory = TargetInventory
  { targetName :: String
  , targetOperations :: [OperationOccurrence]
  , targetLabels :: [LabelOccurrence]
  , targetResiduals :: [RecoveryFailure]
  , targetDiagnosticFailure :: Maybe String
  }

data OperationOccurrence = OperationOccurrence
  { operationModule :: String
  , operationKind :: String
  , operationIdentity :: String
  , operationArguments :: [PreparedRepresentation]
  , operationResults :: PreparedRepresentation
  }

data LabelOccurrence = LabelOccurrence
  { labelModule :: String
  , labelIdentity :: String
  }

inventoryRecoveredTarget
  :: ProjectionContext -> SymbolIdentity -> RecoveredClosure -> TargetInventory
inventoryRecoveredTarget _context target closure =
  case reachableBindings target (closureModules closure) of
    Left failure -> unavailableTargetInventory (renderSymbol target)
      (closureFailures closure) failure
    Right selected ->
      let facts =
            [ (modul, extractPreparedFacts modul tags bindings)
            | (modul, tags, bindings) <- selected
            ]
          operations = concatMap (uncurry operationOccurrences) facts
          labels = concatMap (uncurry labelOccurrences) facts
      in TargetInventory (renderSymbol target) operations labels
           (closureFailures closure) Nothing

unavailableTargetInventory :: String -> [RecoveryFailure] -> String -> TargetInventory
unavailableTargetInventory name residuals failure =
  TargetInventory name [] [] residuals (Just failure)

-- | Test seam for a synthetic structured-STG walk.  The input facts must have
-- been produced by 'extractPreparedFacts'; rendering shares the corpus path.
renderPreparedFactsForTest :: Module -> PreparedFacts -> String
renderPreparedFactsForTest modul facts = renderTargetInventories
  [ TargetInventory "inventory-self-test"
      (operationOccurrences modul facts) (labelOccurrences modul facts) [] Nothing
  ]

renderTargetInventories :: [TargetInventory] -> String
renderTargetInventories targets =
  "{\"version\":1,\"targets\":[" <> intercalate "," (map renderTarget targets) <> "]}"

renderTarget :: TargetInventory -> String
renderTarget target = "{\"name\":" <> jsonString (targetName target)
  <> ",\"operations\":[" <> intercalate "," (map renderOperation (targetOperations target))
  <> "],\"labels\":[" <> intercalate "," (map renderLabel (targetLabels target))
  <> "],\"recovery_failures\":["
  <> intercalate "," (map (jsonString . show) (targetResiduals target)) <> "]"
  <> ",\"diagnostic_failure\":"
  <> maybe "null" jsonString (targetDiagnosticFailure target) <> "}"

renderOperation :: OperationOccurrence -> String
renderOperation operation = "{\"module\":" <> jsonString (operationModule operation)
  <> ",\"kind\":" <> jsonString (operationKind operation)
  <> ",\"identity\":" <> jsonString (operationIdentity operation)
  <> ",\"arguments\":["
  <> intercalate "," (map renderRepresentation (operationArguments operation)) <> "]"
  <> ",\"results\":" <> renderRepresentation (operationResults operation) <> "}"

renderRepresentation :: PreparedRepresentation -> String
renderRepresentation = \case
  PreparedKnownReps reps -> "{\"status\":\"known\",\"reps\":["
    <> intercalate "," (map (jsonString . show) reps) <> "]}"
  PreparedRuntimePolymorphic ty ->
    "{\"status\":\"unavailable\",\"reason\":\"runtime-polymorphic\",\"type\":"
      <> jsonString (showSDocUnsafe (ppr ty)) <> "}"

renderLabel :: LabelOccurrence -> String
renderLabel label = "{\"module\":" <> jsonString (labelModule label)
  <> ",\"identity\":" <> jsonString (labelIdentity label) <> "}"

operationOccurrences :: Module -> PreparedFacts -> [OperationOccurrence]
operationOccurrences modul facts =
  [ let (kind, identity) = exactOperation op
    in OperationOccurrence (renderModule modul) kind identity arguments results
  | PreparedOperation op arguments results <- preparedOperations facts
  ]

labelOccurrences :: Module -> PreparedFacts -> [LabelOccurrence]
labelOccurrences modul facts =
  [ LabelOccurrence (renderModule modul) (unpackFS label)
  | LitLabel label _ <- preparedLiterals facts
  ]

exactOperation :: StgOp -> (String, String)
exactOperation (StgPrimOp op) = ("primop", occNameString (primOpOcc op))
exactOperation (StgPrimCallOp call) = ("primcall", showSDocUnsafe (ppr call))
exactOperation (StgFCallOp call _) = ("foreign", showSDocUnsafe (ppr call))

renderModule :: Module -> String
renderModule modul = unitString (moduleUnit modul) <> ":"
  <> moduleNameString (moduleName modul)

renderSymbol :: SymbolIdentity -> String
renderSymbol identity = intercalate ":" $ case symbolRecordParent identity of
  Nothing -> components
  Just parent -> take 3 components <> [Text.unpack parent, last components]
 where
  components =
    [ Text.unpack (symbolUnit identity)
    , Text.unpack (symbolModule identity)
    , Text.unpack (symbolNamespace identity)
    , Text.unpack (symbolOccurrence identity)
    ]

reachableBindings
  :: SymbolIdentity -> [PreparedModule]
  -> Either String [(Module, StgCgInfos, [CgStgTopBinding])]
reachableBindings target modules = do
  identities <- either (Left . show) Right (preparedTopIdentities modules)
  let allBindings =
        [ (pmModule prepared, pmTagSigs prepared, binding)
        | prepared <- modules
        , (binding, _) <- pmBindings prepared
        ]
      binders = concatMap (topBinders . third) allBindings
  if length binders /= length identities
    then Left "prepared identity/binder cardinality mismatch"
    else do
      let identityByUnique = listToUFM (zip (map varUnique binders) identities)
          topLevel = mkUniqSet (map varUnique binders)
          dependencies = Map.fromListWith Set.union
            [ (identity, referencedTopIdentities identityByUnique modul topLevel single)
            | (modul, _, binding) <- allBindings
            , (binder, single) <- individualTops binding
            , Just identity <- [lookupUFM identityByUnique (varUnique binder)]
            ]
          reachable = close dependencies Set.empty [target]
          selected =
            [ (pmModule prepared, pmTagSigs prepared,
                [ binding
                | (binding, _) <- pmBindings prepared
                , any (binderIsReachable identityByUnique reachable) (topBinders binding)
                ])
            | prepared <- modules
            ]
      if target `Set.member` Set.fromList identities
        then Right [row | row@(_, _, bindings') <- selected, not (null bindings')]
        else Left "target identity is absent from recovered closure"
 where
  third (_, _, value) = value

referencedTopIdentities
  :: UniqFM Unique SymbolIdentity -> Module -> UniqSet Unique -> CgStgTopBinding
  -> Set SymbolIdentity
referencedTopIdentities identities modul topLevel binding = Set.fromList
  [ identity
  | unique <- nonDetEltsUniqSet (topBindingReferences modul topLevel binding)
  , Just identity <- [lookupUFM identities unique]
  ]

binderIsReachable
  :: UniqFM Unique SymbolIdentity -> Set SymbolIdentity -> Id -> Bool
binderIsReachable identities reachable binder =
  maybe False (`Set.member` reachable) (lookupUFM identities (varUnique binder))

close
  :: Map SymbolIdentity (Set SymbolIdentity)
  -> Set SymbolIdentity -> [SymbolIdentity] -> Set SymbolIdentity
close _ visited [] = visited
close dependencies visited (identity : pending)
  | identity `Set.member` visited = close dependencies visited pending
  | otherwise = close dependencies (Set.insert identity visited)
      (Set.toList (Map.findWithDefault Set.empty identity dependencies) <> pending)

topBinders :: CgStgTopBinding -> [Id]
topBinders (StgTopStringLit binder _) = [binder]
topBinders (StgTopLifted (StgNonRec binder _)) = [binder]
topBinders (StgTopLifted (StgRec pairs)) = map fst pairs

individualTops :: CgStgTopBinding -> [(Id, CgStgTopBinding)]
individualTops binding@(StgTopStringLit binder _) = [(binder, binding)]
individualTops binding@(StgTopLifted (StgNonRec binder _)) = [(binder, binding)]
individualTops (StgTopLifted (StgRec pairs)) =
  [ (binder, StgTopLifted (StgNonRec binder rhs)) | (binder, rhs) <- pairs ]
