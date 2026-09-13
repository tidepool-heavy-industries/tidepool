module Tidepool.ExecutionProjection
  ( ProjectionContext(..)
  , ProjectionError(..)
  , projectPrepared
  , projectPreparedTarget
  , preparedTopIdentities
  , preparedTargetReferences
  , projectLiteralAtomForTest
  , assignTopIdentitySpellings
  ) where

import Control.Monad (foldM, forM, unless)
import Control.Monad.State.Strict
import Data.Bits (shiftR)
import Data.ByteString qualified as BS
import Data.List (find)
import Data.Maybe (isJust, isNothing, listToMaybe)
import Tidepool.PreparedBuiltins
  ( DeferredFunction(..), deferredFunction, wiredInErrorKind )
import Data.Map.Strict (Map)
import Data.Map.Strict qualified as Map
import Data.Set (Set)
import Data.Set qualified as Set
import Data.Text (Text)
import Data.Text qualified as Text
import Data.Word (Word32, Word64, Word8)
import GHC.Builtin.PrimOps (PrimOp(..), PrimCall(..), primOpOcc)
import GHC.Builtin.Types (doubleDataCon, intDataCon)
import GHC.Core (AltCon(..))
import GHC.Core.DataCon
  ( DataCon, dataConName, dataConRepArgTys, dataConRepArity, dataConWorkId
  , dataConTag, dataConTyCon, dataConOrigResTy, isMarkedStrict, isUnboxedTupleDataCon )
