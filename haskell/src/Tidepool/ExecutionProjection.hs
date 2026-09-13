module Tidepool.ExecutionProjection
  ( ProjectionContext(..)
  , ProjectionError(..)
  , projectPrepared
  , projectPreparedTarget
  , preparedTopIdentities
  , projectLiteralAtomForTest
  , assignTopIdentitySpellings
  ) where

import Control.Monad (foldM, forM)
import Control.Monad.State.Strict
import Data.Bits (shiftR)
import Data.ByteString qualified as BS
import Data.List (find)
import Data.Maybe (listToMaybe)
import Data.Map.Strict (Map)
import Data.Map.Strict qualified as Map
import Data.Set (Set)
import Data.Set qualified as Set
import Data.Text (Text)
import Data.Text qualified as Text
import Data.Word (Word32, Word64, Word8)
import GHC.Builtin.PrimOps (primOpOcc)
import GHC.Core (AltCon(..))
import GHC.Core.DataCon
  ( DataCon, dataConName, dataConRepArgTys, dataConWorkId
  , dataConTag, dataConTyCon, dataConOrigResTy, isMarkedStrict, isUnboxedTupleDataCon )
import GHC.Core.TyCo.Rep (Scaled(..), Type(..))
import GHC.Core.TyCon qualified as GHC
import GHC.Float (castDoubleToWord64, castFloatToWord32)
import GHC.Stg.Syntax
import GHC.Stg.Syntax qualified as Stg
import GHC.StgToCmm.Closure (importedIdLFInfo)
import GHC.StgToCmm.Types (LambdaFormInfo(..))
import GHC.Types.Literal (LitNumType(..), Literal(..), literalType)
import GHC.Types.Id (isDeadEndId)
import GHC.Types.Name (Name, isExternalName, nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.RepType
  (typePrimRep_maybe, runtimeRepPrimRep_maybe, dataConRuntimeRepStrictness, unwrapType)
import GHC.Types.Unique.Set (mkUniqSet, nonDetEltsUniqSet)
import GHC.Types.Unique (Unique)
import GHC.Types.Unique.FM (UniqFM, listToUFM, lookupUFM)
import GHC.Types.Var (Id, varName, varType, varUnique)
import GHC.Types.Var.Env (VarEnv, emptyVarEnv, extendVarEnv, lookupVarEnv)
import GHC.Types.Var.Set (dVarSetElems)
import GHC.Unit.Module (moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Types (Module, unitString)
import Tidepool.ExecutionIR (topBindingReferences)
import Tidepool.ExecutionSchema
import Tidepool.ExecutionSchema qualified as Schema
import Tidepool.Identity (varId)
import Tidepool.PreparedStg (PreparedModule(..))

data ProjectionContext = ProjectionContext
  { projectionProfile :: Text
  , projectionToolchain :: Text
  , projectionTarget :: TargetDescriptor
  , projectionRetainedGenerations :: Map SymbolIdentity Word64
  , projectionEntry :: SymbolIdentity
  } deriving stock (Eq, Show)

data ProjectionError
  = UnsupportedPreparedShape Text
  | InvalidPreparedIdentity Text
  | InvalidPreparedRepresentation Text
  | InvalidPreparedLayout Text
  | MissingPreparedEntry SymbolIdentity
  | MissingPreparedTop SymbolIdentity
  | UnboundPreparedInternal Text
  deriving stock (Eq, Show)

data PState = PState
  { nextValue :: Word32, nextJoin :: Word32
  , values :: VarEnv ValueId, joins :: VarEnv JoinId
  , topSymbols :: VarEnv SymbolIdentity, topValues :: Map SymbolIdentity ValueId
  , globals :: VarEnv GlobalId, globalDecls :: [GlobalDecl]
  , constructors :: [(DataCon, ConstructorId)], constructorDecls :: [ConstructorDecl]
  , operations :: [((Text, Signature), OperationId)], operationDecls :: [OperationDecl]
  , signatures :: [(Signature, SignatureId)]
  , target :: TargetDescriptor
  , retainedGenerations :: Map SymbolIdentity Word64
  , homeModules :: Set (Text, Text)
  }

type P a = StateT PState (Either ProjectionError) a

-- | Narrow test seam for GHC literals which cannot be written in source Haskell.
projectLiteralAtomForTest :: TargetDescriptor -> Literal -> Either ProjectionError Atom
projectLiteralAtomForTest machine literal = evalStateT (projectLiteralAtom literal)
  (PState 0 0 emptyVarEnv emptyVarEnv emptyVarEnv Map.empty emptyVarEnv [] [] [] [] [] [] machine Map.empty Set.empty)

projectPrepared :: ProjectionContext -> [PreparedModule] -> Either ProjectionError WireProgram
projectPrepared _ [] = Left (UnsupportedPreparedShape "execution program has no modules")
projectPrepared context modules =
  projectPreparedWithTopSymbols context modules (buildTopIdentityMap modules)

-- | Corpus tooling enumerates the same identities that projection resolves,
-- before any target filtering. Preserve module/binding emission order and never
-- infer STG names from filenames emitted by the retired Core translator.
preparedTopIdentities :: [PreparedModule] -> Either ProjectionError [SymbolIdentity]
preparedTopIdentities modules = traverse identityOf
  [ binder
  | prepared <- modules
  , (binding, _) <- pmBindings prepared
  , binder <- topBinders binding
  ]
  where
    identities = buildTopIdentityMap modules
    identityOf binder = maybe
      (Left (UnsupportedPreparedShape "top binder missing from complete identity map"))
      Right (lookupVarEnv identities binder)

projectPreparedWithTopSymbols :: ProjectionContext -> [PreparedModule]
  -> VarEnv SymbolIdentity -> Either ProjectionError WireProgram
projectPreparedWithTopSymbols context modules topIdentityMap = do
  let initial = PState 0 0 emptyVarEnv emptyVarEnv topIdentityMap Map.empty
        emptyVarEnv [] [] [] [] [] [] (projectionTarget context)
        (projectionRetainedGenerations context) (Set.fromList
          [ (Text.pack (unitString (moduleUnit (pmModule prepared))),
             Text.pack (moduleNameString (moduleName (pmModule prepared))))
          | prepared <- modules ])
  (bindingGroups, final) <- runStateT (preallocate modules >> concat <$> mapM projectModule modules) initial
  entry <- maybe (Left (MissingPreparedEntry (projectionEntry context)))
    (pure . topValue) (findTop bindingGroups)
  pure WireProgram
    { programEnvelope = ProgramEnvelope schemaVersion (projectionProfile context)
        (projectionToolchain context) executionAbiVersion (projectionTarget context)
    , programSignatures = map fst (signatures final)
    , programGlobals = globalDecls final
    , programConstructors = constructorDecls final
    , programOperations = operationDecls final
    , programBindings = bindingGroups
    , programEntry = entry
    }
  where
    topValue (TopBinding _ binding) = heapBindingId binding
    findTop = foldr findGroup Nothing
    findGroup group found = case filter
      ((== projectionEntry context) . topSymbol) (groupItems group) of
      top : _ -> Just top
      [] -> found
    groupItems (NonRecursive top) = [top]
    groupItems (Recursive tops) = tops
    topSymbol (TopBinding symbol _) = symbol

-- | Project only the supplied top-level closure reachable from the selected
-- entry. Package imports remain explicit globals for atomic linking. This
-- avoids rejecting unrelated polymorphic bindings while retaining every
-- supplied top-level dependency of the entry.
projectPreparedTarget :: ProjectionContext -> [PreparedModule] -> Either ProjectionError WireProgram
projectPreparedTarget _ [] = Left (UnsupportedPreparedShape "execution program has no modules")
projectPreparedTarget context modules =
  projectPreparedWithTopSymbols context
    [ prepared { pmBindings = filter isReachable (pmBindings prepared) }
    | prepared <- modules
    , any isReachable (pmBindings prepared)
    ] topIdentityMap
  where
    topIdentityMap = buildTopIdentityMap modules
    topUniqueIdentityMap = buildTopUniqueIdentityMap topIdentityMap modules
    allBindings =
      [ (pmModule prepared, binding)
      | prepared <- modules
      , (binding, _) <- pmBindings prepared
      ]
    topLevel = mkUniqSet
      [ varUnique binder
      | (_, binding) <- allBindings
      , binder <- topBinders binding
      ]
    entry = projectionEntry context
    seedSymbols =
      [ symbol
      | (_, binding) <- allBindings
      , binder <- topBinders binding
      , let symbol = mappedTopIdentity binder
      , symbol == entry
      ]
    dependencies = Map.fromListWith (<>)
      [ (mappedTopIdentity binder, Set.fromList
          [ symbol
          | unique <- nonDetEltsUniqSet (topBindingReferences modul topLevel binding)
          , Just symbol <- [lookupUFM topUniqueIdentityMap unique]
          ])
      | (modul, binding) <- allBindings
      , binder <- topBinders binding
      ]
    reachableSymbols = close Set.empty seedSymbols
    isReachable (binding, _) = any
      (\binder -> mappedTopIdentity binder `Set.member` reachableSymbols)
      (topBinders binding)
    close :: Set SymbolIdentity -> [SymbolIdentity] -> Set SymbolIdentity
    close visited [] = visited
    close visited (symbol : pending)
      | symbol `Set.member` visited = close visited pending
      | otherwise = close (Set.insert symbol visited)
          (maybe pending (\next -> Set.toList next <> pending)
            (Map.lookup symbol dependencies))

    mappedTopIdentity binder = lookupVarEnv topIdentityMap binder
      `orElse` idSymbol (topIdentityNamespace binder) binder

    orElse (Just value) _ = value
    orElse Nothing fallback = fallback

topBinders :: CgStgTopBinding -> [Id]
topBinders (StgTopStringLit binder _) = [binder]
topBinders (StgTopLifted binding) = bindingBinders binding

-- | Assign stable identities to internal tops before any target reachability
-- filtering.  Internal names may repeat (and a generated suffix may already
-- be an authored spelling), so reserve every original spelling first and claim
-- either that spelling or the first unused suffix in emission order. The
-- allocation is local to a symbol namespace; external names are retained
-- byte-for-byte while constraining generated suffixes around them.
buildTopIdentityMap :: [PreparedModule] -> VarEnv SymbolIdentity
buildTopIdentityMap modules = foldl insert emptyVarEnv (zip binders assigned)
  where
    binders =
      [ (pmModule prepared, binder)
      | prepared <- modules
      , (binding, _) <- pmBindings prepared
      , binder <- topBinders binding
      ]
    raw (fallback, binder) = idSymbolFor fallback (topIdentityNamespace binder) binder
    symbols = map raw binders
    assigned = assignTopIdentitySpellings
      (zip symbols (map (isExternalName . varName . snd) binders))
    insert mappings ((_, binder), symbol) = extendVarEnv mappings binder symbol

buildTopUniqueIdentityMap :: VarEnv SymbolIdentity -> [PreparedModule]
  -> UniqFM Unique SymbolIdentity
buildTopUniqueIdentityMap topIdentityMap modules = listToUFM
  [ (varUnique binder, symbol)
  | prepared <- modules
  , (binding, _) <- pmBindings prepared
  , binder <- topBinders binding
  , Just symbol <- [lookupVarEnv topIdentityMap binder]
  ]

-- | Deterministic identity allocation shared by projection and collision
-- regressions. The Bool marks an externally named top, whose spelling is
-- retained exactly; all original spellings reserve suffixes for internal tops.
assignTopIdentitySpellings
  :: [(SymbolIdentity, Bool)] -> [SymbolIdentity]
assignTopIdentitySpellings entries = snd (foldl allocateOne
  (externalClaims, []) entries)
  where
    reserved :: Map (Text, Text, Text) (Set Text)
    reserved = Map.fromListWith Set.union
      [ (namespaceKey symbol, Set.singleton (symbolOccurrence symbol))
      | (symbol, _) <- entries
      ]
    externalClaims :: Map (Text, Text, Text) (Set Text)
    externalClaims = Map.fromListWith Set.union
      [ (namespaceKey symbol, Set.singleton (symbolOccurrence symbol))
      | (symbol, external) <- entries
      , external
      ]
    allocateOne (claimedByNamespace, assigned) (symbol, external)
      | external = (claimedByNamespace, assigned <> [symbol])
      | otherwise =
          let key = namespaceKey symbol
              claimed = Map.findWithDefault Set.empty key claimedByNamespace
              reservedNames = Map.findWithDefault Set.empty key reserved
              occurrence = chooseOccurrence (symbolOccurrence symbol)
                claimed reservedNames
              nextClaimed = Set.insert occurrence claimed
          in (Map.insert key nextClaimed claimedByNamespace,
              assigned <> [symbol { symbolOccurrence = occurrence }])

    chooseOccurrence :: Text -> Set Text -> Set Text -> Text
    chooseOccurrence original claimed reservedNames
      | original `Set.notMember` claimed = original
      | otherwise = case listToMaybe
          [ candidate | n <- [1 :: Int ..]
          , let candidate = original <> "." <> Text.pack (show n)
          , candidate `Set.notMember` (claimed `Set.union` reservedNames)
          ] of
          Just value -> value
          Nothing -> original

    namespaceKey symbol =
      (symbolUnit symbol, symbolModule symbol, symbolNamespace symbol)

preallocate :: [PreparedModule] -> P ()
preallocate = mapM_ (mapM_ allocateTop . pmBindings)
  where
    allocateTop (StgTopStringLit binder _, _) = allocateTopValue binder >> pure ()
    allocateTop (StgTopLifted binding, _) = mapM_ allocateTopValue (bindingBinders binding) >> pure ()

projectModule :: PreparedModule -> P [Group TopBinding]
projectModule = mapM (projectTop . fst) . pmBindings

projectTop :: CgStgTopBinding -> P (Group TopBinding)
projectTop (StgTopStringLit binder bytes) = do
  identity <- requireTopValue binder
  symbol <- topIdentity binder
  pure (NonRecursive (TopBinding symbol
    (HeapBinding identity (Bytes bytes))))
projectTop (StgTopLifted (StgNonRec binder rhs)) = NonRecursive <$> projectTopPair binder rhs
projectTop (StgTopLifted (StgRec pairs)) = Recursive <$> mapM (uncurry projectTopPair) pairs

projectTopPair :: Id -> CgStgRhs -> P TopBinding
projectTopPair binder rhs = do
  symbol <- topIdentity binder
  TopBinding symbol <$> (HeapBinding <$> requireTopValue binder <*> projectRhs binder rhs)

projectRhs :: Id -> CgStgRhs -> P HeapRhs
projectRhs binder (StgRhsClosure captures _ update parameters body resultType) = withScope $ do
  captureRefs <- mapM projectReference (dVarSetElems captures)
  parameterIds <- mapM bindValue parameters
  resultReps <- repsForType resultType
  projectedBody <- projectExpr resultReps body
  case update of
    ReEntrant -> Function <$> (internSignature =<< signatureFor parameters resultType)
      <*> pure parameterIds <*> pure captureRefs <*> pure projectedBody
    Updatable -> do
      signature <- internSignature (Signature [] resultReps)
      pure (Thunk signature Memoize captureRefs projectedBody)
    Stg.SingleEntry -> do
      signature <- internSignature (Signature [] resultReps)
      pure (Thunk signature Schema.SingleEntry captureRefs projectedBody)
    JumpedTo -> failShape ("heap binding marked JumpedTo: " <> symbolText (idSymbol "value" binder))
projectRhs _ (StgRhsCon _ con _ _ args _) = Constructor <$> internConstructor con <*> mapM projectArg args

projectExpr :: [RuntimeRep] -> CgStgExpr -> P Expr
projectExpr expected (StgApp function args) = do
  knownJoins <- gets joins
  case lookupVarEnv knownJoins function of
    Just join -> Jump join <$> mapM projectArg args
    Nothing -> case args of
      [] -> do
        reps <- repsForType (varType function)
        case reps of
          [] -> pure (Return [])
          [LiftedRefRep] -> do
            callee <- Ref <$> projectReference function
            signature <- internSignature =<< signatureForApplication [] expected
            pure (Enter callee signature)
          [_] -> Return . pure . Ref <$> projectReference function
          _ -> failRepresentation "zero-argument STG application retains a multi-component variable"
      _ -> do
        projectedArgs <- mapM projectArg args
        callee <- Ref <$> projectReference function
        signature <- internSignature =<< signatureForApplication args expected
        pure (Call callee signature projectedArgs)
projectExpr _ (StgLit literal) = Return . pure <$> projectLiteralAtom literal
projectExpr _ (StgConApp con _ args _)
  | isUnboxedTupleDataCon con = Return <$> mapM projectArg args
  | otherwise = Construct <$> internConstructor con <*> mapM projectArg args
projectExpr _ (StgOpApp op args resultType) = do
  signature <- internSignature =<< signatureForArgs args resultType
  Operation <$> internOperation op signature <*> mapM projectArg args
projectExpr expected (StgCase scrutinee binder altType alts) = do
  binderReps <- repsForType (varType binder)
  projectedScrutinee <- projectExpr binderReps scrutinee
  kind <- projectCaseKind altType
  (identity, alternatives) <- withScope $ do
    identity <- bindValue binder
    alternatives <- mapM (projectAlt expected altType) alts
    pure (identity, alternatives)
  pure (Case projectedScrutinee identity binderReps kind alternatives)
projectExpr expected (StgLet _ binding body) = withScope $
  Let <$> projectLocalGroup binding <*> projectExpr expected body
projectExpr expected (StgLetNoEscape _ binding body) = withScope $
  LetJoins <$> projectJoinGroup binding <*> projectExpr expected body
projectExpr expected (StgTick _ body) = projectExpr expected body

projectCaseKind :: AltType -> P CaseKind
projectCaseKind (AlgAlt tycon) = pure (AlgebraicCase (nameSymbol "type" (GHC.tyConName tycon)))
projectCaseKind (PrimAlt rep) = PrimitiveCase <$> projectRep rep
projectCaseKind (MultiValAlt _) = pure MultiValueCase
projectCaseKind PolyAlt = pure PolymorphicCase

projectAlt :: [RuntimeRep] -> AltType -> CgStgAlt -> P Alternative
projectAlt expected (MultiValAlt _) (GenStgAlt (DataAlt con) binders body)
  | isUnboxedTupleDataCon con = withScope $ Alternative DefaultPattern
      <$> mapM bindValue binders <*> projectExpr expected body
projectAlt expected _ (GenStgAlt con binders body) = withScope $ Alternative <$> projectPattern con
  <*> mapM bindValue binders <*> projectExpr expected body

projectPattern :: AltCon -> P AlternativePattern
projectPattern DEFAULT = pure DefaultPattern
projectPattern (DataAlt con) = ConstructorPattern <$> internConstructor con
projectPattern (LitAlt literal) = LiteralPattern <$> projectLiteral literal

projectLocalGroup :: CgStgBinding -> P (Group HeapBinding)
projectLocalGroup (StgNonRec binder rhs) = do
  projectedRhs <- projectRhs binder rhs
  identity <- bindValue binder
  pure (NonRecursive (HeapBinding identity projectedRhs))
projectLocalGroup (StgRec pairs) = do
  identities <- mapM (bindValue . fst) pairs
  Recursive <$> forM (zip identities pairs) (\(identity, (binder, rhs)) ->
    HeapBinding identity <$> projectRhs binder rhs)

projectJoinGroup :: CgStgBinding -> P (Group JoinBinding)
projectJoinGroup (StgNonRec binder rhs) = do
  identity <- freshJoin
  projected <- projectJoin identity binder rhs
  modify' (\current -> current { joins = extendVarEnv (joins current) binder identity })
  pure (NonRecursive projected)
projectJoinGroup (StgRec pairs) = do
  identities <- mapM (bindJoin . fst) pairs
  Recursive <$> forM (zip identities pairs) (\(identity, (binder, rhs)) ->
    projectJoin identity binder rhs)

projectJoin :: JoinId -> Id -> CgStgRhs -> P JoinBinding
projectJoin identity _ (StgRhsClosure _ _ JumpedTo parameters body resultType) = withScope $ do
  resultReps <- repsForType resultType
  JoinBinding identity <$> (internSignature =<< signatureFor parameters resultType)
    <*> mapM bindValue parameters
    <*> projectExpr resultReps body
projectJoin _ binder _ = failShape
  ("let-no-escape binding lacks JumpedTo form: " <> symbolText (idSymbol "join" binder))

projectArg :: StgArg -> P Atom
projectArg (StgVarArg binder) = do
  reps <- repsForType (varType binder)
  if null reps then pure Void else Ref <$> projectReference binder
projectArg (StgLitArg literal) = projectLiteralAtom literal

projectReference :: Id -> P ValueRef
projectReference binder = do
  known <- gets values
  case lookupVarEnv known binder of
    Just identity -> pure (Local identity)
    Nothing -> do
      topNames <- gets topSymbols
      tops <- gets topValues
      let symbol = lookupVarEnv topNames binder
      case symbol of
        Just home -> case Map.lookup home tops of
          Just identity -> pure (Local identity)
          Nothing -> lift (Left (MissingPreparedTop home))
        Nothing -> Global <$> internGlobal binder

bindingBinders :: CgStgBinding -> [Id]
bindingBinders (StgNonRec binder _) = [binder]
bindingBinders (StgRec pairs) = map fst pairs

allocateTopValue :: Id -> P ValueId
allocateTopValue binder = do
  symbol <- topIdentity binder
  known <- gets topValues
  case Map.lookup symbol known of
    Just _ -> failIdentity ("duplicate top-level value: " <> symbolText symbol)
    Nothing -> do
      identity <- freshValue
      modify' (\current -> current { topValues = Map.insert symbol identity (topValues current) })
      pure identity

requireTopValue :: Id -> P ValueId
requireTopValue binder = do
  symbol <- topIdentity binder
  gets (Map.lookup symbol . topValues) >>= maybe
    (failIdentity ("missing top-level value allocation: " <> symbolText symbol)) pure

topIdentity :: Id -> P SymbolIdentity
topIdentity binder = gets (\st -> lookupVarEnv (topSymbols st) binder) >>= maybe
  (pure (idSymbol (topIdentityNamespace binder) binder)) pure

-- GHC uniques can be reused by binders in disjoint RHS scopes. Each lexical
-- binder gets a fresh wire ID, while the VarEnv tracks only the current scope.
bindValue :: Id -> P ValueId
bindValue binder = do
  identity <- freshValue
  modify' (\current -> current { values = extendVarEnv (values current) binder identity })
  pure identity

freshValue :: P ValueId
freshValue = do
  identity <- ValueId <$> gets nextValue
  modify' (\current -> current { nextValue = nextValue current + 1 })
  pure identity

bindJoin :: Id -> P JoinId
bindJoin binder = do
  identity <- freshJoin
  modify' (\current -> current { joins = extendVarEnv (joins current) binder identity })
  pure identity

freshJoin :: P JoinId
freshJoin = do
  identity <- JoinId <$> gets nextJoin
  modify' (\current -> current { nextJoin = nextJoin current + 1 })
  pure identity

withScope :: P a -> P a
withScope action = do
  savedValues <- gets values
  savedJoins <- gets joins
  result <- action
  modify' (\current -> current { values = savedValues, joins = savedJoins })
  pure result

internGlobal :: Id -> P GlobalId
internGlobal binder
  | not (isExternalName (varName binder)) =
      lift (Left (UnboundPreparedInternal
        (Text.pack (occNameString (nameOccName (varName binder))))))
  | otherwise = case nameModule_maybe (varName binder) of
      Nothing -> lift (Left (InvalidPreparedIdentity
        ("global has no defining module: "
          <> Text.pack (occNameString (nameOccName (varName binder))))))
      Just module_ -> do
        homes <- gets homeModules
        if homeKey module_ `Set.member` homes
          then lift (Left (MissingPreparedTop (idSymbol "value" binder)))
          else internExternalGlobal binder
  where
    homeKey module_ =
      (Text.pack (unitString (moduleUnit module_)),
       Text.pack (moduleNameString (moduleName module_)))
    internExternalGlobal externalBinder = do
      known <- gets globals
      case lookupVarEnv known externalBinder of
        Just identity -> pure identity
        Nothing -> do
          reps <- repsForType (varType externalBinder)
          rep <- case reps of
            [] -> pure VoidRep
            [single] -> pure single
            _ -> failRepresentation "global value has more than one representation component"
          (entry, deadEnd, evaluated) <- importedEntry externalBinder
          signature <- traverse internSignature entry
          existing <- gets globalDecls
          generations <- gets retainedGenerations
          let identity = GlobalId (fromIntegral (length existing))
              symbol = idSymbol "value" externalBinder
              retainedGeneration = Map.lookup symbol generations
              declaration = GlobalDecl symbol rep signature deadEnd evaluated
                retainedGeneration
          modify' (\current -> current
            { globals = extendVarEnv (globals current) externalBinder identity
            , globalDecls = globalDecls current <> [declaration] })
          pure identity

internSignature :: Signature -> P SignatureId
internSignature signature = do
  known <- gets signatures
  case find ((== signature) . fst) known of
    Just (_, identity) -> pure identity
    Nothing -> do
      let identity = SignatureId (fromIntegral (length known))
      modify' (\current -> current { signatures = signatures current <> [(signature, identity)] })
      pure identity

internConstructor :: DataCon -> P ConstructorId
internConstructor con = do
  known <- gets constructors
  case lookup con known of
    Just identity -> pure identity
    Nothing -> do
      reps <- concat <$> mapM (repsForType . scaledThing) (dataConRepArgTys con)
      -- GHC expands strictness along with representation arguments: a strict
      -- unboxed tuple does not make its lifted components strict. Resolve all
      -- representations first, before calling the fixed-representation helper.
      let marks = map isMarkedStrict (dataConRuntimeRepStrictness con)
      if length marks /= length reps
        then failRepresentation "constructor runtime strictness/representation arity mismatch"
        else pure ()
      let fieldStrictness = zipWith (\strict rep -> strict || isUnboxed rep) marks reps
      resultReps <- repsForType (dataConOrigResTy con)
      resultRep <- case resultReps of
        [rep@LiftedRefRep] -> pure rep
        [rep@UnliftedRefRep] -> pure rep
        _ -> failRepresentation "heap constructor lacks a managed result representation"
      layout <- layoutFor reps
      tag <- checkedWord32 "constructor tag" (dataConTag con)
      familySize <- checkedWord32 "constructor family size" (GHC.tyConFamilySize (dataConTyCon con))
      prior <- gets constructorDecls
      let identity = ConstructorId (fromIntegral (length prior))
          declaration = ConstructorDecl
            (nameSymbol "constructor" (dataConName con))
            (nameSymbol "type" (GHC.tyConName (dataConTyCon con)))
            resultRep reps fieldStrictness layout tag familySize
            (varId (dataConWorkId con))
      modify' (\current -> current
        { constructors = constructors current <> [(con, identity)]
        , constructorDecls = constructorDecls current <> [declaration] })
      pure identity
  where
    scaledThing (Scaled _ ty) = ty
    isUnboxed LiftedRefRep = False
    isUnboxed UnliftedRefRep = False
    isUnboxed _ = True

