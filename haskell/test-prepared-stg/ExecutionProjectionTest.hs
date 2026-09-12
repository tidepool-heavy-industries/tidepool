{-# LANGUAGE OverloadedStrings #-}

module ExecutionProjectionTest (projectProjectionContract) where

import Control.Monad (unless)
import Data.Map.Strict qualified as Map
import Tidepool.ExecutionProjection
import Tidepool.ExecutionSchema
import Tidepool.PreparedStg (PreparedModule)

projectProjectionContract :: [PreparedModule] -> IO WireProgram
projectProjectionContract modules = case projectPrepared context modules of
  Left failure -> ioError (userError ("M3 projection failed: " <> show failure))
  Right program -> do
    unless (envelopeSchemaVersion (programEnvelope program) == schemaVersion)
      (ioError (userError "M3 projection used the wrong schema version"))
    unless (not (null (programBindings program)))
      (ioError (userError "M3 projection emitted no bindings"))
    unless (not (null (programGlobals program)))
      (ioError (userError "M3 projection omitted the imported package value"))
    case programGlobals program of
      imported : _ -> case projectPrepared
        (context { projectionRetainedGenerations = Map.singleton (globalIdentity imported) 7 }) modules of
          Left failure -> ioError (userError ("retained import projection failed: " <> show failure))
          Right retained -> unless
            (any ((== Just 7) . globalRequiredGeneration) (programGlobals retained))
            (ioError (userError "M3 projection omitted the retained import generation"))
      [] -> pure ()
    unless (any (or . constructorStrictFields) (programConstructors program))
      (ioError (userError "M3 projection omitted the strict constructor field"))
    unless (any groupIsRecursive (programBindings program)
      || any (groupAny (rhsIsRecursive . heapBindingRhs . topHeap)) (programBindings program))
      (ioError (userError "M3 projection omitted recursive control/data"))
    unless (programEntry program == selectedEntry program)
      (ioError (userError "M3 projection did not select the requested exact entry"))
    case projectPrepared context [] of
      Left (UnsupportedPreparedShape _) -> pure ()
      other -> ioError (userError ("empty program did not produce typed rejection: " <> show other))
    pure program
  where
    context = ProjectionContext "ghc-9.12-prepared-stg" "ghc-9.12.2"
      (TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []) Map.empty
      (SymbolIdentity "main" "M3Vertical" "value" "result")

    selectedEntry program = case
      [ heapBindingId binding
      | group <- programBindings program
      , TopBinding symbol binding <- groupItems group
      , symbolOccurrence symbol == "result"
      ] of
        [entry] -> entry
        entries -> error ("expected one result entry, got " <> show entries)
    groupItems (NonRecursive top) = [top]
    groupItems (Recursive tops) = tops

    topHeap (TopBinding _ binding) = binding
    groupIsRecursive Recursive{} = True
    groupIsRecursive _ = False
    groupAny predicate group = any predicate (groupItems group)
    rhsIsRecursive (Function _ _ _ body) = exprIsRecursive body
    rhsIsRecursive (Thunk _ _ _ body) = exprIsRecursive body
    rhsIsRecursive Constructor{} = False
    exprIsRecursive LetJoins{} = True
    exprIsRecursive (Let group body) = groupAny (rhsIsRecursive . heapBindingRhs) group
      || exprIsRecursive body
    exprIsRecursive (Case scrutinee _ _ alternatives) = exprIsRecursive scrutinee
      || any (\(Alternative _ _ body) -> exprIsRecursive body) alternatives
    exprIsRecursive _ = False