import GHC.Core.TyCo.Rep (Scaled(..), Type(..))
import GHC.Core.Type (splitTyConApp_maybe)
import GHC.Core.TyCon qualified as GHC
import GHC.Data.FastString (unpackFS)
import GHC.Float (castDoubleToWord64, castFloatToWord32)
import GHC.Stg.Syntax
import GHC.Stg.Syntax qualified as Stg
import GHC.StgToCmm.Closure (importedIdLFInfo)
import GHC.StgToCmm.Types (LambdaFormInfo(..))
import GHC.Types.Demand (splitDmdSig)
import GHC.Types.Literal (LitNumType(..), Literal(..), literalType)
import GHC.Types.Id (idDmdSig, isDeadEndId, isDataConWorkId_maybe)
import GHC.Types.ForeignCall qualified as Foreign
import GHC.Types.Name (Name, isExternalName, nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (fieldOcc_maybe, occNameString)
import GHC.Types.RepType
  (typePrimRep_maybe, runtimeRepPrimRep_maybe, dataConRuntimeRepStrictness, unwrapType)
import GHC.Types.Unique.Set (elementOfUniqSet, mkUniqSet, nonDetEltsUniqSet)
import GHC.Types.Unique (Unique)
import GHC.Types.Unique.FM (UniqFM, listToUFM, lookupUFM)
import GHC.Types.Var (Id, varName, varType, varUnique)
import GHC.Types.Var.Env (VarEnv, emptyVarEnv, extendVarEnv, lookupVarEnv)
import GHC.Types.Var.Set (dVarSetElems)
import GHC.Unit.Module (moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Types (Module, unitString)
import GHC.Utils.Outputable (ppr, showSDocUnsafe)
import Tidepool.ExecutionIR (topBindingReferences)
import Tidepool.ExecutionSchema
import Tidepool.ExecutionSchema qualified as Schema
import Tidepool.PreparedFacts (PreparedFacts(..), extractPreparedFacts)
import Tidepool.Identity (varId)
import Tidepool.PreparedStg (PreparedModule(..), PreparedCoverage(..))
import Tidepool.PreparedFormatting
  (FormattingAuthority, FormattingSpec(..), FormattingIntrinsic(..), classifyFormatting)

data ProjectionContext = ProjectionContext
  { projectionProfile :: Text
  , projectionToolchain :: Text
  , projectionTarget :: TargetDescriptor
  , projectionRetainedGenerations :: Map SymbolIdentity Word64
  , projectionEntry :: SymbolIdentity
  , projectionFormattingAuthority :: Maybe FormattingAuthority
  } deriving stock (Eq, Show)

data ProjectionError
  = UnsupportedPreparedShape Text
  | InvalidPreparedIdentity Text
  | InvalidPreparedRepresentation Text
  | InvalidPreparedLayout Text
  | MissingPreparedEntry SymbolIdentity
  | MissingPreparedTop SymbolIdentity
  | UnboundPreparedInternal Text
  | DeferredFunctionSignatureMismatch SymbolIdentity Signature (Maybe Signature)
  | UnsupportedPrimitiveCall Text Signature
  | UnsupportedForeignCall Text Signature
  deriving stock (Eq, Show)

data PState = PState
  { nextValue :: Word32, nextJoin :: Word32
  , values :: VarEnv ValueId, joins :: VarEnv JoinId
  , entryArities :: VarEnv Int
  , topSymbols :: VarEnv SymbolIdentity, topValues :: Map SymbolIdentity ValueId
  , implicitTops :: [TopBinding]
  , implicitValues :: Map SymbolIdentity ValueId
  , globals :: VarEnv GlobalId, globalDecls :: [GlobalDecl]
  , constructors :: [(DataCon, ConstructorId)], constructorDecls :: [ConstructorDecl]
  , operations :: [(Schema.OperationIdentity, Signature, OperationId)]
  , operationDecls :: [OperationDecl]
  , signatures :: [(Signature, SignatureId)]
  , target :: TargetDescriptor
  , retainedGenerations :: Map SymbolIdentity Word64
  , homeModules :: Set (Text, Text)
  , formattingAuthority :: Maybe FormattingAuthority
  }

type P a = StateT PState (Either ProjectionError) a

-- | Narrow test seam for GHC literals which cannot be written in source Haskell.
projectLiteralAtomForTest :: TargetDescriptor -> Literal -> Either ProjectionError Atom
projectLiteralAtomForTest machine literal = evalStateT (projectLiteralAtom literal)
  (PState 0 0 emptyVarEnv emptyVarEnv emptyVarEnv emptyVarEnv Map.empty [] Map.empty emptyVarEnv [] [] [] [] [] [] machine Map.empty Set.empty Nothing)

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
  let initial = PState 0 0 emptyVarEnv emptyVarEnv emptyVarEnv topIdentityMap Map.empty [] Map.empty
        emptyVarEnv [] [] [] [] [] [] (projectionTarget context)
        (projectionRetainedGenerations context) (Set.fromList
          [ (Text.pack (unitString (moduleUnit (pmModule prepared))),
             Text.pack (moduleNameString (moduleName (pmModule prepared))))
          | prepared <- modules, pmCoverage prepared == CompleteSourceModule ])
        (projectionFormattingAuthority context)
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
    , programBindings = map NonRecursive (reverse (implicitTops final)) ++ bindingGroups
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
  let (identities, selected) = selectPreparedTarget context modules
  in projectPreparedWithTopSymbols context selected identities

-- | Exact external value references of the selected top closure. The identity
-- map is always computed before filtering. Recovery uses Ids, never occurrence
-- strings or the imported-only annotations returned by stg2stg.
preparedTargetReferences :: ProjectionContext -> [PreparedModule] -> [Id]
preparedTargetReferences context modules =
  let (_, selected) = selectPreparedTarget context modules
      defined = mkUniqSet [varUnique binder | prepared <- modules
        , (binding, _) <- pmBindings prepared, binder <- topBinders binding]
      referenced = [ binder | prepared <- selected
        , binder <- preparedReferencedIds (extractPreparedFacts
            (pmModule prepared) (pmTagSigs prepared)
            (concatMap (recoveryReferences context . fst) (pmBindings prepared)))
        , isExternalName (varName binder)
        , isNothing (nullaryWorkerConstructor binder)
        , not (elementOfUniqSet (varUnique binder) defined) ]
  in Map.elems (Map.fromList [(idSymbol "value" binder, binder) | binder <- referenced])

-- A registered replacement has no source-body dependencies. Split recursive
-- groups for this fact query so unrelated siblings retain their own references.
recoveryReferences :: ProjectionContext -> CgStgTopBinding -> [CgStgTopBinding]
recoveryReferences context (StgTopLifted (StgRec pairs)) =
  [ StgTopLifted (StgNonRec binder rhs)
  | (binder, rhs) <- pairs, not (registeredReplacement context binder) ]
recoveryReferences context binding
  | any (registeredReplacement context) (topBinders binding) = []
  | otherwise = [binding]

formattingSpec :: ProjectionContext -> Id -> Either ProjectionError (Maybe FormattingSpec)
formattingSpec context binder = case projectionFormattingAuthority context of
  Nothing -> Right Nothing
  Just authority -> case classifyFormatting authority binder of
    Left failure -> Left (UnsupportedPreparedShape (Text.pack (show failure)))
    Right spec -> Right spec

registeredFormatting :: ProjectionContext -> Id -> Bool
registeredFormatting context binder = case formattingSpec context binder of
  Right (Just _) -> True
  _ -> False

registeredReplacement :: ProjectionContext -> Id -> Bool
registeredReplacement context binder =
  registeredFormatting context binder || isJust (deferredFunction binder)

selectPreparedTarget :: ProjectionContext -> [PreparedModule]
  -> (VarEnv SymbolIdentity, [PreparedModule])
selectPreparedTarget context modules =
  (topIdentityMap, [ prepared { pmBindings = filter isReachable (pmBindings prepared) }
    | prepared <- modules
    , any isReachable (pmBindings prepared)
    ])
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
          | unique <- if registeredReplacement context binder then [] else
              nonDetEltsUniqSet (topBindingReferences modul topLevel single)
          , Just symbol <- [lookupUFM topUniqueIdentityMap unique]
          ])
      | (modul, binding) <- allBindings
      , (binder, single) <- individualTops binding
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

individualTops :: CgStgTopBinding -> [(Id, CgStgTopBinding)]
individualTops (StgTopStringLit binder bytes) =
  [(binder, StgTopStringLit binder bytes)]
individualTops (StgTopLifted (StgNonRec binder rhs)) =
  [(binder, StgTopLifted (StgNonRec binder rhs))]
individualTops (StgTopLifted (StgRec pairs)) =
  [(binder, StgTopLifted (StgNonRec binder rhs)) | (binder, rhs) <- pairs]

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
preallocate modules = do
  mapM_ (mapM_ (registerBindingArities . fst) . pmBindings) modules
  mapM_ (mapM_ allocateTop . pmBindings) modules
  where
    allocateTop (StgTopStringLit binder _, _) = allocateTopValue binder >> pure ()
    allocateTop (StgTopLifted binding, _) = mapM_ allocateTopValue (bindingBinders binding) >> pure ()

projectModule :: PreparedModule -> P [Group TopBinding]
projectModule = mapM (projectTop . fst) . pmBindings