checkedWord32 :: Text -> Int -> P Word32
checkedWord32 label value
  | value < 0 = failRepresentation (label <> " is negative")
  | toInteger value > toInteger (maxBound :: Word32) =
      failRepresentation (label <> " exceeds u32")
  | otherwise = pure (fromIntegral value)

internOperation :: StgOp -> SignatureId -> P OperationId
internOperation op signature = case op of
  StgPrimOp primop -> do
      let operationName = Text.pack (occNameString (primOpOcc primop))
      operationSignature <- signatureForId signature
      known <- gets operations
      case lookup (operationName, operationSignature) known of
       Just identity -> pure identity
       Nothing -> do
        prior <- gets operationDecls
        let identity = OperationId (fromIntegral (length prior))
            declaration = OperationDecl operationName signature
        modify' (\current -> current
          { operations = operations current <> [((operationName, operationSignature), identity)]
          , operationDecls = operationDecls current <> [declaration] })
        pure identity
  _ -> failShape "foreign/prim-call operation lacks a structured operation contract"

signatureForId :: SignatureId -> P Signature
signatureForId identity = do
  known <- gets signatures
  case find ((== identity) . snd) known of
    Just (signature, _) -> pure signature
    Nothing -> failIdentity "operation refers to an unknown signature"

signatureFor :: [Id] -> Type -> P Signature
signatureFor args result = Signature <$> (concat <$> mapM (argumentRepsForType . varType) args) <*> repsForType result

