{-# LANGUAGE OverloadedStrings #-}

module ExecutionProjectionTest
  ( projectProjectionContract
  , verifyRetainedImportProjection
  , verifyRetainedImportProjectionExposed
  , verifyHierarchicalTargetModule
  ) where

import Control.Monad (forM_, unless)
import Data.ByteString qualified as BS
import Data.List (nub)
import Data.Map.Strict qualified as Map
import Data.Set qualified as Set
import Data.Text qualified as Text
import GHC.Builtin.Types
  ( doubleRepDataConTy, intRepDataConTy, liftedRepTy, tupleRepDataConTyCon
  , mkPromotedListTy, runtimeRepTy, unliftedRepTy, zeroBitRepTy )
import GHC.Core.Type (mkTyConApp, splitFunTys, splitTyConApp_maybe)
import GHC.Core.DataCon (dataConName, dataConRepArity)
import GHC.Core.TyCon (tyConName)
import GHC.Types.Basic (TypeOrConstraint(TypeLike, ConstraintLike))
import GHC.Types.Literal (Literal(..))
import GHC.Types.Id (idType, isDataConWorkId_maybe)
import GHC.Types.Name (nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.Var (Id, varName)
import GHC.Unit.Module (mkModule, moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Types (stringToUnit)
import GHC.Stg.Syntax
import System.Directory (getCurrentDirectory)
import System.FilePath ((</>))
import Tidepool.PreparedStg (PreparedModule(..))
import Tidepool.PreparedFacts (PreparedFacts(..))
import Tidepool.ExecutionProjection
import Tidepool.ExecutionSchema
import Tidepool.GhcPipeline
  ( PipelineSelection(PreparedStg), PreparedPipelineResult(..), PipelineResult(..)
  , runPipelineSelected, runPipelineSelectedRetaining )
import Tidepool.PreparedFormatting
  (FormattingAuthority(..), classifyFormatting, resolveFormattingAuthority)

projectProjectionContract :: [PreparedModule] -> IO WireProgram
projectProjectionContract modules = do
  verifyPreparedFormatting
  verifyTagToEnumProjection
  topIdentityAllocationContract
  literalProjectionContract
  verifyWiredInErrorProjection
  case projectPrepared context modules of
    Left failure -> ioError (userError ("M3 projection failed: " <> show failure))
    Right program -> do
      unless (envelopeSchemaVersion (programEnvelope program) == schemaVersion)
        (ioError (userError "M3 projection used the wrong schema version"))
      unless (not (null (programBindings program)))
        (ioError (userError "M3 projection emitted no bindings"))
      unless (not (null (programGlobals program)))
        (ioError (userError "M3 projection omitted the imported package value"))
      unless (any callableReverse (programGlobals program))
        (ioError (userError "M3 projection omitted reverse's imported entry signature"))
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
      unless (all constructorTagsAreUsable (programConstructors program))
        (ioError (userError "M3 projection emitted invalid constructor tag/family facts"))
      verifyDistinctConstructorHostIds program
      verifyTopIdentityStability modules program
      verifyNullaryWorkerProjection
      unless (any groupIsRecursive (programBindings program)
        || any (groupAny (rhsIsRecursive . heapBindingRhs . topHeap)) (programBindings program))
        (ioError (userError "M3 projection omitted recursive control/data"))
      unless (programEntry program == selectedEntry program)
        (ioError (userError "M3 projection did not select the requested exact entry"))
      verifyProgramWideValueIds program
      verifyTupleArgumentCall program
      verifyDemandedApplicationResults program
      verifySpecificApplicationShapes program
      verifyTargetClosure context modules
      verifySameOccurrenceIdentity program
      verifyMissingHomeTop context modules
      verifySuiteCollisionRegression
      verifyVoidParameters program
      verifyUnboxedReturn program
      verifyRintDoubleStateToken
      verifyCStringLengthProjection
      verifyTextMemchrProjection
      verifyBottomingSentinelContracts
      verifySmallArrayOperationContracts
      verifyByteArrayOperationContracts
      case projectPrepared context [] of
        Left (UnsupportedPreparedShape _) -> pure ()
        other -> ioError (userError ("empty program did not produce typed rejection: " <> show other))
      pure program
  where
    context = ProjectionContext { projectionProfile = "ghc-9.12-prepared-stg", projectionToolchain = "ghc-9.12.2", projectionTarget = (TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []), projectionRetainedGenerations = Map.empty, projectionEntry = (SymbolIdentity "main" "M3Vertical" "value" "result" Nothing), projectionFormattingAuthority = Nothing, projectionTextUnit = Nothing }

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
    callableReverse global = symbolOccurrence (globalIdentity global) == "reverse"
      && globalEntrySignature global /= Nothing

    topHeap (TopBinding _ binding) = binding
    groupIsRecursive Recursive{} = True
    groupIsRecursive _ = False
    groupAny predicate group = any predicate (groupItems group)
    constructorTagsAreUsable constructor = constructorTag constructor > 0
      && constructorTag constructor <= constructorFamilySize constructor
    rhsIsRecursive (Function _ _ _ body) = exprIsRecursive body
    rhsIsRecursive (Thunk _ _ _ body) = exprIsRecursive body
    rhsIsRecursive Constructor{} = False
    rhsIsRecursive Bytes{} = False
    exprIsRecursive LetJoins{} = True
    exprIsRecursive (Let group body) = groupAny (rhsIsRecursive . heapBindingRhs) group
      || exprIsRecursive body
    exprIsRecursive (Case scrutinee _ _ _ alternatives) = exprIsRecursive scrutinee
      || any (\(Alternative _ _ body) -> exprIsRecursive body) alternatives
    exprIsRecursive _ = False

verifyWiredInErrorProjection :: IO ()
verifyWiredInErrorProjection = do
  root <- getCurrentDirectory
  prepared <- runPipelineSelected PreparedStg
    (root </> "test-prepared-stg" </> "WiredInErrorProjection.hs")
    [root </> "test-prepared-stg"]
  mapM_ (verifyFailureProjection (pprModules prepared))
    ["patternPartial", "bareWired", "papWired", "transitiveWired"]
  mapM_ (verifyShadowProjection (pprModules prepared))
    [ ("shadowedDefinition", entry "shadowedDefinition" Nothing)
    , ("record selector", entry "patError" (Just "Shadow"))
    ]
  where
    entry occurrence parent =
      SymbolIdentity "main" "WiredInErrorProjection" "value" occurrence parent

    projectEntry modules label identity = case projectPreparedTarget
      (ProjectionContext { projectionProfile = "ghc-9.12-prepared-stg", projectionToolchain = "ghc-9.12.2", projectionTarget = (TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []), projectionRetainedGenerations = Map.empty, projectionEntry = identity, projectionFormattingAuthority = Nothing, projectionTextUnit = Nothing })
      modules of
        Left failure -> ioError (userError
          ("wired-in error projection failed for " <> Text.unpack label
            <> ": " <> show failure))
        Right program -> pure program

    verifyFailureProjection modules occurrence = do
      program <- projectEntry modules occurrence (entry occurrence Nothing)
      let wired =
            [ (OperationId (fromIntegral index), declaration,
                signatureAt program (operationSignature declaration))
            | (index, declaration@OperationDecl
                {operationIdentity = WiredInErrorIdentity kind})
                <- zip [0 :: Int ..] (programOperations program)
            , kind == WiredPatternMatch
            ]
          synthetic =
            [ (symbol, binding, signatureAt program signature)
            | group <- programBindings program
            , TopBinding symbol binding@(HeapBinding _ (Function signature _ _ _))
                <- groupItems group
            , symbolOccurrence symbol == "patError"
            , symbolModule symbol == "GHC.Internal.Control.Exception.Base"
            ]
          patErrorGlobals =
            [ globalIdentity global
            | global <- programGlobals program
            , symbolOccurrence (globalIdentity global) == "patError"
            ]
      unless ([signature | (_, _, signature) <- wired]
          == [Signature [AddressRep] NoSuccess])
        (ioError (userError
          ("wired-in patError operation lost its saturated contract for "
            <> Text.unpack occurrence <> ": " <> show wired)))
      case (wired, synthetic) of
        ([(operation, declaration, _)],
          [(_, HeapBinding _ (Function signature [parameter] [] body), entrySignature)]) -> do
          unless (entrySignature == Signature [AddressRep] NoSuccess)
            (ioError (userError "synthesized patError top lost its entry contract"))
          unless (body == Operation operation [Ref (Local parameter)])
            (ioError (userError
              ("synthesized patError top did not call its wired operation: " <> show body)))
          unless (signatureResults entrySignature == NoSuccess
              && signature == operationSignature declaration)
            (ioError (userError "synthesized patError top and operation signatures diverged"))
        found -> ioError (userError
          ("expected one wired operation and synthesized patError function top, got "
            <> show found))
      unless (null patErrorGlobals)
        (ioError (userError
          ("wired-in patError leaked into globals: " <> show patErrorGlobals)))

    verifyShadowProjection modules (label, selectedIdentity) = do
      program <- projectEntry modules label selectedIdentity
      unless (null
          [ operationIdentity
          | OperationDecl operationIdentity _ <- programOperations program
          , WiredInErrorIdentity{} <- [operationIdentity]
          ])
        (ioError (userError
          ("shadowed patError spelling was classified as wired-in for "
            <> Text.unpack label)))

    signatureAt program (SignatureId index) =
      programSignatures program !! fromIntegral index
    groupItems (NonRecursive item) = [item]
    groupItems (Recursive items) = items

topIdentityAllocationContract :: IO ()
topIdentityAllocationContract = do
  let symbol modul namespace occurrence = SymbolIdentity "unit" modul namespace occurrence Nothing
      assigned = assignTopIdentitySpellings
        [ (symbol "A" "local" "sat", False)
        , (symbol "A" "local" "sat.1", False)
        , (symbol "A" "local" "sat", False)
        , (symbol "B" "local" "sat", False)
        , (symbol "A" "value" "external", True)
        , (symbol "A" "value" "external", False)
        ]
      identities = [(symbolModule value, symbolNamespace value, symbolOccurrence value)
                   | value <- assigned]
  unless (identities ==
      [("A", "local", "sat"), ("A", "local", "sat.1")
      ,("A", "local", "sat.2"), ("B", "local", "sat")
      ,("A", "value", "external"), ("A", "value", "external.1")])
    (ioError (userError
      ("internal top identity suffixing was not deterministic: " <> show assigned)))

  let internal = symbol "A" "local" "reverse"
      external = symbol "A" "value" "reverse"
      retained = Map.singleton internal 7
  unless (Map.notMember external retained)
    (ioError (userError
      "internal and external same-spelled identities shared retained-generation state"))

verifyTopIdentityStability :: [PreparedModule] -> WireProgram -> IO ()
verifyTopIdentityStability modules program = do
  expected <- case preparedTopIdentities modules of
    Left failure -> ioError (userError
      ("prepared top identity enumeration failed: " <> show failure))
    Right identities -> pure identities
  let actual =
        [ top
        | group <- programBindings program
        , top <- groupItems group
        ]
      implicitCount = length actual - length expected
      (implicit, original) = splitAt (max 0 implicitCount) actual
      actualOriginal = map topSymbol original
  unless (implicitCount >= 0 && expected == actualOriginal)
    (ioError (userError
      ("projection changed prepared top identities after implicit prefix: "
        <> show (expected, map topSymbol actual))))
  mapM_ (verifyImplicitConstructor program) implicit
  where
    groupItems (NonRecursive item) = [item]
    groupItems (Recursive items) = items
    topSymbol (TopBinding symbol _) = symbol

verifyImplicitConstructor :: WireProgram -> TopBinding -> IO ()
verifyImplicitConstructor program (TopBinding _ (HeapBinding _ rhs)) = case rhs of
  Constructor constructor atoms -> do
    unless (null atoms)
      (ioError (userError "implicit constructor top unexpectedly retained fields"))
    case constructorAt constructor of
      Nothing -> ioError (userError "implicit constructor top referenced an unknown constructor")
      Just declaration -> unless
        (null (constructorFieldReps declaration)
          && null (constructorStrictFields declaration)
          && null (layoutFields (constructorLayout declaration)))
        (ioError (userError "implicit constructor top was not field-free"))
  _ -> ioError (userError "implicit top was not an actual constructor object")
  where
    constructorAt (ConstructorId index) = case
      drop (fromIntegral index) (programConstructors program) of
        declaration : _ -> Just declaration
        [] -> Nothing

verifyNullaryWorkerProjection :: IO ()
verifyNullaryWorkerProjection = do
  root <- getCurrentDirectory
  prepared <- runPipelineSelected PreparedStg
    (root </> "test-prepared-stg" </> "NullaryWorkers.hs")
    [root </> "test-prepared-stg"]
  assertNullaryWorkerReferences prepared
  let context = ProjectionContext { projectionProfile = "ghc-9.12-prepared-stg", projectionToolchain = "ghc-9.12.2", projectionTarget = (TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []), projectionRetainedGenerations = Map.empty, projectionEntry = (SymbolIdentity "main" "NullaryWorkers" "value" "result" Nothing), projectionFormattingAuthority = Nothing, projectionTextUnit = Nothing }
  program <- case projectPreparedTarget context (pprModules prepared) of
    Left failure -> ioError (userError ("nullary worker projection failed: " <> show failure))
    Right value -> pure value
  let nonNullaryConstructors =
        [ occNameString (nameOccName (dataConName constructor))
        | module_ <- pprModules prepared
        , (constructor, _) <- preparedConstructors (pmFacts module_)
        , dataConRepArity constructor > 0
        ]
      implicit =
        [ (symbol, binding)
        | NonRecursive (TopBinding symbol binding) <- programBindings program
        , symbolNamespace symbol == "value"
        , symbolOccurrence symbol `elem` ["[]", "True", "Nothing"]
        ]
      matching occurrence =
        [ binding
        | (symbol, binding) <- implicit
        , symbolOccurrence symbol == occurrence
        ]
      globals = map (symbolOccurrence . globalIdentity) (programGlobals program)
      topOccurrences =
        [ symbolOccurrence symbol
        | group <- programBindings program
        , top <- groupItems group
        , TopBinding symbol _ <- [top]
        ]
  unless (length implicit == 3 && all ((== 1) . length . matching) ["[]", "True", "Nothing"])
    (ioError (userError ("nullary workers were not interned exactly once: " <> show
      [(symbolOccurrence symbol, heapBindingRhs binding) | (symbol, binding) <- implicit])))
  unless (all (fieldFree . snd) implicit)
    (ioError (userError "nullary worker implicit top was not field-free"))
  unless (all (`notElem` globals) ["[]", "True", "Nothing"])
    (ioError (userError ("nullary workers leaked into imported globals: " <> show globals)))
  unless (":" `elem` nonNullaryConstructors)
    (ioError (userError "nullary fixture did not retain a non-nullary constructor worker"))
  unless (":" `notElem` topOccurrences)
    (ioError (userError "non-nullary constructor worker was incorrectly materialized as an implicit top"))
  where
    fieldFree (HeapBinding _ (Constructor _ [])) = True
    fieldFree _ = False
    groupItems (NonRecursive item) = [item]
    groupItems (Recursive items) = items

assertNullaryWorkerReferences :: PreparedPipelineResult -> IO ()
assertNullaryWorkerReferences prepared = do
  let occurrences =
        [ occNameString (nameOccName (varName binder))
        | module_ <- pprModules prepared
        , binder <- preparedReferencedIds (pmFacts module_)
        , Just _ <- [isDataConWorkId_maybe binder]
        ]
  unless (all (`elem` occurrences) ["[]", "True", "Nothing"])
    (ioError (userError
      ("nullary fixture did not retain typed worker references: "
        <> show occurrences)))

verifyDistinctConstructorHostIds :: WireProgram -> IO ()
verifyDistinctConstructorHostIds program = unless
  (all distinctHostId sameTagFamilies)
  (ioError (userError
    "M3 projection collapsed distinct same-tag constructor identities"))
  where
    constructors = programConstructors program
    sameTagFamilies =
      [ (left, right)
      | left <- constructors
      , right <- constructors
      , constructorTag left == constructorTag right
      , constructorFamily left /= constructorFamily right
      ]
    distinctHostId (left, right) = constructorHostId left /= constructorHostId right

-- GHC may reuse an Id unique in unrelated top-level RHSs. Every semantic
-- binder, including parameters and alternatives, still needs its own wire ID.
verifyProgramWideValueIds :: WireProgram -> IO ()
verifyProgramWideValueIds program = unless (length binders == length (nub binders))
  (ioError (userError "M3 projection reused a ValueId across semantic binders"))
  where
    binders = concatMap topGroup (programBindings program)
    topGroup (NonRecursive top) = topBinding top
    topGroup (Recursive tops) = concatMap topBinding tops
    topBinding (TopBinding _ binding) = heapBinding binding
    heapBinding (HeapBinding identity rhs) = identity : rhsBinders rhs
    rhsBinders (Function _ parameters _ body) = parameters <> exprBinders body
    rhsBinders (Thunk _ _ _ body) = exprBinders body
    rhsBinders Constructor{} = []
    rhsBinders Bytes{} = []
    exprBinders (Case scrutinee identity _ _ alternatives) =
      exprBinders scrutinee <> [identity] <> concatMap alternativeBinders alternatives
    exprBinders (Let group body) = heapGroup group <> exprBinders body
    exprBinders (LetJoins group body) = joinGroup group <> exprBinders body
    exprBinders _ = []
    alternativeBinders (Alternative _ parameters body) = parameters <> exprBinders body
    heapGroup (NonRecursive binding) = heapBinding binding
    heapGroup (Recursive bindings) = concatMap heapBinding bindings
    joinGroup (NonRecursive binding) = joinBinding binding
    joinGroup (Recursive bindings) = concatMap joinBinding bindings
    joinBinding (JoinBinding _ _ parameters body) = parameters <> exprBinders body

literalProjectionContract :: IO ()
literalProjectionContract = do
  expect "null address" LitNullAddr (Scalar NullAddressLiteral)
  expect "target-width character" (LitChar 'A')
    (Scalar (WordLiteral 64 (BS.pack [0,0,0,0,0,0,0,65])))
  expect "type rubbish" (LitRubbish TypeLike intRepDataConTy) (Rubbish (IntRep 64))
  expect "constraint rubbish" (LitRubbish ConstraintLike intRepDataConTy) (Rubbish (IntRep 64))
  expect "lifted rubbish" (LitRubbish TypeLike liftedRepTy) (Rubbish LiftedRefRep)
  expect "unlifted rubbish" (LitRubbish TypeLike unliftedRepTy) (Rubbish UnliftedRefRep)
  reject "zero-width rubbish" (LitRubbish TypeLike zeroBitRepTy)
  reject "multi-component rubbish" (LitRubbish TypeLike tupleRep)
  where
    target = TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []
    expect label literal expected = unless
      (projectLiteralAtomForTest target literal == Right expected)
      (ioError (userError ("M3 projection did not retain " <> label)))
    reject label literal = case projectLiteralAtomForTest target literal of
      Left (InvalidPreparedRepresentation _) -> pure ()
      other -> ioError (userError ("M3 projection did not reject " <> label <> ": " <> show other))
    tupleRep = mkTyConApp tupleRepDataConTyCon
      [mkPromotedListTy runtimeRepTy [intRepDataConTy, doubleRepDataConTy]]

verifyTupleArgumentCall :: WireProgram -> IO ()
verifyTupleArgumentCall program = case
  [ body
  | group <- programBindings program
  , TopBinding symbol binding <- groupItems group
  , symbolOccurrence symbol == "tupleArgumentUse"
  , body <- [heapBody binding]
  ] of
    [body] -> unless (hasExpandedTupleCall body)
      (ioError (userError "M3 projection did not retain the expanded tuple argument call"))
    bodies -> ioError (userError ("expected one tupleArgumentUse binding, got " <> show (length bodies)))
  where
    hasExpandedTupleCall expression = case expression of
      Call _ signature arguments -> signatureArguments (signatureAt signature) == [IntRep 64, FloatRep 64]
        && length arguments == 2
      Case scrutinee _ _ _ alternatives -> hasExpandedTupleCall scrutinee
        || any (hasExpandedTupleCall . alternativeBody) alternatives
      Let group body -> any (hasExpandedTupleCall . heapBody) (groupItems group)
        || hasExpandedTupleCall body
      LetJoins group body -> any (hasExpandedTupleCall . joinBody) (groupItems group)
        || hasExpandedTupleCall body
      _ -> False
    signatureAt (SignatureId index) = programSignatures program !! fromIntegral index
    alternativeBody (Alternative _ _ body) = body
    heapBody (HeapBinding _ (Function _ _ _ body)) = body
    heapBody (HeapBinding _ (Thunk _ _ _ body)) = body
    heapBody HeapBinding{} = Return []
    joinBody (JoinBinding _ _ _ body) = body
    groupItems (NonRecursive item) = [item]
    groupItems (Recursive items) = items

-- Calls and enters take their demanded result from the enclosing projection
-- context: RHS/join signatures, case binders for scrutinees, and that same
-- enclosing result for alternatives and bodies.
verifyDemandedApplicationResults :: WireProgram -> IO ()
verifyDemandedApplicationResults program = do
  checked <- sum . concat <$> mapM checkTop (programBindings program)
  unless (checked > 0)
    (ioError (userError "M3 projection fixture emitted no Call or Enter expression"))
  where
    signatureAt (SignatureId index) = programSignatures program !! fromIntegral index
    checkTop :: Group TopBinding -> IO [Int]
    checkTop group = mapM (checkHeap . topHeap) (groupItems group)
    topHeap (TopBinding _ binding) = binding
    checkHeap :: HeapBinding -> IO Int
    checkHeap (HeapBinding _ rhs) = case rhs of
      Function signature _ _ body -> checkExpr (signatureResults (signatureAt signature)) body
      Thunk signature _ _ body -> checkExpr (signatureResults (signatureAt signature)) body
      Constructor{} -> pure 0
      Bytes{} -> pure 0
    checkJoin :: JoinBinding -> IO Int
    checkJoin (JoinBinding _ signature _ body) =
      checkExpr (signatureResults (signatureAt signature)) body
    checkExpr :: ResultContract -> Expr -> IO Int
    checkExpr expected expression = case expression of
      Enter _ signature -> checkResult expected signature
      Call _ signature _ -> checkResult expected signature
      Case scrutinee _ binderResults _ alternatives -> do
        scrutineeCalls <- checkExpr binderResults scrutinee
        alternativeCalls <- sum <$> mapM (checkAlternative expected) alternatives
        pure (scrutineeCalls + alternativeCalls)
      Let group body -> do
        localCalls <- sum <$> mapM checkHeap (groupItems group)
        bodyCalls <- checkExpr expected body
        pure (localCalls + bodyCalls)
      LetJoins group body -> do
        joinCalls <- sum <$> mapM checkJoin (groupItems group)
        bodyCalls <- checkExpr expected body
        pure (joinCalls + bodyCalls)
      _ -> pure 0
    checkAlternative :: ResultContract -> Alternative -> IO Int
    checkAlternative expected (Alternative _ _ body) = checkExpr expected body
    checkResult :: ResultContract -> SignatureId -> IO Int
    checkResult expected signature = do
      unless (signatureResults (signatureAt signature) == expected)
        (ioError (userError "M3 projection call result did not match its demanded context"))
      pure 1
    groupItems (NonRecursive item) = [item]
    groupItems (Recursive items) = items

verifySpecificApplicationShapes :: WireProgram -> IO ()
verifySpecificApplicationShapes program = do
  polymorphicIdentity <- namedBinding "polymorphicIdentity"
  polymorphicIdentityResult <- namedBinding "polymorphicIdentityResult"
  unless (hasCall polymorphicIdentity polymorphicIdentityResult)
    (ioError (userError
      "M3 projection did not retain the oversaturated polymorphicIdentity call with its demanded Int# result"))
  where
    expectedResult = Returns [IntRep 64]
    namedBinding occurrence = case
      [ binding
      | group <- programBindings program
      , TopBinding symbol binding <- groupItems group
      , symbolOccurrence symbol == occurrence
      ] of
        [binding] -> pure binding
        bindings -> ioError (userError
          ("expected one " <> show occurrence <> " binding, got " <> show (length bindings)))
    hasCall callee binding = any expressionMatches (expressions (heapBody binding))
      where
        expressionMatches (Call (Ref (Local target)) signature arguments) =
          target == heapBindingId callee
            && length arguments == 2
            && signatureResults (signatureAt signature) == expectedResult
        expressionMatches _ = False
    expressions expression = expression : case expression of
      Case scrutinee _ _ _ alternatives -> expressions scrutinee
        <> concatMap (expressions . alternativeBody) alternatives
      Let group body -> concatMap (expressions . heapBody) (groupItems group) <> expressions body
      LetJoins group body -> concatMap (expressions . joinBody) (groupItems group) <> expressions body
      _ -> []
    signatureAt (SignatureId index) = programSignatures program !! fromIntegral index
    alternativeBody (Alternative _ _ body) = body
    heapBody (HeapBinding _ (Function _ _ _ body)) = body
    heapBody (HeapBinding _ (Thunk _ _ _ body)) = body
    heapBody HeapBinding{} = Return []
    joinBody (JoinBinding _ _ _ body) = body
    groupItems (NonRecursive item) = [item]
    groupItems (Recursive items) = items

verifyTargetClosure :: ProjectionContext -> [PreparedModule] -> IO ()
verifyTargetClosure context modules = do
  verify "polymorphicIdentityResult"
    (Set.fromList ["polymorphicIdentityResult", "polymorphicIdentity", "demandedCallee"])
  verify "sameOccurrenceResult"
    (Set.fromList ["sameOccurrenceResult", "sameOccurrenceTop"])
  where
    verify occurrence expected = case projectPreparedTarget targetContext modules of
      Left failure -> ioError (userError ("M3 target projection failed: " <> show failure))
      Right program -> do
        unless (actual program == expected)
          (ioError (userError
            ("M3 target projection for " <> show occurrence
              <> " retained the wrong top-level closure: " <> show (actual program))))
        unless (hasExactEntry program)
          (ioError (userError
            ("M3 target projection changed the exact entry identity for "
              <> show occurrence)))
      where
        targetContext = context
          { projectionEntry = SymbolIdentity "main" "M3Vertical" "value" occurrence Nothing }
        actual program = Set.fromList
          [ symbolOccurrence symbol
          | group <- programBindings program
          , TopBinding symbol _ <- groupItems group
          ]
        hasExactEntry program = any
          ((== projectionEntry targetContext) . topSymbol)
          [top | group <- programBindings program, top <- groupItems group]
        topSymbol (TopBinding symbol _) = symbol
    groupItems (NonRecursive item) = [item]
    groupItems (Recursive items) = items

-- This checks the wire reference, not merely the retained occurrence list:
-- a local binder with the same spelling must not displace the home top.
verifySameOccurrenceIdentity :: WireProgram -> IO ()
verifySameOccurrenceIdentity program = do
  top <- namedTop "sameOccurrenceTop"
  result <- namedTop "sameOccurrenceResult"
  unless (hasReferenceTo (heapBody result) (heapBindingId top))
    (ioError (userError
      "same-occurrence projection did not retain the exact home top reference"))
  where
    namedTop occurrence = case
      [ binding
      | group <- programBindings program
      , TopBinding symbol binding <- groupItems group
      , symbolOccurrence symbol == occurrence
      ] of
        [binding] -> pure binding
        bindings -> ioError (userError
          ("expected one " <> show occurrence <> " binding, got " <> show (length bindings)))
    hasReferenceTo expression expected = case expression of
      Call (Ref (Local target)) _ _ -> target == expected
      Enter (Ref (Local target)) _ -> target == expected
      Case scrutinee _ _ _ alternatives -> hasReferenceTo scrutinee expected
        || any (\alternative -> hasReferenceTo (alternativeBody alternative) expected) alternatives
      Let group body -> any (\binding -> hasReferenceTo (heapBody binding) expected) (groupItems group)
        || hasReferenceTo body expected
      LetJoins group body -> any (\binding -> hasReferenceTo (joinBody binding) expected) (groupItems group)
        || hasReferenceTo body expected
      _ -> False
    heapBody (HeapBinding _ (Function _ _ _ body)) = body
    heapBody (HeapBinding _ (Thunk _ _ _ body)) = body
    heapBody HeapBinding{} = Return []
    alternativeBody (Alternative _ _ body) = body
    joinBody (JoinBinding _ _ _ body) = body
    groupItems (NonRecursive item) = [item]
    groupItems (Recursive items) = items

verifyMissingHomeTop :: ProjectionContext -> [PreparedModule] -> IO ()
verifyMissingHomeTop context modules = case projectPrepared context stripped of
  Left (MissingPreparedTop _) -> pure ()
  Left failure -> ioError (userError
    ("absent home top produced the wrong typed rejection: " <> show failure))
  Right _ -> ioError (userError
    "absent home top was projected as an import or otherwise accepted")
  where
    stripped =
      [ prepared { pmBindings = filter (not . isMissingTop . fst) (pmBindings prepared) }
      | prepared <- modules
      ]
    isMissingTop binding = any
      ((== "sameOccurrenceTop") . occNameString . nameOccName . varName)
      (topBindersForTest binding)

verifySuiteCollisionRegression :: IO ()
verifySuiteCollisionRegression = do
  prepared <- runPipelineSelected PreparedStg "test/Suite.hs" ["lib", "test"]
  let context identity = ProjectionContext { projectionProfile = "ghc-9.12-prepared-stg", projectionToolchain = "ghc-9.12.2", projectionTarget = (TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []), projectionRetainedGenerations = mempty, projectionEntry = identity, projectionFormattingAuthority = Nothing, projectionTextUnit = Nothing }
      identity = SymbolIdentity "main" "Suite" "value" "ho_myany" Nothing
  case projectPreparedTarget (context identity) (pprModules prepared) of
    Left failure -> ioError (userError ("Suite collision repro changed: " <> show failure))
    Right program -> do
      let localSats = localSatOccurrences program
      unless (Set.size localSats >= 2
          && all (Text.isPrefixOf "sat.") (Set.toList localSats))
        (ioError (userError
          ("known Suite sat collision lost a home dependency: " <> show localSats)))
      unless (all genuineGlobal (programGlobals program))
        (ioError (userError
          "known Suite sat collision emitted a fake internal global"))
  where
    localSatOccurrences program = Set.fromList
      [ symbolOccurrence symbol
      | group <- programBindings program
      , TopBinding symbol _ <- groupItems group
      , symbolNamespace symbol == "local"
      , Text.isPrefixOf "sat." (symbolOccurrence symbol)
      ]
    groupItems (NonRecursive item) = [item]
    groupItems (Recursive items) = items
    genuineGlobal global = symbolUnit (globalIdentity global) /= "<interactive>"
      && symbolModule (globalIdentity global) /= "<local>"

verifyVoidParameters :: WireProgram -> IO ()
verifyVoidParameters program = case
  [ (signatureAt signature, parameters)
  | group <- programBindings program
  , TopBinding symbol (HeapBinding _ (Function signature parameters _ _)) <- groupItems group
  , symbolOccurrence symbol == "voidParameterPair"
  ] of
    [(signature, parameters)] -> do
      let voidParameters =
            [ parameter
            | (VoidRep, parameter) <- zip (signatureArguments signature) parameters
            ]
      unless (length voidParameters == 2 && length (nub voidParameters) == 2)
        (ioError (userError "M3 projection aliased wired-in void parameters"))
    bindings -> ioError (userError
      ("expected one voidParameterPair binding, got " <> show (length bindings)))
  where
    signatureAt (SignatureId index) = programSignatures program !! fromIntegral index
    groupItems (NonRecursive item) = [item]
    groupItems (Recursive items) = items

verifyUnboxedReturn :: WireProgram -> IO ()
verifyUnboxedReturn program = case
  [ (signatureAt signature, parameters, body)
  | group <- programBindings program
  , TopBinding symbol (HeapBinding _ (Function signature parameters _ body)) <- groupItems group
  , symbolOccurrence symbol == "returnUnboxedArgument"
  ] of
    [(signature, [parameter], Return [Ref (Local returned)])]
      | signatureResults signature == Returns [IntRep 64] && returned == parameter -> pure ()
    matches -> ioError (userError
      ("M3 projection did not return the unboxed parameter directly: " <> show matches))
  where
    signatureAt (SignatureId index) = programSignatures program !! fromIntegral index
    groupItems (NonRecursive item) = [item]
    groupItems (Recursive items) = items

verifyRintDoubleStateToken :: IO ()
verifyRintDoubleStateToken = do
  prepared <- runPipelineSelected PreparedStg
    "test-prepared-stg/RintDouble.hs" ["test-prepared-stg"]
  let identity = SymbolIdentity "main" "RintDouble" "value" "roundSimpleUp" Nothing
      context = ProjectionContext { projectionProfile = "ghc-9.12-prepared-stg", projectionToolchain = "ghc-9.12.2", projectionTarget = (TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []), projectionRetainedGenerations = mempty, projectionEntry = identity, projectionFormattingAuthority = Nothing, projectionTextUnit = Nothing }
  case projectPreparedTarget context (pprModules prepared) of
    Left failure -> ioError (userError
      ("state-bearing rintDouble projection failed: " <> show failure))
    Right program -> case
      [ signatureId
      | OperationDecl (IntrinsicIdentity "rintDouble" CCall) signatureId
          <- programOperations program
      ] of
      [signatureId] ->
        let signature = programSignatures program !! fromIntegral (unSignatureId signatureId)
        in unless (signature == Signature [FloatRep 64, VoidRep] (Returns [FloatRep 64]))
          (ioError (userError
            ("state-bearing rintDouble signature changed: " <> show signature)))
      signatures -> ioError (userError
        ("expected one state-bearing rintDouble operation, got " <> show signatures))
  where
    unSignatureId (SignatureId value) = value

verifyCStringLengthProjection :: IO ()
verifyCStringLengthProjection = do
  prepared <- runPipelineSelected PreparedStg
    "test-prepared-stg/CStringLengthProjection.hs" ["test-prepared-stg"]
  let context entry = ProjectionContext { projectionProfile = "ghc-9.12-prepared-stg", projectionToolchain = "ghc-9.12.2", projectionTarget = (TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []), projectionRetainedGenerations = mempty, projectionEntry = (SymbolIdentity "main" "CStringLengthProjection" "value" entry Nothing), projectionFormattingAuthority = Nothing, projectionTextUnit = Nothing }
  case projectPreparedTarget (context "lengthOf") (pprModules prepared) of
    Left failure -> ioError (userError
      ("GHC.CString strlen projection failed: " <> show failure))
    Right program -> case
      [ programSignatures program !! fromIntegral index
      | OperationDecl (IntrinsicIdentity "strlen" CCall) (SignatureId index)
          <- programOperations program
      ] of
      [Signature [AddressRep, VoidRep] (Returns [IntRep 64])] -> pure ()
      signatures -> ioError (userError
        ("expected exact ghc-prim strlen operation, got " <> show signatures))
  case projectPreparedTarget (context "wrongLength") (pprModules prepared) of
    Left (UnsupportedForeignCall _ (Signature [AddressRep, VoidRep] (Returns [WordRep 64]))) ->
      pure ()
    other -> ioError (userError
      ("wrong-signature strlen was not rejected: " <> show other))

verifyTextMemchrProjection :: IO ()
verifyTextMemchrProjection = do
  prepared <- runPipelineSelected PreparedStg
    "test-prepared-stg/TextMemchrProjection.hs" ["test-prepared-stg"]
  textAuthority <- resolveTextPackageUnit (prHscEnv (pprPipelineResult prepared))
  let context authority entry = ProjectionContext
        { projectionProfile = "ghc-9.12-prepared-stg"
        , projectionToolchain = "ghc-9.12.2"
        , projectionTarget = TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []
        , projectionRetainedGenerations = mempty
        , projectionEntry = SymbolIdentity "main" "TextMemchrProjection" "value" entry Nothing
        , projectionFormattingAuthority = Nothing
        , projectionTextUnit = authority
        }
  case projectPreparedTarget (context textAuthority "commaIndices") (pprModules prepared) of
    Left failure -> ioError (userError
      ("text _hs_text_memchr projection failed: " <> show failure))
    Right program -> case
      [ programSignatures program !! fromIntegral index
      | OperationDecl (IntrinsicIdentity "_hs_text_memchr" CCall) (SignatureId index)
          <- programOperations program
      ] of
      [Signature [UnliftedRefRep, WordRep 64, WordRep 64, WordRep 8, VoidRep]
        (Returns [IntRep 64])] -> pure ()
      signatures -> ioError (userError
        ("expected exact text _hs_text_memchr operation, got " <> show signatures))
  case projectPreparedTarget (context textAuthority "wrongMemchr") (pprModules prepared) of
    Left (UnsupportedForeignCall _
      (Signature [UnliftedRefRep, WordRep 64, WordRep 64, WordRep 8, VoidRep]
        (Returns [WordRep 64]))) -> pure ()
    other -> ioError (userError
      ("wrong-signature _hs_text_memchr was not rejected: " <> show other))
  case projectPreparedTarget (context Nothing "commaIndices") (pprModules prepared) of
    Left (UnsupportedForeignCall _ _) -> pure ()
    other -> ioError (userError
      ("unauthorized _hs_text_memchr was not rejected: " <> show other))

verifyBottomingSentinelContracts :: IO ()
verifyBottomingSentinelContracts = do
  prepared <- runPipelineSelected PreparedStg
    "test-prepared-stg/RaiseContract.hs" ["test-prepared-stg"]
  mapM_ (verify prepared)
    [ ("raiseDivZeroPrimitive", "raiseDivZero#")
    , ("raiseUnderflowPrimitive", "raiseUnderflow#")
    ]
  where
    verify prepared (entryName, operationName) = do
      let context = ProjectionContext { projectionProfile = "ghc-9.12-prepared-stg", projectionToolchain = "ghc-9.12.2", projectionTarget = (TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []), projectionRetainedGenerations = mempty, projectionEntry = (SymbolIdentity "main" "RaiseContract" "value" entryName Nothing), projectionFormattingAuthority = Nothing, projectionTextUnit = Nothing }
      program <- case projectPreparedTarget context (pprModules prepared) of
        Left failure -> ioError (userError
          ("bottoming sentinel projection failed: " <> show (entryName, failure)))
        Right value -> pure value
      let signatureAt (SignatureId index) =
            programSignatures program !! fromIntegral index
          operationAt (OperationId index) =
            programOperations program !! fromIntegral index
          topRhs =
            [ heapBindingRhs binding
            | group <- programBindings program
            , TopBinding symbol binding <- groupItems group
            , symbolOccurrence symbol == entryName
            ]
      case topRhs of
        [Thunk entrySignature _ _ body] ->
          verifyBody program signatureAt operationAt entrySignature body
        [Function entrySignature [] _ body] ->
          verifyBody program signatureAt operationAt entrySignature body
        found -> ioError (userError
          ("bottoming sentinel did not project to one executable top: "
            <> show (entryName, found)))
      where
        verifyBody program signatureAt operationAt entrySignature body =
          case body of
            Operation operation [Void] ->
              verifyContracts program signatureAt operationAt entrySignature operation
            Case (Operation operation [Void]) _ (Returns _) MultiValueCase [] ->
              verifyContracts program signatureAt operationAt entrySignature operation
            _ -> ioError (userError
              ("bottoming sentinel body was neither a direct operation nor its exact "
                <> "checked empty-case discharge: " <> show (entryName, body)))

        verifyContracts program signatureAt operationAt entrySignature operation = do
          unless (signatureResults (signatureAt entrySignature) == NoSuccess)
            (ioError (userError
              ("bottoming sentinel entry returned normally: " <> show entryName)))
          let OperationDecl identity operationSignature = operationAt operation
          unless (identity == PrimOpIdentity operationName
            && signatureAt operationSignature == Signature [VoidRep] NoSuccess)
            (ioError (userError
              ("bottoming sentinel operation contract changed: "
                <> show (entryName, identity, signatureAt operationSignature,
                  programOperations program))))
    groupItems (NonRecursive item) = [item]
    groupItems (Recursive items) = items

verifySmallArrayOperationContracts :: IO ()
verifySmallArrayOperationContracts = do
  prepared <- runPipelineSelected PreparedStg
    "test-prepared-stg/ArrayContract.hs" ["test-prepared-stg"]
  mapM_ (verify prepared)
    [ ("newArrayContract", "newSmallArray#",
        Signature [IntRep 64, LiftedRefRep, VoidRep] (Returns [UnliftedRefRep]))
    , ("readArrayContract", "readSmallArray#",
        Signature [UnliftedRefRep, IntRep 64, VoidRep] (Returns [LiftedRefRep]))
    , ("writeArrayContract", "writeSmallArray#",
        Signature [UnliftedRefRep, IntRep 64, LiftedRefRep, VoidRep] (Returns []))
    ]
  where
    verify prepared (entryName, operationName, expected) = do
      let context = ProjectionContext { projectionProfile = "ghc-9.12-prepared-stg", projectionToolchain = "ghc-9.12.2", projectionTarget = (TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []), projectionRetainedGenerations = mempty, projectionEntry = (SymbolIdentity "main" "ArrayContract" "value" entryName Nothing), projectionFormattingAuthority = Nothing, projectionTextUnit = Nothing }
      program <- case projectPreparedTarget context (pprModules prepared) of
        Left failure -> ioError (userError
          ("small-array projection failed: " <> show (entryName, failure)))
        Right value -> pure value
      let actual =
            [ programSignatures program !! fromIntegral index
            | OperationDecl (PrimOpIdentity name) (SignatureId index)
                <- programOperations program
            , name == operationName
            ]
      unless (actual == [expected])
        (ioError (userError
          ("GHC small-array operation signature changed: "
            <> show (entryName, operationName, expected, actual,
              programOperations program))))

-- W5_FORMATTING: this must use the registered compiler intrinsic, not recover
-- an error placeholder or lose the successful continuation as bottoming Core.
verifyPreparedFormatting :: IO ()
verifyPreparedFormatting = do
  verifyFormattingSourceAuthority
  prepared <- runPipelineSelected PreparedStg
    "test-prepared-stg/FormattingContract.hs" ["test-prepared-stg", "lib"]
  authority <- resolveFormattingAuthority (prHscEnv (pprPipelineResult prepared))
  unless (authority /= Nothing)
    (ioError (userError "W5_FORMATTING: shipped module was not resolved"))
  let context entry = ProjectionContext { projectionProfile = "ghc-9.12-prepared-stg", projectionToolchain = "ghc-9.12.2", projectionTarget = (TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []), projectionRetainedGenerations = mempty, projectionEntry = (SymbolIdentity "main" "FormattingContract" "value" entry Nothing), projectionFormattingAuthority = authority, projectionTextUnit = Nothing }
      cases =
        [ ("formatValue", ["prepared_render_double_bytes"])
        , ("continuation", ["prepared_render_double_bytes"])
        , ("positiveLazyPrecedence", ["prepared_double_needs_precedence"
              , "prepared_render_double_bytes", "prepared_render_double_prec_bytes"])
        , ("negativeZero", ["prepared_double_needs_precedence"
              , "prepared_render_double_bytes", "prepared_render_double_prec_bytes"])
        , ("partialApplication", ["prepared_double_needs_precedence"])
        , ("firstClassApplication", ["prepared_render_double_bytes"])
        ]
  forM_ cases $ \(entry, required) -> do
    let selectedContext = context (Text.pack entry)
    program <- either
      (\failure -> ioError (userError ("W5_FORMATTING " <> entry <> ": " <> show failure)))
      pure (projectPreparedTarget selectedContext (pprModules prepared))
    let names = [Text.unpack name | OperationDecl (IntrinsicIdentity name CCall) _ <- programOperations program]
    unless (all (`elem` names) required)
      (ioError (userError ("W5_FORMATTING missing intrinsic: " <> show (entry, required, names))))
    unless (all (not . sourceFormattingDependency . globalIdentity) (programGlobals program))
      (ioError (userError ("W5_FORMATTING retained a reference-body dependency: " <> entry)))
    unless (all (not . (`elem` ["show", "showsPrec", "pack"])
          . occNameString . nameOccName . varName)
          (preparedTargetReferences selectedContext (pprModules prepared)))
      (ioError (userError ("W5_FORMATTING recovery followed a replacement body: " <> entry)))
    verifyFormattingSignatures program
    if entry == "positiveLazyPrecedence" then verifyLazyPrecedenceBranch program else pure ()
  let wrappers = [binder | modul <- pprModules prepared
        , moduleNameString (moduleName (pmModule modul)) == "Tidepool.Double"
        , (binding, _) <- pmBindings modul, binder <- topBindersForTest binding
        , occNameString (nameOccName (varName binder)) == "renderDouble"]
  case (authority, wrappers) of
    (Just (FormattingAuthority owner), [binder]) -> unless
      (case classifyFormatting
        (FormattingAuthority (mkModule (stringToUnit "wrong-unit") (moduleName owner))) binder of
        Right Nothing -> True
        _ -> False)
      (ioError (userError "W5_FORMATTING same-named wrapper matched the wrong unit"))
    _ -> ioError (userError "W5_FORMATTING could not isolate wrapper and trusted owner")
  where
    sourceFormattingDependency symbol = symbolOccurrence symbol `elem`
      ["show", "showsPrec", "pack"]
    verifyFormattingSignatures program = forM_ (programOperations program) $ \operation ->
      case operation of
        OperationDecl (IntrinsicIdentity name CCall) (SignatureId index)
          | Just expected <- lookup name expectedFormattingSignatures ->
              case drop (fromIntegral index) (programSignatures program) of
                actual : _ -> unless (actual == expected)
                  (ioError (userError ("W5_FORMATTING intrinsic signature drift: " <> show (name, actual))))
                [] -> ioError (userError "W5_FORMATTING intrinsic signature missing")
        _ -> pure ()
    expectedFormattingSignatures =
      [ ("prepared_render_double_bytes", Signature [FloatRep 64] (Returns [UnliftedRefRep]))
      , ("prepared_render_double_prec_bytes", Signature [IntRep 64, FloatRep 64] (Returns [UnliftedRefRep]))
      , ("prepared_double_needs_precedence", Signature [FloatRep 64] (Returns [IntRep 64]))
      ]
    verifyLazyPrecedenceBranch program = case
      [ (parameters, body)
      | group <- programBindings program
      , TopBinding symbol (HeapBinding _ (Function _ parameters _ body)) <- case group of
          NonRecursive top -> [top]
          Recursive tops -> tops
      , symbolOccurrence symbol == "renderDoublePrec"
      ] of
      [([precedence, value], Case (Enter (Ref (Local forced)) _) _ _ _
        [Alternative _ [_] (Case (Operation _ _) _ _ (PrimitiveCase (IntRep 64))
          [Alternative (LiteralPattern (IntLiteral 64 zero)) [] plain
          , Alternative DefaultPattern [] (Case (Enter (Ref (Local forcedPrec)) _) _ _ _ _)])])]
          | forced == value && forcedPrec == precedence && zero == BS.replicate 8 0
          , not (containsEnter precedence plain) -> pure ()
      _ -> ioError (userError "W5_FORMATTING precedence was forced before the negative branch")
    containsEnter wanted expression = case expression of
      Enter (Ref (Local identity)) _ -> identity == wanted
      Case scrutinee _ _ _ alternatives -> containsEnter wanted scrutinee
        || any (\(Alternative _ _ body) -> containsEnter wanted body) alternatives
      Let group body -> any (containsEnterRhs wanted) (groupBindings group)
        || containsEnter wanted body
      _ -> False
    containsEnterRhs wanted binding = case heapBindingRhs binding of
      Function _ _ _ body -> containsEnter wanted body
      Thunk _ _ _ body -> containsEnter wanted body
      _ -> False
    groupBindings (NonRecursive binding) = [binding]
    groupBindings (Recursive bindings) = bindings

verifyFormattingSourceAuthority :: IO ()
verifyFormattingSourceAuthority = do
  let fixture = "test-prepared-stg/FormattingContract.hs"
      check source expected = do
        prepared <- runPipelineSelected PreparedStg fixture [source, "test-prepared-stg", "lib"]
        actual <- resolveFormattingAuthority (prHscEnv (pprPipelineResult prepared))
        unless ((actual /= Nothing) == expected)
          (ioError (userError ("W5_FORMATTING source authority mismatch: " <> source)))
  check "test-prepared-stg/formatting-shadow" False
  check "test-prepared-stg/formatting-copy" True
  verifyFormattingDependencyShadow

verifyFormattingDependencyShadow :: IO ()
verifyFormattingDependencyShadow = do
  prepared <- runPipelineSelected PreparedStg
    "test-prepared-stg/FormattingDependencyShadow.hs"
    ["test-prepared-stg/formatting-dependency-shadow", "test-prepared-stg", "lib"]
  authority <- resolveFormattingAuthority (prHscEnv (pprPipelineResult prepared))
  let homeShadow = [pmModule modul | modul <- pprModules prepared
        , moduleNameString (moduleName (pmModule modul)) == "Data.Text"]
      wrappers = [binder | modul <- pprModules prepared
        , moduleNameString (moduleName (pmModule modul)) == "Tidepool.Double"
        , (binding, _) <- pmBindings modul, binder <- topBindersForTest binding
        , occNameString (nameOccName (varName binder)) == "renderDouble"]
  case (authority, homeShadow, wrappers) of
    (Just trusted, [shadowOwner], [binder]) -> do
      unless (moduleUnit shadowOwner == stringToUnit "main")
        (ioError (userError "W5_FORMATTING home Data.Text shadow was not loaded"))
      unless (case classifyFormatting trusted binder of
        Right (Just _) ->
          let (_, result) = splitFunTys (idType binder)
          in case splitTyConApp_maybe result of
            Just (tycon, []) -> case nameModule_maybe (tyConName tycon) of
              Just textOwner -> moduleNameString (moduleName textOwner) == "Data.Text.Internal"
                && moduleUnit textOwner /= moduleUnit shadowOwner
              Nothing -> False
            _ -> False
        _ -> False)
        (ioError (userError "W5_FORMATTING shipped wrapper did not retain package Text"))
      let context = ProjectionContext { projectionProfile = "ghc-9.12-prepared-stg", projectionToolchain = "ghc-9.12.2", projectionTarget = (TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []), projectionRetainedGenerations = mempty, projectionEntry = (SymbolIdentity "main" "FormattingDependencyShadow" "value" "trusted" Nothing), projectionFormattingAuthority = (Just trusted), projectionTextUnit = Nothing }
      program <- either
        (\failure -> ioError (userError
          ("W5_FORMATTING dependency shadow projection failed: " <> show failure)))
        pure (projectPreparedTarget context (pprModules prepared))
      unless (any isFormattingIntrinsic (programOperations program))
        (ioError (userError "W5_FORMATTING shadowed dependency bypassed the intrinsic"))
    _ -> ioError (userError "W5_FORMATTING dependency shadow or trusted wrapper missing")
  where
    isFormattingIntrinsic (OperationDecl (IntrinsicIdentity name CCall) _) =
      name == "prepared_render_double_bytes"
    isFormattingIntrinsic _ = False

verifyTagToEnumProjection :: IO ()
verifyTagToEnumProjection = do
  prepared <- runPipelineSelected PreparedStg
    "test-prepared-stg/EnumContract.hs" ["test-prepared-stg"]
  mapM_ (verify prepared)
    [ ("colour", "Colour", [(0, "Red"), (1, "Green"), (2, "Blue")])
    , ("boolean", "Bool", [(0, "False"), (1, "True")])
    ]
  where
    verify prepared (entry, family, expected) = do
      let context = ProjectionContext { projectionProfile = "ghc-9.12-prepared-stg", projectionToolchain = "ghc-9.12.2", projectionTarget = (TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []), projectionRetainedGenerations = mempty, projectionEntry = (SymbolIdentity "main" "EnumContract" "value" entry Nothing), projectionFormattingAuthority = Nothing, projectionTextUnit = Nothing }
      program <- either
        (\failure -> ioError (userError ("tagToEnum projection failed: " <> show (entry, failure))))
        pure (projectPreparedTarget context (pprModules prepared))
      unless (all (\operation -> case operationIdentity operation of
          PrimOpIdentity "tagToEnum#" -> False
          _ -> True) (programOperations program))
        (ioError (userError ("tagToEnum remained an operation: " <> show entry)))
      let bodies =
            [ body
            | group <- programBindings program
            , TopBinding symbol (HeapBinding _ (Function _ _ _ body)) <- case group of
                NonRecursive top -> [top]
                Recursive tops -> tops
            , symbolOccurrence symbol == entry
            ]
      alternatives <- case bodies of
        [Case (Return [_]) _ (Returns [IntRep 64]) (PrimitiveCase (IntRep 64)) alts] -> pure alts
        _ -> ioError (userError ("tagToEnum did not lower to a primitive Int case: " <> show (entry, bodies)))
      let actual = [ (tag, constructor)
            | Alternative (LiteralPattern (IntLiteral 64 bytes)) []
                (Construct (ConstructorId index) []) <- alternatives
            , BS.length bytes == 8
            , let tag = fromIntegral (BS.last bytes) :: Int
            , bytes == BS.pack (replicate 7 0 <> [fromIntegral tag])
            , declaration <- take 1 (drop (fromIntegral index) (programConstructors program))
            , symbolOccurrence (constructorFamily declaration) == family
            , let constructor = symbolOccurrence (constructorIdentity declaration)
            , constructorTag declaration == fromIntegral tag + 1
            ]
      unless (length actual == length alternatives && actual == expected)
        (ioError (userError ("tagToEnum constructor family/tag mismatch: "
          <> show (entry, alternatives, actual))))

verifyByteArrayOperationContracts :: IO ()
verifyByteArrayOperationContracts = do
  prepared <- runPipelineSelected PreparedStg
    "test-prepared-stg/ByteArrayContract.hs" ["test-prepared-stg"]
  mapM_ (verify prepared)
    [ ("newByteContract", "newByteArray#",
        Signature [IntRep 64, VoidRep] (Returns [UnliftedRefRep]))
    , ("freezeByteContract", "unsafeFreezeByteArray#",
        Signature [UnliftedRefRep, VoidRep] (Returns [UnliftedRefRep]))
    , ("sizeofByteContract", "sizeofByteArray#",
        Signature [UnliftedRefRep] (Returns [IntRep 64]))
    , ("getSizeofMutableByteContract", "getSizeofMutableByteArray#",
        Signature [UnliftedRefRep, VoidRep] (Returns [IntRep 64]))
    , ("readWord8Contract", "readWord8Array#",
        Signature [UnliftedRefRep, IntRep 64, VoidRep] (Returns [WordRep 8]))
    , ("writeWord8Contract", "writeWord8Array#",
        Signature [UnliftedRefRep, IntRep 64, WordRep 8, VoidRep] (Returns []))
    , ("indexWord8Contract", "indexWord8Array#",
        Signature [UnliftedRefRep, IntRep 64] (Returns [WordRep 8]))
    , ("readIntContract", "readIntArray#",
        Signature [UnliftedRefRep, IntRep 64, VoidRep] (Returns [IntRep 64]))
    , ("writeIntContract", "writeIntArray#",
        Signature [UnliftedRefRep, IntRep 64, IntRep 64, VoidRep] (Returns []))
    , ("indexIntContract", "indexIntArray#",
        Signature [UnliftedRefRep, IntRep 64] (Returns [IntRep 64]))
    ]
  where
    verify prepared (entryName, operationName, expected) = do
      let context = ProjectionContext { projectionProfile = "ghc-9.12-prepared-stg", projectionToolchain = "ghc-9.12.2", projectionTarget = (TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []), projectionRetainedGenerations = mempty, projectionEntry = (SymbolIdentity "main" "ByteArrayContract" "value" entryName Nothing), projectionFormattingAuthority = Nothing, projectionTextUnit = Nothing }
      program <- case projectPreparedTarget context (pprModules prepared) of
        Left failure -> ioError (userError
          ("byte-array projection failed: " <> show (entryName, failure)))
        Right value -> pure value
      let actual =
            [ programSignatures program !! fromIntegral index
            | OperationDecl (PrimOpIdentity name) (SignatureId index)
                <- programOperations program
            , name == operationName
            ]
      unless (actual == [expected])
        (ioError (userError
          ("GHC byte-array operation signature changed: "
            <> show (entryName, operationName, expected, actual,
              programOperations program))))

topBindersForTest :: CgStgTopBinding -> [Id]
topBindersForTest (StgTopStringLit binder _) = [binder]
topBindersForTest (StgTopLifted binding) = case binding of
  StgNonRec binder _ -> [binder]
  StgRec pairs -> map fst pairs

-- | A retained-generation symbol is an executable import: the projection (a)
-- excludes it from recovery (no recovered top-level body in the wire
-- program), and (b) declares it a 'GlobalDecl' carrying 'required_generation'
-- even though 'ImportProducer' is compiled alongside 'ImportConsumer' as a
-- home module (the retained check must come before the home-module
-- rejection, never inferred from module membership). With the map empty,
-- both bindings resolve as ordinary local home tops -- today's behavior.
verifyRetainedImportProjection :: IO ()
verifyRetainedImportProjection = do
  root <- getCurrentDirectory
  let fixtureDir = root </> "test-prepared-stg"
  prepared <- runPipelineSelected PreparedStg
    (fixtureDir </> "ImportConsumer.hs") [fixtureDir]
  let modules = pprModules prepared
      entry = SymbolIdentity "main" "ImportConsumer" "value" "consumerResult" Nothing
      producerValueId = SymbolIdentity "main" "ImportProducer" "value" "producerValue" Nothing
      producerFnId = SymbolIdentity "main" "ImportProducer" "value" "producerFn" Nothing
      baseContext = ProjectionContext
        { projectionProfile = "ghc-9.12-prepared-stg"
        , projectionToolchain = "ghc-9.12.2"
        , projectionTarget = TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []
        , projectionRetainedGenerations = Map.empty
        , projectionEntry = entry
        , projectionFormattingAuthority = Nothing
        , projectionTextUnit = Nothing
        }
  -- map empty -> current behavior: both producer bindings are recovered
  -- locally, and neither is declared as a global.
  case projectPreparedTarget baseContext modules of
    Left failure -> ioError (userError
      ("retained-import baseline projection failed: " <> show failure))
    Right program -> do
      unless (Set.member "producerValue" (recoveredOccurrences program))
        (ioError (userError
          "retained-import baseline omitted producerValue's recovered body"))
      unless (Set.member "producerFn" (recoveredOccurrences program))
        (ioError (userError
          "retained-import baseline omitted producerFn's recovered body"))
      unless (all ((/= producerValueId) . globalIdentity) (programGlobals program))
        (ioError (userError
          "retained-import baseline declared producerValue a global"))
      unless (all ((/= producerFnId) . globalIdentity) (programGlobals program))
        (ioError (userError
          "retained-import baseline declared producerFn a global"))
  -- map set -> both become GlobalDecls carrying the generation, and neither
  -- top-level body is recovered.
  let retainedContext = baseContext
        { projectionRetainedGenerations = Map.fromList
            [(producerValueId, 11), (producerFnId, 11)]
        }
  case projectPreparedTarget retainedContext modules of
    Left failure -> ioError (userError
      ("retained-import projection failed: " <> show failure))
    Right program -> do
      unless (not (Set.member "producerValue" (recoveredOccurrences program)))
        (ioError (userError
          "retained-import projection recovered producerValue's body"))
      unless (not (Set.member "producerFn" (recoveredOccurrences program)))
        (ioError (userError
          "retained-import projection recovered producerFn's body"))
      let globalsByIdentity = [(globalIdentity g, g) | g <- programGlobals program]
      producerValueGlobal <- case lookup producerValueId globalsByIdentity of
        Just value -> pure value
        Nothing -> ioError (userError
          "retained-import projection omitted producerValue's global")
      producerFnGlobal <- case lookup producerFnId globalsByIdentity of
        Just value -> pure value
        Nothing -> ioError (userError
          "retained-import projection omitted producerFn's global")
      unless (globalRequiredGeneration producerValueGlobal == Just 11)
        (ioError (userError
          "retained-import projection did not carry producerValue's generation"))
      unless (globalRequiredGeneration producerFnGlobal == Just 11)
        (ioError (userError
          "retained-import projection did not carry producerFn's generation"))
  verifyRetainedImportProjectionExposed
  where
    recoveredOccurrences program = Set.fromList
      [ symbolOccurrence symbol
      | group <- programBindings program
      , TopBinding symbol _ <- groupItems group
      ]
    groupItems (NonRecursive item) = [item]
    groupItems (Recursive items) = items

-- | Same shape as 'verifyRetainedImportProjection', but proving the
-- COMPILE-TIME half of the mechanism: 'ImportProducerExposed' carries no
-- 'NOINLINE' pragmas (unlike 'ImportProducer'), so nothing stops GHC's own
-- simplifier from inlining its bindings into 'ImportConsumerExposed' UNLESS
-- 'Tidepool.RetainedUnfoldings' withholds their unfoldings during
-- compilation itself. Two separate compiles of the same consumer source
-- prove the pass is gated on the retained-identity set passed to
-- 'runPipelineSelectedRetaining', not merely on the projection-time
-- 'projectionRetainedGenerations' map (which 'verifyRetainedImportProjection'
-- above already covers for the NOINLINE-pragma stand-in):
--
--   * retained set empty at compile time -> today's plain-GHC behavior:
--     both bindings are recovered locally (their exact occurrence names
--     appear as recovered tops), and neither is declared a 'GlobalDecl'.
--   * retained set populated at compile time -> the pass withholds both
--     unfoldings before GHC's simplifier ever runs, so 'consumerResult'
--     keeps plain, unexpanded 'Var' references to both -- exactly the shape
--     'ExecutionProjection' already expects an executable import to have
--     (see 'verifyRetainedImportProjection' above, which covers the same
--     contract for the NOINLINE-pragma stand-in): neither occurrence name
--     is recovered locally, and projecting with a matching
--     'projectionRetainedGenerations' map declares both a 'GlobalDecl'
--     carrying generation 11. (GHC still floats 'producerValue''s own
--     static list into internal top-level pieces WITHIN
--     'ImportProducerExposed' itself -- e.g. @producerValue1@ -- regardless
--     of this pass, exactly as it already does for the real
--     'ImportProducer' fixture; that is 'producerValue''s own module
--     compiling its literal list, not a leak into the consumer, and
--     'ExecutionProjection' already tolerates it the same way for
--     'ImportProducer', so this test does not re-litigate it.)
verifyRetainedImportProjectionExposed :: IO ()
verifyRetainedImportProjectionExposed = do
  root <- getCurrentDirectory
  let fixtureDir = root </> "test-prepared-stg"
      entry = SymbolIdentity "main" "ImportConsumerExposed" "value" "consumerResult" Nothing
      producerValueId = SymbolIdentity "main" "ImportProducerExposed" "value" "producerValue" Nothing
      producerFnId = SymbolIdentity "main" "ImportProducerExposed" "value" "producerFn" Nothing
      contextFor retainedGenerations = ProjectionContext
        { projectionProfile = "ghc-9.12-prepared-stg"
        , projectionToolchain = "ghc-9.12.2"
        , projectionTarget = TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []
        , projectionRetainedGenerations = retainedGenerations
        , projectionEntry = entry
        , projectionFormattingAuthority = Nothing
        , projectionTextUnit = Nothing
        }
      recoveredOccurrences program = Set.fromList
        [ symbolOccurrence symbol
        | group <- programBindings program
        , TopBinding symbol _ <- groupItems group
        , symbolModule symbol == "ImportProducerExposed"
        ]
      groupItems (NonRecursive item) = [item]
      groupItems (Recursive items) = items
  -- Compiled with the pass gated OFF (empty retained set): plain GHC
  -- behavior -- both bindings are recovered locally, and neither is
  -- declared as a global.
  baselineModules <- runPipelineSelectedRetaining PreparedStg Set.empty
    (fixtureDir </> "ImportConsumerExposed.hs") [fixtureDir]
  case projectPreparedTarget (contextFor Map.empty) (pprModules baselineModules) of
    Left failure -> ioError (userError
      ("retained-import-exposed baseline projection failed: " <> show failure))
    Right program -> do
      unless (Set.member "producerValue" (recoveredOccurrences program))
        (ioError (userError
          "retained-import-exposed baseline omitted producerValue's recovered body"))
      unless (Set.member "producerFn" (recoveredOccurrences program))
        (ioError (userError
          "retained-import-exposed baseline omitted producerFn's recovered body"))
      unless (all ((/= producerValueId) . globalIdentity) (programGlobals program))
        (ioError (userError
          "retained-import-exposed baseline declared producerValue a global"))
      unless (all ((/= producerFnId) . globalIdentity) (programGlobals program))
        (ioError (userError
          "retained-import-exposed baseline declared producerFn a global"))
  -- Compiled with the pass gated ON (both symbols retained): neither
  -- occurrence name is recovered locally in the consumer, and both become
  -- 'GlobalDecl's carrying the generation.
  withheldModules <- runPipelineSelectedRetaining PreparedStg
    (Set.fromList [producerValueId, producerFnId])
    (fixtureDir </> "ImportConsumerExposed.hs") [fixtureDir]
  case projectPreparedTarget
      (contextFor (Map.fromList [(producerValueId, 11), (producerFnId, 11)]))
      (pprModules withheldModules) of
    Left failure -> ioError (userError
      ("retained-import-exposed withheld projection failed: " <> show failure))
    Right program -> do
      unless (not (Set.member "producerValue" (recoveredOccurrences program)))
        (ioError (userError
          "retained-import-exposed withheld projection recovered producerValue's body"))
      unless (not (Set.member "producerFn" (recoveredOccurrences program)))
        (ioError (userError
          "retained-import-exposed withheld projection recovered producerFn's body"))
      let globalsByIdentity = [(globalIdentity g, g) | g <- programGlobals program]
      producerValueGlobal <- case lookup producerValueId globalsByIdentity of
        Just value -> pure value
        Nothing -> ioError (userError
          "retained-import-exposed withheld projection omitted producerValue's global")
      producerFnGlobal <- case lookup producerFnId globalsByIdentity of
        Just value -> pure value
        Nothing -> ioError (userError
          "retained-import-exposed withheld projection omitted producerFn's global")
      unless (globalRequiredGeneration producerValueGlobal == Just 11)
        (ioError (userError
          "retained-import-exposed withheld projection did not carry producerValue's generation"))
      unless (globalRequiredGeneration producerFnGlobal == Just 11)
        (ioError (userError
          "retained-import-exposed withheld projection did not carry producerFn's generation"))
  verifyHierarchicalTargetModule

-- | Task (b) regression: a hierarchical module (@module Session.Val.G1
-- where@ in @test-prepared-stg/Session/Val/G1.hs@, whose bare file basename
-- is only @G1@) compiles and projects successfully when passed as the
-- PRIMARY compile target. Before
-- 'Tidepool.GhcPipeline.targetModuleNameFor', every "target module"
-- derivation in 'Tidepool.GhcPipeline' used
-- @capitalize (takeBaseName path)@ -- here, @G1@ -- so the post-loop merge
-- could never find a compiled module actually named @Session.Val.G1@ and
-- failed with "target module 'G1' not found among compiled modules".
verifyHierarchicalTargetModule :: IO ()
verifyHierarchicalTargetModule = do
  root <- getCurrentDirectory
  let fixtureDir = root </> "test-prepared-stg"
  prepared <- runPipelineSelected PreparedStg
    (fixtureDir </> "Session" </> "Val" </> "G1.hs") [fixtureDir]
  let modules = pprModules prepared
      moduleNames = [ moduleNameString (moduleName (pmModule m)) | m <- modules ]
  unless ("Session.Val.G1" `elem` moduleNames)
    (ioError (userError
      ("hierarchical target module test: expected a compiled module named "
        ++ "'Session.Val.G1', got: " ++ show moduleNames)))
  let context = ProjectionContext
        { projectionProfile = "ghc-9.12-prepared-stg"
        , projectionToolchain = "ghc-9.12.2"
        , projectionTarget = TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []
        , projectionRetainedGenerations = Map.empty
        , projectionEntry =
            SymbolIdentity "main" "Session.Val.G1" "value" "hierarchicalValue" Nothing
        , projectionFormattingAuthority = Nothing
        , projectionTextUnit = Nothing
        }
  case projectPreparedTarget context modules of
    Left failure -> ioError (userError
      ("hierarchical target module projection failed: " <> show failure))
    Right _ -> pure ()