registerBindingArities :: CgStgTopBinding -> P ()
registerBindingArities (StgTopStringLit _ _) = pure ()
registerBindingArities (StgTopLifted binding) = registerBindingEntryArities binding

registerBindingEntryArities :: CgStgBinding -> P ()
registerBindingEntryArities (StgNonRec binder rhs) = registerRhsEntryArity binder rhs
registerBindingEntryArities (StgRec pairs) =
  mapM_ (uncurry registerRhsEntryArity) pairs

registerRhsEntryArity :: Id -> CgStgRhs -> P ()
registerRhsEntryArity binder (StgRhsClosure _ _ _ parameters _ _) =
  modify' (\current -> current
    { entryArities = extendVarEnv (entryArities current) binder (length parameters) })
registerRhsEntryArity _ _ = pure ()

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
  formatting <- formattingSpecFor binder
  let project = case deferredFunction binder of
        Just deferred -> projectDeferredRhs binder deferred rhs
        Nothing -> maybe (projectRhs binder rhs)
          (\spec -> projectFormattingRhs spec rhs) formatting
  TopBinding symbol <$> (HeapBinding <$> requireTopValue binder <*> project)

formattingSpecFor :: Id -> P (Maybe FormattingSpec)
formattingSpecFor binder = do
  authority <- gets formattingAuthority
  case authority of
    Nothing -> pure Nothing
    Just owner -> case classifyFormatting owner binder of
      Left failure -> failShape (Text.pack (show failure))
      Right result -> pure result

