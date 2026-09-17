{-# LANGUAGE GADTs #-}

module Main (main) where

import Control.Monad (unless)
import Control.Monad.IO.Class (liftIO)
import Control.Exception (evaluate)
import Data.List (intercalate)
import Data.Text qualified as Text
import GHC
import GHC.Core (Bind(..), maybeUnfoldingTemplate)
import GHC.Core.TyCo.Compare (eqType)
import GHC.Core.Utils qualified as CoreUtils
import GHC.Driver.Session (updOptLevel)
import GHC.Driver.Main (hscTidy)
import GHC.Stg.Syntax qualified as Stg
import GHC.Types.Id (idName, realIdUnfolding)
import GHC.Types.Name (nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.Var (varName, varType)
import GHC.Utils.Outputable (ppr, showSDocUnsafe)
import System.Directory (getCurrentDirectory)
import System.FilePath ((</>))
import System.Exit (ExitCode(..))
import System.Process (proc, readCreateProcessWithExitCode)
import Tidepool.ExecutionProjection
  ( ProjectionContext(..), preparedTargetReferences
  , preparedTopIdentities, projectPreparedTarget )
import Tidepool.ExecutionSchema
  ( Architecture(..), Alternative(..), Atom(..), Endianness(..), Expr(..), Group(..)
  , HeapBinding(..), HeapRhs(..), JoinBinding(..), OperationDecl(..)
  , OperationId(..), OperationIdentity(..), ResultContract(..), RuntimeRep(..)
  , Signature(..), SignatureId(..), SymbolIdentity(..), TargetDescriptor(..)
  , TopBinding(..), ValueRef(..), WireProgram(..) )
import Tidepool.FatIface
  ( FatIfaceLookup(..), newFatIfaceCache, lookupFatIfaceExact
  , newOwnerInterfaceCache )
import Tidepool.GhcPipeline
  ( PipelineSelection(PreparedStg), PreparedPipelineResult(..)
  , PipelineResult(prHscEnv), runPipelineSelected )
import Tidepool.PreparedRecovery (RecoveredClosure(closureModules), recoverPreparedClosure)
import Tidepool.PreparedFacts (extractPreparedFacts)
import Tidepool.PreparedStg
  ( PreparedModule(..), RecoveredModuleFailure(..), prepareModule, prepareRecoveredBodies
  , unelaboratedModule )
import Tidepool.Resolve
  ( BodyOrigin(..), ExactBodyLookup(..), recoverExactBody )

assert :: Bool -> String -> IO ()
assert ok message = unless ok (ioError (userError message))

main :: IO ()
main = do
  root <- getCurrentDirectory
  let source = root </> "test-prepared-stg" </> "RecoveredBody.hs"
  libdir <- trim <$> readProcessGhc ["--print-libdir"]
  runGhc (Just libdir) $ do
    flags <- getSessionDynFlags
    _ <- setSessionDynFlags (updOptLevel 0 flags)
      { importPaths = root : importPaths flags
      , backend = noBackend
      , ghcLink = NoLink
      }
    target <- guessTarget source Nothing Nothing
    setTargets [target]
    _ <- load LoadAllTargets
    summary <- getModSummary (mkModuleName "RecoveredBody")
    parsed <- parseModule summary
    typed <- typecheckModule parsed
    desugared <- desugarModule typed
    hsc <- getSession
    (callerGuts, _) <- liftIO $ hscTidy hsc (coreModule desugared)
    caller <- liftIO $ prepareModule hsc summary (unelaboratedModule callerGuts)
    let entry = callerEntry caller
        context = ProjectionContext
          { projectionProfile = Text.pack "w5-b2-recovered-body"
          , projectionToolchain = Text.pack "ghc-9.12.2"
          , projectionTarget = TargetDescriptor X86_64 LittleEndian 64 64
              (Text.pack "sysv64") []
          , projectionRetainedGenerations = mempty
          , projectionEntry = entry
          , projectionAuxiliaryRoots = []
          , projectionFormattingAuthority = Nothing
          , projectionTextUnit = Nothing
          }
        references = preparedTargetReferences context [caller]
    liftIO $ assert (any isFst references)
      ("-O0 caller did not retain a real package fst reference: "
        ++ intercalate ", " (map (showSDocUnsafe . ppr . idName) references))
    let fstIds = filter isFst references
    cache <- liftIO newFatIfaceCache
    ownerCache <- liftIO newOwnerInterfaceCache
    recovered <- liftIO $ recoverFirst hsc cache fstIds
    case recovered of
      (fstId, ExactBody owner body origin) -> do
        liftIO $ assert (origin == InterfaceUnfolding || origin == FatInterfaceGroup)
          "exact recovery returned an unknown body origin"
        preparedResult <- liftIO $ prepareRecoveredBodies hsc ownerCache owner (bindList body)
        recoveredModule <- case preparedResult of
          Left failure -> liftIO $ ioError (userError
            ("defining-context preparation failed: " ++ show failure))
          Right prepared -> pure prepared
        liftIO $ do
          assert (pmModule recoveredModule == owner)
            "recovered body was prepared under the caller module"
          assert (moduleNameString (moduleName owner) == recoveredModuleName fstId)
            "recovered body module identity did not come from the Id"
          case projectPreparedTarget context [caller, recoveredModule] of
            Left failure -> ioError (userError
              ("caller + exact body projection failed: " ++ show failure))
            Right program -> assert (hasRecoveredTop owner fstId program)
              "projection omitted the recovered defining top"
      (fstId, other) -> liftIO $ ioError (userError
        ("real package fst had no exact body (actual Id "
          ++ showSDocUnsafe (ppr (idName fstId)) ++ "): " ++ showLookup other))
  assertSemigroupSubset root libdir
  assertRecoveredKindRep root
  assertPatErrorBody root
  assertRaiseContracts root
  assertBottomingApplications root
  where
    callerEntry prepared = case
      [ identity
      | identity <- either (error . show) id (preparedTopIdentities [prepared])
      , symbolOccurrence identity == Text.pack "caller"
      ] of
      [identity] -> identity
      found -> error ("expected one caller entry, got " ++ show found)

    isFst identifier = occNameString (nameOccName (varName identifier)) == "fst"

    bindList (NonRec binder body) = [NonRec binder body]
    bindList (Rec pairs) = [Rec pairs]

    recoverFirst _ _ [] = error "recoverFirst called with no fst Id"
    recoverFirst hsc cache (identifier : rest) = do
      result <- recoverExactBody hsc cache identifier
      case result of
        exact@(ExactBody _ _ _) -> pure (identifier, exact)
        _ | null rest -> pure (identifier, result)
          | otherwise -> recoverFirst hsc cache rest

    recoveredModuleName identifier = case nameModule_maybe (varName identifier) of
      Just owner -> moduleNameString (moduleName owner)
      Nothing -> error "fst Id unexpectedly had no defining module"

    hasRecoveredTop owner identifier program = any matches
      [ symbol
      | group <- programBindings program
      , top <- groupItems group
      , symbol <- [topSymbol top]
      ]
      where
        wantedOccurrence = Text.pack (occNameString (nameOccName (varName identifier)))
        matches symbol = symbolModule symbol == Text.pack (moduleNameString (moduleName owner))
          && symbolOccurrence symbol == wantedOccurrence
        topSymbol (TopBinding symbol _) = symbol
        groupItems (NonRecursive top) = [top]
        groupItems (Recursive tops) = tops

    showLookup (ExactBody owner _ origin) = "exact body in " ++ renderModule owner ++ " via " ++ show origin
    showLookup (MissingExactBody name reason) = "missing " ++ renderName name ++ ": " ++ show reason
    showLookup (BodyInterfaceFailure owner reason) = "interface failure in " ++ renderModule owner ++ ": " ++ reason
    showLookup (BodyTypeMismatch owner name requested candidate fallback) =
      "type mismatch in " ++ renderModule owner ++ " for " ++ renderName name
        ++ ": " ++ requested ++ " vs " ++ candidate
        ++ maybe "" ("; " ++) fallback
    showLookup (UnsupportedBodyCapability name) = "unsupported body " ++ renderName name

    renderModule = showSDocUnsafe . ppr
    renderName = showSDocUnsafe . ppr

    trim = reverse . dropWhile (== '\n') . reverse

    readProcessGhc args = do
      (code, out, err) <- readCreateProcessWithExitCode (proc "ghc" args) ""
      case code of
        ExitSuccess -> pure out
        _ -> ioError (userError ("ghc failed: " ++ err))

assertSemigroupSubset :: FilePath -> String -> IO ()
assertSemigroupSubset root libdir = runGhc (Just libdir) $ do
  flags <- getSessionDynFlags
  _ <- setSessionDynFlags (updOptLevel 0 flags)
    { importPaths = [root </> "test-prepared-stg", root </> "lib"] ++ importPaths flags
    , backend = noBackend
    , ghcLink = NoLink
    }
  target <- guessTarget (root </> "test-prepared-stg" </> "RecoveredBody.hs") Nothing Nothing
  setTargets [target]
  _ <- load LoadAllTargets
  summary <- getModSummary (mkModuleName "RecoveredBody")
  parsed <- parseModule summary
  typed <- typecheckModule parsed
  desugared <- desugarModule typed
  hsc <- getSession
  (guts, _) <- liftIO $ hscTidy hsc (coreModule desugared)
  prepared <- liftIO $ prepareModule hsc summary (unelaboratedModule guts)
  let context = ProjectionContext
        { projectionProfile = Text.pack "w5-b2-recovered-same-owner"
        , projectionToolchain = Text.pack "ghc-9.12.2"
        , projectionTarget = TargetDescriptor X86_64 LittleEndian 64 64
            (Text.pack "sysv64") []
        , projectionRetainedGenerations = mempty
        , projectionEntry = SymbolIdentity
            (Text.pack "main") (Text.pack "RecoveredBody") (Text.pack "value")
            (Text.pack "foldableCaller") Nothing
        , projectionAuxiliaryRoots = []
        , projectionFormattingAuthority = Nothing
        , projectionTextUnit = Nothing
        }
      references = preparedTargetReferences context [prepared]
      semigroupReferences = filter isSemigroupOwner references
      monoidProductReferences = filter isMonoidProduct semigroupReferences
  semigroupId <- case monoidProductReferences of
    [value] -> pure value
    found -> liftIO $ ioError (userError
      ("expected one defining semigroup reference, got " ++ show (length found)
        ++ ": " ++ intercalate ", " (map renderId found)))
  cache <- liftIO newFatIfaceCache
  ownerCache <- liftIO newOwnerInterfaceCache
  lookupResult <- liftIO $ recoverExactBody hsc cache semigroupId
  (owner, body) <- case lookupResult of
    ExactBody owner group _ -> pure (owner, group)
    other -> liftIO $ ioError (userError
      ("semigroup reference was not recovered exactly: " ++ showLookup' other))
  recovered <- liftIO $ prepareRecoveredBodies hsc ownerCache owner (bindList body)
  recoveredModule <- case recovered of
    Right value -> pure value
    Left failure -> liftIO $ ioError (userError
      ("same-owner recovered subset did not prepare: " ++ show failure))
  let recoveredReferences = preparedTargetReferences context [prepared, recoveredModule]
      productOneReferences = filter isMonoidProductOne recoveredReferences
  productOneId <- case productOneReferences of
    [value] -> pure value
    found -> liftIO $ ioError (userError
      ("expected one recovered $fMonoidProduct1 reference, got "
        ++ show (length found) ++ ": " ++ intercalate ", " (map renderId found)))
  productOneLookup <- liftIO $ recoverExactBody hsc cache productOneId
  (productOneOwner, productOneBody) <- case productOneLookup of
    ExactBody owner' group _ -> pure (owner', group)
    other -> liftIO $ ioError (userError
      ("$fMonoidProduct1 was not recovered exactly: " ++ showLookup' other))
  liftIO $ assert (productOneOwner == owner)
    "$fMonoidProduct1 defining owner changed during recovery"
  productOnePrepared <- liftIO $ prepareRecoveredBodies hsc ownerCache productOneOwner
    (bindList productOneBody)
  productOneModule <- case productOnePrepared of
    Right value -> pure value
    Left failure -> liftIO $ ioError (userError
      ("$fMonoidProduct1 recovered subset did not prepare: " ++ show failure))
  let allRecovered = [prepared, recoveredModule, productOneModule]
      allReferences = preparedTargetReferences context allRecovered
  liftIO $ assert (any isStimes allReferences)
    ("stimesMonoid1 dependency disappeared from references: "
      ++ intercalate ", " (map renderId allReferences))
  sourceHome <- liftIO $ prepareRecoveredBodies hsc ownerCache (pmModule prepared) []
  case sourceHome of
    Left RecoveredModuleInterfaceFailure{} -> pure ()
    Left failure -> liftIO $ ioError (userError
      ("missing source-home interface reported wrong failure: " ++ show failure))
    Right _ -> liftIO $ ioError (userError
      "missing source-home interface was unexpectedly readable")
  where
    isSemigroupOwner identifier = case nameModule_maybe (varName identifier) of
      Just owner -> moduleNameString (moduleName owner)
        == "GHC.Internal.Data.Semigroup.Internal"
      Nothing -> False
    isStimes identifier = occNameString (nameOccName (varName identifier))
      == "stimesMonoid1"
    isMonoidProduct identifier = occNameString (nameOccName (varName identifier))
      == "$fMonoidProduct"
    isMonoidProductOne identifier = occNameString (nameOccName (varName identifier))
      == "$fMonoidProduct1"
    bindList (NonRec binder body) = [NonRec binder body]
    bindList (Rec pairs) = [Rec pairs]
    renderId identifier = showSDocUnsafe (ppr (idName identifier))
    showLookup' (ExactBody owner _ origin) = "exact body in " ++ renderModule' owner ++ " via " ++ show origin
    showLookup' (MissingExactBody name reason) = "missing " ++ renderName' name ++ ": " ++ show reason
    showLookup' (BodyInterfaceFailure owner reason) = "interface failure in " ++ renderModule' owner ++ ": " ++ reason
    showLookup' (BodyTypeMismatch owner name requested candidate fallback) =
      "type mismatch in " ++ renderModule' owner ++ " for " ++ renderName' name
        ++ ": " ++ requested ++ " vs " ++ candidate
        ++ maybe "" ("; " ++) fallback
    showLookup' (UnsupportedBodyCapability name) = "unsupported body " ++ renderName' name
    renderModule' = showSDocUnsafe . ppr
    renderName' = showSDocUnsafe . ppr

-- GHC.Types:krep$* is an ordinary boxed strict-field constructor body whose
-- STG representation leaves the final PrimRep annotation undefined.  The
-- prepared facts walk must derive its layout from the actual constructor
-- arguments while recovering the real dependency closure for showDouble.
assertRecoveredKindRep :: FilePath -> IO ()
assertRecoveredKindRep root = do
  prepared <- runPipelineSelected PreparedStg
    (root </> "test" </> "Suite.hs") [root </> "lib"]
  let pipeline = pprPipelineResult prepared
      home = pprModules prepared
      entry = SymbolIdentity (Text.pack "main") (Text.pack "Suite")
        (Text.pack "value") (Text.pack "showDouble") Nothing
      context = ProjectionContext
        { projectionProfile = Text.pack "w5-recovered-krep"
        , projectionToolchain = Text.pack "ghc-9.12.2"
        , projectionTarget = TargetDescriptor X86_64 LittleEndian 64 64
            (Text.pack "sysv64") []
        , projectionRetainedGenerations = mempty
        , projectionEntry = entry
        , projectionAuxiliaryRoots = []
        , projectionFormattingAuthority = Nothing
        , projectionTextUnit = Nothing
        }
  cache <- newFatIfaceCache
  ownerCache <- newOwnerInterfaceCache
  closure <- recoverPreparedClosure (prHscEnv pipeline) cache ownerCache context home
  let modules = closureModules closure
      references = preparedTargetReferences context modules
  identities <- case preparedTopIdentities modules of
    Left failure -> ioError (userError
      ("recovered showDouble identities failed: " ++ show failure))
    Right values -> pure values
  _ <- evaluate (length references)
  assert (any isKrepTop identities)
    "recovered showDouble closure lost GHC.Types:krep$*"
  projected <- evaluate (projectPreparedTarget context modules)
  case projected of
    Left _ -> pure ()
    Right program -> do
      _ <- evaluate (length (programBindings program))
      pure ()
  where
    isKrepTop symbol = symbolModule symbol == Text.pack "GHC.Types"
      && symbolOccurrence symbol == Text.pack "krep$*"

-- Representation-polymorphic error workers must never let an incompatible
-- fat-interface body reach pre-CorePrep. This fixture records that patError has
-- no real unfolding, proves the raw fat candidate is incompatible, and then
-- requires typed recovery rejection under the defining owner.
assertPatErrorBody :: FilePath -> IO ()
assertPatErrorBody root = do
  prepared <- runPipelineSelected PreparedStg
    (root </> "test" </> "Suite.hs") [root </> "lib"]
  let pipeline = pprPipelineResult prepared
      home = pprModules prepared
      entry = SymbolIdentity (Text.pack "main") (Text.pack "Suite")
        (Text.pack "value") (Text.pack "qq_patch_invert_involution") Nothing
      context = ProjectionContext
        { projectionProfile = Text.pack "w5-pat-error-body"
        , projectionToolchain = Text.pack "ghc-9.12.2"
        , projectionTarget = TargetDescriptor X86_64 LittleEndian 64 64
            (Text.pack "sysv64") []
        , projectionRetainedGenerations = mempty
        , projectionEntry = entry
        , projectionAuxiliaryRoots = []
        , projectionFormattingAuthority = Nothing
        , projectionTextUnit = Nothing
        }
      patErrors = filter isPatError (preparedTargetReferences context home)
  patError <- case patErrors of
    [value] -> pure value
    found -> ioError (userError
      ("expected one recovered patError reference, got "
        ++ show (length found) ++ ": "
        ++ intercalate ", " (map renderId found)))
  cache <- newFatIfaceCache
  case maybeUnfoldingTemplate (realIdUnfolding patError) of
    Just _ -> ioError (userError
      "patError unexpectedly has a real unfolding; expected fat-interface recovery")
    Nothing -> pure ()
  fatLookup <- lookupFatIfaceExact (prHscEnv pipeline) cache (varName patError)
  fatGroup <- case fatLookup of
    FatIfaceFound group -> pure group
    FatIfaceMissing reason -> ioError (userError
      ("patError fat interface has no candidate: " ++ show reason))
    FatIfaceLoadFailure owner reason -> ioError (userError
      ("patError fat interface failed to load " ++ renderModule owner
        ++ ": " ++ reason))
  let fatPairs = bindPairs fatGroup
      selected = [ (binder, body)
                 | (binder, body) <- fatPairs
                 , varName binder == varName patError ]
  (fatBinder, fatBody) <- case selected of
    [pair] -> pure pair
    found -> ioError (userError
      ("patError fat group selected-binder count was " ++ show (length found)))
  let binderMismatch = not (eqType (idType patError) (idType fatBinder))
      rhsMismatch = not (eqType (idType fatBinder) (CoreUtils.exprType fatBody))
  assert (not binderMismatch)
    "patError fat-interface loader did not reuse the requested wired-in binder"
  assert rhsMismatch
    "patError fat-interface binder/RHS mismatch regression was not exercised"
  result <- recoverExactBody (prHscEnv pipeline) cache patError
  case result of
    BodyTypeMismatch owner name requested candidate _ -> do
      assert (isControlExceptionBase owner && name == varName patError)
        "typed patError mismatch named the wrong defining Id"
      assert (not (null requested) && not (null candidate))
        "typed patError mismatch omitted requested/candidate types"
    ExactBody owner _ origin -> ioError (userError
      ("patError fat candidate mismatch was accepted as exact body via "
        ++ show origin ++ " in " ++ renderModule owner))
    other -> ioError (userError
      ("patError recovery returned an untyped outcome: " ++ showLookup' other))
  where
    isPatError identifier =
      occNameString (nameOccName (varName identifier)) == "patError"
        && maybe False isControlExceptionBase (nameModule_maybe (varName identifier))
    isControlExceptionBase owner =
      moduleNameString (moduleName owner) == "GHC.Internal.Control.Exception.Base"
    bindPairs (NonRec binder body) = [(binder, body)]
    bindPairs (Rec pairs) = pairs
    renderId identifier = showSDocUnsafe (ppr (idName identifier))
    renderModule = showSDocUnsafe . ppr
    showLookup' (ExactBody owner _ origin) =
      "exact body in " ++ renderModule owner ++ " via " ++ show origin
    showLookup' (MissingExactBody name reason) =
      "missing " ++ showSDocUnsafe (ppr name) ++ ": " ++ show reason
    showLookup' (BodyInterfaceFailure owner reason) =
      "interface failure in " ++ renderModule owner ++ ": " ++ reason
    showLookup' (BodyTypeMismatch owner name requested candidate fallback) =
      "type mismatch in " ++ renderModule owner ++ " for "
        ++ showSDocUnsafe (ppr name) ++ ": " ++ requested ++ " vs "
        ++ candidate ++ maybe "" ("; " ++) fallback
    showLookup' (UnsupportedBodyCapability name) =
      "unsupported body " ++ showSDocUnsafe (ppr name)

-- Bottoming primops carry NoSuccess independently of the demanded result
-- type.  Keep both the ordinary exception throw and GHC's divide-by-zero
-- sentinel in a real prepared-STG fixture so projection cannot silently
-- recover the old Returns contract from an Int result alone.
assertRaiseContracts :: FilePath -> IO ()
assertRaiseContracts root = do
  prepared <- runPipelineSelected PreparedStg
    (root </> "test-prepared-stg" </> "RaiseContract.hs")
    [root </> "test-prepared-stg"]
  let entry = SymbolIdentity (Text.pack "main") (Text.pack "RaiseContract")
        (Text.pack "value") (Text.pack "raisePrimitive") Nothing
      context = ProjectionContext
        { projectionProfile = Text.pack "w5-result-contract-raise"
        , projectionToolchain = Text.pack "ghc-9.12.2"
        , projectionTarget = TargetDescriptor X86_64 LittleEndian 64 64
            (Text.pack "sysv64") []
        , projectionRetainedGenerations = mempty
        , projectionEntry = entry
        , projectionAuxiliaryRoots = []
        , projectionFormattingAuthority = Nothing
        , projectionTextUnit = Nothing
        }
  program <- case projectPreparedTarget context (pprModules prepared) of
    Left failure -> ioError (userError
      ("raise-contract projection failed: " ++ show failure))
    Right value -> pure value
  let signatureAt (SignatureId value) =
        programSignatures program !! fromIntegral value
      signatureResult signature = signatureResults (signatureAt signature)
      operationResult (OperationId value) =
        let OperationDecl _ signature =
              programOperations program !! fromIntegral value
        in signatureResult signature
      topRhs =
        [ heapBindingRhs binding
        | group <- programBindings program
        , TopBinding symbol binding <- groupItems group
        , symbolOccurrence symbol == Text.pack "raisePrimitive"
        ]
  case topRhs of
    [rhs] -> do
      let entryResult = case rhs of
            Function signature _ _ _ -> signatureResults (signatureAt signature)
            Thunk signature _ _ _ -> signatureResults (signatureAt signature)
            other -> error ("raisePrimitive has non-executable RHS: " ++ show other)
      assert (entryResult == NoSuccess)
        ("zero-argument bottoming thunk entry was not NoSuccess: " ++ show entryResult)
      -- The contract is that nothing can follow the raise: the body demands a
      -- NoSuccess operation, either directly or as the scrutinee of a case
      -- with no alternatives (the shape projection may give a demanded raise).
      let nonReturningOperation body = case body of
            Operation operation _ -> Just operation
            Case scrutinee _ _ _ [] -> nonReturningOperation scrutinee
            _ -> Nothing
          body = case rhs of
            Function _ _ _ expression -> Just expression
            Thunk _ _ _ expression -> Just expression
            _ -> Nothing
      case body >>= nonReturningOperation of
        Just operation -> assert
          (operationResult operation == NoSuccess)
          "zero-argument bottoming thunk did not retain NoSuccess at its operation"
        Nothing -> ioError (userError
          ("raisePrimitive does not end in a non-returning operation: " ++ show rhs))
    found -> ioError (userError
      ("expected one raisePrimitive top, got " ++ show (length found)))
  let signatures = programSignatures program
      raised =
        [ (name, signatureResults (signatures !! fromIntegral (unSignatureId signature)))
        | OperationDecl (PrimOpIdentity name) signature <- programOperations program
        , name == Text.pack "raise#" || name == Text.pack "raiseDivZero#"
        ]
  case [result | (name, result) <- raised, name == Text.pack "raise#"] of
    [NoSuccess] -> pure ()
    found -> ioError (userError
      ("raise-contract raise# did not preserve NoSuccess: " ++ show found
        ++ "; all operations: " ++ show (programOperations program)))
  where
    unSignatureId (SignatureId value) = value
    groupItems (NonRecursive item) = [item]
    groupItems (Recursive items) = items

assertBottomingApplications :: FilePath -> IO ()
assertBottomingApplications root = do
  partial <- projectEntry "bottomingPartial"
  assertPartialBottoming partial
  called <- projectEntry "bottomingCalled"
  assertSaturatedBottoming called "bottomingCalled" "$wbottomingUnary" [IntRep 64]
  tupleCalled <- projectEntry "bottomingTupleCalled"
  assertSaturatedBottoming tupleCalled "bottomingTupleCalled" "bottomingTuple"
    [IntRep 64, FloatRep 64]
  voidCalled <- projectEntry "bottomingVoidCalled"
  assertSaturatedBottoming voidCalled "bottomingVoidCalled" "bottomingVoid" [VoidRep]
  where
    projectEntry occurrence = do
      prepared <- runPipelineSelected PreparedStg
        (root </> "test-prepared-stg" </> "RaiseContract.hs")
        [root </> "test-prepared-stg"]
      let context = ProjectionContext
            { projectionProfile = Text.pack "w5-result-contract-arity"
            , projectionToolchain = Text.pack "ghc-9.12.2"
            , projectionTarget = TargetDescriptor X86_64 LittleEndian 64 64
                (Text.pack "sysv64") []
            , projectionRetainedGenerations = mempty
            , projectionEntry = SymbolIdentity (Text.pack "main")
                (Text.pack "RaiseContract") (Text.pack "value")
                (Text.pack occurrence) Nothing
            , projectionAuxiliaryRoots = []
            , projectionFormattingAuthority = Nothing
            , projectionTextUnit = Nothing
            }
          modules = if occurrence == "bottomingPartial"
            then map preservePartialCall (pprModules prepared)
            else pprModules prepared
      case projectPreparedTarget context modules of
        Left failure -> ioError (userError
          ("bottoming " ++ occurrence ++ " projection failed: " ++ show failure))
        Right program -> pure program

    -- CorePrep eta-expands the fixture's PAP into a one-argument `sat`
    -- closure. Restore the same worker application with one supplied argument
    -- so projection is tested at the unsaturated call boundary.
    preservePartialCall prepared = prepared
      { pmBindings = bindings
      , pmFacts = extractPreparedFacts (pmModule prepared) (pmTagSigs prepared)
          (map fst bindings)
      }
      where
        bindings = map restore (pmBindings prepared)
        restore (Stg.StgTopLifted (Stg.StgNonRec binder
                 (Stg.StgRhsClosure captures ccs update [_]
                   (Stg.StgCase _ _ _ [Stg.GenStgAlt _ _
                     (Stg.StgApp worker [first, _])]) _)), annotations)
          | occNameString (nameOccName (varName binder)) == "sat"
          , occNameString (nameOccName (varName worker)) == "$wbottomingBinary" =
              (Stg.StgTopLifted (Stg.StgNonRec binder
                (Stg.StgRhsClosure captures ccs update []
                  (Stg.StgApp worker [first]) (varType binder))), annotations)
        restore (Stg.StgTopLifted (Stg.StgNonRec binder rhs), _)
          | occNameString (nameOccName (varName binder)) == "sat" =
              error ("unexpected prepared sat: " ++ showSDocUnsafe (ppr rhs))
        restore binding = binding

    assertPartialBottoming program = do
      let consumerCalls = allCalls (topBody program "bottomingPartial")
      assert (any isConsumerCall consumerCalls)
        ("bottomingPartial did not pass the PAP closure to partialConsumer: "
          ++ show consumerCalls)
      let entry = signatureAt program (topSignature program "$wbottomingBinary")
      assert (signatureArguments entry == [IntRep 64, IntRep 64]
          && signatureResults entry == NoSuccess)
        ("$wbottomingBinary entry did not retain its two-argument bottoming contract: "
          ++ show entry)
      let partialCalls =
            [ (callee, signatureAt program signature, arguments)
            | (callee, signature, arguments) <- allCalls (topBody program "sat")
            ]
      assert (any isPartialCall partialCalls)
        ("bottomingPartial did not retain a partial Call node: " ++ show partialCalls)
      where
        isConsumerCall (callee, _, arguments) =
          callee == Ref (Local (topId program "partialConsumer"))
            && arguments == [Ref (Local (topId program "sat"))]
        isPartialCall (callee, signature, arguments) =
          callee == Ref (Local (topId program "$wbottomingBinary"))
            && length arguments == 1
            && length arguments < length (signatureArguments
                 (signatureAt program (topSignature program "$wbottomingBinary")))
            && signatureArguments signature == [IntRep 64]
            && signatureResults signature == Returns [LiftedRefRep]

    assertSaturatedBottoming program occurrence calleeName expectedArguments = do
      assertTopResultContract program occurrence NoSuccess
      let calleeEntry = signatureAt program (topSignature program calleeName)
      assert (signatureArguments calleeEntry == expectedArguments
          && signatureResults calleeEntry == NoSuccess)
        (calleeName ++ " entry did not retain the expected bottoming arity: "
          ++ show calleeEntry)
      let calls =
            [ (callee, signatureAt program signature, arguments)
            | (callee, signature, arguments) <- allCalls (topBody program occurrence)
            ]
          matching =
            [ (callee, signature, arguments)
            | (callee, signature, arguments) <- calls
            , callee == Ref (Local (topId program calleeName))
            , signatureArguments signature == expectedArguments
            , signatureResults signature == NoSuccess
            ]
      case matching of
        [(_, _, arguments)] -> assert (length arguments == length expectedArguments)
          (occurrence ++ " call argument count disagrees with its signature")
        [] -> ioError (userError
          (occurrence ++ " did not retain a projected saturated Call with expected signature; calls: "
            ++ show calls))
        found -> ioError (userError
          (occurrence ++ " retained multiple matching saturated Calls: " ++ show found))

    topBody program occurrence = case
      [heapBindingRhs binding
      | group <- programBindings program
      , TopBinding symbol binding <- groupItems group
      , symbolOccurrence symbol == Text.pack occurrence
      ] of
      [Function _ _ _ body] -> body
      [Thunk _ _ _ body] -> body
      [rhs] -> error (occurrence ++ " has non-executable RHS: " ++ show rhs)
      found -> error ("expected one " ++ occurrence ++ " top, got " ++ show (length found))

    topId program occurrence = case
      [heapBindingId binding
      | group <- programBindings program
      , TopBinding symbol binding <- groupItems group
      , symbolOccurrence symbol == Text.pack occurrence
      ] of
      [identifier] -> identifier
      found -> error ("expected one " ++ occurrence ++ " top Id, got " ++ show found
        ++ "; tops: " ++ show (topNames program))

    topSignature program occurrence = case
      [rhsSignature (heapBindingRhs binding)
      | group <- programBindings program
      , TopBinding symbol binding <- groupItems group
      , symbolOccurrence symbol == Text.pack occurrence
      ] of
      [signature] -> signature
      found -> error ("expected one " ++ occurrence ++ " top signature, got " ++ show found
        ++ "; tops: " ++ show (topNames program))

    topNames program =
      [symbolOccurrence symbol
      | group <- programBindings program
      , TopBinding symbol _ <- groupItems group
      ]

    assertTopResultContract program occurrence expected =
      case [signature
           | group <- programBindings program
           , TopBinding symbol binding <- groupItems group
           , symbolOccurrence symbol == Text.pack occurrence
           , signature <- [rhsSignature (heapBindingRhs binding)]
           ] of
        [signature] -> assert (signatureResult program signature == expected)
          (occurrence ++ " entry contract was " ++ show (signatureAt program signature)
            ++ ", expected " ++ show expected)
        found -> ioError (userError
          ("expected one executable " ++ occurrence ++ " top, got " ++ show (length found)))

    rhsSignature (Function signature _ _ _) = signature
    rhsSignature (Thunk signature _ _ _) = signature
    rhsSignature rhs = error ("non-executable RHS has no signature: " ++ show rhs)

    allCalls expression = case expression of
      Call callee signature arguments -> (callee, signature, arguments)
        : []
      Case scrutinee _ _ _ alternatives ->
        allCalls scrutinee <> concatMap (allCalls . alternativeBody) alternatives
      Let group body -> allHeap group <> allCalls body
      LetJoins group body -> allJoin group <> allCalls body
      _ -> []
      where
        alternativeBody (Alternative _ _ body) = body
        allHeap (NonRecursive binding) = allCalls (heapBody binding)
        allHeap (Recursive bindings) = concatMap (allCalls . heapBody) bindings
        allJoin (NonRecursive binding) = allCalls (joinBody binding)
        allJoin (Recursive bindings) = concatMap (allCalls . joinBody) bindings
        heapBody (HeapBinding _ (Function _ _ _ body)) = body
        heapBody (HeapBinding _ (Thunk _ _ _ body)) = body
        heapBody _ = Return []
        joinBody (JoinBinding _ _ _ body) = body

    signatureAt program (SignatureId value) =
      programSignatures program !! fromIntegral value
    signatureResult program signature = signatureResults (signatureAt program signature)

    groupItems (NonRecursive item) = [item]
    groupItems (Recursive items) = items