-- Imported LF information is authoritative. In its absence GHC uses positive
-- representation arity as function evidence, but never guesses a thunk from
-- zero arity. A CAF returning a function has a zero-argument entry, not all the
-- arrows in the returned function's type.
--
-- `importedIdLFInfo` is partial for GHC's wired-in unused-argument descriptor.
-- Such a zero-width argument is projected directly as Void and never reaches
-- internGlobal, so this query remains restricted to genuine imported entries.
importedEntry :: Id -> P (Maybe Signature, Bool, Bool)
importedEntry binder = case importedIdLFInfo binder of
  LFReEntrant _ arity _ _ -> do
    (arguments, result) <- splitRepArguments arity (varType binder)
    signature <- Signature arguments <$> entryResults result
    pure (Just signature, isDeadEndId binder, True)
  LFThunk{} -> do
    signature <- Signature [] <$> entryResults (varType binder)
    pure (Just signature, isDeadEndId binder, False)
  LFCon{} -> pure (Nothing, False, True)
  LFUnlifted -> pure (Nothing, False, True)
  LFUnknown{} -> pure (Nothing, False, False)
  LFLetNoEscape -> failShape "imported join has no heap/global entry"
  where
    -- wave4:PRELUDE_HASKELL: globalDeadEnd carries the independent evidence.
    -- An empty vector here is not an assertion that a bottoming call returns
    -- zero values, nor that it raises instead of diverging.
    entryResults ty
      | isDeadEndId binder = pure []
      | otherwise = repsForType ty