-- A registered wrapper is a normal function top. The source body has already
-- established non-bottoming demand facts in GHC; only its dependencies and
-- executable body are replaced at this projection boundary.
projectFormattingRhs :: FormattingSpec -> CgStgRhs -> P HeapRhs
projectFormattingRhs spec (StgRhsClosure _ _ ReEntrant parameters _ resultType) = withScope $ do
  let expected = case formattingKind spec of
        RenderDouble -> [LiftedRefRep]
        RenderDoublePrec -> [LiftedRefRep, LiftedRefRep]
  actual <- concat <$> mapM (argumentRepsForType . varType) parameters
  result <- repsForType resultType
  unless (actual == expected && result == [LiftedRefRep])
    (failRepresentation "registered formatting wrapper has unexpected prepared entry reps")
  textConstructor <- internConstructor (formattingTextConstructor spec)
  textFields <- concat <$> mapM (repsForType . scaledThing)
    (dataConRepArgTys (formattingTextConstructor spec))
  unless (textFields == [UnliftedRefRep, IntRep 64, IntRep 64])
    (failRepresentation "Text constructor must contain byte array, offset, length")
  parameters' <- mapM bindValue parameters
  signature <- internSignature (Signature expected (Returns [LiftedRefRep]))
  body <- formattingBody spec textConstructor parameters'
  pure (Function signature parameters' [] body)
  where scaledThing (Scaled _ ty) = ty
projectFormattingRhs _ _ = failShape "registered formatting wrapper is not a reentrant closure"

formattingBody :: FormattingSpec -> ConstructorId -> [ValueId] -> P Expr
formattingBody spec textConstructor parameters = do
  (boxedDouble, boxedPrecedence) <- case (formattingKind spec, parameters) of
    (RenderDouble, [value]) -> pure (value, Nothing)
    (RenderDoublePrec, [precedence, value]) -> pure (value, Just precedence)
    _ -> failShape "registered formatting wrapper has unexpected prepared arity"
  enter <- internSignature (Signature [] (Returns [LiftedRefRep]))
  doubleConstructor <- internConstructor doubleDataCon
  doubleCase <- freshValue
  rawDouble <- freshValue
  let doubleAtom = Ref (Local rawDouble)
      doubleFamily = AlgebraicCase (nameSymbol "type"
        (GHC.tyConName (dataConTyCon doubleDataCon)))
  rendered <- case formattingKind spec of
    RenderDouble -> renderText textConstructor RenderDouble [doubleAtom]
    RenderDoublePrec -> do
      precedence <- maybe
        (failShape "precedence wrapper omitted its boxed Int") pure boxedPrecedence
      needSignature <- internSignature (Signature [FloatRep 64] (Returns [IntRep 64]))
      need <- internSyntheticOperation
        (Schema.IntrinsicIdentity "prepared_double_needs_precedence" Schema.CCall)
        needSignature
      decision <- freshValue
      plain <- renderText textConstructor RenderDouble [doubleAtom]
      intConstructor <- internConstructor intDataCon
      intCase <- freshValue
      rawInt <- freshValue
      negative <- renderText textConstructor RenderDoublePrec
        [Ref (Local rawInt), doubleAtom]
      let intFamily = AlgebraicCase (nameSymbol "type"
            (GHC.tyConName (dataConTyCon intDataCon)))
          forcePrecedence = Case (Enter (Ref (Local precedence)) enter)
            intCase (Returns [LiftedRefRep]) intFamily
            [Alternative (ConstructorPattern intConstructor) [rawInt] negative]
      pure (Case (Operation need [doubleAtom]) decision (Returns [IntRep 64])
        (PrimitiveCase (IntRep 64))
        [ Alternative (LiteralPattern (IntLiteral 64 (BS.replicate 8 0))) [] plain
        , Alternative DefaultPattern [] forcePrecedence ])
  pure (Case (Enter (Ref (Local boxedDouble)) enter) doubleCase
    (Returns [LiftedRefRep]) doubleFamily
    [Alternative (ConstructorPattern doubleConstructor) [rawDouble] rendered])

renderText :: ConstructorId -> FormattingIntrinsic -> [Atom] -> P Expr
renderText textConstructor kind arguments = do
  let (label, argumentReps) = case kind of
        RenderDouble -> ("prepared_render_double_bytes", [FloatRep 64])
        RenderDoublePrec -> ("prepared_render_double_prec_bytes", [IntRep 64, FloatRep 64])
  renderSignature <- internSignature (Signature argumentReps (Returns [UnliftedRefRep]))
  render <- internSyntheticOperation (Schema.IntrinsicIdentity label Schema.CCall) renderSignature
  sizeSignature <- internSignature (Signature [UnliftedRefRep] (Returns [IntRep 64]))
  size <- internSyntheticOperation (Schema.PrimOpIdentity "sizeofByteArray#") sizeSignature
  bytesCase <- freshValue
  bytesValue <- freshValue
  lengthCase <- freshValue
  lengthValue <- freshValue
  let bytes = Ref (Local bytesValue)
      text = Construct textConstructor
        [bytes, Scalar (IntLiteral 64 (BS.replicate 8 0)), Ref (Local lengthValue)]
  pure (Case (Operation render arguments) bytesCase (Returns [UnliftedRefRep])
    MultiValueCase [Alternative DefaultPattern [bytesValue]
      (Case (Operation size [bytes]) lengthCase (Returns [IntRep 64])
        MultiValueCase [Alternative DefaultPattern [lengthValue] text])])

projectRhs :: Id -> CgStgRhs -> P HeapRhs
projectRhs binder (StgRhsClosure captures _ update parameters body resultType) = withScope $ do
  captureRefs <- mapM projectReference (dVarSetElems captures)
  parameterIds <- mapM bindValue parameters
  resultContract <- resultContractFor binder (length parameters) resultType
  projectedBody <- projectBody resultContract resultType body
  case update of
    ReEntrant -> Function <$> (internSignature =<< signatureFor parameters resultContract)
      <*> pure parameterIds <*> pure captureRefs <*> pure projectedBody
    Updatable -> do
      signature <- internSignature (Signature [] resultContract)
      pure (Thunk signature Memoize captureRefs projectedBody)
    Stg.SingleEntry -> do
      signature <- internSignature (Signature [] resultContract)
      pure (Thunk signature Schema.SingleEntry captureRefs projectedBody)
    JumpedTo -> failShape ("heap binding marked JumpedTo: " <> symbolText (idSymbol "value" binder))
projectRhs _ (StgRhsCon _ con _ _ args _) = Constructor <$> internConstructor con <*> mapM projectArg args

-- | A bottoming enclosing binding need not call a statically bottoming callee
-- (a function parameter is the ordinary counterexample). Preserve that callee's
-- concrete result convention, then discharge the enclosing no-success promise
-- with an empty case. A returned value takes the typed integrity path, never a
-- fabricated successful result. Runtime-polymorphic dead ends still require
-- their own callee evidence; no representation is guessed for them.
projectBody :: ResultContract -> Type -> CgStgExpr -> P Expr
projectBody NoSuccess resultType body
  | Just _ <- typePrimRep_maybe resultType = do
      results <- Returns <$> repsForType resultType
      expression <- projectExpr results body
      binder <- freshValue
      pure (Case expression binder results MultiValueCase [])
projectBody expected _ body = projectExpr expected body

projectExpr :: ResultContract -> CgStgExpr -> P Expr
projectExpr expected (StgApp function args) = do
  knownJoins <- gets joins
  case lookupVarEnv knownJoins function of
    Just join -> Jump join <$> mapM projectArg args
    Nothing -> case args of
      [] -> do
        deadEnd <- deadEndApplicationSaturated function args
        if deadEnd
          then do
            callee <- Ref <$> projectReference function
            signature <- internSignature (Signature [] NoSuccess)
            pure (Enter callee signature)
          else do
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
        deadEnd <- deadEndApplicationSaturated function args
        signature <- if deadEnd
          then internSignature =<< signatureForArgsNoSuccess args
          else internSignature =<< signatureForApplication args expected
        pure (Call callee signature projectedArgs)
projectExpr _ (StgLit literal) = Return . pure <$> projectLiteralAtom literal
projectExpr _ (StgConApp con _ args _)
  | isUnboxedTupleDataCon con = Return <$> mapM projectArg args
  | otherwise = Construct <$> internConstructor con <*> mapM projectArg args
projectExpr _ (StgOpApp (StgPrimOp TagToEnumOp) args resultType) =
  projectTagToEnum args resultType
projectExpr _ (StgOpApp (StgPrimOp primop) args _)
  | primop `elem` [RaiseOp, RaiseDivZeroOp, RaiseUnderflowOp] = do
    signature <- internSignature =<< signatureForArgsNoSuccess args
    Operation <$> internOperation (StgPrimOp primop) signature <*> mapM projectArg args
projectExpr _ (StgOpApp op args resultType) = do
  signature <- internSignature =<< signatureForArgs args resultType
  Operation <$> internOperation op signature <*> mapM projectArg args
projectExpr expected (StgCase scrutinee binder altType alts) = do
  scrutineeResults <- case alts of
    [] | typePrimRep_maybe (varType binder) == Nothing -> pure NoSuccess
    _ -> Returns <$> repsForType (varType binder)
  projectedScrutinee <- projectExpr scrutineeResults scrutinee
  kind <- projectCaseKind altType
  (identity, alternatives) <- withScope $ do
    identity <- bindValue binder
    alternatives <- mapM (projectAlt expected altType) alts
    pure (identity, alternatives)
  pure (Case projectedScrutinee identity scrutineeResults kind alternatives)
projectExpr expected (StgLet _ binding body) = withScope $
  Let <$> projectLocalGroup binding <*> projectExpr expected body
projectExpr expected (StgLetNoEscape _ binding body) = withScope $
  LetJoins <$> projectJoinGroup binding <*> projectExpr expected body
projectExpr expected (StgTick _ body) = projectExpr expected body

-- | GHC supplies the complete enumeration through the result type. Lower its
-- zero-based tag to ordinary classified cases, never reconstruct a family from
-- constructors encountered elsewhere. An invalid tag takes the typed case-failure
-- path; there is no fabricated default constructor.
projectTagToEnum :: [StgArg] -> Type -> P Expr
projectTagToEnum [argument] resultType = do
  family <- case splitTyConApp_maybe resultType of
    Just (tycon, _) | GHC.isEnumerationTyCon tycon -> pure tycon
    _ -> failRepresentation "tagToEnum# requires GHC enumeration result evidence"
  bits <- gets (targetWordWidth . target)
  actual <- argumentRepsForType $ case argument of
    StgVarArg value -> varType value
    StgLitArg literal -> literalType literal
  unless (actual == [IntRep bits])
    (failRepresentation "tagToEnum# requires a machine Int argument")
  atom <- projectArg argument
  binder <- freshValue
  alternatives <- forM (GHC.tyConDataCons family) $ \constructor -> do
    identity <- internConstructor constructor
    let tag = toInteger (dataConTag constructor) - 1
    pure (Alternative (LiteralPattern (IntLiteral bits (integerBytes bits tag)))
      [] (Construct identity []))
  pure (Case (Return [atom]) binder (Returns [IntRep bits])
    (PrimitiveCase (IntRep bits)) alternatives)
projectTagToEnum _ _ = failRepresentation "tagToEnum# requires exactly one argument"

projectCaseKind :: AltType -> P CaseKind
projectCaseKind (AlgAlt tycon) = pure (AlgebraicCase (nameSymbol "type" (GHC.tyConName tycon)))
projectCaseKind (PrimAlt rep) = PrimitiveCase <$> projectRep rep
projectCaseKind (MultiValAlt _) = pure MultiValueCase
projectCaseKind PolyAlt = pure PolymorphicCase

projectAlt :: ResultContract -> AltType -> CgStgAlt -> P Alternative
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
  registerRhsEntryArity binder rhs
  projectedRhs <- projectRhs binder rhs
  identity <- bindValue binder
  pure (NonRecursive (HeapBinding identity projectedRhs))
projectLocalGroup (StgRec pairs) = do
  mapM_ (uncurry registerRhsEntryArity) pairs
  identities <- mapM (bindValue . fst) pairs
  Recursive <$> forM (zip identities pairs) (\(identity, (binder, rhs)) ->
    HeapBinding identity <$> projectRhs binder rhs)

projectJoinGroup :: CgStgBinding -> P (Group JoinBinding)
projectJoinGroup (StgNonRec binder rhs) = do
  registerRhsEntryArity binder rhs
  identity <- freshJoin
  projected <- projectJoin identity binder rhs
  modify' (\current -> current { joins = extendVarEnv (joins current) binder identity })
  pure (NonRecursive projected)
projectJoinGroup (StgRec pairs) = do
  mapM_ (uncurry registerRhsEntryArity) pairs
  identities <- mapM (bindJoin . fst) pairs
  Recursive <$> forM (zip identities pairs) (\(identity, (binder, rhs)) ->
    projectJoin identity binder rhs)

projectJoin :: JoinId -> Id -> CgStgRhs -> P JoinBinding
projectJoin identity binder (StgRhsClosure _ _ JumpedTo parameters body resultType) = withScope $ do
  resultContract <- resultContractFor binder (length parameters) resultType
  JoinBinding identity <$> (internSignature =<< signatureFor parameters resultContract)
    <*> mapM bindValue parameters
    <*> projectBody resultContract resultType body
projectJoin _ binder _ = failShape
  ("let-no-escape binding lacks JumpedTo form: " <> symbolText (idSymbol "join" binder))

projectArg :: StgArg -> P Atom
projectArg (StgVarArg binder) = do
  reps <- repsForType (varType binder)
  if null reps then pure Void else Ref <$> projectReference binder
projectArg (StgLitArg literal) = projectLiteralAtom literal

projectReference :: Id -> P ValueRef
projectReference binder | Just kind <- wiredInErrorKind binder =
  Local <$> internWiredInError binder kind
projectReference binder | Just deferred <- deferredFunction binder =
  Local <$> deferredFunctionReference binder deferred
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
        Nothing -> case nullaryWorkerConstructor binder of
          Just con -> Local <$> internNullaryWorker binder con
          Nothing -> Global <$> internGlobal binder

deferredFunctionReference :: Id -> DeferredFunction -> P ValueId
deferredFunctionReference binder deferred = do
  topNames <- gets topSymbols
  tops <- gets topValues
  case lookupVarEnv topNames binder of
    Just symbol -> case Map.lookup symbol tops of
      Just identity -> pure identity
      Nothing -> lift (Left (MissingPreparedTop symbol))
    Nothing -> internDeferredFunction binder deferred

projectDeferredRhs :: Id -> DeferredFunction -> CgStgRhs -> P HeapRhs
projectDeferredRhs binder deferred
    (StgRhsClosure _ _ ReEntrant parameters _ resultType) = withScope $ do
  result <- resultContractFor binder (length parameters) resultType
  actual <- signatureFor parameters result
  requireDeferredSignature binder deferred (Just actual)
  parameterIds <- mapM bindValue parameters
  deferredFunctionRhs deferred parameterIds
projectDeferredRhs binder deferred _ = do
  requireDeferredSignature binder deferred Nothing
  failShape "unreachable deferred function signature check"

internDeferredFunction :: Id -> DeferredFunction -> P ValueId
internDeferredFunction binder deferred = do
  let symbol = idSymbol "value" binder
  existing <- gets (Map.lookup symbol . implicitValues)
  case existing of
    Just identity -> pure identity
    Nothing -> do
      (actual, _) <- importedEntry binder
      requireDeferredSignature binder deferred actual
      identity <- freshValue
      parameters <- mapM (const freshValue)
        (signatureArguments (deferredSignature deferred))
      rhs <- deferredFunctionRhs deferred parameters
      modify' (\current -> current
        { implicitValues = Map.insert symbol identity (implicitValues current)
        , implicitTops = TopBinding symbol (HeapBinding identity rhs) : implicitTops current
        })
      pure identity

