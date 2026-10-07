{-# LANGUAGE LambdaCase #-}
{-# LANGUAGE OverloadedStrings #-}

module CellSplitterCases where

import Control.Monad (forM, forM_, unless, void, when)
import Control.Concurrent (forkIO, newEmptyMVar, putMVar, takeMVar)
import Control.Exception (AsyncException(ThreadKilled), SomeException, bracket, evaluate, finally, fromException, throwIO, try)
import qualified Data.Map.Strict as Map
import Control.Monad.IO.Class (liftIO)
import Data.IORef (newIORef, modifyIORef', readIORef, writeIORef)
import Data.List (intercalate, isInfixOf, isPrefixOf, isSuffixOf, sort, tails)
import Data.Char (isDigit)
import Data.Data (Data, Typeable, cast, gmapQ)
import Data.Dynamic (fromDynamic)
import qualified Data.ByteString as BS
import qualified Data.ByteString.Lazy as BSL
import qualified Data.Text as Text
import Codec.CBOR.Encoding (encodeListLen, encodeString)
import Codec.CBOR.Read (deserialiseFromBytes)
import Codec.CBOR.Term (Term(..), decodeTerm)
import Codec.CBOR.Write (toStrictByteString)
import GHC hiding (Target)
import GHC.Builtin.Types (intTy, intDataCon)
import GHC.Builtin.Names (genClassKey)
import GHC.Core.Class (className)
import GHC.Core.InstEnv (is_cls, is_tys)
import GHC.Core.TyCon (tyConName)
import GHC.Core qualified as Core
import GHC.Core.DataCon (dataConWorkId, dataConRepArgTys, dataConTheta)
import GHC.Core.Coercion (mkPrimEqPred)
import GHC.Core.Type (mkInvisFunTys, isPredTy, splitTyConApp_maybe)
import GHC.Core.Predicate (isCoVarType)
import GHC.Core.TyCo.Rep (Scaled(..))
import GHC.Core.TyCo.Compare (eqType)
import GHC.Unit.Module.ModGuts (CgGuts, cg_binds)
import GHC.Stg.Syntax (CgStgTopBinding)
import GHC.Stg.Syntax qualified as Stg
import GHC.Types.Literal (Literal(..), LitNumType(..))
import GHC.Types.Var.Set (IdSet)
import GHC.Types.Var (isId, isCoVar, varType, varName)
import System.Mem.StableName (StableName, makeStableName)
import GHC.Types.Name.Occurrence (mkVarOcc, occNameString)
import GHC.Types.Name (nameModule_maybe, nameOccName, getOccString, nameUnique)
import GHC.Tc.Types (TcGblEnv, tcg_rn_decls, tcg_mod, tcg_type_env, tcg_insts)
import GHC.Types.TypeEnv (typeEnvIds)
import GHC.Tc.Utils.TcType (tcSplitSigmaTy)
import GHC.Types.SourceText (il_value)
import GHC.Types.PkgQual (RawPkgQual(..))
import GHC.Types.Fixity (Fixity(..))
import GHC.Driver.Session (parseDynamicFilePragma, gopt_set, PackageDBFlag(..), PkgDbRef(..))
import GHC.Driver.Backend (interpreterBackend)
import GHC.Driver.Env (HscEnv, hsc_HPT)
import GHC.Unit.Home.ModInfo (lookupHpt, hm_iface)
import GHC.Unit.Module.ModIface (mi_iface_hash, mi_mod_hash, mi_usages)
import GHC.Unit.Module.Deps (Usage(..))
import GHC.Utils.Fingerprint (fingerprintByteString)
import GHC.Parser.Header (getOptions)
import GHC.Driver.Config.Parser (initParserOpts)
import GHC.Data.StringBuffer (stringToStringBuffer)
import qualified GHC.Parser as Parser
import GHC.Parser.Lexer (ParseResult(..), initParserState, unP)
import GHC.Data.FastString (mkFastString, unpackFS)
import GHC.Types.SrcLoc (mkRealSrcLoc)
import GHC.Types.SourceError (SourceError)
import Tidepool.Agent.Assignment.Internal (NameError (..), renderNameError)
import Tidepool.Binders
import Tidepool.TurnSource
  ( spliceTemplate, generatedScaffoldModuleName, renameScaffoldModuleHeader
  , captureCompilerDefaultRecipe, qualifyCompilerDefault, preambleDefaultDeclaration, preambleImportMarker )
import Tidepool.SessionArtifacts (mkBoundBinders)
import Tidepool.DiagJson (Diag (..), DiagSeverity(..), DependencyLoadFailure(..), diagsFromSourceError)
import Tidepool.ExtractUtil (getLibdir, shaHex)
import Tidepool.Test.Runner (requiredInput)
import Tidepool.ExecutionSchema (SymbolIdentity(..))
import Tidepool.ExactScope (readExactScope)
import Tidepool.CompilerProducts (newPreparedOriginalInterfaceArtifacts)
import Tidepool.ExactHydration
  ( OriginalInterfaceArtifacts, newOriginalInterfaceArtifacts, newOriginalInterfaceArtifactsWithSessionCaptures
  , originalInterfaceBytes, originalInterfaceSha256 )
import Tidepool.FinalizedModuleArtifacts
  ( FinalizedModuleArtifacts, captureFinalizedModuleArtifacts, materializeFinalizedModuleArtifacts, finalizedLocalAdmissions, localFinalizedRequirements, finalizedInterfaceSeals, finalizedValueInterfaceSeals, encodeFinalizedModuleArtifacts )
import GHC.Utils.Outputable (ppr, showSDocUnsafe)
import Tidepool.PlannedDeclaration (hydratePlannedDeclarationInventory, transformProgramDeclarationImports)
import Tidepool.GhcPipeline
import Tidepool.ExtractRequest
  ( InspectionRequest(..), RequestField(..), RequestShapeError(..), WorkerRequest(..)
  , validateRequestShape, workerArgv, workerRequestFromArgv )
import Tidepool.Introspection (InfoEntry(..), InspectionResult(..), runInspection)
import Tidepool.DependencyEvidence
import Tidepool.Session
  ( Generation(..), SessionModule(..), SessionModuleKind(..), SessionScope(..)
  , mkThinSessionIface, writeSessionIface, injectSessionIface, renderSessionModule
  , sessionHiPath, capturedSessionInterface, injectSessionScopeWithCaptures )
import Tidepool.PreparedStg (pmModule, pmBindings)
import Tidepool.Timing
  ( InterfaceStage(..), InterfaceReuse(..), measureModuleInterface )
import System.Directory
  ( getTemporaryDirectory, createDirectory, createDirectoryIfMissing
  , removeFile, removeDirectoryRecursive, copyFile
  , getPermissions, setPermissions, setOwnerExecutable )
import System.FilePath ((</>), takeDirectory)
import System.IO (openTempFile, hClose, hFlush, readFile', stderr)
import GHC.IO.Handle (hDuplicate, hDuplicateTo)
import System.Environment (lookupEnv, setEnv, unsetEnv)

requestValidationChecks :: IO ()
requestValidationChecks = do
  certificationRequestValidation
  checkingSourceRequestRoundTrip
  requestShapeValidation
  requestFieldOrdering
  putStrLn "request validation: 4 groups passed (certification, source-check round-trip, mode shapes, field ordering)"

generatedScaffoldIdentityChecks :: IO ()
generatedScaffoldIdentityChecks = do
  let fields = ["scope-semantic", "protected template", "rendered source", "retained:main:M:value:1", "item:digest"]
      owner = generatedScaffoldModuleName fields
      template = "{-# LANGUAGE OverloadedStrings #-}\nmodule Expr where\n__result = {{TURN}}\n"
  assertEqual "generated owner is stable for equal semantic inputs"
    owner (generatedScaffoldModuleName fields)
  unless (owner /= generatedScaffoldModuleName (fields ++ ["different generation"]))
    (fail "generated owner ignored a retained generation change")
  unless (owner /= generatedScaffoldModuleName (fields ++ ["different admission digest"]))
    (fail "generated owner ignored an admission digest change")
  unless (owner /= generatedScaffoldModuleName (fields ++ ["different rendered source"]))
    (fail "generated owner ignored a rendered source change")
  renamed <- either fail pure (renameScaffoldModuleHeader owner template)
  assertEqual "generated owner rename changes only the protected header"
    ("{-# LANGUAGE OverloadedStrings #-}\nmodule " ++ owner ++ " where\n__result = {{TURN}}\n") renamed
  case renameScaffoldModuleHeader owner (template ++ "-- module Expr where\n") of
    Left _ -> pure ()
    Right _ -> fail "generated owner rename accepted an ambiguous protected header"

dependencyQualifierChecks :: IO ()
dependencyQualifierChecks = do
  let spellings =
        [ (DependencyUnqualified, "none")
        , (DependencyOtherUnit "package-owner", "other:package-owner")
        , (DependencyThisUnit "main", "this:main")
        , (DependencyThisUnit "other:main", "this:other:main")
        ]
  forM_ spellings $ \(qualifier, wire) -> do
    assertEqual "dependency qualifier wire spelling" wire (renderDependencyQualifier qualifier)
    assertEqual "dependency qualifier admission" (Just qualifier) (parseDependencyQualifier wire)
  assertEqual "dependency qualifier order preserves canonical wire rows"
    (sort (map snd spellings))
    (map renderDependencyQualifier (sort (map fst spellings)))
  forM_ ["", "none:", "this:", "other:", "main", "OTHER:main"] $ \wire ->
    assertEqual "invalid dependency qualifier rejected" Nothing (parseDependencyQualifier wire)
  let evidence = DependencyEvidence True True []
        [DependencyResolution qualifier "Owner" False Nothing [] | (qualifier, _) <- spellings]
        [] [DependencyModule "main" "Consumer" False "/Consumer.hs"
          [DependencyImport qualifier "Owner" False Nothing | (qualifier, _) <- spellings]
          ProductReady]
      resolution wire = "{\"qualifier\":\"" ++ wire
        ++ "\",\"module\":\"Owner\",\"boot\":false,\"selected\":null,\"candidates\":[]}"
      imported wire = "{\"qualifier\":\"" ++ wire
        ++ "\",\"module\":\"Owner\",\"boot\":false,\"selected\":null}"
      expected = "{\"version\":4,\"cache_safe\":true,\"selection_complete\":true,\"sources\":[],\"resolutions\":["
        ++ intercalate "," (map (resolution . snd) spellings)
        ++ "],\"packages\":[],\"modules\":[{\"unit\":\"main\",\"module\":\"Consumer\""
        ++ ",\"boot\":false,\"source\":\"/Consumer.hs\",\"imports\":["
        ++ intercalate "," (map (imported . snd) spellings) ++ "],\"product\":\"ready\"}]}"
  assertEqual "dependency evidence retains version 4 qualifier bytes" expected (renderDependencyEvidence evidence)
  putStrLn "dependency qualifiers: admission and unchanged evidence encoding passed"

requestOwnedParserDefaults :: IO ()
requestOwnedParserDefaults = do
  flags <- defaultParserDynFlags
  declaration <- declarationSourceWithTemplateFlags flags checkTemplate
    "import Data.List\nanswer = sort []\n"
  case declaration of
    Right source -> assertEqual "request-owned parser defaults imports" 1
      (length (prologueImports (declarationPrologue source)))
    Left failure -> fail ("request-owned parser defaults: " ++ renderCellSplitError failure)

functionValueInterfaceCompilation :: IO ()
functionValueInterfaceCompilation = bracket temporary removeDirectoryRecursive $ \root -> do
  let path = root </> "FunctionValueProducer.hs"
  writeFile path "module FunctionValueProducer where\n__result :: IO (() -> Int)\n__result = pure (\\() -> (42 :: Int))\n"
  prepared <- runPipelineSelected PreparedStg path [root]
  bound <- mkBoundBinders ["captured"] 1 root (pprPipelineResult prepared)
  case bound of
    [binder] | bbTier binder == RetainOpaque -> pure ()
    _ -> fail "function-returning native producer did not retain its thin Val interface"
  where
    temporary = do
      parent <- getTemporaryDirectory
      (path,handle) <- openTempFile parent "tidepool-function-value-iface"
      hClose handle
      removeFile path
      createDirectory path
      pure path

fixtureBindingType :: String -> TcGblEnv -> IO Type
fixtureBindingType name environment = case
    [varType identifier | identifier <- typeEnvIds (tcg_type_env environment)
      , getOccString identifier == name
      , nameModule_maybe (varName identifier) == Just (tcg_mod environment)] of
  [ty] -> pure ty
  _ -> fail ("missing or ambiguous original fixture binding: " ++ name)

-- Exercise the tier issuer and later thin-interface consumer with the real
-- GHC sigma type, rather than deriving expected tiers from rendered text.
sigmaValueInterfaceCompilation :: IO ()
sigmaValueInterfaceCompilation = bracket temporary removeDirectoryRecursive $ \root -> do
  let producerPath = root </> "SigmaValueProducer.hs"
      fixturePath = "test-cell-splitter/fixtures/sigma-retention/SigmaValueProducer.hs"
      owner = SessionModule ValMod (Generation 1)
      scope = SessionScope root [owner] Nothing Nothing
  copyFile fixturePath producerPath
  prepared <- runPipelineSelected PreparedStg producerPath [root]
  let result = pprPipelineResult prepared
      environment = prTargetTcGblEnv result
  forM_ [("scalar", True), ("plain", False), ("function", True)
        , ("boxedEquality", True), ("erased", False)
        , ("nested", True), ("nestedErased", False), ("recursive", True)] $ \(name, expected) -> do
    ty <- fixtureBindingType name environment
    assertEqual ("runtime closure classification: " ++ name) expected
      (isClosureType ty)
  actionType <- fixtureBindingType "__result" environment
  let (actionBinders, actionPredicates, _) = tcSplitSigmaTy actionType
  assertEqual "producer action has no outer forall" 0 (length actionBinders)
  assertEqual "producer action takes no dictionaries" 0 (length actionPredicates)
  assertEqual "rank-N result contains a dictionary closure" True
    (isClosureType (stripMonadHead actionType))
  scalarRhs <- case [rhs | (identifier, rhs) <- Core.flattenBinds (prBinds result)
                        , getOccString identifier == "scalar"] of
    [rhs] -> pure rhs
    _ -> fail "producer did not retain its original scalar Core binding"
  let (scalarArguments, _) = Core.collectBinders scalarRhs
  unless (any (\argument -> isId argument && not (isCoVar argument)
                         && isPredTy (varType argument)) scalarArguments)
    (fail "scalar Core has no runtime dictionary argument")
  assertEqual "primitive equality evidence erases" False
    (isClosureType (mkInvisFunTys [mkPrimEqPred intTy intTy] intTy))
  binders <- mkBoundBinders ["capturedNumber"] 1 root result
  case binders of
    [binder] -> assertEqual "genuine rank-N value is retained opaque" RetainOpaque (bbTier binder)
    _ -> fail "sigma producer did not issue exactly one captured binder"
  -- Separate authored consumers select distinct Num dictionaries from the same
  -- persisted value using the wrapper's source-declared rank-N accessor.
  forM_ ["Int", "Double"] $ \numberType -> do
    let consumerPath = root </> ("SigmaValue" ++ numberType ++ ".hs")
    writeFile consumerPath (unlines
      [ "module SigmaValue" ++ numberType ++ " where"
      , "import " ++ showSDocUnsafe (ppr (renderSessionModule owner)) ++ " (capturedNumber)"
      , "import SigmaValueProducer (sigmaNumber)"
      , "__result :: " ++ numberType
      , "__result = sigmaNumber capturedNumber + 2"
      ])
    _ <- runPipelineSessionSelected (PreparedProducts Nothing) mempty GeneralCompile
      (Just scope) consumerPath [root] Nothing
    pure ()
  where
    temporary = do
      parent <- getTemporaryDirectory
      (path, handle) <- openTempFile parent "tidepool-sigma-value-iface"
      hClose handle
      removeFile path
      createDirectory path
      pure path

-- Constructor dictionaries are physical worker fields, even though they
-- are absent from the source-level argument list. Only stored closures need
-- opaque retention; boxed equality data and erased coercions remain distinct.
constructorEvidenceClassification :: IO ()
constructorEvidenceClassification = bracket temporary removeDirectoryRecursive $ \root -> do
  let path = root </> "ConstructorEvidenceProducer.hs"
  copyFile "test-cell-splitter/fixtures/sigma-retention/ConstructorEvidenceProducer.hs" path
  prepared <- runPipelineSelected PreparedStg path [root]
  let result = pprPipelineResult prepared
      environment = prTargetTcGblEnv result
      bindingType name = fixtureBindingType name environment
      constructorFields ty = case splitTyConApp_maybe ty of
        Just (constructor, _) -> pure (concatMap dataConRepArgTys (tyConDataCons constructor))
        _ -> fail "constructor evidence fixture has no nominal type"
  forM_ [("some", True), ("number", True), ("equality", False)
        , ("primitive", False), ("plain", False), ("function", True)
        , ("numScalar", True), ("forallOnly", False), ("boxedScalar", True)] $ \(name, expected) -> do
    ty <- bindingType name
    assertEqual ("physical closure classification: " ++ name) expected (isClosureType ty)
  forM_ ["some", "number"] $ \name -> do
    fields <- bindingType name >>= constructorFields
    unless (any (\(Scaled _ field) -> isPredTy field && not (isCoVarType field)
                && isClosureType field) fields)
      (fail ("constructor has no real stored dictionary closure: " ++ name))
  equalityFields <- bindingType "equality" >>= constructorFields
  unless (any (\(Scaled _ field) -> isPredTy field && not (isCoVarType field)) equalityFields)
    (fail "boxed equality control has no stored dictionary")
  primitive <- bindingType "primitive"
  case splitTyConApp_maybe primitive of
    Just (constructor, _) -> unless
      (any (any isCoVarType . dataConTheta) (tyConDataCons constructor))
      (fail "GADT control has no actual primitive equality evidence")
    _ -> fail "primitive equality control has no nominal type"
  assertEqual "primitive equality sigma stays erased" False
    (isClosureType (mkInvisFunTys [mkPrimEqPred intTy intTy] intTy))
  binders <- mkBoundBinders ["capturedSome"] 1 root result
  case binders of
    [binder] -> assertEqual "constructor dictionary closure retained opaque" RetainOpaque (bbTier binder)
    _ -> fail "constructor producer did not issue exactly one actual capture"
  where
    temporary = do
      parent <- getTemporaryDirectory
      (path, handle) <- openTempFile parent "tidepool-constructor-evidence"
      hClose handle
      removeFile path
      createDirectory path
      pure path

-- A genuinely published thin value interface remains a type dependency in the
-- next cell. Its consumed bytes must survive mutation of the session path.
sessionValueFinalizedDependency :: IO ()
sessionValueFinalizedDependency = bracket temporary removeDirectoryRecursive $ \root -> do
  let producerPath = root </> "ValueProducer.hs"
      consumerPath = root </> "ValueConsumer.hs"
      owner = SessionModule ValMod (Generation 1)
      scope = SessionScope root [owner] Nothing Nothing
      valuePath = sessionHiPath root owner
  writeFile producerPath "module ValueProducer where\n__result :: IO (() -> Int)\n__result = pure (\\() -> (42 :: Int))\n"
  producer <- runPipelineSelected PreparedStg producerPath [root]
  _ <- mkBoundBinders ["captured"] 1 root (pprPipelineResult producer)
  consumedBytes <- BS.readFile valuePath
  writeFile consumerPath (unlines
    [ "module ValueConsumer where"
    , "import " ++ showSDocUnsafe (ppr (renderSessionModule owner)) ++ " (captured)"
    , "__result :: Int"
    , "__result = captured ()"
    ])
  prepared <- runPipelineSessionSelected (PreparedProducts Nothing) mempty GeneralCompile
    (Just scope) consumerPath [root] Nothing
  let result = pprPipelineResult prepared
      env = prHscEnv result
  (valueOwner, capturedBytes) <- case prInjectedSessionInterfaces result of
    [snapshot] -> pure (capturedSessionInterface snapshot)
    _ -> fail "later cell did not capture its one selected value interface"
  unless (capturedBytes == consumedBytes) (fail "injection captured different interface bytes")
  when (renderSessionModule owner `Map.member` pprFinalizedModules prepared)
    (fail "thin value interface was promoted to source finalization")
  -- A provisional HPT entry alone still cannot satisfy finalization.
  absent <- newOriginalInterfaceArtifacts env (pprFinalizedModules prepared) [] root
  missing <- originalInterfaceSha256 absent valueOwner
  unless (missing == Nothing) (fail "ambient HPT issued an original interface seal")
  let capture originals = captureFinalizedModuleArtifacts originals env
        (pprFinalizedModules prepared) (pprPackageImports prepared)
        (preparedFreshDependencies prepared) root
  refused <- try (capture absent) :: IO (Either SomeException FinalizedModuleArtifacts)
  case refused of
    Left _ -> pure ()
    Right _ -> fail "finalization accepted an absent value-dependency capture"
  removeFile valuePath
  removeFile (valuePath ++ ".packages")
  removeFile (valuePath ++ ".requirements")
  originals <- newPreparedOriginalInterfaceArtifacts prepared root
  retainedBytes <- originalInterfaceBytes originals valueOwner
  unless (retainedBytes == Just consumedBytes)
    (fail "original interface seal reread the mutable session path")
  sealed <- originalInterfaceSha256 originals valueOwner >>= maybe
    (fail "selected value interface has no dependency seal") pure
  finalized <- capture originals
  let requirements = concatMap (Map.toList . localFinalizedRequirements)
        (Map.elems (finalizedLocalAdmissions finalized))
  unless (any (\((_, name), sha) -> name == moduleNameString (renderSessionModule owner)
            && sha == sealed) requirements)
    (fail "later source finalization omitted its exact thin value dependency")
  let selectedSeal = ((Text.pack "main",Text.pack (moduleNameString (renderSessionModule owner))),Text.pack sealed)
  unless (selectedSeal `elem` finalizedInterfaceSeals finalized
      && finalizedValueInterfaceSeals finalized == [selectedSeal])
    (fail "certificate closure omitted the selected captured value seal")
  unless (all ((/= moduleNameString (renderSessionModule owner)) . snd)
      (Map.keys (finalizedLocalAdmissions finalized)))
    (fail "type-only capture issued source/native authority")
  -- The versioned envelope carries the same immutable payload after removal
  -- of all three mutable input files, without manufacturing a source row.
  let encoded = toStrictByteString (encodeFinalizedModuleArtifacts finalized)
  when (BS.null encoded) (fail "captured value finalization envelope is empty")
  -- Decode the actual packet, rather than predicting its content-addressed
  -- filenames. Both source and type-only rows must be owned by the new root.
  let payload fields offset = case drop offset fields of
        TString path : TString sha : _ -> pure (Text.unpack path,Text.unpack sha)
        _ -> fail "captured payload descriptor is malformed"
      modulePayload (TList fields) | length fields == 11 = do
        ordinary <- mapM (payload fields) [3,6]
        core <- case fields !! 9 of
          TNull -> pure []
          TList descriptor -> (:[]) <$> payload descriptor 0
          _ -> fail "captured Core descriptor is malformed"
        pure (ordinary ++ core)
      modulePayload _ = fail "captured module row is malformed"
      valuePayload (TList fields) | length fields == 9 = mapM (payload fields) [2,5]
      valuePayload _ = fail "captured value row is malformed"
  payloads <- case deserialiseFromBytes decodeTerm (BSL.fromStrict encoded) of
    Right (remaining,TList [_,_,TList modules,TList values]) | BSL.null remaining, not (null values) ->
      concat <$> sequence (map modulePayload modules ++ map valuePayload values)
    _ -> fail "captured envelope lost its genuine type-only value row"
  let projected = root </> "projected-packet"
  moved <- materializeFinalizedModuleArtifacts projected finalized
  unless (toStrictByteString (encodeFinalizedModuleArtifacts moved) == encoded)
    (fail "packet custody changed captured descriptors or seals")
  movedAgain <- materializeFinalizedModuleArtifacts projected finalized
  unless (toStrictByteString (encodeFinalizedModuleArtifacts movedAgain) == encoded)
    (fail "identical packet materialization changed its evidence")
  forM_ payloads $ \(path,sha) -> do
    original <- BS.readFile (root </> path)
    owned <- BS.readFile (projected </> path)
    unless (owned == original && shaHex owned == sha)
      (fail "projected packet omitted or changed a captured payload")
  let (changedPath,_) = head payloads
  originalPayload <- BS.readFile (root </> changedPath)
  when (BS.null originalPayload) (fail "captured payload is empty")
  let changedOutput = BS.cons (BS.head originalPayload + 1) (BS.tail originalPayload)
  BS.writeFile (projected </> changedPath) changedOutput
  unchangedOrigin <- BS.readFile (root </> changedPath)
  unless (unchangedOrigin == originalPayload)
    (fail "packet output mutation also changed its independent original capture")
  conflicting <- try (materializeFinalizedModuleArtifacts projected finalized)
    `finally` BS.writeFile (projected </> changedPath) originalPayload
    :: IO (Either SomeException FinalizedModuleArtifacts)
  case conflicting of
    Left _ -> pure ()
    Right _ -> fail "conflicting existing packet bytes were replaced or accepted"
  _ <- materializeFinalizedModuleArtifacts projected finalized
  BS.appendFile (root </> changedPath) "changed"
  changed <- try (materializeFinalizedModuleArtifacts (root </> "changed-packet") finalized)
    `finally` BS.writeFile (root </> changedPath) originalPayload
    :: IO (Either SomeException FinalizedModuleArtifacts)
  case changed of
    Left _ -> pure ()
    Right _ -> fail "changed captured payload acquired new packet custody"
  forM_ payloads $ \(path,_) -> removeFile (root </> path)
  forM_ payloads $ \(path,sha) -> do
    owned <- BS.readFile (projected </> path)
    unless (shaHex owned == sha)
      (fail "projected custody depended on the removed capture directory")
  -- Reinject an actually issued interface with altered nominal evidence. The
  -- source census and checking HPT still cannot authorize that sidecar.
  _ <- mkBoundBinders ["captured"] 1 root (pprPipelineResult producer)
  BS.writeFile (valuePath ++ ".requirements") (toStrictByteString
    (encodeListLen 1 <> encodeListLen 2 <> encodeString "main" <> encodeString "UnselectedNominalOwner"))
  (_, malformedCaptures) <- injectSessionScopeWithCaptures scope env
  malformedOriginals <- newOriginalInterfaceArtifactsWithSessionCaptures env
    (pprFinalizedModules prepared) [] malformedCaptures root
  malformed <- try (capture malformedOriginals) :: IO (Either SomeException FinalizedModuleArtifacts)
  case malformed of
    Left _ -> pure ()
    Right _ -> fail "captured value requirement sidecar overrode its decoded binding types"
  _ <- mkBoundBinders ["captured"] 1 root (pprPipelineResult producer)
  BS.appendFile (valuePath ++ ".packages") "changed"
  (_, malformedPackages) <- injectSessionScopeWithCaptures scope env
  packageOriginals <- newOriginalInterfaceArtifactsWithSessionCaptures env
    (pprFinalizedModules prepared) [] malformedPackages root
  invalidPackage <- try (capture packageOriginals) :: IO (Either SomeException FinalizedModuleArtifacts)
  case invalidPackage of
    Left _ -> pure ()
    Right _ -> fail "captured value accepted a changed owning package sidecar"
  removeFile (valuePath ++ ".packages")
  removeFile (valuePath ++ ".requirements")
  -- A different genuine capture for the same owner cannot replace selected bytes.
  replacement <- mkThinSessionIface env owner [(mkVarOcc "replacement", intTy)]
  writeSessionIface env root owner replacement
  (_, changed) <- injectSessionScopeWithCaptures scope env
  conflict <- try (newOriginalInterfaceArtifactsWithSessionCaptures env
    (pprFinalizedModules prepared) [] (prInjectedSessionInterfaces result ++ changed) root)
      :: IO (Either SomeException OriginalInterfaceArtifacts)
  case conflict of
    Left _ -> pure ()
    Right _ -> fail "conflicting selected session captures were accepted"
  -- Decoding bytes for another exact owner must not issue a snapshot.
  let otherOwner = SessionModule ValMod (Generation 2)
      otherPath = sessionHiPath root otherOwner
  createDirectoryIfMissing True (takeDirectory otherPath)
  BS.writeFile otherPath consumedBytes
  wrongOwner <- try (injectSessionIface root otherOwner env) :: IO (Either SomeException HscEnv)
  case wrongOwner of
    Left _ -> pure ()
    Right _ -> fail "session injection admitted another interface owner"
  where
    temporary = do
      parent <- getTemporaryDirectory
      (path, handle) <- openTempFile parent "tidepool-session-finalized-dependency"
      hClose handle
      removeFile path
      createDirectory path
      pure path

programOriginalImportsCompilation :: IO ()
programOriginalImportsCompilation = bracket temporary removeDirectoryRecursive $ \root -> do
  let originalDirectory = root </> "Tidepool" </> "Session" </> "Lib"
      first = originalDirectory </> "G1.hs"
      second = originalDirectory </> "G2.hs"
      shadow = originalDirectory </> "G3.hs"
      target = root </> "ProgramOriginalConsumer.hs"
  createDirectoryIfMissing True originalDirectory
  writeFile first "module Tidepool.Session.Lib.G1 (a,T(..)) where\ndata T = Old\na :: Int\na = 1\n__result = (0 :: Int)\n"
  writeFile second "module Tidepool.Session.Lib.G2 (b) where\nb :: Int\nb = 2\n__result = (0 :: Int)\n"
  writeFile shadow "module Tidepool.Session.Lib.G3 (a,T(..)) where\ndata T = New\na :: Bool\na = True\n__result = (0 :: Int)\n"
  let source body = unlines
        [ "module ProgramOriginalConsumer where"
        , "import Tidepool.Session.Lib.G1"
        , "import Tidepool.Session.Lib.G2"
        , "import Tidepool.Session.Lib.G3"
        , "__result :: (Bool,Int)"
        , "__result = " ++ body
        ]
  writeFile target (source "(Tidepool.Session.Lib.G3.a,Tidepool.Session.Lib.G1.a + Tidepool.Session.Lib.G2.b)")
  withResidentPipelineSelectedRequests [root] $ \runRequest -> do
    rendered <- runRequest (pure ()) $ \compiler -> do
      checked <- compiler CheckedEnvironment mempty GeneralCompile Nothing target [root] Nothing
      let environment = crHscEnv checked
      inventories <- mapM (\owner -> do
        original <- maybe (fail "original Lib interface is absent") pure (lookupHpt (hsc_HPT environment) (mkModuleName owner))
        hydratePlannedDeclarationInventory ("main",owner)
          (show (mi_iface_hash (mi_final_exts (hm_iface original)))) environment >>= either fail pure)
        ["Tidepool.Session.Lib.G1","Tidepool.Session.Lib.G2","Tidepool.Session.Lib.G3"]
      writeFile target (source "(a,Tidepool.Session.Lib.G1.a + b)")
      libdir <- getLibdir
      transformed <- runGhc (Just libdir) $ do
        setSession environment
        targetSpec <- guessTarget target Nothing Nothing
        setTargets [targetSpec]
        _ <- depanal [] False
        summary <- getModSummary (mkModuleName "ProgramOriginalConsumer")
        parsed <- parseModule summary
        liftIO (transformProgramDeclarationImports inventories Nothing environment parsed)
      let rendered = "{-# LANGUAGE PatternSynonyms #-}\n" ++ showSDocUnsafe (ppr (pm_parsed_source transformed))
      _ <- evaluate (length rendered)
      pure rendered
    writeFile target (rendered ++ "\n__legacy = Tidepool.Session.Lib.G1.Old\n")
    runRequest (pure ()) $ \compiler -> void (compiler CheckedEnvironment mempty GeneralCompile Nothing target [root] Nothing)
    writeFile target (rendered ++ "\n__legacy = Old\n")
    rejected <- try (runRequest (pure ()) $ \compiler -> void (compiler CheckedEnvironment mempty GeneralCompile Nothing target [root] Nothing))
      :: IO (Either SomeException ())
    case rejected of
      Left _ -> pure ()
      Right _ -> fail "replacing a declaration head retained its old child unqualified"
  where
    temporary = do
      parent <- getTemporaryDirectory
      (path,handle) <- openTempFile parent "tidepool-program-originals"
      hClose handle
      removeFile path
      createDirectory path
      pure path

-- Serialized value interfaces carry the exact exported binder's fixity.
-- Nested same-spelling binders and captured expression-local operators have
-- different renamed identities and cannot supply that row.
sessionFixitiesCompilation :: IO ()
sessionFixitiesCompilation = bracket temporary removeDirectoryRecursive $ \root -> do
  let scopeRoot = root </> "session"
      build fixture generation binders expected = do
        let path = root </> fixture ++ ".hs"
            owner = SessionModule ValMod (Generation generation)
        readFile ("test-cell-splitter/fixtures/session-fixities" </> fixture ++ ".hs") >>= writeFile path
        prepared <- runPipelineSelected PreparedStg path [root]
        let result = pprPipelineResult prepared
        _ <- mkBoundBinders binders generation scopeRoot result
        hydrated <- injectSessionIface scopeRoot owner (prHscEnv result)
        iface <- maybe (fail "serialized value interface was not installed") (pure . hm_iface)
          (lookupHpt (hsc_HPT hydrated) (renderSessionModule owner))
        unless (mi_fixities iface == expected)
          (fail (fixture ++ " exported another binder's fixity"))
        pure owner
      check label owner expected = do
        let path = root </> label ++ ".hs"
            scope = SessionScope scopeRoot [owner] Nothing Nothing
        writeFile path (unlines ["module " ++ label ++ " where"
          , "import " ++ showSDocUnsafe (ppr (renderSessionModule owner)) ++ " (minus)"
          , "__result = 10 `minus` 3 `minus` 1"])
        checked <- runPipelineSessionSelected CheckedEnvironment mempty GeneralCompile
          (Just scope) path [root] Nothing
        case [body | FunBind { fun_id = name, fun_matches = MG { mg_alts = matches } }
                  <- (collectFixityData (tcg_rn_decls (crTargetTcGblEnv checked)) :: [HsBind GhcRn])
                  , occNameString (nameOccName (unLoc name)) == "__result"
                  , L _ Match { m_grhss = GRHSs { grhssGRHSs = [L _ (GRHS _ [] body)] } }
                  <- unLoc matches] of
          [body] -> valueOf owner body >>= assertEqual label expected
          _ -> fail "fixity consumer has no unique renamed result"
  right <- build "RightFixity" 1 ["minus"] [(mkVarOcc "minus", Fixity 5 InfixR)]
  check "RightConsumer" right 8
  left <- build "LeftFixity" 2 ["minus"] [(mkVarOcc "minus", Fixity 5 InfixL)]
  check "LeftConsumer" left 6
  defaultOwner <- build "DefaultFixity" 3 ["minus"] []
  check "DefaultConsumer" defaultOwner 6
  check "OlderCapturedConsumer" right 8
  _ <- build "CapturedFixity" 4 ["__observation"] []
  multi <- build "MultiFixity" 5 ["minus", "plus"]
    [(mkVarOcc "minus", Fixity 5 InfixR), (mkVarOcc "plus", Fixity 7 InfixL)]
  check "MultiConsumer" multi 8
  pure ()
  where
    valueOf :: SessionModule -> LHsExpr GhcRn -> IO Integer
    valueOf owner expression = case unLoc expression of
      HsPar _ inner -> valueOf owner inner
      HsOverLit _ OverLit { ol_val = HsIntegral literal } -> pure (il_value literal)
      OpApp _ left operator right -> do
        case unLoc operator of
          HsVar _ name -> unless
            (occNameString (nameOccName (unLoc name)) == "minus"
              && fmap moduleName (nameModule_maybe (unLoc name)) == Just (renderSessionModule owner))
            (fail "fixity consumer resolved another original binder")
          _ -> fail "fixity consumer has no resolved operator Name"
        (-) <$> valueOf owner left <*> valueOf owner right
      _ -> fail "fixity consumer changed its sample expression"
    temporary = do
      parent <- getTemporaryDirectory
      (path,handle) <- openTempFile parent "tidepool-session-fixities"
      hClose handle
      removeFile path
      createDirectory path
      pure path

collectFixityData :: (Data value, Typeable selected) => value -> [selected]
collectFixityData value = case cast value of
  Just selected -> [selected]
  Nothing -> concat (gmapQ collectFixityData value)

compilerDefaultRecipeChecks :: IO ()
compilerDefaultRecipeChecks = do
  flags <- defaultParserDynFlags
  let template = unlines
        [ "{-# LANGUAGE ImportQualifiedPost #-}"
        , "{{CELL_PRAGMAS}}"
        , "module CompilerDefaults where"
        , "import Prelude qualified as TidepoolCompilerDefaultInt"
        , "import Data.Text qualified as TidepoolCompilerDefaultText"
        , "{{CELL_IMPORTS}}"
        ] ++ preambleDefaultDeclaration ++ "{{CELL_DECLS}}\n{{CELL_BODY}}\n"
      authored = "import qualified Data.Text as TidepoolCompilerDefaultDouble\n"
        ++ "default (TidepoolCompilerDefaultDouble.Text)\n"
      wrappers = ["{{CELL_PRAGMAS}}", "{{CELL_IMPORTS}}", "{{CELL_DECLS}}", "{{CELL_BODY}}"]
  plan <- analyzeOrderedCellWithFlags flags template authored >>= either (fail . show) pure
  rendered <- either fail pure (renderCellCheckSource template plan)
  unless ("default (TidepoolCompilerDefaultIntX.Int, TidepoolCompilerDefaultDoubleX.Double, TidepoolCompilerDefaultTextX.Text)"
      `isInfixOf` rendered
      && "default (TidepoolCompilerDefaultDouble.Text)" `isInfixOf` rendered
      && all (not . (`isInfixOf` rendered)) wrappers) $
    fail "compiler default aliases collided with native import namespaces or rewrote an authored default"
  -- A bare authored/default fixture does not issue a compiler default recipe.
  plain <- either fail pure (captureCompilerDefaultRecipe flags "module Plain where\nimport Prelude\ndefault (Int)\n")
  unchanged <- either fail pure (qualifyCompilerDefault plain [] "module Plain where\nimport Prelude\ndefault (Int)\n")
  unless (unchanged == "module Plain where\nimport Prelude\ndefault (Int)\n") $
    fail "compiler default recipe changed an authored declaration"
  forM_ ["module Malformed where\n" ++ preambleImportMarker ++ "default (Text)\n"
    , "module Malformed where\n" ++ preambleImportMarker ++ preambleDefaultDeclaration] $ \malformed ->
      unless (case captureCompilerDefaultRecipe flags malformed of Left _ -> True; Right _ -> False) $
        fail "malformed compiler default recipe silently became an authored default"
  let declarationTemplate = "{-# LANGUAGE StandaloneDeriving, DeriveGeneric #-}\nmodule DeclarationDefaults where\nimport Prelude\n"
        ++ preambleDefaultDeclaration ++ "{{TURN}}\n__result = ()\n"
  declarationPlan <- analyzeOrderedCellWithFlags flags declarationTemplate
    "import Data.List (sort)\ndata Shadow = Shadow\n" >>= either (fail . show) pure
  unless (any ((== RetainedGeneratedImport) . locatedImportIntent)
      (prologueImports (cellPlanPrologue declarationPlan))) $
    fail "declaration default regression lacks its generated imports"
  let declarationSource = DeclarationSource (cellPlanPrologue declarationPlan) (cellPlanDeclarationBase declarationPlan)
  declaration <- either fail pure (renderDeclarationForTemplate declarationTemplate declarationSource)
  let noDefaultTemplate = "module DeclarationImports where\nimport Prelude\n{{CELL_IMPORTS}}\n__before = ()\n{{TURN}}\n"
      noDefaultSource = declarationSource
        { declarationPrologue = (declarationPrologue declarationSource)
            { prologueCompilerDefault = plain } }
  withoutDefault <- either fail pure (renderDeclarationForTemplate noDefaultTemplate noDefaultSource)
  forM_ [declaration, withoutDefault] $ \renderedDeclaration -> do
    declarationFlags <- templateParserFlags flags renderedDeclaration >>= either (fail . show) pure
    case unP Parser.parseModule (initParserState (initParserOpts declarationFlags)
        (stringToStringBuffer renderedDeclaration)
        (mkRealSrcLoc (mkFastString "<declaration-defaults>") 1 1)) of
      PFailed _ -> fail "declaration header placed authored/generated imports after declarations"
      POk _ parsed -> assertEqual "declaration header retains parsed prologue imports" True
        (mkModuleName "Data.List" `elem` map (unLoc . ideclName . unLoc) (hsmodImports (unLoc parsed)))
  case renderDeclarationForTemplate "module MissingImports where\n__before = ()\n{{TURN}}\n" noDefaultSource of
    Left failure | "lacks {{CELL_IMPORTS}}" `isInfixOf` failure -> pure ()
    _ -> fail "declaration template without an import slot accepted nonempty imports"
  let noImports = noDefaultSource
        { declarationPrologue = (declarationPrologue noDefaultSource) { prologueImports = [] } }
  _ <- either fail pure (renderDeclarationForTemplate "module NoImports where\n{{TURN}}\n" noImports)
  pure ()

orderedInferenceSegments :: IO ()
orderedInferenceSegments = do
  forM_ ["-fdefer-type-errors", "-fdefer-typed-holes", "-fdefer-out-of-scope-variables"] $ \option -> do
    deferred <- analyzeOrderedCell template ("{-# OPTIONS_GHC " ++ option ++ " #-}\npure missingName")
    case deferred of
      Left CellPrologueFailure {} -> pure ()
      _ -> fail ("ordered program accepted deferred errors: " ++ option)
  rejected <- analyzeOrderedCell template "let { infixr 5 `minus`; minus = (-) :: Int -> Int -> Int }\n10 `minus` 3 `minus` 1"
  case rejected of
    Left CellUnsupportedLocalFixity {} -> pure ()
    _ -> fail ("ordered program accepted a local fixity absent from future Val evidence: " ++ show rejected)
  plan <- analyzeOrderedCell template source >>= either (fail . renderCellSplitError) pure
  let segments = cellInferenceSegments plan
      kinds = map (map (sbKind . cellAnalysisVerdict) . cellPlanItems) segments
      ordinals = map (map cellAnalysisSourceOrdinal . concatMap cellAnalysisSourceItems . cellPlanItems) segments
  assertEqual "ordered declaration and executable runs" [[KBind], [KDecl], [KBind, KBind], [KDecl], [KExpr]] kinds
  assertEqual "ordered segments retain source ordinals" [[0], [1], [2, 3], [4], [5]] ordinals
  case segments of
    [_, firstDeclaration, _, secondDeclaration, _] -> do
      assertEqual "first declaration owns its generated Generic" ["First"]
        (map genericDeclarationTarget (cellPlanGenericDeclarations firstDeclaration))
      assertEqual "second declaration owns its generated Generic" ["Second"]
        (map genericDeclarationTarget (cellPlanGenericDeclarations secondDeclaration))
      let firstSource = concatMap cellAnalysisSource (cellPlanItems firstDeclaration)
          secondSource = concatMap cellAnalysisSource (cellPlanItems secondDeclaration)
      assertEqual "first declaration excludes later source" False ("Second" `isInfixOf` firstSource)
      assertEqual "second declaration excludes earlier source" False ("First" `isInfixOf` secondSource)
    _ -> fail "unexpected ordered segment count"
  forM_
    [ "answer = (43 :: Int)"
    , "answer :: Int\nanswer = 43"
    , "answer = (43 :: Int)\nother = answer + 1"
    ] $ \declarations -> do
      imported <- analyzeOrderedCell template
        ("import Data.List\n" ++ declarations)
        >>= either (fail . renderCellSplitError) pure
      assertEqual "segmentation preserves each parser-owned source item"
        (concatMap cellAnalysisSourceItems (cellPlanItems imported))
        (concatMap (concatMap cellAnalysisSourceItems . cellPlanItems)
          (cellInferenceSegments imported))
      case map cellPlanItems (cellInferenceSegments imported) of
        [[prologue], [declaration]] -> do
          assertEqual "import prologue retains its own original owner" True
            (cellAnalysisPrologueOnly prologue)
          assertEqual "prologue contains no authored declaration" ""
            (cellAnalysisSource prologue)
          assertEqual "authored declarations retain their own original owner" False
            (cellAnalysisPrologueOnly declaration)
          assertEqual "declaration source is not duplicated across owners" 1
            (length [() | suffix <- tails (cellAnalysisSource declaration)
                        , "answer =" `isPrefixOf` suffix])
        _ -> fail "import prologue and authored declarations share a segment"
  importedType <- analyzeOrderedCell template "import Data.List\ndata Imported = Imported"
    >>= either (fail . renderCellSplitError) pure
  case cellInferenceSegments importedType of
    [prologue, declaration] -> do
      assertEqual "import prologue has no generated Generic" []
        (map genericDeclarationTarget (cellPlanGenericDeclarations prologue))
      assertEqual "authored type owns its generated Generic" ["Imported"]
        (map genericDeclarationTarget (cellPlanGenericDeclarations declaration))
      assertEqual "import prologue has no generated display" []
        (map structuralDisplayTargetName (cellPlanStructuralDisplayTargets prologue))
      assertEqual "authored type owns its generated display" ["Imported"]
        (map structuralDisplayTargetName (cellPlanStructuralDisplayTargets declaration))
    _ -> fail "import and type declaration share a segment"
  let retainedTemplate = unlines
        [ if line == "module CellCheck where"
            then unlines [line
              , "import qualified GHC.Generics as TidepoolCompilerGeneric"
              , "import qualified Tidepool.Inspection.Display as TidepoolCompilerDisplay"]
            else line
        | line <- lines template ]
  retainedPlan <- analyzeOrderedCell retainedTemplate "data Retained = Retained"
    >>= either (fail . renderCellSplitError) pure
  let imports = map locatedImportSource (prologueImports (cellPlanPrologue retainedPlan))
      displayAlias = cellPlanStructuralDisplayAlias retainedPlan
      declarations = concatMap (concatMap cellAnalysisSource . cellPlanItems)
        (cellInferenceSegments retainedPlan)
  [genericAlias] <- pure
    [alias | imported <- imports
      , ["import", "qualified", "GHC.Generics", "as", alias] <- [words imported]]
  assertEqual "generated Generic alias avoids retained template imports" True
    (genericAlias /= "TidepoolCompilerGeneric")
  assertEqual "generated Generic uses its selected import alias" True
    ((genericAlias ++ ".Generic") `isInfixOf` declarations)
  assertEqual "generated Display alias avoids retained template imports" True
    (displayAlias /= "TidepoolCompilerDisplay")
  assertEqual "generated Display imports its selected alias" True
    (("import qualified Tidepool.Inspection.Display as " ++ displayAlias) `elem` imports)
  assertEqual "generated Display uses its selected import alias" True
    ((displayAlias ++ ".Display") `isInfixOf` declarations)
  where
    source = unlines
      [ "first <- pure (0 :: Int)"
      , "data First = First"
      , "middle <- pure first"
      , "middleAgain <- pure middle"
      , "data Second = Second"
      , "middleAgain"
      ]
    template = unlines
      [ "{-# LANGUAGE DeriveGeneric, StandaloneDeriving #-}"
      , "{{CELL_PRAGMAS}}"
      , "module CellCheck where"
      , "{{CELL_IMPORTS}}"
      , "{{CELL_DECLS}}"
      , "__tidepool_cell_check = do { {{CELL_BODY}} } :: Maybe ()"
      ]

checkingSourceRequestRoundTrip :: IO ()
checkingSourceRequestRoundTrip = do
  let fields = [Input "Complete.hs", CheckSource, Include "source-graph"
               , ModuleCandidates "candidates.cbor", BuildProductsDir "interfaces"]
  [flag, payload] <- pure (workerArgv fields)
  case workerRequestFromArgv (workerArgv fields) of
    Right (Just request) -> do
      assertEqual "checking mode retained" True (requestCheckSource request)
      assertEqual "complete input retained" ["Complete.hs"] (requestFiles request)
      assertEqual "candidate manifest retained" (Just "candidates.cbor") (requestModuleCandidates request)
      assertEqual "no product output" Nothing (requestOutDir request)
      assertEqual "no cell rewriting" False (requestCell request)
    other -> fail ("checking source request round trip failed: " ++ show other)
  case workerRequestFromArgv ["--worker-request-v16", payload] of
    Left _ -> pure ()
    other -> fail ("retired request protocol accepted: " ++ show other)
  let retiredPayload = take 14 payload ++ "36" ++ drop 16 payload
  case workerRequestFromArgv [flag, retiredPayload] of
    Left _ -> pure ()
    other -> fail ("retired request bytes accepted: " ++ show other)

  [singleFlag, singlePayload] <- pure (workerArgv [ActivationPreview])
  forM_ ["22", "2d"] $ \retiredTag -> do
    let retiredField = take (length singlePayload - 2) singlePayload ++ retiredTag
    case workerRequestFromArgv [singleFlag, retiredField] of
      Left message | "retired field tag" `isInfixOf` message -> pure ()
      other -> fail ("retired fold/type-pin field accepted: " ++ show other)

certificationRequestValidation :: IO ()
certificationRequestValidation = do
  let valid = [Input "Probe.hs", Targets ["probe"], Include "lib"
              , SessionRoot "/session", BuildProductsDir "/products"
              , CertifyHomeProducts]
  case workerRequestFromArgv (workerArgv valid) of
    Right (Just request) | requestCertifyHomeProducts request -> pure ()
    other -> fail ("home-product certification request rejected: " ++ show other)
  case workerRequestFromArgv (workerArgv (valid ++ [SessionArtifacts "/scope.cbor"])) of
    Right (Just request) | requestSessionArtifacts request == Just "/scope.cbor" -> pure ()
    other -> fail ("home-product certification rejected explicit exact context: " ++ show other)
  forM_ [ Cell, Classify, Turn, InspectType "Int", InspectTypeBatch "Batch.hs"
        , DeclarationJoin "join.cbor", BindGen 1, InjectVal "Val1"
        , ModuleCandidates "candidates.cbor"
        , ActivationPreview, TargetModuleOnly
        , RetainedGeneration (SymbolIdentity "main" "Producer" "value" "value" Nothing) 1
        ] $ \field ->
    case workerRequestFromArgv (workerArgv (valid ++ [field])) of
      Left _ -> pure ()
      other -> fail ("home-product certification accepted incompatible field "
        ++ show field ++ ": " ++ show other)
  forM_ [[CertifyHomeProducts], valid ++ [Input "Other.hs"]] $ \fields ->
    case workerRequestFromArgv (workerArgv fields) of
      Left _ -> pure ()
      other -> fail ("home-product certification accepted ambiguous input: " ++ show other)

requestShapeValidation :: IO ()
requestShapeValidation = do
  let request fields = case workerRequestFromArgv (workerArgv fields) of
        Right (Just decoded) -> pure decoded
        other -> fail ("request shape fixture failed to decode: " ++ show other)
      cases =
        [ ("source check accepts explicit compiler inputs"
          , [Input "Probe.hs", CheckSource, Include "lib", ModuleCandidates "scope", BuildProductsDir "products"]
          , Right ())
        , ("cell plan accepts common compile context"
          , [Input "Cell.hs", CellPlan, Include "lib", OutputDir "out", Target "Main", Targets ["A"], SessionRoot "session"]
          , Right ())
        ]
        ++ [("source check rejects " ++ label, [Input "Probe.hs", CheckSource, field], Left InvalidSourceCheckShape)
           | (label, field) <- sourceCheckExcludedFields]
        ++ [("cell plan rejects " ++ label, [Input "Cell.hs", CellPlan, field], Left InvalidCellPlanShape)
           | (label, field) <- cellPlanExcludedFields]
        ++ [ ("source check rejects multiple inputs", [Input "A.hs", Input "B.hs", CheckSource], Left InvalidSourceCheckShape)
           , ("cell plan rejects multiple inputs", [Input "A.hs", Input "B.hs", CellPlan], Left InvalidCellPlanShape)
           , ("source-check error precedence is retained", [Input "Cell.hs", CheckSource, CellPlan], Left InvalidSourceCheckShape)
           ]
  forM_ cases $ \(label, fields, expected) -> do
    decoded <- request fields
    assertEqual label expected (validateRequestShape decoded)
  where
    sourceCheckExcludedFields =
      [ ("output directory", OutputDir "out"), ("target", Target "Main"), ("target group", Targets ["Main"])
      , ("module-only target", TargetModuleOnly), ("binding generation", BindGen 1)
      , ("session injection", InjectVal "Val1"), ("session root", SessionRoot "session")
      , ("session incarnation", SessionIncarnation "1"), ("session artifacts", SessionArtifacts "scope")
      , ("turn mode", Turn), ("turn template", TurnTemplate "kind" "template")
      , ("turn output", TurnOut "turn"), ("turn verdict", TurnVerdict "ok")
      , ("classification mode", Classify), ("classification output", ClassifyOut "classify")
      , ("cell mode", Cell), ("cell plan mode", CellPlan), ("cell template", CellTemplate "template")
      , ("cell output", CellOut "cell"), ("harness profile", HarnessProfile)
      , ("declaration join", DeclarationJoin "join"), ("declaration join output", DeclarationJoinOut "join-out")
      , ("activation preview", ActivationPreview)
      , ("inspection strictness", InspectionStrict)
      , ("inspection output", InspectOut "inspect"), ("type batch", InspectTypeBatch "batch")
      , ("inspection query", InspectType "Int")
      , ("retained generation", RetainedGeneration (SymbolIdentity "main" "M" "value" "x" Nothing) 1)
      ]
    cellPlanExcludedFields =
      [ ("cell mode", Cell), ("turn mode", Turn), ("classification mode", Classify)
      , ("inspection query", InspectType "Int"), ("session artifacts", SessionArtifacts "scope")
      , ("declaration join", DeclarationJoin "join"), ("candidate authority", ModuleCandidates "scope")
      , ("activation preview", ActivationPreview)
      , ("binding generation", BindGen 1)
      , ("retained generation", RetainedGeneration (SymbolIdentity "main" "M" "value" "x" Nothing) 1)
      ]

requestFieldOrdering :: IO ()
requestFieldOrdering = do
  let identity = SymbolIdentity "main" "Producer" "value" "value" Nothing
      fields =
        [ Input "first.hs", Targets ["a", "b"], Include "one", InjectVal "Val1"
        , TurnTemplate "first" "one.tpl", InspectType "Int", RetainedGeneration identity 1
        , TurnVerdict "old"
        , Input "second.hs", Targets ["c", "d"], Include "two", InjectVal "Val2"
        , TurnTemplate "second" "two.tpl", InspectInfo "answer"
        , RetainedGeneration identity 2, TurnVerdict "new"
        ]
  case workerRequestFromArgv (workerArgv fields) of
    Right (Just request) -> do
      assertEqual "input order" ["first.hs", "second.hs"] (requestFiles request)
      assertEqual "target groups retain order" ["a", "b", "c", "d"] (requestTargets request)
      assertEqual "include order" ["one", "two"] (requestIncludes request)
      assertEqual "injected value order" ["Val1", "Val2"] (requestInjectVals request)
      assertEqual "template order" [("first", "one.tpl"), ("second", "two.tpl")] (requestTurnTemplates request)
      assertEqual "inspection order" [InspectTypeOf "Int", InspectNameInfo "answer"] (requestInspections request)
      assertEqual "duplicate map key remains last-write-wins" (Just 2)
        (Map.lookup identity (requestRetainedGenerations request))
      assertEqual "duplicate scalar remains last-write-wins" (Just "new")
        (requestTurnVerdict request)
    other -> fail ("ordered request failed to decode: " ++ show other)

multilineLetPlacement :: DynFlags -> IO ()
multilineLetPlacement flags = do
  let template = "{{TURN_STMT}}pure ({{BINDERS}})\n"
      signed = unlines
        [ "let findings :: [Text]"
        , "    findings ="
        , "      [\"ready\"]"
        ]
      expected = unlines
        [ "let { findings :: [Text]"
        , "    ;findings ="
        , "      [\"ready\"]"
        , " }"
        , "pure (findings)"
        ]
  assertEqual "signature and equation retain their separator"
    expected (spliceTemplate template signed "findings")
  assertEqual "RHS continuation is not a new declaration"
    "let { checks =\n      [1, 2]\n }\npure (checks)\n"
    (spliceTemplate template "let checks =\n      [1, 2]\n" "checks")
  assertEqual "explicit-brace let is kept"
    "let { checks = [1, 2] }\npure (checks)\n"
    (spliceTemplate template "let { checks = [1, 2] }" "checks")
  forM_ ["let checks =\n      [1, 2]\n", signed] $ \source ->
    case splitCellWithFlags flags source of
      Right [item] -> assertEqual "valid multiline let stays one bind item"
        KBind (sbKind (classifyWithFlags flags (cellSourceText item)))
      other -> fail ("valid multiline let split unexpectedly: " ++ show other)
  assertEqual "under-indented wave RHS is not a valid binding"
    KExpr (sbKind (classifyWithFlags flags "let checks =\n  [1, 2]\n"))
  assertEqual "under-indented typed wave RHS is not a valid binding"
    KExpr (sbKind (classifyWithFlags flags "let findings :: [Text] =\n  [\"ready\"]\n"))

multilineLetCompilation :: IO ()
multilineLetCompilation = bracket temporary removeDirectoryRecursive $ \root -> do
  let includes = ["lib"]
      template name = unlines
        [ "{-# LANGUAGE OverloadedStrings #-}"
        , "module " ++ name ++ " where"
        , "import Data.Text (Text)"
        , "result :: IO Int"
        , "result = do { {{TURN_STMT}}; pure (length {{BINDERS}}) }"
        ]
      cases =
        [ ("LetChecks", "let checks =\n      [1, 2]\n", "checks")
        , ("LetFindings", "let findings :: [Text]\n    findings =\n      [\"ready\"]\n", "findings")
        ]
  withResidentPipelineSelectedRequests includes $ \runRequest ->
    runRequest (pure ()) $ \compiler -> forM_ cases $ \(name, source, binder) -> do
      let path = root </> (name ++ ".hs")
      writeFile path (spliceTemplate (template name) source binder)
      _ <- compiler CheckedEnvironment mempty GeneralCompile Nothing path includes Nothing
      pure ()
  where
    temporary = do
      parent <- getTemporaryDirectory
      (path, handle) <- openTempFile parent "tidepool-multiline-let"
      hClose handle
      removeFile path
      createDirectory path
      pure path

-- A source-name collision must not authorize stale Core or prepared STG.
-- The GHC interpreter is an independent value oracle: it does not consume the
-- Tidepool memo, dependency flags or prepared products being checked here.
quasiQuoteSourceReuseCompilation :: IO ()
quasiQuoteSourceReuseCompilation = bracket temporary removeDirectoryRecursive $ \root -> do
  let fixtures = "test-cell-splitter/fixtures/quasiquote-reuse"
      quoterDirectory = root </> "Tidepool" </> "QQ"
      input = root </> "quote-input"
      target = root </> "ShadowUser.hs"
      history = [37, 91, 37] :: [Integer]
      inspect compiler expected = do
        writeFile input (show expected)
        prepared <- compiler PreparedStg mempty GeneralCompile Nothing target [] Nothing
        actual <- boxedIntProducts "ShadowQuoted" "value" prepared
        assertEqual "fresh source observes the current external input" (expected, expected) actual
        pure actual
  createDirectoryIfMissing True quoterDirectory
  copyFile (fixtures </> "ShadowValidate.hs") (quoterDirectory </> "Validate.hs")
  copyFile (fixtures </> "ShadowUser.hs") target
  quoted <- Text.pack <$> readFile' (fixtures </> "ShadowQuoted.hs")
  writeFile (root </> "ShadowQuoted.hs")
    (Text.unpack (Text.replace "QUOTE_INPUT_PATH" (Text.pack input) quoted))
  warm <- withResidentPipelineSelectedRequests [root] $ \runRequest -> do
    within <- runRequest (pure ()) $ \compiler -> mapM (inspect compiler) history
    -- Closing the transaction discards memo state. Reopening on the same
    -- resident compiler must agree with both the warm history and cold GHC.
    across <- forM history $ \expected -> runRequest (pure ()) $ \compiler -> inspect compiler expected
    assertEqual "transaction restart agrees with warm quotation history" within across
    pure within
  cold <- forM history $ \expected -> do
    writeFile input (show expected)
    prepared <- runPipelineSelected PreparedStg target [root]
    boxedIntProducts "ShadowQuoted" "value" prepared
  assertEqual "fresh compiler sessions agree with warm quotation history" warm cold
  oracle <- forM history $ \expected -> do
    writeFile input (show expected)
    coldQuasiQuoteValue [root] target "ShadowUser"
  assertEqual "quoted Core agrees with independent cold GHC" oracle (map (fromInteger . fst) warm)
  assertEqual "quoted prepared STG agrees with independent cold GHC" oracle (map (fromInteger . snd) warm)
  where
    temporary = do
      parent <- getTemporaryDirectory
      (path, handle) <- openTempFile parent "tidepool-quasiquote-reuse"
      hClose handle
      removeFile path
      createDirectory path
      pure path

-- Use the deployed source resource itself. A local module with its spelling
-- is a separate negative control above, never a stand-in for this provider.
pinnedQuasiQuoteSourceCompilation :: IO ()
pinnedQuasiQuoteSourceCompilation = bracket temporary removeDirectoryRecursive $ \root -> do
  let fixtures = "test-cell-splitter/fixtures/quasiquote-reuse"
      target = root </> "PinnedUser.hs"
      includes = [root, "lib"]
  forM_ ["PinnedQuoted.hs", "PinnedUser.hs"] $ \name ->
    copyFile (fixtures </> name) (root </> name)
  withResidentPipelineSelected includes $ \compiler -> do
    fresh <- compiler PreparedStg mempty GeneralCompile Nothing target [] Nothing
    freshSharing <- compilerProductSharing fresh
    repeated <- compiler PreparedStg mempty GeneralCompile Nothing target [] Nothing
    repeatedSharing <- compilerProductSharing repeated
    assertQuotedSourceUncached "pinned provider, fresh" fresh
    assertQuotedSourceUncached "pinned provider, repeated" repeated
    assertCoreSharing "actual pinned quotation is recompiled" False ["PinnedQuoted"] freshSharing repeatedSharing
    assertStgSharing "actual pinned quotation is prepared afresh" False ["PinnedQuoted"] freshSharing repeatedSharing
    assertSameInterfaceEvidenceAndPreparedShape "stable pinned quotation" fresh repeated
  actual <- coldQuasiQuoteValue includes target "PinnedUser"
  assertEqual "independent GHC executes the real pinned provider" 23 actual
  where
    temporary = do
      parent <- getTemporaryDirectory
      (path, handle) <- openTempFile parent "tidepool-pinned-quasiquote"
      hClose handle
      removeFile path
      createDirectory path
      pure path

assertQuotedSourceUncached :: String -> PreparedPipelineResult -> IO ()
assertQuotedSourceUncached label prepared = do
  let evidence = preparedFreshDependencies prepared
  when (dependencyCacheSafe evidence || dependencySelectionComplete evidence) $
    fail (label ++ " authorized ordinary source reuse")

-- This fixture's exported value must be exactly a boxed integer in both
-- compiler products, not merely contain the expected literal somewhere.
boxedIntProducts :: String -> String -> PreparedPipelineResult -> IO (Integer, Integer)
boxedIntProducts owner occurrence prepared = do
  finalized <- maybe (fail "quoted owner has no finalized Core") pure
    (Map.lookup (mkModuleName owner) (pprFinalizedModules prepared))
  core <- case [value | (binder, rhs) <- Core.flattenBinds (cg_binds (finalizedTidyGuts finalized))
      , getOccString binder == occurrence
      , Core.App (Core.Var constructor) (Core.Lit (LitNumber LitNumInt value)) <- [rhs]
      , constructor == dataConWorkId intDataCon] of
    [value] -> pure value
    _ -> fail "quoted Core value is not one boxed Int literal"
  stg <- case [value | modul <- pprModules prepared, moduleNameString (moduleName (pmModule modul)) == owner
      , (Stg.StgTopLifted binding, _) <- pmBindings modul, (binder, rhs) <- pairs binding
      , getOccString binder == occurrence
      , Stg.StgRhsCon _ constructor _ _ [Stg.StgLitArg (LitNumber LitNumInt value)] _ <- [rhs]
      , constructor == intDataCon] of
    [value] -> pure value
    _ -> fail "quoted prepared value is not one boxed Int literal"
  pure (core, stg)
  where
    pairs (Stg.StgNonRec binder rhs) = [(binder, rhs)]
    pairs (Stg.StgRec bindings) = bindings

coldQuasiQuoteValue :: [FilePath] -> FilePath -> String -> IO Int
coldQuasiQuoteValue includes target owner = do
  libdir <- getLibdir
  runGhc (Just libdir) $ do
    flags <- getSessionDynFlags
    _ <- setSessionDynFlags (gopt_set flags Opt_ForceRecomp)
      { backend = interpreterBackend, ghcLink = LinkInMemory
      , importPaths = includes, packageDBFlags = [PackageDB GlobalPkgDb, ClearPackageDBs] }
    targetSpec <- guessTarget target Nothing Nothing
    setTargets [targetSpec]
    load LoadAllTargets >>= \case
      Failed -> fail "independent GHC quotation compilation failed"
      Succeeded -> pure ()
    imported <- parseImportDecl ("import qualified " ++ owner)
    setContext [IIDecl imported]
    actual <- dynCompileExpr (owner ++ ".result")
    maybe (fail "independent GHC quotation result is not Int") pure (fromDynamic actual)

untrackedCompileTimeCompilation :: IO ()
untrackedCompileTimeCompilation = bracket temporary removeDirectoryRecursive $ \root -> do
  let dependency = root </> "QuasiQuoteDependency.hs"
      templateDependency = root </> "TemplateDependency.hs"
      cppDependency = root </> "CppDependency.hs"
      cppUser = root </> "CppUser.hs"
      externalPreprocessor = root </> "untracked-preprocessor"
      preprocessorInput = root </> "preprocessor-input.txt"
      preprocessedDependency = root </> "PreprocessedDependency.hs"
      preprocessedUser = root </> "PreprocessedUser.hs"
      noQuoteUser = root </> "NoQuoteUser.hs"
      target = root </> "QuasiQuoteTarget.hs"
      -- A local provider may use the same spelling as a shipped provider.
      -- Neither that spelling nor an import/reexport shape proves purity.
      qqDir = root </> "Tidepool" </> "QQ"
      qqLabel = qqDir </> "Label.hs"
      -- Compile through consumers so these modules own dependency products.
      labelDependency = root </> "LabelDependency.hs"
      labelUser = root </> "LabelUser.hs"
      localQuoter = root </> "LocalQuoteQuoter.hs"
      localQuoteDependency = root </> "LocalQuoteDependency.hs"
      localQuoteUser = root </> "LocalQuoteUser.hs"
  writeFile dependency $ unlines
    [ "{-# LANGUAGE QuasiQuotes #-}"
    , "module QuasiQuoteDependency (value) where"
    , "value :: Int"
    , "value = 42"
    ]
  writeFile templateDependency $ unlines
    [ "{-# LANGUAGE TemplateHaskell #-}"
    , "module TemplateDependency (other) where"
    , "other :: Int"
    , "other = 1"
    ]
  writeFile noQuoteUser $ unlines
    [ "{-# LANGUAGE QuasiQuotes #-}"
    , "module NoQuoteUser where"
    , "import QuasiQuoteDependency (value)"
    , "result = value"
    ]
  writeFile cppDependency $ unlines
    [ "{-# LANGUAGE CPP #-}"
    , "module CppDependency where"
    , "value = (42 :: Int)"
    ]
  writeFile cppUser $ unlines
    [ "module CppUser where"
    , "import CppDependency (value)"
    , "result = value"
    ]
  writeFile preprocessorInput "outside the Haskell source graph"
  writeFile externalPreprocessor $ unlines
    [ "#!/usr/bin/env sh"
    , "cat " ++ show preprocessorInput ++ " >/dev/null"
    , "cat \"$2\" >\"$3\""
    ]
  preprocessorPermissions <- getPermissions externalPreprocessor
  setPermissions externalPreprocessor (setOwnerExecutable True preprocessorPermissions)
  writeFile preprocessedDependency $ unlines
    [ "{-# OPTIONS_GHC -F -pgmF " ++ show externalPreprocessor ++ " #-}"
    , "module PreprocessedDependency where"
    , "value = (42 :: Int)"
    ]
  writeFile preprocessedUser $ unlines
    [ "module PreprocessedUser where"
    , "import PreprocessedDependency (value)"
    , "result = value"
    ]
  writeFile target $ unlines
    [ "module QuasiQuoteTarget where"
    , "import QuasiQuoteDependency (value)"
    , "import TemplateDependency (other)"
    , "result = value + other"
    ]
  createDirectoryIfMissing True qqDir
  writeFile qqLabel $ unlines
    [ "module Tidepool.QQ.Label (label) where"
    , "import Language.Haskell.TH (litE, stringL)"
    , "import Language.Haskell.TH.Quote (QuasiQuoter(..))"
    , "label :: QuasiQuoter"
    , "label = QuasiQuoter"
    , "  { quoteExp = \\source -> litE (stringL source)"
    , "  , quotePat = \\_ -> fail \"label is expression-only\""
    , "  , quoteType = \\_ -> fail \"label is expression-only\""
    , "  , quoteDec = \\_ -> fail \"label is expression-only\""
    , "  }"
    ]
  writeFile labelDependency $ unlines
    [ "{-# LANGUAGE QuasiQuotes #-}"
    , "module LabelDependency (value) where"
    , "import Tidepool.QQ.Label (label)"
    , "value :: String"
    , "value = [label|orbit-motif|]"
    ]
  writeFile labelUser $ unlines
    [ "module LabelUser where"
    , "import LabelDependency (value)"
    , "result = value"
    ]
  writeFile localQuoter $ unlines
    [ "module LocalQuoteQuoter (myqq) where"
    , "import Language.Haskell.TH (litE, stringL)"
    , "import Language.Haskell.TH.Quote (QuasiQuoter(..))"
    , "myqq :: QuasiQuoter"
    , "myqq = QuasiQuoter"
    , "  { quoteExp = \\source -> litE (stringL source)"
    , "  , quotePat = \\_ -> fail \"myqq is expression-only\""
    , "  , quoteType = \\_ -> fail \"myqq is expression-only\""
    , "  , quoteDec = \\_ -> fail \"myqq is expression-only\""
    , "  }"
    ]
  writeFile localQuoteDependency $ unlines
    [ "{-# LANGUAGE QuasiQuotes #-}"
    , "module LocalQuoteDependency (value) where"
    , "import LocalQuoteQuoter (myqq)"
    , "value :: String"
    , "value = [myqq|hello|]"
    ]
  writeFile localQuoteUser $ unlines
    [ "module LocalQuoteUser where"
    , "import LocalQuoteDependency (value)"
    , "result = value"
    ]
  -- Open imports and reexports carry the same conservative quotation fact.
  let decoyModule = root </> "Decoy.hs"
      openImportDependency = root </> "OpenImportDependency.hs"
      openImportUser = root </> "OpenImportUser.hs"
      reexportModule = root </> "LabelReexport.hs"
      reexportDependency = root </> "ReexportDependency.hs"
      reexportUser = root </> "ReexportUser.hs"
  writeFile decoyModule $ unlines
    [ "module Decoy (decoy) where"
    , "decoy :: Int"
    , "decoy = 0"
    ]
  writeFile openImportDependency $ unlines
    [ "{-# LANGUAGE QuasiQuotes #-}"
    , "module OpenImportDependency (value) where"
    , "import Tidepool.QQ.Label"
    , "import Decoy"
    , "value :: String"
    , "value = [label|orbit-motif|]"
    ]
  writeFile openImportUser $ unlines
    [ "module OpenImportUser where"
    , "import OpenImportDependency (value)"
    , "result = value"
    ]
  writeFile reexportModule $ unlines
    [ "module LabelReexport (label) where"
    , "import Tidepool.QQ.Label (label)"
    ]
  writeFile reexportDependency $ unlines
    [ "{-# LANGUAGE QuasiQuotes #-}"
    , "module ReexportDependency (value) where"
    , "import LabelReexport (label)"
    , "value :: String"
    , "value = [label|orbit-motif|]"
    ]
  writeFile reexportUser $ unlines
    [ "module ReexportUser where"
    , "import ReexportDependency (value)"
    , "result = value"
    ]
  direct <- runPipelineSelected PreparedStg target [root]
  let evidence = preparedFreshDependencies direct
  when (dependencyCacheSafe evidence || dependencySelectionComplete evidence) $
    fail "TemplateHaskell source produced complete dependency evidence"
  noQuotes <- runPipelineSelected PreparedStg noQuoteUser [root]
  assertComplete "QuasiQuotes enabled without occurrences" noQuotes
  cpp <- runPipelineSelected PreparedStg cppUser [root]
  assertIncomplete "CPP enabled" cpp
  preprocessed <- runPipelineSelected PreparedStg preprocessedUser [root]
  assertIncomplete "external preprocessor with an untracked input" preprocessed
  withResidentPipelineSelected [root] $ \compile -> do
    cold <- compile PreparedStg mempty GeneralCompile Nothing target [] Nothing
    coldSharing <- compilerProductSharing cold
    warm <- compile PreparedStg mempty GeneralCompile Nothing target [] Nothing
    warmSharing <- compilerProductSharing warm
    assertIncomplete "TemplateHaskell source, repeated" warm
    assertSameInterfaceEvidenceAndPreparedShape "conservative compile-time gate preserves native output" cold warm
    assertCoreSharing "unused QuasiQuotes extension retains its tracked owner" True
      ["QuasiQuoteDependency"] coldSharing warmSharing
    assertCoreSharing "TemplateHaskell source is finalized afresh" False
      ["TemplateDependency"] coldSharing warmSharing
    preprocessedCold <- compile PreparedStg mempty GeneralCompile Nothing preprocessedUser [] Nothing
    preprocessedColdSharing <- compilerProductSharing preprocessedCold
    assertIncomplete "external preprocessor, fresh" preprocessedCold
    writeFile preprocessorInput "changed outside the Haskell source graph"
    preprocessedWarm <- compile PreparedStg mempty GeneralCompile Nothing preprocessedUser [] Nothing
    preprocessedWarmSharing <- compilerProductSharing preprocessedWarm
    assertIncomplete "external preprocessor, repeated" preprocessedWarm
    assertSameInterfaceEvidenceAndPreparedShape "untracked external input with unchanged output" preprocessedCold preprocessedWarm
    assertCoreSharing "untracked preprocessor input prevents native owner reuse" False
      ["PreprocessedDependency"] preprocessedColdSharing preprocessedWarmSharing
    let quotedSource label owner user = do
          fresh <- compile PreparedStg mempty GeneralCompile Nothing user [] Nothing
          freshSharing <- compilerProductSharing fresh
          assertIncomplete (label ++ ", fresh") fresh
          repeated <- compile PreparedStg mempty GeneralCompile Nothing user [] Nothing
          repeatedSharing <- compilerProductSharing repeated
          assertIncomplete (label ++ ", repeated") repeated
          assertSameInterfaceEvidenceAndPreparedShape (label ++ " preserves output") fresh repeated
          assertCoreSharing (label ++ " recompiles canonical owner") False [owner] freshSharing repeatedSharing
          assertStgSharing (label ++ " prepares a fresh body") False [owner] freshSharing repeatedSharing
    quotedSource "local same-name pure quasiquote" "LabelDependency" labelUser
    quotedSource "local same-name pure quasiquote via open import" "OpenImportDependency" openImportUser
    quotedSource "local same-name pure quasiquote via reexport" "ReexportDependency" reexportUser
    localCold <- compile PreparedStg mempty GeneralCompile Nothing localQuoteUser [] Nothing
    localColdSharing <- compilerProductSharing localCold
    assertIncomplete "local quasiquote, fresh" localCold
    localWarm <- compile PreparedStg mempty GeneralCompile Nothing localQuoteUser [] Nothing
    localWarmSharing <- compilerProductSharing localWarm
    assertIncomplete "local quasiquote, repeated" localWarm
    assertSameInterfaceEvidenceAndPreparedShape "local quasiquote preserves native output" localCold localWarm
    assertCoreSharing "local quasiquoter prevents owner reuse" False
      ["LocalQuoteDependency"] localColdSharing localWarmSharing
    let dependent = root </> "quote-input.txt"
    writeFile dependent "tracked input"
    -- Use a fresh quoter identity: reusing an already linked TH provider
    -- would observe its old bytecode rather than this fixture's implementation.
    writeFile (qqDir </> "Validate.hs") $ unlines
      [ "module Tidepool.QQ.Validate (uri) where"
      , "import Language.Haskell.TH (litE, stringL)"
      , "import Language.Haskell.TH.Syntax (addDependentFile)"
      , "import Language.Haskell.TH.Quote (QuasiQuoter(..))"
      , "uri :: QuasiQuoter"
      , "uri = QuasiQuoter"
      , "  { quoteExp = \\source -> addDependentFile " ++ show dependent ++ " >> litE (stringL source)"
      , "  , quotePat = \\_ -> fail \"label is expression-only\""
      , "  , quoteType = \\_ -> fail \"label is expression-only\""
      , "  , quoteDec = \\_ -> fail \"label is expression-only\""
      , "  }"
      ]
    let dependentQuote = root </> "DependentQuote.hs"
        dependentUser = root </> "DependentUser.hs"
    writeFile dependentQuote $ unlines
      [ "{-# LANGUAGE QuasiQuotes #-}"
      , "module DependentQuote where"
      , "import Tidepool.QQ.Validate (uri)"
      , "value :: String"
      , "value = [uri|tracked|]"
      ]
    writeFile dependentUser $ unlines
      [ "module DependentUser where"
      , "import DependentQuote (value)"
      , "result :: String"
      , "result = value"
      ]
    originalDependentBytes <- BS.readFile dependent
    withDependentFile <- compile PreparedStg mempty GeneralCompile Nothing dependentUser [] Nothing
    assertIncomplete "local same-name provider with a dependent file" withDependentFile
    dependentSharing <- compilerProductSharing withDependentFile
    writeFile dependent "changed tracked input"
    changedDependentBytes <- BS.readFile dependent
    dependentWarm <- compile PreparedStg mempty GeneralCompile Nothing dependentUser [] Nothing
    dependentWarmSharing <- compilerProductSharing dependentWarm
    assertIncomplete "local same-name provider with a changed dependent file" dependentWarm
    assertSamePreparedAbiAndShape "dependent-file recheck preserves ABI and typed output"
      withDependentFile dependentWarm
    let quoteInterface prepared = maybe (fail "dependent-file quoter lost its finalized interface")
          (pure . hm_iface . finalizedHomeModInfo)
          (Map.lookup (mkModuleName "DependentQuote") (pprFinalizedModules prepared))
        dependentFingerprint interface = case
            [usg_file_hash usage | usage@UsageFile{} <- mi_usages interface
              , unpackFS (usg_file_path usage) == dependent] of
          [fingerprint] -> pure fingerprint
          _ -> fail "dependent-file quoter lost its unique actual file usage"
    originalInterface <- quoteInterface withDependentFile
    changedInterface <- quoteInterface dependentWarm
    originalFingerprint <- dependentFingerprint originalInterface
    changedFingerprint <- dependentFingerprint changedInterface
    assertEqual "original dependent-file usage seals the actual file bytes"
      (fingerprintByteString originalDependentBytes) originalFingerprint
    assertEqual "rechecked dependent-file usage seals the changed file bytes"
      (fingerprintByteString changedDependentBytes) changedFingerprint
    when (originalFingerprint == changedFingerprint
        || mi_iface_hash (mi_final_exts originalInterface) == mi_iface_hash (mi_final_exts changedInterface)) $
      fail "changed dependent-file input retained stale usage or full interface evidence"
    assertCoreSharing "addDependentFile prevents reuse" False
      ["DependentQuote"] dependentSharing dependentWarmSharing
    -- Observe the value only after the ordinary consumer's measured cache
    -- checks. This splice cannot make those checks conservative on its own.
    let valueOracle = root </> "DependentValueOracle.hs"
    writeFile valueOracle $ unlines
      [ "{-# LANGUAGE TemplateHaskell #-}"
      , "module DependentValueOracle where"
      , "import DependentQuote (value)"
      , "import Language.Haskell.TH (litE, stringL)"
      , "result :: String"
      , "result = $(if value == \"tracked\" then litE (stringL value)"
      , "  else fail \"dependent-file quoter changed its compiled value\")"
      ]
    void (compile PreparedStg mempty GeneralCompile Nothing valueOracle [] Nothing)
  where
    assertComplete label result = do
      let evidence = preparedFreshDependencies result
      unless (dependencyCacheSafe evidence && dependencySelectionComplete evidence) $
        fail (label ++ " did not produce complete dependency evidence")
    assertIncomplete label result = do
      let evidence = preparedFreshDependencies result
      when (dependencyCacheSafe evidence || dependencySelectionComplete evidence) $
        fail (label ++ " produced complete dependency evidence")
    temporary = do
      parent <- getTemporaryDirectory
      (path, handle) <- openTempFile parent "tidepool-untracked-compile-time"
      hClose handle
      removeFile path
      createDirectory path
      pure path

dependencyEvidenceCompilation :: IO ()
dependencyEvidenceCompilation = bracket temporary removeDirectoryRecursive $ \root -> do
  let home = root </> "WitnessA.hs"
      boot = root </> "WitnessA.hs-boot"
      sibling = root </> "WitnessB.hs"
      types = root </> "WitnessTypes.hs"
      target = root </> "WitnessTarget.hs"
  writeFile types "module WitnessTypes where\ndata T = T\n"
  writeFile boot $ unlines
    [ "module WitnessA where"
    , "import WitnessTypes (T)"
    , "value :: T"
    ]
  writeFile home $ unlines
    [ "module WitnessA where"
    , "import WitnessB (helper)"
    , "import WitnessTypes (T)"
    , "value :: T"
    , "value = helper"
    ]
  writeFile sibling $ unlines
    [ "module WitnessB where"
    , "import {-# SOURCE #-} WitnessA (value)"
    , "helper = value"
    ]
  writeFile target $ unlines
    [ "{-# LANGUAGE PackageImports #-}"
    , "module WitnessTarget where"
    , "import qualified Data.Text as Text"
    , "import qualified \"containers\" Data.Map as Map"
    , "import WitnessA (value)"
    , "result = (Text.length (Text.pack \"x\"), value, Map.size Map.empty)"
    ]
  prepared <- runPipelineSelected PreparedStg target [root]
  let evidence = preparedFreshDependencies prepared
      resolutions = dependencyResolutions evidence
      selectedPaths = [path | resolution <- resolutions
                            , Just path <- [dependencyResolutionSelected resolution]]
      packageWitnesses = [resolution | resolution <- resolutions
        , dependencyResolutionModule resolution == "Data.Text"]
      qualifiedWitnesses = [resolution | resolution <- resolutions
        , dependencyResolutionModule resolution == "Data.Map"]
  unless (preparedExactCompilation prepared == Nothing) $
    fail "ordinary source compile acquired exact-scope evidence"
  forM_ [("WitnessTarget",[("main","WitnessA")]),("WitnessB",[("main","WitnessA")])] $ \(owner,expected) -> do
    requirements <- either fail pure (preparedHomeRequirements prepared "main" owner)
    assertEqual "complete source requirements preserve ordinary and SOURCE edges and exclude packages"
      expected requirements
  unless (any (isSuffixOf "WitnessA.hs-boot") selectedPaths) $
    fail "SOURCE import did not retain its selected boot-interface witness"
  unless (any (isSuffixOf "WitnessA.hs") selectedPaths) $
    fail "ordinary home import did not retain its selected source witness"
  unless (case packageWitnesses of
      [resolution] -> dependencyResolutionSelected resolution == Nothing
        && any (isSuffixOf ("Data" </> "Text.hs"))
          (dependencyResolutionCandidates resolution)
      _ -> False) $
    fail "package import did not retain absent higher-priority home candidates"
  unless ("Data.Text" `elem` dependencyPackages evidence) $
    fail "package import was not recorded in dependency evidence"
  unless (case qualifiedWitnesses of
      [resolution] -> case dependencyResolutionQualifier resolution of
        DependencyOtherUnit _ -> dependencyResolutionSelected resolution == Nothing
          && null (dependencyResolutionCandidates resolution)
        _ -> False
      _ -> False) $
    fail "qualified package import was not distinguished from home lookup"
  previousTiming <- lookupEnv "TIDEPOOL_TIMING"
  setEnv "TIDEPOOL_TIMING" "1"
  (withResidentPipelineSelected [root] $ \compile -> do
      _ <- compile PreparedStg mempty GeneralCompile Nothing target [] Nothing
      appendFile boot "\n-- boot-only mutation\n"
      (_, changedLog) <- captureStderr root "boot-changed" $
        compile PreparedStg mempty GeneralCompile Nothing target [] Nothing
      assertContains "boot-only mutation invalidates its SOURCE importer"
        "tidepool-memo-miss module=WitnessB" changedLog
      assertContains "boot fingerprint participates in home dependency validity"
        "same-home-dependencies=False" changedLog
      appendFile types "\n-- transitive boot dependency mutation\n"
      (_, transitiveLog) <- captureStderr root "boot-dependency-changed" $
        compile PreparedStg mempty GeneralCompile Nothing target [] Nothing
      assertContains "dependency imported by boot interface invalidates SOURCE importer"
        "tidepool-memo-miss module=WitnessB" transitiveLog
      assertContains "transitive boot dependency fingerprint participates in validity"
        "same-home-dependencies=False" transitiveLog)
    `finally` maybe (unsetEnv "TIDEPOOL_TIMING") (setEnv "TIDEPOOL_TIMING") previousTiming
  where
    temporary = do
      parent <- getTemporaryDirectory
      (path, handle) <- openTempFile parent "tidepool-dependency-evidence"
      hClose handle
      removeFile path
      createDirectory path
      pure path

-- | Two checkouts resolving byte-identical dependency modules at different
-- absolute paths (a worktree-per-actor checkout against the same workspace)
-- must still hit the memo: the dependency's content is unchanged, only the
-- selected path differs, and a path string cannot change compiled Core.
-- 'Dep'/'Target' live under two sibling roots with identical content;
-- 'Importer' (fixed location, never itself duplicated) imports 'Target'
-- indirectly so 'Target' is never the compile's own evicted entry — only an
-- indirect dependency, matching the parent/child worktree shape.
pathInsensitiveWitnessCompilation :: IO ()
pathInsensitiveWitnessCompilation = bracket temporary removeDirectoryRecursive $ \root -> do
  let workDir = root </> "work"
      rootA = root </> "rootA"
      rootB = root </> "rootB"
      sharedRoot = root </> "shared"
      importer = workDir </> "Importer.hs"
      depContent =
        [ "module Dep where"
        , "value :: Int"
        , "value = 1"
        ]
      targetContent =
        [ "module Target where"
        , "import Dep (value)"
        , "import Shared (shared)"
        , "result :: Int"
        , "result = value + shared"
        ]
  createDirectoryIfMissing True workDir
  createDirectoryIfMissing True rootA
  createDirectoryIfMissing True rootB
  createDirectoryIfMissing True sharedRoot
  writeFile (sharedRoot </> "Shared.hs") "module Shared where\nshared = (1 :: Int)\n"
  writeFile (rootA </> "Dep.hs") (unlines depContent)
  writeFile (rootB </> "Dep.hs") (unlines depContent)
  writeFile (rootA </> "Target.hs") (unlines targetContent)
  writeFile (rootB </> "Target.hs") (unlines targetContent)
  writeFile importer $ unlines
    [ "module Importer where"
    , "import Target (result)"
    , "total :: Int"
    , "total = result + 1"
    ]
  withResidentPipelineSelectedRequests [workDir] $ \runRequest ->
    runRequest (pure ()) $ \compile -> do
      cold <- compile PreparedStg mempty GeneralCompile Nothing importer [rootA, sharedRoot] Nothing
      coldSharing <- compilerProductSharing cold
      warm <- compile PreparedStg mempty GeneralCompile Nothing importer [rootB, sharedRoot] Nothing
      warmSharing <- compilerProductSharing warm
      let evidence = preparedFreshDependencies warm
          obsoletePaths = [path | resolution <- dependencyResolutions evidence
            , path <- dependencyResolutionCandidates resolution, rootA `isPrefixOf` path]
          selectedPaths = [path | resolution <- dependencyResolutions evidence
            , Just path <- [dependencyResolutionSelected resolution]]
      unless (null obsoletePaths) $
        fail ("previous request roots leaked through shared module summary: " ++ show obsoletePaths)
      unless (all (`elem` selectedPaths) [rootB </> "Dep.hs", rootB </> "Target.hs"]) $
        fail "changed import roots did not retain their current source selections"
      unless (dependencyCacheSafe evidence && dependencySelectionComplete evidence) $
        fail "changed import roots lost complete dependency evidence"
      assertSameInterfaceEvidenceAndPreparedShape "path-insensitive source selection preserves output" cold warm
      assertCoreSharing "byte-identical dependencies reuse finalized owners across roots" True
        ["Dep", "Target", "Shared"] coldSharing warmSharing
      assertStgSharing "byte-identical dependencies reuse prepared bodies across roots" True
        ["Dep", "Target", "Shared"] coldSharing warmSharing
      writeFile (rootB </> "Dep.hs") "module Dep where\nvalue :: Int\nvalue = missingValue\n"
      invalid <- try (compile PreparedStg mempty GeneralCompile Nothing importer [rootB, sharedRoot] Nothing)
        :: IO (Either DependencyLoadFailure PreparedPipelineResult)
      case invalid of
        Left (DependencySourceFailure diagnostics) ->
          unless (any (\diagnostic -> case dFile diagnostic of
              Just (path, _, _, _, _) -> path == rootB </> "Dep.hs"
              Nothing -> False) diagnostics) $
            fail "changed-root dependency error did not identify the current source"
        Left DependencyWorkerFailure -> fail "changed-root dependency error became a worker failure"
        Right _ -> fail "path-insensitive reuse ignored changed source bytes"
  where
    temporary = do
      parent <- getTemporaryDirectory
      (path, handle) <- openTempFile parent "tidepool-path-insensitive-witness"
      hClose handle
      removeFile path
      createDirectory path
      pure path


validationMemoCompilation :: IO ()
validationMemoCompilation = bracket temporary removeDirectoryRecursive $ \root -> do
  let base = root </> "WarmBase.hs"
      reexport = root </> "WarmReexport.hs"
      child = root </> "WarmChild.hs"
      target = root </> "WarmTarget.hs"
      producer = root </> "MemoProducer.hs"
      facade = root </> "MemoFacade.hs"
      consumer = root </> "MemoConsumer.hs"
      memoTarget = root </> "MemoTarget.hs"
      chainLength = 16 :: Int
      chainName index = "WarmChain" ++ show index
  writeFile base "module WarmBase (value) where\nvalue :: Int\nvalue = 42\n"
  writeFile reexport "module WarmReexport (value) where\nimport WarmBase (value)\n"
  writeFile child "module WarmChild (value) where\nimport WarmReexport (value)\n"
  forM_ [1 .. chainLength] $ \index -> do
    let previous = if index == 1 then "WarmChild" else chainName (index - 1)
    writeFile (root </> chainName index ++ ".hs") $ unlines
      [ "module " ++ chainName index ++ " (value) where"
      , "import " ++ previous ++ " (value)"
      ]
  writeFile target $ unlines
    [ "module WarmTarget where"
    , "import " ++ chainName chainLength ++ " (value)"
    , "result = value"
    ]
  writeFile producer "module MemoProducer (value) where\nvalue :: Int\nvalue = 42\n"
  writeFile facade "module MemoFacade (value) where\nimport MemoProducer (value)\n"
  writeFile consumer "module MemoConsumer (result) where\nimport MemoFacade (value)\nresult = value + 1\n"
  writeFile memoTarget "module MemoTarget where\nimport MemoConsumer (result)\nfinal = result\n"
  withResidentPipelineSelected [root] $ \compile -> do
    cold <- compile PreparedStg mempty GeneralCompile Nothing target [] Nothing
    coldSharing <- compilerProductSharing cold
    warm <- compile PreparedStg mempty GeneralCompile Nothing target [] Nothing
    warmSharing <- compilerProductSharing warm
    let allOwners = sort (["WarmBase", "WarmReexport", "WarmChild", "WarmTarget"]
          ++ map chainName [1 .. chainLength])
        validationOwners = ["WarmReexport", "WarmChild"] ++ map chainName [1 .. chainLength]
    assertEqual "chain keeps every canonical interface owner" allOwners
      (sort (map moduleNameString (Map.keys (pprFinalizedModules cold))))
    assertEqual "reexport chain selects only executable owners" ["WarmBase", "WarmTarget"]
      (preparedOwnerNames cold)
    assertSameInterfaceEvidenceAndPreparedShape "validation-only chain preserves prepared target" cold warm
    assertCoreSharing "transaction retains canonical validation-only facts" True validationOwners coldSharing warmSharing
    assertStgSharing "transaction retains reachable dependency body" True ["WarmBase"] coldSharing warmSharing
    assertStgSharing "request target body is prepared afresh" False ["WarmTarget"] coldSharing warmSharing
    executableCold <- compile PreparedStg mempty GeneralCompile Nothing memoTarget [] Nothing
    executableColdSharing <- compilerProductSharing executableCold
    assertEqual "reexport consumer selects its defining executable owner"
      ["MemoConsumer", "MemoProducer", "MemoTarget"] (preparedOwnerNames executableCold)
    executableWarm <- compile PreparedStg mempty GeneralCompile Nothing memoTarget [] Nothing
    executableWarmSharing <- compilerProductSharing executableWarm
    assertSameInterfaceEvidenceAndPreparedShape "complete cached product preserves native output" executableCold executableWarm
    assertCoreSharing "transaction retains executable canonical owners" True
      ["MemoProducer", "MemoFacade", "MemoConsumer"] executableColdSharing executableWarmSharing
    assertStgSharing "transaction retains executable prepared bodies" True
      ["MemoProducer", "MemoConsumer"] executableColdSharing executableWarmSharing
    previousDrop <- lookupEnv "TIDEPOOL_TEST_DROP_MEMO_INTERFACE"
    regenerated <- (do
        setEnv "TIDEPOOL_TEST_DROP_MEMO_INTERFACE" "MemoProducer"
        compile PreparedStg mempty GeneralCompile Nothing memoTarget [] Nothing)
      `finally` maybe (unsetEnv "TIDEPOOL_TEST_DROP_MEMO_INTERFACE")
                      (setEnv "TIDEPOOL_TEST_DROP_MEMO_INTERFACE") previousDrop
    regeneratedSharing <- compilerProductSharing regenerated
    assertSameInterfaceEvidenceAndPreparedShape "interface recovery restores paired prepared output" executableCold regenerated
    assertCoreSharing "missing retained interface regenerates its canonical owner" False
      ["MemoProducer"] executableWarmSharing regeneratedSharing
    assertStgSharing "producer interface loss regenerates producer and dependent bodies" False
      ["MemoProducer", "MemoConsumer"] executableWarmSharing regeneratedSharing
  where
    temporary = do
      parent <- getTemporaryDirectory
      (path, handle) <- openTempFile parent "tidepool-validation-memo"
      hClose handle
      removeFile path
      createDirectory path
      pure path

-- CHECK targets keep typed source failures while dependencies retain their
-- own failure boundary, including when either participates in a boot cycle.
checkedLoadBoundaryCompilation :: IO ()
checkedLoadBoundaryCompilation = bracket temporary removeDirectoryRecursive $ \root -> do
  let target = root </> "CheckedLoadTarget.hs"
      dependency = root </> "CheckedLoadDependency.hs"
      cycleA = root </> "CheckedLoadCycleA.hs"
      cycleB = root </> "CheckedLoadCycleB.hs"
      compile path = runPipelineSelected CheckedEnvironment path [root]
      expectTargetFailure label path = do
        result <- try (compile path) :: IO (Either SomeException CheckedEnvironmentResult)
        case result of
          Left failure | Just (_ :: SourceError) <- fromException failure -> pure ()
          Left failure -> fail (label ++ " lost its typed target error: " ++ show failure)
          Right _ -> fail (label ++ " accepted an authored duplicate instance")
  writeFile target $ unlines
    [ "module CheckedLoadTarget where"
    , "data Custom = Custom"
    , "instance Show Custom where show _ = \"first\""
    , "instance Show Custom where show _ = \"second\""
    ]
  expectTargetFailure "leaf CHECK" target
  writeFile dependency "module CheckedLoadDependency where\nvalue = missingDependencyName\n"
  writeFile target "module CheckedLoadTarget where\nimport CheckedLoadDependency\nvalue' = value\n"
  result <- try (compile target) :: IO (Either SomeException CheckedEnvironmentResult)
  case result of
    Left failure | Just (DependencySourceFailure _) <- fromException failure -> pure ()
    Left failure -> fail ("dependency error changed ownership: " ++ show failure)
    Right _ -> fail "CHECK accepted an invalid dependency"
  writeFile (root </> "CheckedLoadCycleA.hs-boot")
    "module CheckedLoadCycleA where\nvalue :: Int\n"
  writeFile cycleB $ unlines
    [ "module CheckedLoadCycleB where"
    , "import {-# SOURCE #-} CheckedLoadCycleA (value)"
    , "helper :: Int"
    , "helper = value"
    ]
  let validCycle = unlines
        [ "module CheckedLoadCycleA where"
        , "import CheckedLoadCycleB (helper)"
        , "value :: Int"
        , "value = helper"
        ]
  writeFile cycleA (validCycle ++ unlines
    [ "data Custom = Custom"
    , "instance Show Custom where show _ = \"first\""
    , "instance Show Custom where show _ = \"second\""
    ])
  expectTargetFailure "boot-cycle CHECK" cycleA
  writeFile cycleA validCycle
  writeFile target "module CheckedLoadTarget where\nimport CheckedLoadCycleA (value)\nresult = value\n"
  _ <- compile target
  pure ()
  where
    temporary = do
      parent <- getTemporaryDirectory
      (path, handle) <- openTempFile parent "tidepool-checked-load-boundary"
      hClose handle
      removeFile path
      createDirectory path
      pure path

-- Only type-of queries consume numbered probes; other results retain query order.
mixedInspectionCompilation :: IO ()
mixedInspectionCompilation = bracket temporary removeDirectoryRecursive $ \root -> do
  let target = root </> "MixedInspection.hs"
  writeFile target $ unlines
    [ "module MixedInspection where"
    , "__tidepool_inspect_0 = (7 :: Int)"
    , "__tidepool_inspect_1 = True"
    ]
  withResidentPipelineSelectedRequests [root] $ \runRequest ->
    runRequest (pure ()) $ \compiler -> do
      checked <- compiler CheckedEnvironment mempty GeneralCompile Nothing target [root] Nothing
      inspected <- runInspection
        (crHscEnv checked) (crTargetTcGblEnv checked)
        (crTargetRdrEnv checked) (crInspectionProbes checked)
        [InspectTypeOf "7", InspectNameInfo "missingInspectionName", InspectTypeOf "True"]
      case inspected of
        [InspectionType "7" first _, InspectionNotFound "missingInspectionName", InspectionType "True" second _] -> do
          assertContains "first query uses probe zero" "Int" first
          assertContains "non-type query leaves the second probe ordinal unchanged" "Bool" second
        other -> fail ("mixed inspection returned an unexpected result: " ++ show other)
  where
    temporary = do
      parent <- getTemporaryDirectory
      (path, handle) <- openTempFile parent "tidepool-mixed-inspection"
      hClose handle
      removeFile path
      createDirectory path
      pure path

-- Metadata compilation must not enter the target's executable pipeline.
-- A changed dependency must still be checked on the following request.
metadataCompilation :: IO ()
metadataCompilation = bracket temporary removeDirectoryRecursive $ \root -> do
  let dependency = root </> "MetadataDependency.hs"
      target = root </> "MetadataTarget.hs"
  writeFile dependency $ unlines
    [ "module MetadataDependency where"
    , "data Box a = Box a"
    , "value :: Box Int"
    , "value = Box 7"
    ]
  writeFile target $ unlines
    [ "module MetadataTarget where"
    , "import MetadataDependency"
    , "__tidepool_inspect_0 = value"
    ]
  recoveryClears <- newIORef (0 :: Int)
  withResidentPipelineSelectedRequests [root] $ \runRequest -> do
    runRequest (modifyIORef' recoveryClears (+ 1)) $ \compiler -> do
      checked <- compiler CheckedEnvironment mempty GeneralCompile Nothing target [root] Nothing
      assertEqual "checked target keeps its original owner" (mkModuleName "MetadataTarget")
        (moduleName (tcg_mod (crTargetTcGblEnv checked)))
      unless (Map.member "__tidepool_inspect_0" (crInspectionProbes checked)) $
        fail "checked target lost its typed inspection probe"
      unless (case lookupHpt (hsc_HPT (crHscEnv checked)) (mkModuleName "MetadataTarget") of
          Nothing -> True
          Just _ -> False) $
        fail "metadata leaf constructed an unused target interface"
      inspected <- runInspection
        (crHscEnv checked)
        (crTargetTcGblEnv checked)
        (crTargetRdrEnv checked)
        (crInspectionProbes checked)
        [InspectTypeOf "value", InspectModule "MetadataTarget" False,
          InspectModule "MetadataDependency" False, InspectModule "MissingMetadataModule" False]
      case inspected of
        [InspectionType "value" rendered _, InspectionBrowse "MetadataTarget" False entries,
            InspectionBrowse "MetadataDependency" False dependencies, InspectionModuleNotFound "MissingMetadataModule"] -> do
          assertContains "inspection resolves the checked local probe" "Box Int" rendered
          assertEqual "checked target browsing uses its own exports"
            ["__tidepool_inspect_0"] (map infoName entries)
          unless (any ((== "value") . infoName) dependencies) $
            fail "metadata browsing lost its imported module interface"
        _ -> fail ("metadata inspection returned an unexpected result: " ++ show inspected)
      writeFile target $ unlines
        [ "module MetadataTarget (visible) where"
        , "import MetadataDependency"
        , "visible = value"
        , "hidden = value"
        ]
      restricted <- compiler CheckedEnvironment mempty GeneralCompile Nothing target [root] Nothing
      exported <- runInspection (crHscEnv restricted) (crTargetTcGblEnv restricted)
        (crTargetRdrEnv restricted) (crInspectionProbes restricted) [InspectModule "MetadataTarget" True]
      case exported of
        [InspectionBrowse "MetadataTarget" True entries] ->
          assertEqual "checked target explicit exports omit private and imported names"
            ["visible"] (map infoName entries)
        _ -> fail ("explicit export browse returned an unexpected result: " ++ show exported)
    writeFile dependency "module MetadataDependency where\nvalue = missingDependencyName\n"
    rejected <- try (runRequest (modifyIORef' recoveryClears (+ 1)) $ \compiler -> do
      _ <- compiler CheckedEnvironment mempty GeneralCompile Nothing target [root] Nothing
      pure ()) :: IO (Either DependencyLoadFailure ())
    case rejected of
      Left (DependencySourceFailure diagnostics) ->
        unless (any ((== DiagError) . dSeverity) diagnostics) $
          fail "metadata dependency rejection lost its source diagnostic"
      Left DependencyWorkerFailure -> fail "metadata dependency rejection became a worker failure"
      Right _ -> fail "metadata reused an invalid dependency"
    cleared <- readIORef recoveryClears
    assertEqual "successful close, failed attempt and rejected close clear recovery graphs" 3 cleared
  where
    temporary = do
      parent <- getTemporaryDirectory
      (path, handle) <- openTempFile parent "tidepool-metadata"
      hClose handle
      removeFile path
      createDirectory path
      pure path

-- A source-less value interface activates the session pipeline. Canonical
-- finalization supplies every original interface/Core pair; the session tier
-- additionally prepares bodies that ordinary reachability left unselected.
preparedSessionLeafCompilation :: IO ()
preparedSessionLeafCompilation = bracket temporary removeDirectoryRecursive $ \root -> do
  let scopeRoot = root </> "session"
      valueModule = SessionModule ValMod (Generation 1)
      seed = root </> "SessionSeed.hs"
      target = root </> "PreparedSessionLeaf.hs"
      unused = root </> "SessionUnused.hs"
      consumer = root </> "SessionConsumer.hs"
      unreachable = root </> "SessionUnreachable.hs"
      ordinary = root </> "OrdinaryFirst.hs"
      scope = SessionScope scopeRoot [valueModule] Nothing Nothing
  writeFile seed "module SessionSeed where\nseed = 1 :: Int\n"
  writeFile unused "module SessionUnused (unused) where\nunused :: Int\nunused = 5\n"
  writeFile consumer "module SessionConsumer (used) where\nimport SessionUnused (unused)\nused = unused + 1\n"
  writeFile unreachable "module SessionUnreachable (other) where\nother = 10 :: Int\n"
  writeFile ordinary "module OrdinaryFirst where\nimport SessionConsumer (used)\nimport SessionUnreachable ()\nresult = used\n"
  seeded <- runPipelineSelected PreparedStg seed [root]
  let environment = prHscEnv (pprPipelineResult seeded)
  iface <- mkThinSessionIface environment valueModule [(mkVarOcc "prior", intTy)]
  writeSessionIface environment scopeRoot valueModule iface
  writeFile target $ unlines
    [ "module PreparedSessionLeaf where"
    , "import Tidepool.Session.Val.G1 (prior)"
    , "import SessionConsumer (used)"
    , "import SessionUnreachable ()"
    , "__result = prior + used"
    ]
  withResidentPipelineSelected [root] $ \compiler -> do
    ordinaryPrepared <- compiler PreparedStg mempty GeneralCompile Nothing ordinary [root] Nothing
    ordinarySharing <- compilerProductSharing ordinaryPrepared
    unless ("SessionUnreachable" `notElem` preparedOwnerNames ordinaryPrepared
        && Map.member (mkModuleName "SessionUnreachable") (pprFinalizedModules ordinaryPrepared)) $
      fail "ordinary request lost validation-only ownership or prepared its unreachable import"
    prepared <- compiler PreparedStg mempty GeneralCompile (Just scope) target [root] Nothing
    preparedSharing <- compilerProductSharing prepared
    assertEqual "session tier prepares every fresh source owner"
      ["PreparedSessionLeaf", "SessionConsumer", "SessionUnreachable", "SessionUnused"]
      (preparedOwnerNames prepared)
    unless (Map.member (mkModuleName "PreparedSessionLeaf") (pprFinalizedModules prepared)) $
      fail "session leaf lacks its canonical interface/Core owner"
    assertCoreSharing "session tier retains preceding canonical owners" True
      ["SessionUnused", "SessionConsumer", "SessionUnreachable"] ordinarySharing preparedSharing
    assertStgSharing "session tier retains already prepared dependency bodies" True
      ["SessionUnused", "SessionConsumer"] ordinarySharing preparedSharing
    case prResultType (pprPipelineResult prepared) of
      Just inferred -> unless (eqType inferred intTy) (fail "session leaf changed its native result type")
      Nothing -> fail "session leaf lost its native result type"
    previousDrop <- lookupEnv "TIDEPOOL_TEST_DROP_MEMO_INTERFACE"
    regenerated <- (do
        setEnv "TIDEPOOL_TEST_DROP_MEMO_INTERFACE" "SessionUnused"
        compiler PreparedStg mempty GeneralCompile (Just scope) target [root] Nothing)
      `finally` maybe (unsetEnv "TIDEPOOL_TEST_DROP_MEMO_INTERFACE")
                      (setEnv "TIDEPOOL_TEST_DROP_MEMO_INTERFACE") previousDrop
    regeneratedSharing <- compilerProductSharing regenerated
    assertSameInterfaceEvidenceAndPreparedShape "session interface recovery restores the canonical prepared product" prepared regenerated
    assertCoreSharing "session tier rebuilds owner with missing retained interface" False
      ["SessionUnused"] preparedSharing regeneratedSharing
    assertStgSharing "session tier rebuilds affected producer and consumer bodies" False
      ["SessionUnused", "SessionConsumer"] preparedSharing regeneratedSharing
  where
    temporary = do
      parent <- getTemporaryDirectory
      (path, handle) <- openTempFile parent "tidepool-prepared-session-leaf"
      hClose handle
      removeFile path
      createDirectory path
      pure path

-- The first case is the retained public notebook input whose consumer
-- derives demand Generic before GHC reports duplicate declarations. The
-- variants distinguish actual class identities from their source spelling.
explicitGenericDerivationRecovery :: IO ()
explicitGenericDerivationRecovery = genericDerivationRecovery
  "Notebook.cell.hs" ["ScopePing", "ScopeTools"] []

qualifiedAndStandaloneGenericRecovery :: IO ()
qualifiedAndStandaloneGenericRecovery = genericDerivationRecovery
  "ResolvedVariants.cell.hs" ["Qualified", "Standalone", "Reexported"]
  ["Automatic", "ForeignIdentity"]

genericDerivationRecovery :: FilePath -> [String] -> [String] -> IO ()
genericDerivationRecovery fixture explicit automatic =
  withGenericCompiler fixture $ \compile plan -> do
    -- The exact authored derives are valid without their generated duplicates.
    -- Retain the automatic-only companions while checking this prerequisite.
    (_, baseline) <- checkCellInstances compile (omitCellGenericDeclarations explicit plan)
    assertGenericInstances (explicit ++ automatic) baseline
    assertEqual "parser retains candidates until GHC resolves their class" True
      (all (`elem` map genericDeclarationTarget (cellPlanGenericDeclarations plan)) (explicit ++ automatic))
    (accepted, result) <- checkCellInstances compile plan
    assertEqual "authored Generic keeps its unique native instance" False
      (any (`elem` map genericDeclarationTarget (cellPlanGenericDeclarations accepted)) explicit)
    assertEqual "missing Generic and unrelated same-spelling classes retain companions" True
      (all (`elem` map genericDeclarationTarget (cellPlanGenericDeclarations accepted)) automatic)
    assertGenericInstances (explicit ++ automatic) result
    -- The accepted source also succeeds without the retry loop, so its
    -- validity cannot depend on swallowing a failed compiler operation.
    ordinary <- compile accepted
    assertGenericInstances (explicit ++ automatic) ordinary

authoredGenericConflictsRemainErrors :: IO ()
authoredGenericConflictsRemainErrors =
  withGenericCompiler "AuthoredConflict.cell.hs" $ \compile plan -> do
    let requireRefusal :: Either SomeException result -> IO ()
        requireRefusal outcome = case outcome of
          Right _ -> fail "conflicting authored Generic instances were accepted"
          Left failure -> case fromException failure of
            Just sourceError -> requireSourceDiagnostics (diagsFromSourceError sourceError)
            Nothing -> case fromException failure of
              Just (DependencySourceFailure diagnostics) -> requireSourceDiagnostics diagnostics
              _ -> fail ("authored Generic conflict lost its source failure: " ++ show failure)
        requireSourceDiagnostics diagnostics = unless
          (any ((== DiagError) . dSeverity) diagnostics)
          (fail "authored Generic refusal has no native error diagnostic")
    -- The same target and class compile with one real authored instance
    -- before either conflict refusal is allowed to satisfy this control.
    source <- readFile "test-cell-splitter/fixtures/explicit-generic/AuthoredConflictBaseline.cell.hs"
    baselinePlan <- genericDeclarationPlan source
    (_, baseline) <- checkCellInstances compile (omitCellGenericDeclarations ["Conflicting"] baselinePlan)
    assertGenericInstances ["Conflicting"] baseline
    -- Removing the generated candidate leaves the real authored conflict.
    -- Recovery must preserve that refusal rather than authorizing either one.
    authored <- try (compile (omitCellGenericDeclarations ["Conflicting"] plan))
      :: IO (Either SomeException CheckedEnvironmentResult)
    requireRefusal authored
    recovered <- try (checkCellInstances compile plan)
      :: IO (Either SomeException (CellSourcePlan, CheckedEnvironmentResult))
    requireRefusal recovered

withGenericCompiler
  :: FilePath
  -> ((CellSourcePlan -> IO CheckedEnvironmentResult) -> CellSourcePlan -> IO result)
  -> IO result
withGenericCompiler fixture use = bracket structuralDisplayDirectory removeDirectoryRecursive $ \root -> do
  effects <- requiredInput "TIDEPOOL_TEST_EFFECTS_DIR"
  prelude <- requiredInput "TIDEPOOL_PRELUDE_DIR"
  source <- readFile ("test-cell-splitter/fixtures/explicit-generic" </> fixture)
  plan <- genericDeclarationPlan source
  let includes = [root, "test-cell-splitter", effects, prelude]
      path = root </> "GenericDerivation.hs"
  withResidentPipelineSelectedRequests includes $ \runRequest ->
    runRequest (pure ()) $ \compiler -> do
      let compile current = do
            rendered <- either fail pure (renderCellCheckSource genericDerivationTemplate current)
            writeFile path rendered
            compiler CheckedEnvironment mempty
              (GeneratedInstanceCheck (cellGeneratedInstanceRecipe current) OriginalDeclarationCompile)
              Nothing path includes Nothing
      use compile plan

-- Match Main's declaration preparation boundary. The import prologue and
-- executable runs have their own segments; companion refinement consumes
-- one authored declaration run rather than a reconstructed whole-cell plan.
genericDeclarationPlan :: String -> IO CellSourcePlan
genericDeclarationPlan source = do
  whole <- analyzeOrderedCell genericDerivationTemplate source >>= either (fail . renderCellSplitError) pure
  case [segment | segment <- cellInferenceSegments whole
    , not (null (cellPlanGenericDeclarations segment))] of
    [segment] -> pure segment
    _ -> fail "Generic fixture did not select one original declaration segment"

assertGenericInstances :: [String] -> CheckedEnvironmentResult -> IO ()
assertGenericInstances targets result = do
  let environment = crTargetTcGblEnv result
      owned = [occNameString (nameOccName (tyConName constructor))
        | instance' <- tcg_insts environment
        , nameUnique (className (is_cls instance')) == genClassKey
        , ty <- is_tys instance'
        , Just (constructor, _) <- [splitTyConApp_maybe ty]
        , nameModule_maybe (tyConName constructor) == Just (tcg_mod environment)]
  unless (not (null targets)) (fail "Generic native control selected no targets")
  forM_ targets $ \target -> assertEqual ("unique native Generic instance for " ++ target)
    1 (length (filter (== target) owned))

genericDerivationTemplate :: String
genericDerivationTemplate = unlines
  [ "{-# LANGUAGE DeriveGeneric, StandaloneDeriving, FlexibleInstances, FlexibleContexts, UndecidableInstances #-}"
  , "{{CELL_PRAGMAS}}"
  , "module GenericDerivation where"
  , "{{CELL_IMPORTS}}"
  , "{{CELL_DECLS}}"
  , "__tidepoolCellExpression :: value -> Maybe ()"
  , "__tidepoolCellExpression _ = pure ()"
  , "__tidepool_cell_check = do { {{CELL_BODY}} } :: Maybe ()"
  ]

data DisplayTestScope = OrdinaryDisplayTest | LegacyDisplayScopeTest

structuralDisplayCompilation :: DisplayTestScope -> FilePath -> IO ()
structuralDisplayCompilation LegacyDisplayScopeTest _ = bracket structuralDisplayDirectory removeDirectoryRecursive $ \root -> do
  let path = root </> "exact-scope.cbor"
      bytes = toStrictByteString (encodeListLen 7
        <> encodeString "TPEXACTSCOPE" <> encodeString "2"
        <> foldMap encodeString (replicate 2 (Text.replicate 64 "0"))
        <> encodeListLen 0 <> encodeListLen 0 <> encodeListLen 0)
  BS.writeFile path bytes
  result <- readExactScope path
  case result of
    Left _ -> pure ()
    Right _ -> fail "legacy scope authorized structural companion compilation"
  unchanged <- BS.readFile path
  unless (unchanged == bytes) (fail "legacy scope refusal changed its producer bytes")
structuralDisplayCompilation OrdinaryDisplayTest effectsRoot = bracket structuralDisplayDirectory removeDirectoryRecursive $ \root -> do
  source <- readFile "test-cell-splitter/DisplayFields.cell.hs"
  plan <- analyzeCell template source >>= either (fail . renderCellSplitError) pure
  let includes = ["lib", "test-cell-splitter", effectsRoot]
  withResidentPipelineSelectedRequests includes $ \runRequest -> runRequest (pure ()) $ \compiler -> do
    let compile current = do
          rendered <- either fail pure (renderCellCheckSource template current)
          let path = root </> "CellCheck.hs"
          writeFile path rendered
          compiler CheckedEnvironment mempty
            (GeneratedInstanceCheck (cellGeneratedInstanceRecipe current) GeneralCompile)
            Nothing path includes Nothing
    (accepted, _) <- checkCellInstances compile plan
    assertEqual "resolved authored Display instances retained" False
      (any (`elem` map structuralDisplayTargetName (cellPlanStructuralDisplayTargets accepted)) ["Custom", "Reexported"])
    assertEqual "resolved authored Generic instances retained" False
      (any (`elem` map genericDeclarationTarget (cellPlanGenericDeclarations accepted)) ["Authored", "Standalone", "Reexported"])
    assertEqual "unrelated qualified classes do not suppress generated instances" True
      ("ForeignClass" `elem` map structuralDisplayTargetName (cellPlanStructuralDisplayTargets accepted)
        && "ForeignClass" `elem` map genericDeclarationTarget (cellPlanGenericDeclarations accepted))
    assertEqual "specialized custom instance preserves general structure" True
      ("Special" `elem` map structuralDisplayTargetName (cellPlanStructuralDisplayTargets accepted))
    assertEqual "authored Show remains an explicit text rendering choice" True
      ("Presented" `elem` map structuralDisplayTargetName (cellPlanStructuralDisplayTargets accepted))
    assertEqual "authored Show keeps the automatic Generic" True
      ("Presented" `elem` map genericDeclarationTarget (cellPlanGenericDeclarations accepted))
    assertEqual "unsupported automatic Generic derivations omitted" False
      (any ((`elem` ["Poly", "HiddenPoly", "Unboxed"]) . genericDeclarationTarget) (cellPlanGenericDeclarations accepted))
    assertEqual "unsupported Generic has no structural companion" False
      (any ((`elem` ["Poly", "HiddenPoly", "Unboxed"]) . structuralDisplayTargetName) (cellPlanStructuralDisplayTargets accepted))
    assertEqual "authored Generic retains its structural companion" True
      (all (`elem` map structuralDisplayTargetName (cellPlanStructuralDisplayTargets accepted)) ["Authored", "Standalone"])
    let generated = cellPlanStructuralDisplayDeclarations accepted
    assertContains "structural companion delegates once to Generic" ".genericDisplayTree" generated
    assertContains "parameterized representation context" ".Rep (Parameter a)" generated
    assertContains "symbolic datatype instance head" ".Display ((:+:) a b)" generated
    assertContains "recursive datatype companion" ".Display (Fields a)" generated
    assertContains "higher-kind field companion" ".Display (Higher f a)" generated
    assertContains "family field companion" ".Display (FamilyField a)" generated
    unless (not ("Text.pack" `isInfixOf` generated)) $
      fail "structural companions must not generate eager text conversion"
    _ <- compile accepted
    invalidSource <- readFile "test-cell-splitter/ExplicitInvalidGeneric.cell.hs"
    invalidPlan <- analyzeCell template invalidSource >>= either (fail . renderCellSplitError) pure
    invalid <- try (checkCellInstances compile invalidPlan)
      :: IO (Either SourceError (CellSourcePlan, CheckedEnvironmentResult))
    case invalid of
      Left _ -> pure ()
      Right _ -> fail "explicit invalid Generic instance must remain a user error"
  where
    template = unlines
      [ "{-# LANGUAGE OverloadedStrings, DeriveGeneric, StandaloneDeriving, FlexibleInstances, FlexibleContexts, UndecidableInstances #-}"
      , "{{CELL_PRAGMAS}}"
      , "module CellCheck where"
      , "{{CELL_IMPORTS}}"
      , "{{CELL_DECLS}}"
      , "__tidepool_cell_check = do { {{CELL_BODY}} } :: Maybe ()"
      ]

structuralDisplayDirectory :: IO FilePath
structuralDisplayDirectory = do
  parent <- getTemporaryDirectory
  (path, handle) <- openTempFile parent "tidepool-cell-display"
  hClose handle
  removeFile path
  createDirectory path
  pure path

-- Escaping callbacks have no authority after their owning transaction closes.
-- Rejection leaves the active or next transaction able to compile real source.
compilerLifecycleCompilation :: IO ()
compilerLifecycleCompilation = bracket temporary removeDirectoryRecursive $ \root -> do
  let target = root </> "CompilerLifecycle.hs"
      valid = "module CompilerLifecycle where\nanswer :: Int\nanswer = 42\n"
  writeFile target valid
  (releasedCompile, closedRunner) <- withResidentPipelineSelectedRequests [root] $ \runRequest -> do
    released <- runRequest (pure ()) $ \compiler -> do
      let compile = void (compiler CheckedEnvironment mempty GeneralCompile Nothing target [root] Nothing)
      assertRejected "nested transaction" (runRequest (pure ()) (\_ -> pure ()))
      compile
      assertRejected "compiler called on another thread" (onAnotherThread compile)
      compile
      pure compile
    assertRejected "compiler called after request release" released
    assertRejected "runner called on another thread" $
      onAnotherThread (runRequest (pure ()) (\_ -> pure ()))
    runRequest (pure ()) $ \compiler ->
      void (compiler CheckedEnvironment mempty GeneralCompile Nothing target [root] Nothing)
    clears <- newIORef (0 :: Int)
    runRequest (modifyIORef' clears (+ 1)) $ \compiler -> do
      let compile = void (compiler CheckedEnvironment mempty GeneralCompile Nothing target [root] Nothing)
      writeFile target "module CompilerLifecycle where\nanswer :: Int\nanswer = missing\n"
      assertRejected "invalid source" compile
      readIORef clears >>= assertEqual "source failure clears external recovery graphs before retry" 1
      writeFile target valid
      compile
    readIORef clears >>= assertEqual "request close clears external recovery graphs" 2
    cancelled <- try (runRequest (pure ()) $ \compiler -> do
      _ <- compiler CheckedEnvironment mempty GeneralCompile Nothing target [root] Nothing
      throwIO ThreadKilled) :: IO (Either SomeException ())
    case cancelled of
      Left failure | fromException failure == Just ThreadKilled -> pure ()
      _ -> fail "request cancellation did not propagate"
    runRequest (pure ()) $ \compiler ->
      void (compiler CheckedEnvironment mempty GeneralCompile Nothing target [root] Nothing)
    pure (released, runRequest (pure ()) (\_ -> pure ()))
  assertRejected "compiler called after resident close" releasedCompile
  assertRejected "runner called after resident close" closedRunner
  where
    assertRejected label action = do
      result <- try action :: IO (Either SomeException ())
      case result of
        Left _ -> pure ()
        Right _ -> fail (label ++ " unexpectedly succeeded")
    onAnotherThread action = do
      settled <- newEmptyMVar
      _ <- forkIO ((try action :: IO (Either SomeException ())) >>= putMVar settled)
      takeMVar settled >>= either throwIO pure
    temporary = do
      parent <- getTemporaryDirectory
      (path, handle) <- openTempFile parent "tidepool-compiler-lifecycle"
      hClose handle
      removeFile path
      createDirectory path
      pure path

-- Object identity observes the transaction cache's immutable Core/STG payloads,
-- not the result wrappers or compiler session. Interface hashes and selected
-- owners independently check semantic equivalence.
data CompilerProductSharing = CompilerProductSharing
  { sharedCore :: Map.Map ModuleName (StableName CgGuts)
  , sharedStg :: Map.Map ModuleName (StableName [(CgStgTopBinding, IdSet)])
  }

compilerProductSharing :: PreparedPipelineResult -> IO CompilerProductSharing
compilerProductSharing prepared = do
  cores <- traverse (\owner -> evaluate (finalizedTidyGuts owner) >>= makeStableName)
    (pprFinalizedModules prepared)
  stg <- traverse (\owner -> evaluate (pmBindings owner) >>= makeStableName)
    (Map.fromList [(moduleName (pmModule owner), owner) | owner <- pprModules prepared])
  pure (CompilerProductSharing cores stg)

assertCoreSharing :: String -> Bool -> [String] -> CompilerProductSharing -> CompilerProductSharing -> IO ()
assertCoreSharing label expected owners before after = forM_ owners $ \owner -> do
  let name = mkModuleName owner
  case (Map.lookup name (sharedCore before), Map.lookup name (sharedCore after)) of
    (Just left, Just right) -> assertEqual (label ++ ": " ++ owner) expected (left == right)
    _ -> fail (label ++ ": missing finalized owner " ++ owner)

assertStgSharing :: String -> Bool -> [String] -> CompilerProductSharing -> CompilerProductSharing -> IO ()
assertStgSharing label expected owners before after = forM_ owners $ \owner -> do
  let name = mkModuleName owner
  case (Map.lookup name (sharedStg before), Map.lookup name (sharedStg after)) of
    (Just left, Just right) -> assertEqual (label ++ ": " ++ owner) expected (left == right)
    _ -> fail (label ++ ": missing prepared owner " ++ owner)

preparedOwnerNames :: PreparedPipelineResult -> [String]
preparedOwnerNames = sort . map (moduleNameString . moduleName . pmModule) . pprModules

assertSameInterfaceEvidenceAndPreparedShape :: String -> PreparedPipelineResult -> PreparedPipelineResult -> IO ()
assertSameInterfaceEvidenceAndPreparedShape label before after = do
  assertSameInterfaceEvidence label before after
  assertSamePreparedAbiAndShape label before after

assertSameInterfaceEvidence :: String -> PreparedPipelineResult -> PreparedPipelineResult -> IO ()
assertSameInterfaceEvidence label before after = do
  let interfaces = Map.map (mi_iface_hash . mi_final_exts . hm_iface . finalizedHomeModInfo)
        . pprFinalizedModules
  assertEqual (label ++ ": complete interface evidence") (interfaces before) (interfaces after)

-- ABI, selected bodies and typed result shape are independent of recompilation
-- evidence such as UsageFile. Binding counts observe inventory, not values.
assertSamePreparedAbiAndShape :: String -> PreparedPipelineResult -> PreparedPipelineResult -> IO ()
assertSamePreparedAbiAndShape label before after = do
  let interfaces = Map.map (mi_mod_hash . mi_final_exts . hm_iface . finalizedHomeModInfo)
        . pprFinalizedModules
  assertEqual (label ++ ": module ABI") (interfaces before) (interfaces after)
  assertEqual (label ++ ": selected prepared owners") (preparedOwnerNames before) (preparedOwnerNames after)
  assertEqual (label ++ ": merged binding inventory")
    (length (prBinds (pprPipelineResult before))) (length (prBinds (pprPipelineResult after)))
  case (prResultType (pprPipelineResult before), prResultType (pprPipelineResult after)) of
    (Just left, Just right) -> unless (eqType left right) (fail (label ++ ": result type changed"))
    (Nothing, Nothing) -> pure ()
    _ -> fail (label ++ ": result type disappeared")

memoLifecycleCompilation :: IO ()
memoLifecycleCompilation = bracket temporary removeDirectoryRecursive requestMemoLifecycle
  where
    temporary = do
      parent <- getTemporaryDirectory
      (path, handle) <- openTempFile parent "tidepool-memo-lifecycle"
      hClose handle
      removeFile path
      createDirectory path
      pure path

requestMemoLifecycle :: FilePath -> IO ()
requestMemoLifecycle root = do
  let dependencyDir = root </> "Tidepool" </> "Session" </> "Lib"
      dependencyPath = dependencyDir </> "G1.hs"
      libraryPath = root </> "MemoLibrary.hs"
      targetPath = root </> "MemoTarget.hs"
      otherTargetPath = root </> "MemoOther.hs"
      validDependency = unlines
        [ "module Tidepool.Session.Lib.G1 (dependency) where"
        , "dependency :: Int"
        , "dependency = 41"
        ]
      invalidDependency = unlines
        [ "module Tidepool.Session.Lib.G1 (dependency) where"
        , "dependency :: Int"
        , "dependency = missing"
        ]
      dependencies = ["Tidepool.Session.Lib.G1", "MemoLibrary"]
  createDirectoryIfMissing True dependencyDir
  writeFile dependencyPath validDependency
  writeFile libraryPath "module MemoLibrary (one) where\none :: Int\none = 1\n"
  writeFile targetPath $ unlines
    [ "module MemoTarget where"
    , "import Tidepool.Session.Lib.G1 (dependency)"
    , "import MemoLibrary (one)"
    , "result :: Int"
    , "result = dependency + one"
    ]
  writeFile otherTargetPath "module MemoOther where\nother :: Int\nother = 2\n"
  withResidentPipelineSelectedRequests [root] $ \runRequest -> do
    let compileIn scope purpose compiler = compiler PreparedStg mempty purpose scope targetPath [root] Nothing
        compile = compileIn Nothing GeneralCompile
        incarnate = Just (SessionScope root [] Nothing (Just "7"))
        fresh = runRequest (pure ()) $ \compiler -> do
          result <- compile compiler
          sharing <- compilerProductSharing result
          pure (result, sharing)
    (anonymous, anonymousSharing) <- fresh
    (next, nextSharing) <- fresh
    assertSameInterfaceEvidenceAndPreparedShape "normal request cleanup preserves output" anonymous next
    assertCoreSharing "normal close releases compiler graphs" False dependencies anonymousSharing nextSharing
    failedGraphs <- newIORef Nothing
    failedRequest <- try (runRequest (pure ()) $ \compiler -> do
      result <- compile compiler
      sharing <- compilerProductSharing result
      writeIORef failedGraphs (Just sharing)
      throwIO (userError "request failure after compile"))
      :: IO (Either SomeException ())
    case failedRequest of
      Left _ -> pure ()
      Right _ -> fail "exception cleanup probe unexpectedly succeeded"
    (afterException, afterExceptionSharing) <- fresh
    assertSameInterfaceEvidenceAndPreparedShape "exceptional cleanup preserves output" next afterException
    failedSharing <- readIORef failedGraphs >>= maybe (fail "exception probe did not reach compilation") pure
    assertCoreSharing "exceptional close releases compiler graphs" False dependencies failedSharing afterExceptionSharing
    runRequest (pure ()) $ \compiler -> do
      cold <- compileIn incarnate GeneralCompile compiler
      coldSharing <- compilerProductSharing cold
      warm <- compileIn incarnate LookupTypeCompile compiler
      warmSharing <- compilerProductSharing warm
      assertSameInterfaceEvidenceAndPreparedShape "purpose change preserves native prepared result" cold warm
      assertCoreSharing "transaction reuses finalized dependencies" True dependencies coldSharing warmSharing
      assertStgSharing "transaction reuses prepared dependencies" True dependencies coldSharing warmSharing
      assertCoreSharing "purpose-sensitive target is fresh" False ["MemoTarget"] coldSharing warmSharing
      writeFile dependencyPath invalidDependency
      changed <- try (compileIn incarnate GeneralCompile compiler)
        :: IO (Either DependencyLoadFailure PreparedPipelineResult)
      case changed of
        Left (DependencySourceFailure diagnostics) ->
          unless (any (\diagnostic -> dSeverity diagnostic == DiagError
              && dFile diagnostic == Just (dependencyPath, 3, 14, 3, 21)) diagnostics) $
            fail "changed invalid session dependency lost its exact source error"
        Left DependencyWorkerFailure -> fail "changed dependency became a worker failure"
        Right _ -> fail "changed invalid session dependency reused a stale memo entry"
      writeFile dependencyPath validDependency
      retry <- compileIn incarnate GeneralCompile compiler
      retrySharing <- compilerProductSharing retry
      assertSameInterfaceEvidenceAndPreparedShape "same-request retry restores output" cold retry
      assertCoreSharing "source rejection releases graphs before retry" False dependencies coldSharing retrySharing
    (restored, restoredSharing) <- runRequest (pure ()) $ \compiler -> do
      result <- compileIn incarnate GeneralCompile compiler
      sharing <- compilerProductSharing result
      pure (result, sharing)
    runRequest (pure ()) $ \compiler ->
      void (compiler PreparedStg mempty GeneralCompile Nothing otherTargetPath [root] Nothing)
    (following, followingSharing) <- runRequest (pure ()) $ \compiler -> do
      result <- compileIn incarnate GeneralCompile compiler
      sharing <- compilerProductSharing result
      pure (result, sharing)
    assertSameInterfaceEvidenceAndPreparedShape "fresh incarnated requests preserve prepared output" restored following
    assertCoreSharing "incarnation retains no graphs across requests" False dependencies restoredSharing followingSharing

captureStderr :: FilePath -> String -> IO a -> IO (a, String)
captureStderr root label action = do
  (path, handle) <- openTempFile root label
  saved <- hDuplicate stderr
  result <- (hDuplicateTo handle stderr >> action) `finally` do
    hFlush stderr
    hDuplicateTo saved stderr
    hClose saved
    hClose handle
  output <- readFile' path
  removeFile path
  pure (result, output)

interfaceMeasurementDiagnostics :: IO ()
interfaceMeasurementDiagnostics = bracket temporary removeDirectoryRecursive $ \root -> do
  (_, output) <- captureStderr root "interface-measurements" $ do
    _ <- measureModuleInterface True 101 "Checked" CheckedEnvironmentInterface HptMiss (pure ())
    _ <- measureModuleInterface True 102 "Registered" SessionRegistrationInterface MemoMiss (pure ())
    pure ()
  case filter (isPrefixOf "tidepool-timing-module-detail ") (lines output) of
    [checked, registered] -> do
      validateInterfaceMeasurement "Checked" "checked_environment" "hpt_miss" checked
      validateInterfaceMeasurement "Registered" "session_registration" "memo_miss" registered
    rows -> fail ("expected two interface measurement rows, got " ++ show rows)
  where
    temporary = do
      parent <- getTemporaryDirectory
      (path, handle) <- openTempFile parent "tidepool-interface-measurements"
      hClose handle
      removeFile path
      createDirectory path
      pure path

-- | Precedent: 379da60e6 ("labels: one validator, and a rejection that
-- teaches the label/path distinction") extended 'renderNameError' so a
-- fork-group path pasted where 'batch'\/'subgroup' expect one kebab label
-- names the mistake and shows the fix, instead of leaving a bare
-- 'InvalidKebabName' constructor for the model to puzzle out. This pins that
-- rendering directly (no compile needed: 'renderNameError' is pure), and
-- keeps the plain-kebab rendering for a non-path rejection unchanged.
renderNameErrorTeachesGroupPaths :: IO ()
renderNameErrorTeachesGroupPaths = do
  let pathRejection = renderNameError (InvalidKebabName "correction-20260924/core-execution")
  assertContains "group-path rejection names the offending path"
    "\"correction-20260924/core-execution\"" (Text.unpack pathRejection)
  assertContains "group-path rejection says it is a path, not a label"
    "is a path, not a label" (Text.unpack pathRejection)
  assertContains "group-path rejection points at subgroup's relative contract"
    "`subgroup` is already relative to your own path" (Text.unpack pathRejection)
  assertContains "group-path rejection shows how to build a two-segment path"
    "`batch campaign group`" (Text.unpack pathRejection)
  assertEqual "a non-path invalid label keeps the plain kebab rule"
    "label \"Bad Label\" is not kebab-case: lowercase ASCII letters, digits and single hyphens only, not starting or ending with a hyphen"
    (Text.unpack (renderNameError (InvalidKebabName "Bad Label")))

-- | GHC's own "Ambiguous occurrence" diagnostic already names each candidate,
-- but not in a form a model can paste back as a fix, and it never says what
-- to do. 'Tidepool.DiagJson.envelopeToDiag' appends one line naming every
-- candidate in copyable, fully-qualified form plus the two fixes: qualify
-- the use, or hide one import. This compiles a genuine two-import ambiguity
-- through the typed target diagnostic path and checks the same renderer
-- used by @app/Main.hs@'s @reportDiags@: both qualified candidates and fixes.
ambiguousOccurrenceHintCompilation :: IO ()
ambiguousOccurrenceHintCompilation = bracket temporary removeDirectoryRecursive $ \root -> do
  let reviewPath = root </> "Review.hs"
      workPath = root </> "Work.hs"
      targetPath = root </> "AmbiguousTarget.hs"
  writeFile reviewPath $ unlines
    [ "module Review (candidateSummary) where"
    , "candidateSummary :: Int"
    , "candidateSummary = 1"
    ]
  writeFile workPath $ unlines
    [ "module Work (candidateSummary) where"
    , "candidateSummary :: Int"
    , "candidateSummary = 2"
    ]
  writeFile targetPath $ unlines
    [ "module AmbiguousTarget where"
    , "import Review"
    , "import Work"
    , "result :: Int"
    , "result = candidateSummary"
    ]
  withResidentPipelineSelectedRequests [root] $ \runRequest -> do
    rejected <- try (runRequest (pure ()) $ \compiler ->
        compiler CheckedEnvironment mempty GeneralCompile Nothing targetPath [root] Nothing)
      :: IO (Either SourceError CheckedEnvironmentResult)
    case rejected of
      Right _ -> fail "ambiguous candidateSummary occurrence unexpectedly compiled"
      Left sourceError -> do
        let diagnostics = diagsFromSourceError sourceError
        unless (any ((/= Nothing) . dFile) diagnostics) $
          fail "ambiguous occurrence lost its source span"
        let rendered = intercalate "\n" (map dMessage diagnostics)
        assertContains "ambiguous occurrence names the first qualified candidate"
          "Review.candidateSummary" rendered
        assertContains "ambiguous occurrence names the second qualified candidate"
          "Work.candidateSummary" rendered
        assertContains "ambiguous occurrence suggests qualifying the use"
          "qualify the use" rendered
        assertContains "ambiguous occurrence suggests hiding an import"
          "hide one import" rendered
  where
    temporary = do
      parent <- getTemporaryDirectory
      (path, handle) <- openTempFile parent "tidepool-ambiguous-occurrence"
      hClose handle
      removeFile path
      createDirectory path
      pure path

validateInterfaceMeasurement :: String -> String -> String -> String -> IO ()
validateInterfaceMeasurement expectedModule expectedStage expectedReuse row = do
  assertEqual "interface measurement module" (Just expectedModule) (field "module")
  assertEqual "interface measurement parent" (Just "module_interface") (field "parent")
  assertEqual "interface measurement phase" (Just "make_iface") (field "phase")
  assertEqual "interface measurement stage" (Just expectedStage) (field "stage")
  assertEqual "interface measurement reuse" (Just expectedReuse) (field "reuse")
  forM_ ["request", "ms", "wall_ns", "cpu_ns"] assertDecimal
  assertEqual "RTS counter scope" (Just "process_delta") (field "rts_scope")
  case field "rts" of
    Just "enabled" -> forM_ rtsCounters assertDecimal
    Just "unavailable" -> forM_ rtsCounters $ \name ->
      assertEqual ("unavailable RTS counter " ++ name) (Just "unavailable") (field name)
    status -> fail ("unexpected RTS availability in interface measurement: " ++ show status)
  where
    fields =
      [ (name, drop 1 value)
      | token <- words row
      , let (name, value) = break (== '=') token
      , not (null value)
      ]
    field name = lookup name fields
    assertDecimal name = case field name of
      Just value | not (null value) && all isDigit value -> pure ()
      value -> fail ("non-decimal interface measurement field " ++ name ++ ": " ++ show value)
    rtsCounters = ["allocated_bytes", "gc_cpu_ns", "gc_elapsed_ns", "gcs"]

semicolonStatementRefinement :: DynFlags -> IO ()
semicolonStatementRefinement flags = do
  forM_ cases $ \(label, statements, expected) -> do
    newline <- analyze (intercalate "\n" statements)
    inline <- analyze (intercalate "; " statements)
    assertEqual (label ++ " newline statement identities") expected (identities newline)
    assertEqual (label ++ " semicolon statement identities") expected (identities inline)
    assertEqual (label ++ " ordered authored ordinals") [0 .. length statements - 1]
      (map cellAnalysisSourceOrdinal (concatMap cellAnalysisSourceItems inline))
    assertEqual (label ++ " source slices exclude synthetic terminal") statements
      (map cellAnalysisSource inline)
  coordinates <- analyze "pure \"λ;γ\";\tpure \"δ\""
  assertEqual "Unicode and tab source slices" ["pure \"λ;γ\"", "pure \"δ\""]
    (map cellAnalysisSource coordinates)
  assertEqual "GHC tab columns in original source" [1, 17]
    (map (cellStartColumn . cellAnalysisSpan) coordinates)
  indented <- analyze "\n  pure \"a\"; pure \"b\""
  assertEqual "indented statements preserve exact authored token slices"
    ["pure \"a\"", "pure \"b\""] (map cellAnalysisSource indented)
  assertEqual "leading blank line remains in authored coordinates" [2, 2]
    (map (cellStartLine . cellAnalysisSpan) indented)
  assertEqual "non-column-one original items preserve parser columns" [3, 13]
    (map (cellStartColumn . cellAnalysisSpan) indented)
  comments <- analyze "pure (17 :: Int); {- not; a; statement -} pure (19 :: Int) -- trailing; comment"
  assertEqual "comment semicolons do not become statements" [KExpr, KExpr]
    (map (sbKind . cellAnalysisVerdict) comments)
  assertEqual "trailing comment cannot swallow parser terminal"
    ["pure (17 :: Int)", "pure (19 :: Int)"] (map cellAnalysisSource comments)
  empty <- analyze "-- only; a comment\n"
  assertEqual "comment-only cell has no executable statements" [] empty
  declarations <- analyze "identity :: a -> a; identity value = value"
  assertEqual "same-line authored declarations remain one original" [KDecl]
    (map (sbKind . cellAnalysisVerdict) declarations)
  malformed <- analyzeOrderedCellWithFlags flags checkTemplate "answer <- ; pure answer"
  case malformed of
    Left CellStatementParseFailure {} -> pure ()
    _ -> fail ("invalid statement list received executable identities: " ++ show malformed)
  where
    analyze source = analyzeOrderedCellWithFlags flags checkTemplate source
      >>= either (fail . renderCellSplitError) (pure . cellPlanItems)
    identities = map (\item ->
      (sbKind (cellAnalysisVerdict item), cellAnalysisBindingForm item,
        sbBinders (cellAnalysisVerdict item)))
    cases =
      [ ("two observations", ["pure (17 :: Int)", "pure (19 :: Int)"],
          [(KExpr, Nothing, []), (KExpr, Nothing, [])])
      , ("action and observation", ["answer <- pure (7 :: Int)", "pure answer"],
          [(KBind, Just ActionBinding, ["answer"]), (KExpr, Nothing, [])])
      , ("let declarations retain nested semicolons",
          ["let { identity value = value; constant value _ = value }", "pure (identity (7 :: Int))"],
          [(KBind, Just LetBinding, ["identity", "constant"]), (KExpr, Nothing, [])])
      , ("nested explicit do", ["answer <- do { pure (); pure (7 :: Int) }", "pure answer"],
          [(KBind, Just ActionBinding, ["answer"]), (KExpr, Nothing, [])])
      , ("nested implicit do", ["answer <- (do\n  pure ()\n  pure (7 :: Int))", "pure answer"],
          [(KBind, Just ActionBinding, ["answer"]), (KExpr, Nothing, [])])
      , ("strings", ["pure \"left;right\"", "pure \"next\""],
          [(KExpr, Nothing, []), (KExpr, Nothing, [])])
      , ("quasiquotes", ["quoted <- [q|left;right|]", "pure quoted"],
          [(KBind, Just ActionBinding, ["quoted"]), (KExpr, Nothing, [])])
      ]

lexicalIslands :: DynFlags -> IO ()
lexicalIslands flags = do
  items <- split flags lexicalCell
  assertEqual "lexical item count" 6 (length items)
  assertEqual
    "lexical starts"
    [1, 5, 6, 10, 15, 21]
    (map (cellStartLine . cellSourceSpan) items)
  assertEqual
    "lexical kinds"
    [KDecl, KDecl, KDecl, KBind, KBind, KExpr]
    (map (sbKind . classifyWithFlags flags . cellSourceText) items)
  quasiquote <- sourceAt 3 items
  multiline <- sourceAt 4 items
  assertContains "quasiquote body" "echo right\n|]" quasiquote
  assertContains "multiline body" "column-one\n\nstill string" multiline
  where
    lexicalCell =
      unlines
        [ "data Verdict"
        , "  = Accept String"
        , "  | Repair [String]"
        , ""
        , "score :: Verdict -> Int"
        , "score = \\case"
        , "  Accept _ -> 1"
        , "  Repair xs -> negate (length xs)"
        , ""
        , "reviewers <- [bash|"
        , "echo left"
        , ""
        , "echo right"
        , "|]"
        , "text <- pure \"\"\""
        , "column-one"
        , ""
        , "still string"
        , "\"\"\""
        , ""
        , "case text of"
        , "  _ -> reviewers"
        ]

commentsPragmasAndLayout :: DynFlags -> IO ()
commentsPragmasAndLayout flags = do
  items <- split flags layoutCell
  assertEqual "layout item count" 4 (length items)
  assertEqual
    "layout starts"
    [1, 2, 8, 12]
    (map (cellStartLine . cellSourceSpan) items)
  pragma <- sourceAt 0 items
  commented <- sourceAt 1 items
  withWhere <- sourceAt 2 items
  assertContains "pragma remains intact" "MultilineStrings" pragma
  assertContains "nested comment remains intact" "{- inner -}" commented
  assertContains "where remains continuation" "  where\n    answer = 1" withWhere
  assertEqual
    "layout kinds after pragma"
    [KDecl, KDecl, KBind]
    (map (sbKind . classifyWithFlags flags . cellSourceText) (drop 1 items))
  where
    layoutCell =
      unlines
        [ "{-# LANGUAGE MultilineStrings #-}"
        , "value ="
        , "  {- outer"
        , "     {- inner -}"
        , "  -}"
        , "  1"
        , ""
        , "withWhere x = answer + x"
        , "  where"
        , "    answer = 1"
        , ""
        , "next <- pure (withWhere value)"
        ]

declarationsBecomeOneCellItem :: DynFlags -> IO ()
declarationsBecomeOneCellItem flags = do
  analyzed <- analyzeCellWithFlags flags checkTemplate cell
  case analyzed of
    Left failure -> fail ("cell analysis failed: " ++ show failure)
    Right plan -> do
      let items = cellPlanItems plan
      assertEqual "grouped cell item count" 3 (length items)
      assertEqual
        "grouped cell kinds"
        [KDecl, KBind, KExpr]
        (map (sbKind . cellAnalysisVerdict) items)
      case items of
        declaration : _ -> do
          let source = cellAnalysisSource declaration
          assertContains "group includes signature" "evenCell :: Int -> Bool" source
          assertContains "group includes first equation" "evenCell 0 = True" source
          assertContains "group includes mutual reference" "oddCell n = evenCell" source
          assertEqual
            "group retains declaration ordinals"
            [0..5]
            (map cellAnalysisSourceOrdinal (cellAnalysisSourceItems declaration))
          assertEqual
            "group retains declaration starts"
            [1..6]
            (map
              (cellStartLine . cellAnalysisSourceSpan)
              (cellAnalysisSourceItems declaration))
        [] -> fail "grouped cell returned no declaration item"
  where
    cell = unlines
      [ "evenCell :: Int -> Bool"
      , "evenCell 0 = True"
      , "evenCell n = oddCell (n - 1)"
      , "oddCell :: Int -> Bool"
      , "oddCell 0 = False"
      , "oddCell n = evenCell (n - 1)"
      , "answer <- pure (evenCell 4)"
      , "answer"
      ]

checkTemplate :: String
checkTemplate = unlines
  [ "{-# LANGUAGE LambdaCase, QuasiQuotes, MultilineStrings, StandaloneDeriving #-}"
  , "{{CELL_PRAGMAS}}"
  , "module CellCheck where"
  , "import GHC.Generics (Generic)"
  , "{{CELL_IMPORTS}}"
  , "{{CELL_DECLS}}"
  , "__tidepool_cell_check = do { {{CELL_BODY}} }"
  ]

automaticGenericPlans :: DynFlags -> IO ()
automaticGenericPlans flags = do
  result <- analyzeCellWithFlags flags checkTemplate source
  case result of
    Left failure -> fail ("automatic Generic plan failed: " ++ renderCellSplitError failure)
    Right plan -> case cellPlanItems plan of
      declaration : _ -> do
        let rendered = cellAnalysisSource declaration
        assertEqual "generated helper imports retain exact intent"
          [RetainedGeneratedImport | _ <- prologueImports (cellPlanPrologue plan)]
          (map locatedImportIntent (prologueImports (cellPlanPrologue plan)))
        assertContains "parameterized data instance" "deriving instance TidepoolCompilerGeneric.Generic (Packet a)" rendered
        assertContains "parameterized newtype instance" "deriving instance TidepoolCompilerGeneric.Generic (Wrapper a)" rendered
        assertEqual "generated instances are not receipt items" [0..7]
          (map cellAnalysisSourceOrdinal (cellAnalysisSourceItems declaration))
        assertContains "authored class identity is deferred to GHC" "Generic (Explicit)" rendered
        assertEqual "standalone candidate awaits typed identity resolution" 2
          (occurrences "Generic (Manual a)" rendered)
        unless (not ("Generic (Witness a)" `isInfixOf` rendered)) $
          fail "GADT received an automatic Generic instance"
        unless (not ("Generic (Hidden a)" `isInfixOf` rendered)) $
          fail "existential received an automatic Generic instance"
        checked <- either fail pure (renderCellCheckSource checkTemplate plan)
        assertContains "check source carries generated declaration"
          "deriving instance TidepoolCompilerGeneric.Generic (Packet a)" checked
      [] -> fail "automatic Generic cell omitted declaration item"
  where
    source = unlines
      [ "{-# LANGUAGE GADTs, StandaloneDeriving, ExistentialQuantification #-}"
      , "data Packet a = Packet (a -> a) a"
      , "newtype Wrapper a = Wrapper (Packet a)"
      , "data Explicit = Explicit deriving Generic"
      , "data Manual a = Manual a"
      , "deriving instance Generic (Manual a)"
      , "data Witness a where"
      , "  Witness :: Int -> Witness Int"
      , "data Hidden a = forall b. Hidden b"
      ]

noStandaloneDerivingLeavesCellUntouched :: DynFlags -> IO ()
noStandaloneDerivingLeavesCellUntouched flags = do
  result <- analyzeCellWithFlags flags checkTemplate source
  case result of
    Right plan -> case cellPlanItems plan of
      declaration : _ -> unless (not ("deriving instance Generic" `isInfixOf` cellAnalysisSource declaration)) $
        fail "NoStandaloneDeriving must suppress generated Generic syntax"
      [] -> fail "NoStandaloneDeriving cell omitted declaration item"
    Left failure -> fail ("NoStandaloneDeriving plan failed: " ++ renderCellSplitError failure)
  where
    source = unlines
      [ "{-# LANGUAGE NoStandaloneDeriving #-}"
      , "data Local = Local Int"
      ]

-- | A display-expression cell whose last real token is an infix operator
-- (e.g. a model typo like @respond ("...") .@) must be rejected with a
-- direct diagnostic at check time, before 'renderExecutable' would wrap it
-- in a synthesized left section and turn the missing operand into a
-- confusing type error instead of a clear syntax error. A trailing operator
-- that is followed by more of the SAME item on a later line (a legitimate
-- multi-line operator chain, operator leading or trailing) must not be
-- rejected.
danglingOperatorCells :: DynFlags -> IO ()
danglingOperatorCells flags = do
  assertDangling "trailing dot" "." "respond (x) .\n"
  assertDangling "trailing dollar" "$" "f $\n"
  assertDangling "trailing backquoted operator" "`elem`" "x `elem`\n"
  assertNotDangling "leading-operator continuation line" (unlines
    [ "f x"
    , "  . g y"
    ])
  assertNotDangling "trailing-operator continuation line" (unlines
    [ "f x ."
    , "  g y"
    ])
  where
    assertDangling label operatorText source = do
      result <- analyzeCellWithFlags flags checkTemplate source
      case result of
        Left (CellDanglingOperatorFailure _ actualOperatorText) ->
          assertEqual (label ++ " operator text") operatorText actualOperatorText
        other -> fail (label ++ ": expected a dangling-operator rejection, got " ++ show other)
    assertNotDangling label source = do
      result <- analyzeCellWithFlags flags checkTemplate source
      case result of
        Right plan -> case cellPlanItems plan of
          [item] -> assertEqual (label ++ " verdict") KExpr (sbKind (cellAnalysisVerdict item))
          items -> fail (label ++ ": expected exactly one item, got " ++ show (length items))
        Left failure -> fail (label ++ ": expected acceptance, got " ++ renderCellSplitError failure)

prologuePlans :: DynFlags -> IO ()
prologuePlans flags = do
  let source = unlines
        [ "{-# LANGUAGE NoLambdaCase #-}"
        , "{-# OPTIONS_GHC -Wno-unused-imports #-}"
        , "import qualified Data.Map.Strict as Map"
        , "answer = Map.empty"
        ]
  result <- analyzeCellWithFlags flags checkTemplate source
  case result of
    Left failure -> fail (renderCellSplitError failure)
    Right plan -> do
      let prologue = cellPlanPrologue plan
      assertEqual "pragma kinds"
        [LanguagePragma, OptionsGhcPragma]
        (map locatedPragmaKind (prologuePragmas prologue))
      assertEqual "pragma starts" [1, 2]
        (map (cellStartLine . locatedPragmaSpan) (prologuePragmas prologue))
      assertEqual "import starts" [3]
        (map (cellStartLine . locatedImportSpan) (prologueImports prologue))
      assertEqual "normalized import" ["import qualified Data.Map.Strict as Map"]
        (map locatedImportSource (prologueImports prologue))
      assertEqual "authored alias preserves parsed owner and qualifier"
        [AuthoredSourceImport (mkModuleName "Data.Map.Strict") NoRawPkgQual]
        (map locatedImportIntent (prologueImports prologue))
      case cellPlanItems plan of
        declaration : _ -> do
          assertEqual "prologue plus a declaration is not prologue-only" False
            (cellAnalysisPrologueOnly declaration)
          assertEqual "grouped source ordinals" [0..3]
            (map cellAnalysisSourceOrdinal (cellAnalysisSourceItems declaration))
          let body = cellAnalysisSource declaration
          assertContains "declaration retained" "answer = Map.empty" body
          if "import qualified" `isInfixOf` body
            then fail ("declaration body retained import: " ++ body)
            else pure ()
        [] -> fail "cell plan omitted declaration"
      checked <- either fail pure (renderCellCheckSource checkTemplate plan)
      assertContains "rendered cell pragma" "{-# LANGUAGE NoLambdaCase #-}" checked
      assertContains "rendered cell import" "import qualified Data.Map.Strict as Map" checked
  captured <- analyzeCellWithFlags flags checkTemplate "let alias = retainedValue\n"
  case captured of
    Right plan -> assertEqual "template imports do not become authored demands" []
      (map locatedImportIntent (prologueImports (cellPlanPrologue plan)))
    Left failure -> fail (renderCellSplitError failure)
  packaged <- analyzeCellWithFlags flags checkTemplate (unlines
    [ "{-# LANGUAGE PackageImports #-}"
    , "import qualified \"containers\" Data.Map.Strict as Map"
    , "let answer = Map.empty"
    ])
  case packaged of
    Right plan -> case map locatedImportIntent (prologueImports (cellPlanPrologue plan)) of
      [AuthoredSourceImport owner (RawPkgQual _)] ->
        assertEqual "package qualifier retains parsed module" (mkModuleName "Data.Map.Strict") owner
      other -> fail ("package-qualified authored import lost its intent: " ++ show other)
    Left failure -> fail (renderCellSplitError failure)
  importOnly <- analyzeCellWithFlags flags checkTemplate "import Data.List\n"
  case importOnly of
    Right (CellSourcePlan { cellPlanItems = [item] }) -> do
      assertEqual "import-only kind" KDecl (sbKind (cellAnalysisVerdict item))
      assertEqual "import-only body" "" (cellAnalysisSource item)
      assertEqual "import-only item is classified as prologue, not a definition" True
        (cellAnalysisPrologueOnly item)
      assertEqual "import-only ordinals" [0]
        (map cellAnalysisSourceOrdinal (cellAnalysisSourceItems item))
    other -> fail ("import-only plan: " ++ show other)
  cpp <- analyzeCellWithFlags flags checkTemplate "{-# LANGUAGE CPP #-}\nvalue = 1\n"
  case cpp of
    Left (CellPrologueFailure sourceSpan _) ->
      assertEqual "CPP location" 1 (cellStartLine sourceSpan)
    other -> fail ("CPP should be rejected with a location: " ++ show other)
  latePragma <- analyzeCellWithFlags flags checkTemplate
    "value = 1\n{-# LANGUAGE ImplicitParams #-}\n"
  case latePragma of
    Left (CellPrologueFailure sourceSpan _) ->
      assertEqual "late pragma location" 2 (cellStartLine sourceSpan)
    other -> fail ("late pragma should be rejected: " ++ show other)
  pragmaOnly <- analyzeCellWithFlags flags checkTemplate "{-# LANGUAGE NoLambdaCase #-}\n"
  case pragmaOnly of
    Right (CellSourcePlan { cellPlanItems = [item] }) -> do
      assertEqual "pragma-only kind" KDecl (sbKind (cellAnalysisVerdict item))
      assertEqual "pragma-only body" "" (cellAnalysisSource item)
      assertEqual "pragma-only item is classified as prologue, not a definition" True
        (cellAnalysisPrologueOnly item)
    other -> fail ("pragma-only plan: " ++ show other)
  pragmaBeforeExpression <- analyzeCellWithFlags flags checkTemplate
    "{-# LANGUAGE OverloadedLabels, OverloadedRecordDot #-}\n1 + 1\n"
  case pragmaBeforeExpression of
    Right (CellSourcePlan { cellPlanItems = [header, item] }) -> do
      assertEqual "leading pragma is a prologue" True (cellAnalysisPrologueOnly header)
      assertEqual "leading pragma preserves the expression" KExpr
        (sbKind (cellAnalysisVerdict item))
      assertEqual "expression source kind" [KExpr]
        (map cellAnalysisSourceKind (cellAnalysisSourceItems item))
    other -> fail ("leading pragma plan: " ++ show other)
  disabled <- analyzeCellWithFlags flags checkTemplate
    "{-# LANGUAGE NoQuasiQuotes #-}\nf = [bash|echo hello|]\n"
  case disabled of
    Right (CellSourcePlan { cellPlanItems = [_, item] }) ->
      assertEqual "NoQuasiQuotes survives classification" KExpr
        (sbKind (cellAnalysisVerdict item))
    other -> fail ("NoQuasiQuotes plan: " ++ show other)
  let commentedImports = unlines
        [ "-- leading comment"
        , "{- outer {- nested -} -}"
        , "import Data.List"
        , "  ( sort"
        , "  , nub"
        , "  )"
        , "-- between imports"
        , "import qualified Data.Map.Strict as Map"
        , "answer = sort []"
        ]
  commented <- analyzeCellWithFlags flags checkTemplate commentedImports
  case commented of
    Right plan -> do
      assertEqual "commented import count" 2
        (length (prologueImports (cellPlanPrologue plan)))
      let rendered = map locatedImportSource (prologueImports (cellPlanPrologue plan))
      if any ('\n' `elem`) rendered
        then fail ("imports were not single-line: " ++ show rendered)
        else pure ()
      case cellPlanItems plan of
        declaration : _ ->
          assertEqual "commented import source ordinals" [0..2]
            (map cellAnalysisSourceOrdinal (cellAnalysisSourceItems declaration))
        [] -> fail "commented import cell omitted declaration"
    Left failure -> fail ("commented import plan: " ++ renderCellSplitError failure)
  let longImport = unlines
        [ "import Data.List"
        , "  ( sort, nub, intercalate, intersperse, permutations, subsequences"
        , "  , tails, inits, transpose, group, groupBy, sortBy, sortOn"
        , "  , unfoldr, partition, span, break, stripPrefix, isPrefixOf"
        , "  , isSuffixOf, isInfixOf, find, findIndex, findIndices"
        , ")"
        , "answer = sort []"
        ]
  longResult <- analyzeCellWithFlags flags checkTemplate longImport
  case longResult of
    Right plan -> do
      assertEqual "long import count" 1
        (length (prologueImports (cellPlanPrologue plan)))
      case map locatedImportSource (prologueImports (cellPlanPrologue plan)) of
        [rendered] | '\n' `elem` rendered ->
          fail ("long import wrapped: " ++ show rendered)
        [_] -> pure ()
        other -> fail ("unexpected long imports: " ++ show other)
    Left failure -> fail ("long import plan: " ++ renderCellSplitError failure)
  declaration <- declarationSourceWithTemplate checkTemplate commentedImports
  case declaration of
    Right normalized -> do
      assertEqual "turn prologue imports" 2
        (length (prologueImports (declarationPrologue normalized)))
      if "import Data.List" `isInfixOf` declarationBody normalized
        then fail "turn declaration body retained import"
        else pure ()
    Left failure -> fail ("turn declaration source: " ++ renderCellSplitError failure)
  executableComments <- analyzeCellWithFlags flags checkTemplate (unlines
    [ "first <- pure (1 :: Int)"
    , "-- between statements"
    , "{- nested {- comment -} -}"
    , "second <- pure (first + 1)"
    ])
  case executableComments of
    Right plan -> do
      assertEqual "comments do not create expression items" [KBind, KBind]
        (map (sbKind . cellAnalysisVerdict) (cellPlanItems plan))
      assertEqual "statement lines after comments" [1, 4]
        (map (cellStartLine . cellAnalysisSpan) (cellPlanItems plan))
    Left failure -> fail ("executable comments: " ++ renderCellSplitError failure)

split :: DynFlags -> String -> IO [CellSourceItem]
split flags source =
  case splitCellWithFlags flags source of
    Left failure -> fail ("cell split failed: " ++ show failure)
    Right items -> pure items

sourceAt :: Int -> [CellSourceItem] -> IO String
sourceAt index items =
  case drop index items of
    item : _ -> pure (cellSourceText item)
    [] -> fail ("missing cell source item " ++ show index)

assertEqual :: (Eq a, Show a) => String -> a -> a -> IO ()
assertEqual label expected actual =
  unless (expected == actual) $
    fail (label ++ ": expected " ++ show expected ++ ", got " ++ show actual)

assertContains :: String -> String -> String -> IO ()
assertContains label needle haystack =
  unless (needle `isInfixOf` haystack) $
    fail (label ++ ": missing " ++ show needle ++ " in " ++ show haystack)

occurrences :: String -> String -> Int
occurrences needle = length . filter (isPrefixOf needle) . tails