signatureForArgs :: [StgArg] -> Type -> P Signature
signatureForArgs args result = Signature <$> (concat <$> mapM argReps args) <*> repsForType result
  where
    argReps (StgVarArg binder) = argumentRepsForType (varType binder)
    argReps (StgLitArg literal) = argumentRepsForType (literalType literal)

-- The STG context, not the callee's source type, says what this application
-- must produce. Arguments retain their actual unarised representations while
-- the enclosing RHS, join, or case supplies the demanded result group.
signatureForApplication :: [StgArg] -> [RuntimeRep] -> P Signature
signatureForApplication args demandedResult = Signature
  <$> (concat <$> mapM argReps args) <*> pure demandedResult
  where
    argReps (StgVarArg binder) = argumentRepsForType (varType binder)
    argReps (StgLitArg literal) = argumentRepsForType (literalType literal)

-- Imported LF arity is expressed in GHC's callable representation view. Only
-- imported entries need this source-type traversal; STG application sites use
-- their actual arguments plus an already-threaded demanded result instead.
splitRepArguments :: Int -> Type -> P ([RuntimeRep], Type)
splitRepArguments 0 ty = pure ([], ty)
splitRepArguments supplied ty = case unwrapType ty of
  FunTy _ _ argument result -> do
    reps <- argumentRepsForType argument
    if supplied < length reps
      then failRepresentation "application splits an unarised source argument"
      else do
        (remaining, finalResult) <- splitRepArguments (supplied - length reps) result
        pure (reps <> remaining, finalResult)
  _ -> failRepresentation "application exceeds its GHC function type"