requireDeferredSignature
  :: Id -> DeferredFunction -> Maybe Signature -> P ()
requireDeferredSignature binder deferred actual =
  unless (actual == Just (deferredSignature deferred))
    (lift (Left (DeferredFunctionSignatureMismatch (idSymbol "value" binder)
      (deferredSignature deferred) actual)))

deferredFunctionRhs :: DeferredFunction -> [ValueId] -> P HeapRhs
deferredFunctionRhs deferred parameters = do
  let signatureValue = deferredSignature deferred
      arguments = zipWith deferredArgument
        (signatureArguments signatureValue) parameters
  signature <- internSignature signatureValue
  operation <- internSyntheticOperation
    (Schema.CapabilityIdentity (deferredCapability deferred)) signature
  pure (Function signature parameters [] (Operation operation arguments))

deferredArgument :: RuntimeRep -> ValueId -> Atom
deferredArgument VoidRep _ = Void
deferredArgument _ identity = Ref (Local identity)

-- | Synthesized functions preserve bare references and partial application;
-- failure occurs only upon saturation, through an ordinary operation body.
internWiredInError :: Id -> Schema.WiredInErrorKind -> P ValueId
internWiredInError binder kind = do
  let symbol = idSymbol "value" binder
  existing <- gets (Map.lookup symbol . implicitValues)
  case existing of
    Just identity -> pure identity
    Nothing -> do
      identity <- freshValue
      parameters <- if kind == Schema.WiredAbsentSumField then pure []
        else pure <$> freshValue
      signature <- internSignature (Signature
        (map (const AddressRep) parameters) NoSuccess)
      operation <- internSyntheticOperation (Schema.WiredInErrorIdentity kind) signature
      let rhs = Function signature parameters []
            (Operation operation (map (Ref . Local) parameters))
      modify' (\current -> current
        { implicitValues = Map.insert symbol identity (implicitValues current)
        , implicitTops = TopBinding symbol (HeapBinding identity rhs) : implicitTops current
        })
      pure identity

