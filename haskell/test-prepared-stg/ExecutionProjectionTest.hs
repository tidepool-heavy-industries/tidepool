{-# LANGUAGE OverloadedStrings #-}

module ExecutionProjectionTest (projectProjectionContract) where

import Control.Monad (unless)
import Data.ByteString qualified as BS
import Data.List (nub)
import Data.Map.Strict qualified as Map
import Data.Set qualified as Set
import Data.Text qualified as Text
import GHC.Builtin.Types
  ( doubleRepDataConTy, intRepDataConTy, liftedRepTy, tupleRepDataConTyCon
  , mkPromotedListTy, runtimeRepTy, unliftedRepTy, zeroBitRepTy )
import GHC.Core.Type (mkTyConApp)
import GHC.Core.DataCon (dataConName, dataConRepArity)
import GHC.Types.Basic (TypeOrConstraint(TypeLike, ConstraintLike))
import GHC.Types.Literal (Literal(..))
import GHC.Types.Id (isDataConWorkId_maybe)
import GHC.Types.Name (nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.Var (Id, varName)
import GHC.Stg.Syntax
import System.Directory (getCurrentDirectory)
import System.FilePath ((</>))
import Tidepool.PreparedStg (PreparedModule(..))
import Tidepool.PreparedFacts (PreparedFacts(..))
import Tidepool.ExecutionProjection
import Tidepool.ExecutionSchema
import Tidepool.GhcPipeline
  ( PipelineSelection(PreparedStg), PreparedPipelineResult(..)
  , runPipelineSelected )

projectProjectionContract :: [PreparedModule] -> IO WireProgram
projectProjectionContract modules = do
  topIdentityAllocationContract
  literalProjectionContract
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
      case projectPrepared context [] of
        Left (UnsupportedPreparedShape _) -> pure ()
        other -> ioError (userError ("empty program did not produce typed rejection: " <> show other))
      pure program
  where
    context = ProjectionContext "ghc-9.12-prepared-stg" "ghc-9.12.2"
      (TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []) Map.empty
      (SymbolIdentity "main" "M3Vertical" "value" "result" Nothing)

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
  let context = ProjectionContext "ghc-9.12-prepared-stg" "ghc-9.12.2"
        (TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []) Map.empty
        (SymbolIdentity "main" "NullaryWorkers" "value" "result" Nothing)
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
    checkExpr :: [RuntimeRep] -> Expr -> IO Int
    checkExpr expected expression = case expression of
      Enter _ signature -> checkResult expected signature
      Call _ signature _ -> checkResult expected signature
      Case scrutinee _ binderReps _ alternatives -> do
        scrutineeCalls <- checkExpr binderReps scrutinee
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
    checkAlternative :: [RuntimeRep] -> Alternative -> IO Int
    checkAlternative expected (Alternative _ _ body) = checkExpr expected body
    checkResult :: [RuntimeRep] -> SignatureId -> IO Int
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
    expectedResult = [IntRep 64]
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
  let context identity = ProjectionContext "ghc-9.12-prepared-stg" "ghc-9.12.2"
        (TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []) mempty identity
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
      | signatureResults signature == [IntRep 64] && returned == parameter -> pure ()
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
      context = ProjectionContext "ghc-9.12-prepared-stg" "ghc-9.12.2"
        (TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []) mempty identity
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
        in unless (signature == Signature [FloatRep 64, VoidRep] [FloatRep 64])
          (ioError (userError
            ("state-bearing rintDouble signature changed: " <> show signature)))
      signatures -> ioError (userError
        ("expected one state-bearing rintDouble operation, got " <> show signatures))
  where
    unSignatureId (SignatureId value) = value

topBindersForTest :: CgStgTopBinding -> [Id]
topBindersForTest (StgTopStringLit binder _) = [binder]
topBindersForTest (StgTopLifted binding) = case binding of
  StgNonRec binder _ -> [binder]
  StgRec pairs -> map fst pairs