-- Void positions count toward semantic saturation even though they have no
-- register or payload component after unarisation.
argumentRepsForType :: Type -> P [RuntimeRep]
argumentRepsForType ty = do
  reps <- repsForType ty
  pure (if null reps then [VoidRep] else reps)

repsForType :: Type -> P [RuntimeRep]
repsForType ty = maybe (failRepresentation "runtime-polymorphic representation")
  (mapM projectRep) (typePrimRep_maybe ty)

projectRep :: GHC.PrimRep -> P RuntimeRep
projectRep (GHC.BoxedRep (Just GHC.Lifted)) = pure LiftedRefRep
projectRep (GHC.BoxedRep (Just GHC.Unlifted)) = pure UnliftedRefRep
projectRep (GHC.BoxedRep Nothing) = failRepresentation "runtime-polymorphic boxed representation"
projectRep GHC.AddrRep = pure AddressRep
projectRep GHC.IntRep = targetWidth IntRep
projectRep GHC.WordRep = targetWidth WordRep
projectRep GHC.Int8Rep = pure (IntRep 8)
projectRep GHC.Word8Rep = pure (WordRep 8)
projectRep GHC.Int16Rep = pure (IntRep 16)
projectRep GHC.Word16Rep = pure (WordRep 16)
projectRep GHC.Int32Rep = pure (IntRep 32)
projectRep GHC.Word32Rep = pure (WordRep 32)
projectRep GHC.Int64Rep = pure (IntRep 64)
projectRep GHC.Word64Rep = pure (WordRep 64)
projectRep GHC.FloatRep = pure (FloatRep 32)
projectRep GHC.DoubleRep = pure (FloatRep 64)
projectRep GHC.VecRep{} = failRepresentation "vector representation"