-- | A genuinely nullary data-con worker denotes an evaluated object, not an
-- executable import. Requiring no representation arguments also excludes
-- workers whose logical Void arguments still require application.
nullaryWorkerConstructor :: Id -> Maybe DataCon
nullaryWorkerConstructor binder = do
  con <- isDataConWorkId_maybe binder
  if dataConRepArity con == 0 && null (dataConRepArgTys con)
      && not (isUnboxedTupleDataCon con)
    then Just con
    else Nothing

-- | Materialize one ordinary constructor top per authoritative worker identity.
-- These field-free objects precede source tops and need no body recovery.
internNullaryWorker :: Id -> DataCon -> P ValueId
internNullaryWorker binder con = do
  let symbol = idSymbol "value" binder
  existing <- gets (Map.lookup symbol . implicitValues)
  case existing of
    Just identity -> pure identity
    Nothing -> do
      collision <- gets (Map.member symbol . topValues)
      if collision
        then failIdentity ("constructor worker collides with prepared top: " <> symbolText symbol)
        else pure ()
      constructor <- internConstructor con
      identity <- freshValue
      modify' (\current -> current
        { implicitValues = Map.insert symbol identity (implicitValues current)
        , implicitTops = TopBinding symbol (HeapBinding identity (Constructor constructor []))
            : implicitTops current
        })
      pure identity

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
  savedEntryArities <- gets entryArities
  result <- action
  modify' (\current -> current
    { values = savedValues, joins = savedJoins
    , entryArities = savedEntryArities })
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
          reps <- case (typePrimRep_maybe (varType externalBinder), importedIdLFInfo externalBinder) of
            (Nothing, LFThunk{}) -> do
              bottomingThunk <- deadEndApplicationSaturated externalBinder []
              if bottomingThunk
                then pure [LiftedRefRep]
                else repsForType (varType externalBinder)
            _ -> repsForType (varType externalBinder)
          rep <- case reps of
            [] -> pure VoidRep
            [single] -> pure single
            _ -> failRepresentation "global value has more than one representation component"
          (entry, evaluated) <- importedEntry externalBinder
          signature <- traverse internSignature entry
          existing <- gets globalDecls
          generations <- gets retainedGenerations
          let identity = GlobalId (fromIntegral (length existing))
              symbol = idSymbol "value" externalBinder
              retainedGeneration = Map.lookup symbol generations
              declaration = GlobalDecl symbol rep signature evaluated
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
internOperation op signature = do
  operationSignature <- signatureForId signature
  operationIdentity <- case op of
    StgPrimOp GetCurrentCCSOp
      | operationSignature == Signature [LiftedRefRep, VoidRep] (Returns [AddressRep]) ->
          pure (Schema.CapabilityIdentity "ghc:getCurrentCCS")
    StgPrimOp primop -> pure (Schema.PrimOpIdentity
      (Text.pack (occNameString (primOpOcc primop))))
    -- ghc-internal's rounding helper is an external C implementation, not an
    -- interface body. Preserve its exact target and convention; native
    -- admission independently checks the same signature before lowering it.
    StgFCallOp (Foreign.CCall (Foreign.CCallSpec
      (Foreign.StaticTarget _ label _ _) Foreign.CCallConv _)) _
      | unpackFS label == "rintDouble"
      , operationSignature == Signature [FloatRep 64] (Returns [FloatRep 64])
          || operationSignature == Signature [FloatRep 64, VoidRep]
              (Returns [FloatRep 64]) ->
          pure (Schema.IntrinsicIdentity "rintDouble" Schema.CCall)
    -- GHC.CString's c_strlen is a ghc-prim foreign import with no Haskell body.
    StgFCallOp (Foreign.CCall (Foreign.CCallSpec
      (Foreign.StaticTarget _ label (Just unit) _) Foreign.CCallConv Foreign.PlayRisky)) _
      | unpackFS label == "strlen"
      , unitString unit == "ghc-prim"
      , operationSignature == Signature [AddressRep, VoidRep] (Returns [IntRep 64]) ->
          pure (Schema.IntrinsicIdentity "strlen" Schema.CCall)
    StgPrimCallOp (PrimCall label unit)
      | unitString unit == "ghc-internal"
      , unpackFS label == "stg_cloneMyStackzh"
      , operationSignature == Signature [VoidRep] (Returns [UnliftedRefRep]) ->
          pure (Schema.CapabilityIdentity "ghc:cloneMyStack")
      | unitString unit == "ghc-internal"
      , unpackFS label == "stg_decodeStackzh"
      , operationSignature == Signature [UnliftedRefRep, VoidRep] (Returns [UnliftedRefRep]) ->
          pure (Schema.CapabilityIdentity "ghc:decodeStack")
    StgFCallOp (Foreign.CCall (Foreign.CCallSpec
      (Foreign.StaticTarget _ label (Just unit) _) Foreign.CCallConv Foreign.PlaySafe)) _
      | unitString unit == "ghc-internal"
      , unpackFS label == "lookupIPE"
      , operationSignature == Signature [AddressRep, AddressRep, VoidRep] (Returns [WordRep 8]) ->
          pure (Schema.CapabilityIdentity "ghc:lookupIPE")
    StgPrimCallOp call -> lift . Left $
      UnsupportedPrimitiveCall (Text.pack (showSDocUnsafe (ppr call))) operationSignature
    StgFCallOp call _ -> lift . Left $
      UnsupportedForeignCall (Text.pack (showSDocUnsafe (ppr call))) operationSignature
  internSyntheticOperation operationIdentity signature

internSyntheticOperation :: Schema.OperationIdentity -> SignatureId -> P OperationId
internSyntheticOperation operationIdentity signature = do
  operationSignature <- signatureForId signature
  known <- gets operations
  case find (matches operationIdentity operationSignature) known of
    Just (_, _, identity) -> pure identity
    Nothing -> do
      prior <- gets operationDecls
      let identity = OperationId (fromIntegral (length prior))
          declaration = OperationDecl operationIdentity signature
      modify' (\current -> current
        { operations = operations current <> [(operationIdentity, operationSignature, identity)]
        , operationDecls = operationDecls current <> [declaration] })
      pure identity
  where
    matches wantedIdentity wantedSignature (knownIdentity, knownSignature, _) =
      wantedIdentity == knownIdentity && wantedSignature == knownSignature

signatureForId :: SignatureId -> P Signature
signatureForId identity = do
  known <- gets signatures
  case find ((== identity) . snd) known of
    Just (signature, _) -> pure signature
    Nothing -> failIdentity "operation refers to an unknown signature"

signatureFor :: [Id] -> ResultContract -> P Signature
signatureFor args result = Signature <$> (concat <$> mapM (argumentRepsForType . varType) args) <*> pure result

-- Imported LF information is authoritative. In its absence GHC uses positive
-- representation arity as function evidence, but never guesses a thunk from
-- zero arity. A CAF returning a function has a zero-argument entry, not all the
-- arrows in the returned function's type.
--
-- `importedIdLFInfo` is partial for GHC's wired-in unused-argument descriptor.
-- Such a zero-width argument is projected directly as Void and never reaches
-- internGlobal, so this query remains restricted to genuine imported entries.
importedEntry :: Id -> P (Maybe Signature, Bool)
importedEntry binder = case importedIdLFInfo binder of
  LFReEntrant _ arity _ _ -> do
    (arguments, result) <- splitRepArguments arity (varType binder)
    signature <- Signature arguments <$> entryResults arity result
    pure (Just signature, True)
  LFThunk{} -> do
    signature <- Signature [] <$> entryResults 0 (varType binder)
    pure (Just signature, False)
  LFCon{} -> pure (Nothing, True)
  LFUnlifted -> pure (Nothing, True)
  LFUnknown{} -> pure (Nothing, False)
  LFLetNoEscape -> failShape "imported join has no heap/global entry"
  where
    entryResults actual ty = do
      if isDeadEndId binder
        then do
          threshold <- demandRepThreshold binder
          if actual >= threshold then pure NoSuccess else Returns <$> repsForType ty
        else Returns <$> repsForType ty