targetWidth :: (Word8 -> RuntimeRep) -> P RuntimeRep
targetWidth constructor = constructor . targetWordWidth <$> gets target

layoutFor :: [RuntimeRep] -> P CheckedLayout
layoutFor reps = do
  machine <- gets target
  let stored = filter (/= VoidRep) reps
  (fields, end, alignment) <- foldM (place machine) ([], 0, 1) stored
  pure (CheckedLayout fields alignment (alignUp end alignment) (map isRoot stored))
  where
    place machine (fields, cursor, greatest) rep = do
      size <- repBytes machine rep
      let alignment = max 1 size; offset = alignUp cursor alignment
      pure (fields <> [FieldLayout rep offset], offset + size, max greatest alignment)
    isRoot LiftedRefRep = True
    isRoot UnliftedRefRep = True
    isRoot _ = False

repBytes :: TargetDescriptor -> RuntimeRep -> P Word32
repBytes machine rep = width $ case rep of
  VoidRep -> 0
  LiftedRefRep -> targetPointerWidth machine
  UnliftedRefRep -> targetPointerWidth machine
  AddressRep -> targetPointerWidth machine
  IntRep bits -> bits
  WordRep bits -> bits
  FloatRep bits -> bits
  where
    width 0 = pure 0
    width bits | bits `mod` 8 == 0 = pure (fromIntegral bits `div` 8)
    width _ = failLayout "non-byte runtime width"

alignUp :: Word32 -> Word32 -> Word32
alignUp value alignment = ((value + alignment - 1) `div` alignment) * alignment

-- | Unarise splits multi-representation rubbish and removes zero-width rubbish.
-- Both TYPE and CONSTRAINT use the same resolved physical representation; no
-- GHC kind/type needs to cross the execution boundary.
projectLiteralAtom :: Literal -> P Atom
projectLiteralAtom (LitRubbish _ runtimeRep) =
  case runtimeRepPrimRep_maybe runtimeRep of
    Just [rep] -> Rubbish <$> projectRep rep
    Just _ -> failRepresentation "rubbish literal was not unarised to one component"
    Nothing -> failRepresentation "runtime-polymorphic rubbish literal"
projectLiteralAtom literal = Scalar <$> projectLiteral literal

projectLiteral :: Literal -> P ScalarLiteral
projectLiteral literal = case literal of
  LitChar character -> do
    machine <- gets target
    let bits = targetWordWidth machine
    pure (WordLiteral bits (integerBytes bits (fromIntegral (fromEnum character))))
  LitString bytes -> pure (BytesLiteral bytes)
  LitNumber kind value -> numeric kind value
  LitFloat value -> pure (FloatLiteral 32 (wordBytes 4 (fromIntegral (castFloatToWord32 (fromRational value)))))
  LitDouble value -> pure (FloatLiteral 64 (wordBytes 8 (castDoubleToWord64 (fromRational value))))
  LitNullAddr -> pure NullAddressLiteral
  LitRubbish{} -> failShape "rubbish literal cannot be an alternative pattern"
  LitLabel{} -> failShape "relocatable label literal"
  where
    numeric LitNumBigNat _ = failShape "BigNat literal"
    numeric kind value = do
      machine <- gets target
      let (signed, bits) = case kind of
            LitNumInt -> (True, targetWordWidth machine)
            LitNumInt8 -> (True, 8); LitNumInt16 -> (True, 16)
            LitNumInt32 -> (True, 32); LitNumInt64 -> (True, 64)
            LitNumWord -> (False, targetWordWidth machine)
            LitNumWord8 -> (False, 8); LitNumWord16 -> (False, 16)
            LitNumWord32 -> (False, 32); LitNumWord64 -> (False, 64)
          bytes = integerBytes bits value
      pure (if signed then IntLiteral bits bytes else WordLiteral bits bytes)