signatureForArgs :: [StgArg] -> Type -> P Signature
signatureForArgs args result = Signature <$> (concat <$> mapM argReps args) <*> (Returns <$> repsForType result)
  where
    argReps (StgVarArg binder) = argumentRepsForType (varType binder)
    argReps (StgLitArg literal) = argumentRepsForType (literalType literal)

-- The STG context, not the callee's source type, says what this application
-- must produce. Arguments retain their actual unarised representations while
-- the enclosing RHS, join, or case supplies the demanded result group.
signatureForApplication :: [StgArg] -> ResultContract -> P Signature
signatureForApplication args demandedResult = Signature
  <$> (concat <$> mapM argReps args) <*> demandedResultForApplication
  where
    demandedResultForApplication = case demandedResult of
      Returns reps -> pure (Returns reps)
      NoSuccess -> failShape "demanded NoSuccess lacks callee evidence"
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

resultContractFor :: Id -> Int -> Type -> P ResultContract
resultContractFor binder actual ty
  | isDeadEndId binder = do
      threshold <- demandRepThreshold binder
      if actual >= threshold then pure NoSuccess else Returns <$> repsForType ty
  | otherwise = Returns <$> repsForType ty

signatureForArgsNoSuccess :: [StgArg] -> P Signature
signatureForArgsNoSuccess args = Signature <$> (concat <$> mapM argReps args) <*> pure NoSuccess
  where
    argReps (StgVarArg binder) = argumentRepsForType (varType binder)
    argReps (StgLitArg literal) = argumentRepsForType (literalType literal)

-- Demand signatures count source arguments. Convert those arguments through
-- the binder type so an unboxed tuple contributes all payload reps and a
-- zero-width argument contributes one Void slot.
demandRepThreshold :: Id -> P Int
demandRepThreshold binder = sourceArgumentRepSlots sourceArity (varType binder)
  where
    sourceArity = length (fst (splitDmdSig (idDmdSig binder)))

sourceArgumentRepSlots :: Int -> Type -> P Int
sourceArgumentRepSlots 0 _ = pure 0
sourceArgumentRepSlots remaining ty = case unwrapType ty of
  FunTy _ _ argument result -> do
    reps <- argumentRepsForType argument
    rest <- sourceArgumentRepSlots (remaining - 1) result
    pure (length reps + rest)
  _ -> failRepresentation "demand signature exceeds function type"

-- Bottoming evidence belongs to an entered call, not to a partial application
-- of a bottoming function. Local entries come from the actual prepared-STG
-- closure parameter list; only external entries use their imported LF arity.
deadEndApplicationSaturated :: Id -> [StgArg] -> P Bool
deadEndApplicationSaturated function args
  | not (isDeadEndId function) = pure False
  | otherwise = do
      threshold <- demandRepThreshold function
      actual <- knownEntryArity function
      pure (maybe False (\entry -> entry >= threshold && length args >= entry) actual)

knownEntryArity :: Id -> P (Maybe Int)
knownEntryArity function = do
  local <- gets (\st -> lookupVarEnv (entryArities st) function)
  case local of
    Just arity -> pure (Just arity)
    Nothing
      | isExternalName (varName function) -> pure (importedEntryArity function)
      | otherwise -> pure Nothing

importedEntryArity :: Id -> Maybe Int
importedEntryArity binder = case importedIdLFInfo binder of
  LFReEntrant _ arity _ _ -> Just arity
  LFThunk{} -> Just 0
  LFCon{} -> Nothing
  LFUnlifted -> Nothing
  LFUnknown{} -> Nothing
  LFLetNoEscape -> Nothing

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
    (if isExternalName name
       then Text.pack . unpackFS <$> fieldOcc_maybe (nameOccName name)
       else Nothing)
  Nothing -> case fallback of
    Just modul -> SymbolIdentity (Text.pack (unitString (moduleUnit modul)))
      (Text.pack (moduleNameString (moduleName modul))) namespace
      (Text.pack (occNameString (nameOccName name))) Nothing
    Nothing -> SymbolIdentity "<interactive>" "<local>" namespace
      (Text.pack (occNameString (nameOccName name))) Nothing

symbolText :: SymbolIdentity -> Text
symbolText symbol = case symbolRecordParent symbol of
  Nothing -> symbolUnit symbol <> ":" <> symbolModule symbol <> ":"
    <> symbolNamespace symbol <> ":" <> symbolOccurrence symbol
  Just parent -> symbolUnit symbol <> ":" <> symbolModule symbol <> ":"
    <> symbolNamespace symbol <> ":" <> parent <> ":" <> symbolOccurrence symbol

failShape :: Text -> P a
failShape = lift . Left . UnsupportedPreparedShape
failIdentity :: Text -> P a
failIdentity = lift . Left . InvalidPreparedIdentity
failRepresentation :: Text -> P a
failRepresentation = lift . Left . InvalidPreparedRepresentation
failLayout :: Text -> P a
failLayout = lift . Left . InvalidPreparedLayout