integerBytes :: Word8 -> Integer -> BS.ByteString
integerBytes bits value = BS.pack
  [ fromIntegral (normalized `shiftR` (byte * 8))
  | byte <- reverse [0 .. fromIntegral bits `div` 8 - 1] ]
  where normalized = value `mod` (2 ^ bits)

wordBytes :: Int -> Word64 -> BS.ByteString
wordBytes count value = BS.pack
  [ fromIntegral (value `shiftR` (byte * 8)) | byte <- reverse [0 .. count - 1] ]

idSymbol :: Text -> Id -> SymbolIdentity
idSymbol namespace = nameSymbol namespace . varName

idSymbolFor :: Module -> Text -> Id -> SymbolIdentity
idSymbolFor fallback namespace binder = nameSymbolFor fallback namespace (varName binder)

topIdentityNamespace :: Id -> Text
topIdentityNamespace binder
  | isExternalName (varName binder) = "value"
  | otherwise = "local"

nameSymbol :: Text -> Name -> SymbolIdentity
nameSymbol namespace = nameSymbolWithFallback Nothing namespace

nameSymbolFor :: Module -> Text -> Name -> SymbolIdentity
nameSymbolFor fallback namespace = nameSymbolWithFallback (Just fallback) namespace

nameSymbolWithFallback :: Maybe Module -> Text -> Name -> SymbolIdentity
nameSymbolWithFallback fallback namespace name = case nameModule_maybe name of
  Just modul -> SymbolIdentity (Text.pack (unitString (moduleUnit modul)))
    (Text.pack (moduleNameString (moduleName modul))) namespace
    (Text.pack (occNameString (nameOccName name)))
  Nothing -> case fallback of
    Just modul -> SymbolIdentity (Text.pack (unitString (moduleUnit modul)))
      (Text.pack (moduleNameString (moduleName modul))) namespace
      (Text.pack (occNameString (nameOccName name)))
    Nothing -> SymbolIdentity "<interactive>" "<local>" namespace
      (Text.pack (occNameString (nameOccName name)))

symbolText :: SymbolIdentity -> Text
symbolText symbol = symbolUnit symbol <> ":" <> symbolModule symbol <> ":"
  <> symbolNamespace symbol <> ":" <> symbolOccurrence symbol

failShape :: Text -> P a
failShape = lift . Left . UnsupportedPreparedShape
failIdentity :: Text -> P a
failIdentity = lift . Left . InvalidPreparedIdentity
failRepresentation :: Text -> P a
failRepresentation = lift . Left . InvalidPreparedRepresentation
failLayout :: Text -> P a
failLayout = lift . Left . InvalidPreparedLayout
