module Main (main, tests) where

import Tidepool.PreparedStg.Internal (PreparedModule(..))
import Tidepool.Test.Runner (TestTree, runTests, testCase, testGroup)

import Control.Exception (SomeException, bracket, evaluate, fromException, throwIO, try)
import Control.Monad (unless)
import Data.ByteString qualified as BS
import Data.Foldable (toList)
import Data.IntMap.Strict qualified as IntMap
import Data.IORef (modifyIORef')
import Data.List (isInfixOf, nub, sort)
import Data.String (fromString)
import Data.Set qualified as Set
import Data.Text qualified as Text
import Data.Text.IO qualified as TextIO
import GHC (moduleNameString, mkModuleName, ModSummary(ms_location), ms_mod_name)
import GHC.Driver.Env (HscEnv(..), hsc_mod_graph, hscUpdateHPT)
import GHC.Unit.Module.Graph (ModuleGraphNode(..), mgModSummaries')
import GHC.Unit.Module.ModGuts (CgGuts(..))
import GHC.Unit.Home.ModInfo (HomeModInfo(..), addToHpt)
import GHC.Unit.Env (UnitEnv(..))
import GHC.Unit.External (ExternalUnitCache(..), ExternalPackageState(..))
import GHC.Unit.Module.ModDetails (ModDetails(..))
import GHC.Unit.Module.Env (extendModuleEnv)
import GHC.Types.TypeEnv (emptyTypeEnv, plusTypeEnv)
import GHC.Builtin.Types (boolTy)
import GHC.Core (Expr(..), bindersOf, bindersOfBinds, flattenBinds)
import GHC.Core.DataCon (dataConName, dataConRepArgTys)
import GHC.Core.DataCon qualified as DC
import GHC.Core.TyCon (PromDataConInfo(NoPromInfo))
import GHC.Core.TyCon qualified as TC
import GHC.Core.Type (mkTyConApp)
import GHC.Builtin.Types.Prim (wordPrimTy)
import GHC.Core.FVs (exprSomeFreeVarsList)
import GHC.Core.TyCo.Rep (Scaled(..), Type(TyConApp))
import GHC.Types.Id (idName, idType)
import GHC.Types.Var (varName, varUnique)
import GHC.Types.Unique.Set (elementOfUniqSet)
import GHC.Types.Name (nameOccName, setNameUnique)
import GHC.Types.Unique (mkUnique, getKey)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.Id.Make (nospecId)
import GHC.Types.RepType (typePrimRep_maybe)
import GHC.Core.TyCo.Compare (eqType)
import Tidepool.SiteClassifier
  ( SiteFailure(..), classifySiteOccurrence, isNospecVar, stripNospecSpine )
import GHC.Unit.Types (moduleName)
import System.Directory
  ( createDirectory, createDirectoryIfMissing, getTemporaryDirectory, makeAbsolute, removeFile, removePathForcibly )
import System.IO (openTempFile, hClose)
import System.Environment (getEnv)
import System.Mem.StableName (StableName, makeStableName)
import System.FilePath ((</>), normalise)
import GHC.Types.SourceError (SourceError)
import Tidepool.DiagJson (Diag(..), DiagSeverity(..), diagsFromSourceError)
import Tidepool.GhcPipeline
  ( PipelineSelection(..), PreparedPipelineResult, pprPipelineResult, pprModules, pprFinalizedModules
  , CompilePurpose(..), prHscEnv, prCanonicalInterfaceAdmissions, runPipelineSelected
  , withResidentPipelineSelected )
import Tidepool.PreparedStg
  ( PreparedCoverage(..), pmModule, pmCoverage, pmBindings, pmYieldSites, pmPreparedSites, pmTypeGraph, pmSiteRejections
  , RecoveredModuleInput(..), prepareRecoveredModule, newPreparedBodyCache, newPreparedBodyPreparer
  , preparedUsesSiteAuthority, filterPreparedBindings, preparedRejectsIntrinsic
  , resolvePreparedSiteEnvironment, preparedSiteDependenciesMatch, preparedSiteDependenciesEquivalent )
import Tidepool.FinalizedModule (FinalizedModule(..))
import Tidepool.ExactHydration (forkExactContext)
import Tidepool.FatIface.Internal (issueOwnerInterfaceContext)
import GHC.Unit.Module.ModIface (mi_iface_hash, mi_final_exts, mi_module)
import Tidepool.FatIface
  ( OwnerInterfaceContext, ownerInterfaceLocation, ownerInterfaceTyCons, ownerInterfaceEntries, newOwnerInterfaceCache, cacheOwnerInterface )
import qualified Data.Map.Strict as Map
import qualified Tidepool.ExecutionProjection as Projection
import qualified Tidepool.ExecutionSchema as Schema
import qualified Tidepool.TypePolicy as TypePolicy
import Tidepool.EffectSchema
  ( SiteDelivery(..), SiteType(..), SiteWireSource(..), YieldSite(..)
  , sitedVerbs, vsDelivery, vsName, vsWireSource )
import Tidepool.ExecutionEncode (encodeWireProgram)
import Tidepool.ExecutionIR
  ( LiteralInventory(..), PreparedFact(..), PreparedInventory(..), PreparedSupport(..), inventoryPreparedModule
  , renderPreparedInventory )
import Tidepool.PreparedSites
  ( PreparedSite(..), SiteRejection(..), buildYieldSite, lookupPreparedVerb, resolvePreparedSiblings )
import RetainedPluginTest (verifyCompilerReuse, verifyPreparedScope)
import ModuleProductRoundtripTest
  ( verifyModuleProductInterfaceRoundtrip, verifyOriginalProductCatalogue )
import TypeEvidenceChecks (runTypeEvidenceChecks)
import Tidepool.PreparedJson (JsonAuthority, resolveJsonAuthority)
import Tidepool.HostBindingAuthority
  ( HostBindingAuthority(..), resolveHostBindingAuthorities
  , hostBindingRepresentationForType, hostBindingRepresentationAuthority
  , hostBindingRepresentationConstructors )

assert :: Bool -> String -> IO ()
assert ok message = unless ok (ioError (userError message))

preparedShape :: PreparedPipelineResult -> [(String, Int)]
preparedShape = sort . map shape . pprModules
  where
    shape prepared =
      ( moduleNameString (moduleName (pmModule prepared))
      , length (pmBindings prepared)
      )

allPreparedEvidence :: PreparedPipelineResult -> [String]
allPreparedEvidence = sort . map (renderPreparedInventory . inventoryPreparedModule) . pprModules

assertProductionFacts :: PreparedPipelineResult -> IO ()
assertProductionFacts result = do
  let facts = concatMap (foldr (:) [] . inventoryFacts . inventoryPreparedModule) (pprModules result)
      literals = concatMap (foldr (:) [] . inventoryLiterals . inventoryPreparedModule) (pprModules result)
  assert (any isImport facts) "prepared inventory omitted imported identities"
  assert (any isOperation facts) "prepared inventory omitted operation signatures"
  assert (any isTagCount facts) "prepared inventory omitted tag-inference evidence"
  assert (any isConstructor facts) "prepared inventory omitted constructor layouts"
  assert (any isNulPolicy facts) "prepared inventory omitted the explicit embedded-NUL support decision"
  assert (any isNulLiteral literals) "prepared inventory did not retain the embedded-NUL bytes"
 where
  isImport ImportedValue{} = True; isImport _ = False
  isOperation OperationSignature{} = True; isOperation _ = False
  isTagCount TagInferenceCount{} = True; isTagCount _ = False
  isConstructor ConstructorLayout{} = True; isConstructor _ = False
  isNulPolicy (SupportStatus EmbeddedNulStringsRetainedAsModifiedUtf8) = True; isNulPolicy _ = False
  isNulLiteral (StringLiteral bytes _) = 0 `elem` bytes || [0xC0, 0x80] `isInfixOf` bytes
  isNulLiteral _ = False

preparedEvidence :: String -> PreparedPipelineResult -> (String, [YieldSite])
preparedEvidence wanted result = case filter isWanted (pprModules result) of
  [prepared] ->
    (renderPreparedInventory (inventoryPreparedModule prepared), pmYieldSites prepared)
  modules -> error ("expected one prepared module " ++ wanted ++ ", got "
    ++ show (map (moduleNameString . moduleName . pmModule) modules))
  where
    isWanted = (== wanted) . moduleNameString . moduleName . pmModule

-- | Project one entry of a prepared fixture module, as the extractor does.
projectEntry :: PreparedPipelineResult -> String -> String
  -> Map.Map Schema.SymbolIdentity Word -> Either Projection.ProjectionError Schema.WireProgram
projectEntry result modul entry retained =
  projectEntryWithAux result modul entry [] retained

projectEntryWithJsonAuthority :: Projection.ProjectionContext -> PreparedPipelineResult
  -> String -> Either Projection.ProjectionError Schema.WireProgram
projectEntryWithJsonAuthority context result entry =
  Projection.projectPreparedTarget
    (context { Projection.projectionEntry = (Projection.projectionEntry context)
      { Schema.symbolOccurrence = fromString entry } }) (pprModules result)

jsonProjectionContext :: JsonAuthority -> PreparedPipelineResult -> String -> String
  -> IO Projection.ProjectionContext
jsonProjectionContext authority result modul entry = do
  textAuthority <- Projection.resolveTextPackageUnit (prHscEnv (pprPipelineResult result))
    >>= maybe (ioError (userError "JSON projection lacks its selected compiler Text package")) pure
  pure Projection.ProjectionContext
    { Projection.projectionProfile = "ghc-9.12-prepared-stg"
    , Projection.projectionToolchain = "ghc-9.12.2"
    , Projection.projectionTarget =
        Schema.TargetDescriptor Schema.X86_64 Schema.LittleEndian 64 64 "sysv64" []
    , Projection.projectionRetainedGenerations = mempty
    , Projection.projectionCurrentOriginals = mempty
    , Projection.projectionEntry = Schema.SymbolIdentity "main" (fromString modul)
        "value" (fromString entry) Nothing
    , Projection.projectionAuxiliaryRoots = []
    , Projection.projectionFormattingAuthority = Nothing
    , Projection.projectionTimeAuthority = Nothing
    , Projection.projectionJsonAuthority = Just authority
    , Projection.projectionTextUnit = Just textAuthority
    }

-- | 'projectEntry' plus a set of auxiliary root occurrences in the same
-- module (mirroring the fixed entries beside a turn's resume entry): admitted
-- the same way as a turn's own auxiliary roots, so a
-- fixture can assert that an auxiliary root's own result type is interned
-- even when the selected entry never otherwise reaches it.
projectEntryWithAux :: PreparedPipelineResult -> String -> String -> [String]
  -> Map.Map Schema.SymbolIdentity Word -> Either Projection.ProjectionError Schema.WireProgram
projectEntryWithAux result = projectModulesWithAux (pprModules result)

projectModulesWithAux :: [PreparedModule] -> String -> String -> [String]
  -> Map.Map Schema.SymbolIdentity Word -> Either Projection.ProjectionError Schema.WireProgram
projectModulesWithAux modules modul entry auxEntries retained =
  Projection.projectPreparedTarget context modules
  where
    context = Projection.ProjectionContext
      { Projection.projectionProfile = "ghc-9.12-prepared-stg"
      , Projection.projectionToolchain = "ghc-9.12.2"
      , Projection.projectionTarget =
          Schema.TargetDescriptor Schema.X86_64 Schema.LittleEndian 64 64 "sysv64" []
      , Projection.projectionRetainedGenerations = Map.map fromIntegral retained
      , Projection.projectionCurrentOriginals = mempty
      , Projection.projectionEntry = Schema.SymbolIdentity "main" (fromString modul) "value" (fromString entry) Nothing
      , Projection.projectionAuxiliaryRoots =
          [ Schema.SymbolIdentity "main" (fromString modul) "value" (fromString aux) Nothing
          | aux <- auxEntries ]
      , Projection.projectionFormattingAuthority = Nothing
      , Projection.projectionTimeAuthority = Nothing
      , projectionJsonAuthority = Nothing
      , Projection.projectionTextUnit = Nothing
      }

-- | A deferred typed-site failure is raised exactly when projection reaches it.
assertSiteRejection :: String -> String -> Either Projection.ProjectionError Schema.WireProgram -> IO ()
assertSiteRejection label needle outcome = case outcome of
  Left (Projection.RejectedTypedSite message)
    | needle `isInfixOf` show message -> pure ()
  other -> ioError (userError (label ++ ": expected a typed-site rejection containing "
    ++ show needle ++ ", got " ++ either show (const "a projected program") other))

assertProjects :: String -> Either Projection.ProjectionError Schema.WireProgram -> IO ()
assertProjects label outcome = case outcome of
  Right _ -> pure ()
  Left failure -> ioError (userError (label ++ ": projection failed: " ++ show failure))

verifyJsonDependencyAuthority :: FilePath -> IO ()
verifyJsonDependencyAuthority dir = do
  let fixture = "test-prepared-stg/JsonAuthorityContract.hs"
      shadowRoot = dir </> "json-shadow"
      shadowDirectory = shadowRoot </> "Tidepool" </> "Aeson"
  trusted <- runPipelineSelected PreparedStg fixture ["test-prepared-stg", "lib"]
  trustedAuthority <- resolveJsonAuthority (prHscEnv (pprPipelineResult trusted))
  assert (trustedAuthority /= Nothing) "shipped JSON dependency graph lacks authority"
  trustedOwner <- maybe (ioError (userError "installed JSON owners did not resolve")) pure
    trustedAuthority
  verifyJsonLayoutDemand trustedOwner trusted
  verifyNominalJsonConstructorDemand dir
  source <- readFile "lib/Tidepool/Aeson/Scientific.hs"
  let shadow = Text.replace "coefficient (Scientific c _) = c"
        "coefficient (Scientific c _) = c + 1" (Text.pack source)
  assert (shadow /= Text.pack source) "Scientific shadow fixture did not change semantics"
  createDirectoryIfMissing True shadowDirectory
  writeFile (shadowDirectory </> "Scientific.hs") (Text.unpack shadow)
  substituted <- runPipelineSelected PreparedStg fixture
    [shadowRoot, "test-prepared-stg", "lib"]
  substitutedAuthority <- resolveJsonAuthority (prHscEnv (pprPipelineResult substituted))
  assert (substitutedAuthority == Nothing)
    "JSON authority admitted a same-shaped Scientific dependency with changed semantics"
  let eitherRoot = dir </> "json-either-shadow"
      eitherDirectory = eitherRoot </> "GHC" </> "Internal" </> "Data"
  createDirectoryIfMissing True eitherDirectory
  writeFile (eitherDirectory </> "Either.hs") (unlines
    [ "{-# LANGUAGE NoImplicitPrelude #-}"
    , "module GHC.Internal.Data.Either (Either(..)) where"
    , "data Either a b = Left a | Right a"
    ])
  writeFile (eitherRoot </> "Prelude.hs") (unlines
    [ "{-# LANGUAGE PackageImports #-}"
    , "module Prelude (module Base, Either(..)) where"
    , "import \"base\" Prelude as Base hiding (Either(..))"
    , "import GHC.Internal.Data.Either (Either(..))"
    ])
  let eitherFixture = eitherRoot </> "JsonEitherAuthorityContract.hs"
  writeFile eitherFixture (unlines
    [ "module JsonEitherAuthorityContract where"
    , "import Data.Text (Text)"
    , "import Tidepool.Aeson.Value (Value, eitherDecodeValue)"
    , "result :: Text -> Either Text Value"
    , "result = eitherDecodeValue"
    ])
  shadowedEither <- runPipelineSelected PreparedStg eitherFixture
    [eitherRoot, "test-prepared-stg", "lib"]
  shadowedEitherAuthority <- resolveJsonAuthority
    (prHscEnv (pprPipelineResult shadowedEither))
  authority <- maybe (ioError (userError "installed JSON owners did not resolve")) pure
    shadowedEitherAuthority
  eitherContext <- jsonProjectionContext authority shadowedEither
    "JsonEitherAuthorityContract" "result"
  case projectEntryWithJsonAuthority eitherContext shadowedEither "result" of
    Left (Projection.UnsupportedPreparedShape detail)
      | "InvalidJsonType" `Text.isInfixOf` detail -> pure ()
    outcome -> ioError (userError
      ("JSON authority admitted a home-shadowed Either dependency: "
        ++ either show (const "projected") outcome))

verifyJsonLayoutDemand :: JsonAuthority -> PreparedPipelineResult -> IO ()
verifyJsonLayoutDemand authority result = do
  context <- jsonProjectionContext authority result "JsonAuthorityContract" "unrelated"
  let plain = context { Projection.projectionJsonAuthority = Nothing }
      project = Projection.projectPreparedTarget
      require :: Show failure => String -> Either failure value -> IO value
      require label = either (ioError . userError . ((label ++ ": ") ++) . show) pure
  unrelated <- require "unrelated target" (project context (pprModules result))
  plainUnrelated <- require "unrelated target without authority" (project plain (pprModules result))
  assert (unrelated == plainUnrelated)
    "unused JSON authority changed an unrelated executable's declarations or bytes"
  prepared <- case filter ((== "JsonAuthorityContract") . moduleNameString . moduleName . pmModule)
      (pprModules result) of
    [value] -> pure value
    _ -> ioError (userError "JSON demand fixture lacks its prepared owner")
  -- Reuse the same compiled owner, but project an unrelated Int entry. Its
  -- executable closure cannot accidentally supply the host representation.
  let bindingType occurrence = case
        [idType binder | (binding, _) <- pmBindings prepared
          , binder <- Projection.topBinders binding
          , occNameString (nameOccName (idName binder)) == occurrence] of
        [ty] -> pure ty
        _ -> ioError (userError ("missing unique host representation type: " ++ occurrence))
      pipeline = pprPipelineResult result
  jsonType <- bindingType "hostValue"
  textType <- bindingType "result"
  authorities <- resolveHostBindingAuthorities [jsonType, textType] (prHscEnv pipeline)
    (prCanonicalInterfaceAdmissions pipeline)
  jsonRepresentation <- maybe (ioError (userError "Value lacks a complete host representation")) pure
    (hostBindingRepresentationForType authorities jsonType)
  textRepresentation <- maybe (ioError (userError "Text lacks a complete host representation")) pure
    (hostBindingRepresentationForType authorities textType)
  assert (hostBindingRepresentationAuthority jsonRepresentation == JsonValueAuthority
      && hostBindingRepresentationAuthority textRepresentation == TextAuthority)
    "host representation lost its authenticated root authority"
  selected <- require "unrelated prepared selection"
    (Projection.prepareProjection plain (pprModules result))
  (jsonCarrier, jsonConstructors) <- require "bound JSON representation"
    (Projection.projectSelectedWithHostBindings [jsonRepresentation] selected)
  case Schema.programJsonLayout jsonCarrier of
    Just layout -> assert (length (toList layout) == 18 && all
        (\(Schema.ConstructorId index) -> fromIntegral index < length (Schema.programConstructors jsonCarrier))
        (toList layout)) "bound JSON representation lacks a complete constructor table"
    Nothing -> ioError (userError "bound JSON representation omitted its authenticated layout")
  assert (not (any isJsonOperation (Schema.programOperations jsonCarrier)))
    "bound JSON representation unexpectedly depends on executable JSON operations"
  (textCarrier, textConstructors) <- require "bound Text representation"
    (Projection.projectSelectedWithHostBindings [textRepresentation] selected)
  let retains representation admitted = all
        (\expected -> any ((== dataConName expected) . dataConName) admitted)
        (hostBindingRepresentationConstructors representation)
  assert (retains jsonRepresentation jsonConstructors
      && retains textRepresentation textConstructors
      && Schema.programJsonLayout textCarrier == Nothing)
    "bound host constructors were omitted or Text acquired JSON roles"
  putStrLn $ "bound_host_representation_projections json_layout_roles="
    ++ show (length . toList <$> Schema.programJsonLayout jsonCarrier)
    ++ " json_constructor_rows=" ++ show (length jsonConstructors)
    ++ " text_constructor_rows=" ++ show (length textConstructors)
  let selectionFor owner occurrences = Just (Set.fromList [ fromIntegral ordinal
        | (ordinal, (binding, _)) <- zip [0 :: Int ..] (pmBindings owner)
        , any ((`elem` occurrences) . occNameString . nameOccName . idName)
            (Projection.topBinders binding) ])
      selection = selectionFor prepared ["unrelated"]
  groups <- require "unrelated original group"
    (Projection.projectPreparedModuleGroupsSelected context prepared selection)
  plainGroups <- require "unrelated original group without authority"
    (Projection.projectPreparedModuleGroupsSelected plain prepared selection)
  assert (not (null groups) && groups == plainGroups)
    "unused JSON authority changed an original group's declarations or ordinals"
  hostGroups <- require "host original groups"
    (Projection.projectPreparedModuleGroupsSelected context prepared
      (selectionFor prepared ["hostValue"]))
  assert (length hostGroups == 1 && all
      ((/= Nothing) . Schema.projectedJsonLayout . Schema.projectedBody) hostGroups)
    "JSON host original group omitted authenticated roles"
  valueModule <- case filter ((== "Tidepool.Aeson.Value") . moduleNameString . moduleName . pmModule)
      (pprModules result) of
    [value] -> pure value
    _ -> ioError (userError "JSON demand fixture lacks its authenticated Value module")
  jsonGroups <- require "intrinsic original groups"
    (Projection.projectPreparedModuleGroupsSelected context valueModule
      (selectionFor valueModule ["encodeValue", "eitherDecodeValue"]))
  assert (length jsonGroups == 2 && all
      ((/= Nothing) . Schema.projectedJsonLayout . Schema.projectedBody) jsonGroups)
    "JSON intrinsic original groups omitted authenticated roles"
  mapM_ (\entry -> do
      program <- require entry (projectEntryWithJsonAuthority context result entry)
      assert (Schema.programJsonLayout program /= Nothing)
        (entry ++ ": omitted required JSON layout"))
    ["result", "encodeOnly", "decodeOnly", "hostValue", "polymorphicValue"]
  mapM_ (\entry -> do
      program <- require entry (projectEntryWithJsonAuthority context result entry)
      assert (any isJsonOperation (Schema.programOperations program))
        (entry ++ ": intrinsic regression fixture lacks JSON operations")) ["encodeOnly", "decodeOnly"]
  host <- require "host carrier" (projectEntryWithJsonAuthority context result "hostValue")
  assert (not (any isJsonOperation (Schema.programOperations host)))
    "host carrier regression fixture unexpectedly uses JSON operations"
  let nestedContext = context { Projection.projectionAuxiliaryRoots =
        [Schema.SymbolIdentity "main" "JsonAuthorityContract" "value" "nestedValue" Nothing] }
  nested <- require "nested JSON evidence" (project nestedContext (pprModules result))
  assert (Schema.programJsonLayout nested /= Nothing)
    "nested JSON type evidence omitted authenticated roles"
  assert (not (IntMap.null (Schema.typeGraphNodes (Schema.programTypes nested)))
      && not (any isJsonOperation (Schema.programOperations nested)))
    "nested type regression fixture unexpectedly requires JSON operations"
 where
  isJsonOperation declaration = case Schema.operationIdentity declaration of
    Schema.JsonDecodeIdentity{} -> True
    Schema.JsonEncodeIdentity -> True
    _ -> False

-- Separately loaded nominal evidence may use a different GHC Unique. The
-- roles must survive while the canonical physical declarations still agree.
verifyNominalJsonConstructorDemand :: FilePath -> IO ()
verifyNominalJsonConstructorDemand dir = do
  createDirectoryIfMissing True (dir </> "Tidepool" </> "Effects")
  writeRequestSiteFixture dir
  writeFile (dir </> "Tidepool" </> "Actor.hs") (unlines
    [ "{-# LANGUAGE DataKinds, ExplicitForAll #-}"
    , "module Tidepool.Actor where"
    , "import Tidepool.Internal.RequestSite (RequestSite)"
    , "{-# OPAQUE receive #-}"
    , "receive :: forall answer. String -> Maybe answer"
    , "receive _ = Nothing"
    , "{-# OPAQUE receiveSited #-}"
    , "receiveSited :: forall answer. RequestSite '[] answer -> String -> Maybe answer"
    , "receiveSited _ _ = Nothing"
    ])
  let source = dir </> "JsonNominalAnswer.hs"
  writeFile source (unlines
    [ "{-# LANGUAGE TypeApplications #-}"
    , "module JsonNominalAnswer where"
    , "import Tidepool.Aeson.Value (Value)"
    , "import Tidepool.Actor"
    , "answer :: Maybe Value"
    , "answer = receive @Value \"json\""
    ])
  result <- runPipelineSelected PreparedStg source [dir, "lib"]
  authority <- resolveJsonAuthority (prHscEnv (pprPipelineResult result))
    >>= maybe (ioError (userError "nominal JSON fixture lacks authority")) pure
  let valueTyCons = [ tc | prepared <- pprModules result
        , Schema.TypeDeclaration tc _ _ _ <- IntMap.elems (TypePolicy.tgNodes (pmTypeGraph prepared))
        , occNameString (nameOccName (TC.tyConName tc)) == "Value" ]
  valueTyCon <- case valueTyCons of
    tc : _ -> pure tc
    [] -> ioError (userError "nominal JSON fixture lacks Value type evidence")
  context <- jsonProjectionContext authority result "JsonNominalAnswer" "answer"
  let newUnique = mkUnique 'z' 54322
      otherTyCon = valueTyCon
        { TC.tyConUnique = newUnique
        , TC.tyConName = setNameUnique (TC.tyConName valueTyCon) newUnique
        , TC.tyConNullaryTy = TyConApp otherTyCon [] }
      clone constructor = DC.mkDataCon (dataConName constructor) False
        (dataConName constructor) (DC.dataConSrcBangs constructor)
        [] [] [] (DC.dataConConcreteTyVars constructor) [] [] []
        (DC.dataConOrigArgTys constructor) (mkTyConApp otherTyCon []) NoPromInfo
        otherTyCon (DC.dataConTag constructor) [] (DC.dataConWorkId constructor)
        (case DC.dataConBoxer constructor of
          Nothing -> DC.NoDataConRep
          Just boxer -> DC.DCR (DC.dataConWrapId constructor) boxer
            (DC.dataConRepArgTys constructor) (DC.dataConRepStrictness constructor)
            (DC.dataConImplBangs constructor))
      replace prepared = prepared { preparedTypeGraph = (pmTypeGraph prepared)
        { Schema.typeGraphNodes = IntMap.map (\node -> case node of
            Schema.TypeDeclaration tc flags form restriction | tc == valueTyCon ->
              Schema.TypeDeclaration otherTyCon flags form restriction
            Schema.TypeConstructorTemplate con | DC.dataConTyCon con == valueTyCon ->
              Schema.TypeConstructorTemplate (clone con)
            _ -> node) (TypePolicy.tgNodes (pmTypeGraph prepared)) } }
      project modules = either (ioError . userError . show) pure
        (Projection.projectPreparedTarget context modules)
  assert (otherTyCon /= valueTyCon) "nominal JSON fixture did not change GHC identity"
  original <- project (pprModules result)
  separate <- project (map replace (pprModules result))
  assert (Schema.programJsonLayout original /= Nothing && original == separate)
    "equal nominal JSON constructor evidence lost canonical roles or declarations"

-- An O0 bytecode interface exposes a private helper that the prepared O2 body
-- removes. Importers must receive the prepared owner's interface, including on
-- a warm request where that owner comes from the compiler memo.
verifyPreparedPrivateImports :: IO ()
verifyPreparedPrivateImports = do
  let source = "test-prepared-stg/PreparedPrivateClient.hs"
      includes = ["test-prepared-stg"]
      check label result = case projectEntry result "PreparedPrivateClient" "result" Map.empty of
        Left failure -> ioError (userError (label ++ ": " ++ show failure))
        Right program -> assert
          (all ((/= "main") . Schema.symbolUnit . Schema.globalIdentity) (Schema.programGlobals program))
          (label ++ ": unresolved home implementation " ++ show (Schema.programGlobals program))
  direct <- runPipelineSelected PreparedStg source includes
  check "private TH dependency, direct" direct
  withResidentPipelineSelected includes $ \compile -> do
    cold <- compile PreparedStg mempty GeneralCompile Nothing source [] Nothing
    check "private TH dependency, cold" cold
    warm <- compile PreparedStg mempty GeneralCompile Nothing source [] Nothing
    check "private TH dependency, warm" warm
    assert (preparedShape cold == preparedShape warm)
      "warm private TH dependency changed prepared module shape"

verifyProjectionInterning :: FilePath -> IO ()
verifyProjectionInterning dir = do
  vertical <- runPipelineSelected PreparedStg "test-prepared-stg/M3Vertical.hs"
    ["test-prepared-stg"]
  let context = Projection.ProjectionContext
        { Projection.projectionProfile = "ghc-9.12-prepared-stg"
        , Projection.projectionToolchain = "ghc-9.12.2"
        , Projection.projectionTarget =
            Schema.TargetDescriptor Schema.X86_64 Schema.LittleEndian 64 64 "sysv64" []
        , Projection.projectionRetainedGenerations = mempty
        , Projection.projectionCurrentOriginals = mempty
        , Projection.projectionEntry = Schema.SymbolIdentity "main" "M3Vertical" "value" "result" Nothing
        , Projection.projectionAuxiliaryRoots = []
        , Projection.projectionFormattingAuthority = Nothing
        , Projection.projectionTimeAuthority = Nothing
        , Projection.projectionJsonAuthority = Nothing
        , Projection.projectionTextUnit = Nothing
        }
      project = Projection.projectPrepared context (pprModules vertical)
  program <- either (ioError . userError . show) pure project
  assert (length (Schema.programSignatures program) > 1
    && length (Schema.programGlobals program) > 1
    && length (Schema.programConstructors program) > 1
    && length (Schema.programOperations program) > 1)
    "interning fixture does not exercise distinct table insertions"
  repeated <- either (ioError . userError . show) pure project
  assert (encodeWireProgram program == encodeWireProgram repeated)
    "projection encoding was not deterministic"
  putStrLn ("interned table sizes (signatures/globals/constructors/operations): "
    ++ show (length (Schema.programSignatures program), length (Schema.programGlobals program),
      length (Schema.programConstructors program), length (Schema.programOperations program)))
  strictPlain <- writePlainConstructorEvidenceFixture dir
  effects <- getEnv "TIDEPOOL_TEST_EFFECTS_DIR"
  strictPlainResult <- runPipelineSelected PreparedStg strictPlain
    [dir, "lib", effects]
  verifyRepeatedConstructorEvidence strictPlainResult
  putStrLn "projection interning: deterministic bytes and 16 constructor-conflict paths passed"

writePlainConstructorEvidenceFixture :: FilePath -> IO FilePath
writePlainConstructorEvidenceFixture dir = do
  let strictOwned = dir </> "StrictOwned.hs"
      strictPlain = dir </> "StrictPlainMetadata.hs"
  writeFile strictOwned (unlines
    [ "{-# OPTIONS_GHC -O0 #-}"
    , "module StrictOwned where"
    , "data Automatic = Automatic !Int"
    , "data NoUnpack = NoUnpack {-# NOUNPACK #-} !Int"
    , "data ExplicitUnpack = ExplicitUnpack {-# UNPACK #-} !Int"
    ])
  writeFile strictPlain (strictMetadataSource "StrictPlainMetadata" False)
  pure strictPlain

strictMetadataSource :: String -> Bool -> String
strictMetadataSource modul quoted = unlines $
  [ "{-# LANGUAGE DataKinds, TypeApplications #-}" ] ++ quasiquoteLanguage quoted ++
  [ "module " ++ modul ++ " where"
  , "import StrictOwned"
  , "import Tidepool.Actor"
  , "import Control.Monad.Freer (Eff)"
  , "import Tidepool.Effects.Core (ActorLocal)"
  ] ++ quasiquoteBindings quoted ++
  [ "automatic :: Eff '[ActorLocal Maybe] Automatic"
  , "automatic = receive @Automatic @Maybe (\\_ -> error \"metadata handler is not invoked\")"
  , "noUnpack :: Eff '[ActorLocal Maybe] NoUnpack"
  , "noUnpack = receive @NoUnpack @Maybe (\\_ -> error \"metadata handler is not invoked\")"
  , "explicitUnpack :: Eff '[ActorLocal Maybe] ExplicitUnpack"
  , "explicitUnpack = receive @ExplicitUnpack @Maybe (\\_ -> error \"metadata handler is not invoked\")"
  ]

quasiquoteLanguage :: Bool -> [String]
quasiquoteLanguage False = []
quasiquoteLanguage True = ["{-# LANGUAGE QuasiQuotes #-}"]

quasiquoteBindings :: Bool -> [String]
quasiquoteBindings False = []
quasiquoteBindings True =
  [ "import Tidepool.Aeson.Value (Value)"
  , "import Tidepool.QQ (j)"
  , "quotedValue :: Value"
  , "quotedValue = [j|42|]"
  ]

-- | TH's bytecode provisioning must not change a constructor declared by an
-- unchanged home module.  The graph checks run after metadata preparation and
-- before projection or execution; the executable checks then compare the
-- declarations a shared prepared machine receives from ordinary and quoted
-- source.
verifyConstructorRepresentations :: FilePath -> IO ()
verifyConstructorRepresentations dir = do
  strictPlain <- writePlainConstructorEvidenceFixture dir
  let strictQuoted = dir </> "StrictQuotedMetadata.hs"
      scientificPlain = dir </> "ScientificPlain.hs"
      scientificQuoted = dir </> "ScientificQuoted.hs"
      scientificMetadataPlain = dir </> "ScientificMetadataPlain.hs"
      scientificMetadataQuoted = dir </> "ScientificMetadataQuoted.hs"
  writeFile strictQuoted (strictMetadataSource "StrictQuotedMetadata" True)
  writeFile scientificPlain (unlines
    [ "module ScientificPlain where"
    , "import Tidepool.Aeson.Scientific"
    , "result :: Scientific"
    , "result = scientific 42 0"
    ])
  writeFile scientificQuoted (unlines
    [ "{-# LANGUAGE QuasiQuotes #-}"
    , "module ScientificQuoted where"
    , "import Tidepool.Aeson.Value (Value)"
    , "import Tidepool.QQ (j)"
    , "result :: Value"
    , "result = [j|42|]"
    ])
  writeFile scientificMetadataPlain (scientificMetadataSource "ScientificMetadataPlain" False)
  writeFile scientificMetadataQuoted (scientificMetadataSource "ScientificMetadataQuoted" True)
  effects <- getEnv "TIDEPOOL_TEST_EFFECTS_DIR"
  let includes = [dir, "lib", effects]
  strictPlainResult <- runPipelineSelected PreparedStg strictPlain includes
  strictQuotedResult <- runPipelineSelected PreparedStg strictQuoted includes
  scientificPlainResult <- runPipelineSelected PreparedStg scientificPlain includes
  scientificQuotedResult <- runPipelineSelected PreparedStg scientificQuoted includes
  scientificMetadataPlainResult <- runPipelineSelected PreparedStg scientificMetadataPlain includes
  scientificMetadataQuotedResult <- runPipelineSelected PreparedStg scientificMetadataQuoted includes
  assertMetadataReps "Automatic" strictPlainResult strictQuotedResult
    ["IntRep"]
  assertMetadataReps "NoUnpack" strictPlainResult strictQuotedResult
    ["BoxedRep (Just Lifted)"]
  assertMetadataReps "ExplicitUnpack" strictPlainResult strictQuotedResult
    ["IntRep"]
  assertMetadataReps "Scientific" scientificMetadataPlainResult scientificMetadataQuotedResult
    ["BoxedRep (Just Lifted)", "IntRep"]
  verifyRepeatedConstructorEvidence strictPlainResult
  plainProgram <- project scientificPlainResult "ScientificPlain"
  quotedProgram <- project scientificQuotedResult "ScientificQuoted"
  plainDecl <- namedDeclaration "Scientific" plainProgram
  quotedDecl <- namedDeclaration "Scientific" quotedProgram
  assert (plainDecl == quotedDecl)
    ("Scientific constructor declaration changed across ordinary and quasiquoted source: "
      ++ show (Schema.constructorFieldReps plainDecl) ++ " /= "
      ++ show (Schema.constructorFieldReps quotedDecl))
  assert (Schema.constructorFieldReps plainDecl == [Schema.LiftedRefRep, Schema.IntRep 64])
    ("Scientific did not retain the canonical physical representation: "
      ++ show (Schema.constructorFieldReps plainDecl))
 where
  scientificMetadataSource modul quoted = unlines $
    [ "{-# LANGUAGE DataKinds, TypeApplications #-}" ] ++ quasiquoteLanguage quoted ++
    [ "module " ++ modul ++ " where"
    , "import Tidepool.Aeson.Value (Value)"
    , "import Tidepool.Actor"
    , "import Control.Monad.Freer (Eff)"
    , "import Tidepool.Effects.Core (ActorLocal)"
    ] ++ quasiquoteBindings quoted ++
    [ "result :: Eff '[ActorLocal Maybe] Value"
    , "result = receive @Value @Maybe (\\_ -> error \"metadata handler is not invoked\")"
    ]
  assertMetadataReps occurrence plain quoted expected = do
    let plainReps = typeGraphReps occurrence plain
        quotedReps = typeGraphReps occurrence quoted
    assert (plainReps == [expected])
      ("ordinary metadata did not retain " ++ occurrence ++ " representation: "
        ++ show plainReps)
    assert (quotedReps == plainReps)
      ("TH/QQ metadata changed " ++ occurrence ++ " representation: "
        ++ show plainReps ++ " /= " ++ show quotedReps)
  typeGraphReps occurrence result = nub
    [ map show (concatMap (maybe [] id . typePrimRep_maybe . scaledThing)
        (dataConRepArgTys constructor))
    | prepared <- pprModules result
    , Schema.TypeConstructorTemplate constructor <- IntMap.elems (TypePolicy.tgNodes (pmTypeGraph prepared))
    , dataConOccurrence constructor == occurrence
    ]
  scaledThing (Scaled _ ty) = ty
  dataConOccurrence constructor = occNameString (nameOccName (dataConName constructor))
  project result modul = case projectEntry result modul "result" mempty of
    Left failure -> ioError (userError ("Scientific projection failed: " ++ show failure))
    Right program -> pure program
  namedDeclaration occurrence program = case filter (hasOccurrence occurrence)
      (Schema.programConstructors program) of
    [declaration] -> pure declaration
    declarations -> ioError (userError ("expected one " ++ occurrence
      ++ " declaration, got " ++ show declarations))
  hasOccurrence occurrence declaration =
    Schema.symbolModule (Schema.constructorIdentity declaration) == "Tidepool.Aeson.Scientific"
      && Schema.symbolOccurrence (Schema.constructorIdentity declaration) == fromString occurrence

-- Independently construct conflicting GHC evidence at the shared projection
-- boundary. Equal GHC uniques are not proof of equal physical declarations.
verifyRepeatedConstructorEvidence :: PreparedPipelineResult -> IO ()
verifyRepeatedConstructorEvidence result = do
  let originals = [ con | prepared <- pprModules result
        , Schema.TypeConstructorTemplate con <- IntMap.elems (TypePolicy.tgNodes (pmTypeGraph prepared)), occNameString (nameOccName (dataConName con)) == "NoUnpack" ]
  original <- case originals of
    con : _ -> pure con
    [] -> ioError (userError "constructor collision fixture lacks NoUnpack evidence")
  let clone name sourceFields runtimeFields = case DC.dataConBoxer original of
        Nothing -> error "constructor collision fixture lacks a wrapper boxer"
        Just boxer -> DC.mkDataCon name False name (DC.dataConSrcBangs original)
          [] [] [] (DC.dataConConcreteTyVars original) [] [] [] sourceFields
          (DC.dataConOrigResTy original) NoPromInfo (DC.dataConTyCon original)
          (DC.dataConTag original) [] (DC.dataConWorkId original)
          (DC.DCR (DC.dataConWrapId original) boxer runtimeFields
            (DC.dataConRepStrictness original) (DC.dataConImplBangs original))
      name = dataConName original
      otherName = setNameUnique name (mkUnique 'z' 54321)
      fields = DC.dataConOrigArgTys original
      runtimeFields = DC.dataConRepArgTys original
      conflictingFields = [Scaled multiplicity wordPrimTy | Scaled multiplicity _ <- fields]
      replace pair prepared = prepared
        { preparedTypeGraph = Schema.TypeGraph
            (IntMap.unions [Schema.typeGraphNodes graph | graph <- copies])
            (IntMap.unions [Schema.typeGraphEdges graph | graph <- copies])
        , preparedPreparedSites = concat
            [ [site { psWireNode = shifted offset (psWireNode site)
                    , psInputNodes = map (shifted offset) (psInputNodes site)
                    , psSite = (psSite site)
                        { ysSite = ysSite (psSite site) + fromIntegral ordinal } }
                | site <- pmPreparedSites prepared]
            | (ordinal, offset) <- zip [0 :: Int ..] offsets ] }
       where
        graph = pmTypeGraph prepared
        stride = IntMap.size (TypePolicy.tgNodes graph)
        offsets = [ordinal * stride | ordinal <- [0 .. length pair - 1]]
        shifted offset (Schema.TypeNodeId raw) = Schema.TypeNodeId (raw + fromIntegral offset)
        copies = [Schema.TypeGraph
          (IntMap.fromAscList [(index + offset, case node of
            Schema.TypeConstructorTemplate con | con == original -> Schema.TypeConstructorTemplate replacement
            _ -> node) | (index, node) <- IntMap.toAscList (TypePolicy.tgNodes graph)])
          (IntMap.fromAscList [(index + offset, [(role, shifted offset child) | (role, child) <- children])
            | (index, children) <- IntMap.toAscList (TypePolicy.tgEdges graph)])
          | (offset, replacement) <- zip offsets pair]
      project pair = projectModulesWithAux
        (map (replace pair) (pprModules result))
        "StrictPlainMetadata" "noUnpack" [] mempty
      target = case filter ((== "StrictPlainMetadata")
            . moduleNameString . moduleName . pmModule) (pprModules result) of
        [prepared] -> prepared
        prepared -> error ("constructor collision fixture expected one target module, got "
          ++ show (map (moduleNameString . moduleName . pmModule) prepared))
      replaceNominal occurrence replacement prepared = prepared { preparedTypeGraph = (pmTypeGraph prepared)
        { Schema.typeGraphNodes = IntMap.map (\node -> case node of
            Schema.TypeConstructorTemplate con
              | occNameString (nameOccName (dataConName con)) == occurrence ->
                  Schema.TypeConstructorTemplate replacement
            _ -> node) (TypePolicy.tgNodes (pmTypeGraph prepared)) } }
      siteOwnedBy occurrence site = occurrence `isInfixOf`
        Text.unpack (ysOrigin (psSite site))
      noUnpackRoot = case filter (siteOwnedBy "noUnpack") (pmPreparedSites target) of
        [site] -> psWireNode site
        sites -> error ("constructor collision fixture expected one noUnpack site, got "
          ++ show (length sites))
      graphModule bindingName replacement =
        let selected = filter (siteOwnedBy bindingName) (pmPreparedSites target)
            rooted = [ if bindingName == "automatic"
                then site { psWireNode = noUnpackRoot }
                else site
              | site <- selected ]
            reach = Projection.admitReachFacts (map (varUnique . psOwner) selected)
              [Projection.preparedModuleReachFacts context target] Projection.emptyPreparedReachability
            ownsSelected (binding, _) = any
              ((`elementOfUniqSet` Projection.reachedUniques reach) . varUnique)
              (Projection.topBinders binding)
        in (replaceNominal "NoUnpack" replacement target)
          { preparedBindings = filter ownsSelected (pmBindings target)
          , preparedPreparedSites = rooted
          , preparedSiteRejections = []
          }
      graphRoots = [varUnique (psOwner site) | site <- pmPreparedSites target
        , any (`siteOwnedBy` site) ["noUnpack", "automatic"]]
      sharedReach = Projection.admitReachFacts graphRoots
        (map (Projection.preparedModuleReachFacts context) (pprModules result))
        Projection.emptyPreparedReachability
      -- Select the actual cross-module body closure, preserving the compiler's
      -- intrinsic helper omission independently of each graph's type evidence.
      otherModules = [filterPreparedBindings (\(binding, _) ->
          any ((`elementOfUniqSet` Projection.reachedUniques sharedReach) . varUnique)
            (Projection.topBinders binding)
          && not (any (preparedRejectsIntrinsic prepared) (Projection.topBinders binding))) prepared
        | prepared <- pprModules result
        , moduleNameString (moduleName (pmModule prepared)) /= "StrictPlainMetadata"]
      bindingKey (binding, _) = sort (map (getKey . varUnique) (Projection.topBinders binding))
      bindingKeys = Set.fromList . map bindingKey . pmBindings
      -- The graphs retain independent type/site evidence but share the same
      -- genuine STG groups. Give each shared group one defining body.
      graphInputs first second =
        let firstGraph = graphModule "noUnpack" first
            secondGraph = graphModule "automatic" second
            defined = bindingKeys firstGraph
        in [firstGraph, filterPreparedBindings
            ((`Set.notMember` defined) . bindingKey) secondGraph]
      projectAcrossGraphs first second = Projection.projectPrepared context
        (otherModules ++ graphInputs first second)
      context = Projection.ProjectionContext
        { Projection.projectionProfile = "ghc-9.12-prepared-stg"
        , Projection.projectionToolchain = "ghc-9.12.2"
        , Projection.projectionTarget =
            Schema.TargetDescriptor Schema.X86_64 Schema.LittleEndian 64 64 "sysv64" []
        , Projection.projectionRetainedGenerations = mempty
        , Projection.projectionCurrentOriginals = mempty
        , Projection.projectionEntry = Schema.SymbolIdentity "main"
            (fromString "StrictPlainMetadata") "value" (fromString "noUnpack") Nothing
        , Projection.projectionAuxiliaryRoots = []
        , Projection.projectionFormattingAuthority = Nothing
        , Projection.projectionTimeAuthority = Nothing
        , Projection.projectionJsonAuthority = Nothing
        , Projection.projectionTextUnit = Nothing
        }
      rejects pair = case project pair of
        Left (Projection.InvalidPreparedIdentity detail) ->
          assert ("conflicting physical declarations" `Text.isInfixOf` detail)
            ("unexpected constructor rejection: " ++ show detail)
        outcome -> ioError (userError ("conflicting constructor evidence was not rejected: "
          ++ either show (const "accepted") outcome))
      rejectsAcrossGraphs first second = case projectAcrossGraphs first second of
        Left (Projection.InvalidPreparedIdentity detail) ->
          assert ("conflicting physical declarations" `Text.isInfixOf` detail)
            ("unexpected cross-graph constructor rejection: " ++ show detail)
        outcome -> ioError (userError
          ("conflicting constructor evidence in separate graphs was not rejected: "
            ++ either show (const "accepted") outcome))
  assert (not (Set.null (bindingKeys (graphModule "noUnpack" original)
    `Set.intersection` bindingKeys (graphModule "automatic" original))))
    "independent constructor graphs do not exercise a shared original binding group"
  _ <- either (ioError . userError . show) pure (projectAcrossGraphs original original)
  canonical <- either (ioError . userError . show) pure (project [original, original])
  repeated <- either (ioError . userError . show) pure
    (project [original, clone otherName fields runtimeFields])
  assert (encodeWireProgram repeated == encodeWireProgram canonical)
    "equivalent constructor provenance changed IDs, table order or encoded bytes"
  mapM_ (\changed -> do
    rejects [original, changed]
    rejects [changed, original]
    rejectsAcrossGraphs original changed
    rejectsAcrossGraphs changed original)
    [ clone name conflictingFields conflictingFields
    , clone otherName conflictingFields conflictingFields
    , clone name fields conflictingFields
    , clone otherName fields conflictingFields
    ]
assertWireSite :: String -> Schema.SiteDelivery -> String
  -> Either Projection.ProjectionError Schema.WireProgram -> IO ()
assertWireSite label delivery family outcome = case outcome of
  Left failure -> ioError (userError (label ++ ": projection failed: " ++ show failure))
  Right program -> case Schema.programSites program of
    [site]
      | Schema.siteDelivery site == delivery
      , Just identity <- nominalSiteIdentity (Schema.programTypes program) (Schema.siteWire site)
      , Schema.symbolOccurrence identity == fromString family -> pure ()
    sites -> ioError (userError (label ++ ": unexpected site evidence " ++ show sites
      ++ " in " ++ show (Schema.programTypes program)))

nominalSiteIdentity :: Schema.TypeGraph -> Schema.TypeNodeId -> Maybe Schema.SymbolIdentity
nominalSiteIdentity graph root = do
  body <- child root Schema.TypeBody
  declaration <- child body Schema.TypeHead
  case node declaration of
    Just (Schema.TypeDeclaration identity _ _ _) -> Just identity
    _ -> Nothing
 where
  node (Schema.TypeNodeId raw) = IntMap.lookup (fromIntegral raw) (Schema.typeGraphNodes graph)
  child (Schema.TypeNodeId raw) role = case
      [target | (actual, target) <- IntMap.findWithDefault [] (fromIntegral raw) (Schema.typeGraphEdges graph), actual == role] of
    [target] -> Just target
    _ -> Nothing

expectAuthoredParseFailure :: String -> FilePath -> IO result -> IO ()
expectAuthoredParseFailure label target action = do
  outcome <- try (action >> pure ()) :: IO (Either SomeException ())
  case outcome of
    Left failure -> case (fromException failure :: Maybe SourceError) of
      Nothing -> throwIO failure
      Just sourceError -> do
        targetPath <- normalise <$> makeAbsolute target
        diagnosticPaths <- mapM makeAbsolute
          [ path
          | Diag { dFile = Just (path, _, _, _, _), dSeverity = DiagError }
              <- diagsFromSourceError sourceError
          ]
        unless (targetPath `elem` map normalise diagnosticPaths) $
          ioError (userError
            (label ++ ": expected an authored parse diagnostic at " ++ targetPath
              ++ ", got " ++ show (diagsFromSourceError sourceError)))
    Right _ -> ioError (userError (label ++ ": expected authored source parse failure"))

main :: IO ()
main = runTests tests

tests :: TestTree
tests = testGroup "prepared-stg-pipeline"
  [ testCase "original constructor reply identities and type evidence" $
      withCaseScratch "constructor-reply-identity" runTypeEvidenceChecks
  , testCase "recovered typed sites and intrinsic refusal" $
      withCaseScratch "recovered-typed-sites" verifyRecoveredTypedPreparation
  , testCase "prepared products reuse unchanged site dependencies across cells" $
      withCaseScratch "prepared-cache-lifetime" verifyTypedPreparationCacheLifetime
  , testCase "site dependency histories refuse changed and newly available authority" $
      withCaseScratch "prepared-site-dependencies" verifySiteDependencyHistories
  , testCase "original prepared pipeline and compiler authority" fullMain
  , testCase "projection interning and constructor conflicts" $
      withCaseScratch "projection-interning" verifyProjectionInterning
  , testCase "module product interface roundtrip" $
      withCaseScratch "module-product-roundtrip" verifyModuleProductInterfaceRoundtrip
  , testCase "original product catalogue" $
      withCaseScratch "original-product-catalogue" verifyOriginalProductCatalogue
  ]

-- Every control starts from actual GHC finalized output. The recovered body
-- cache receives its defining location/type context from that same output.
writeTypedPreparationFixture :: FilePath -> IO FilePath
writeTypedPreparationFixture dir = do
  writeRequestSiteFixture dir
  readFile "test-prepared-stg/site-fixtures/TypedPreparationActor.hs" >>=
    writeFile (dir </> "Tidepool" </> "Actor.hs")
  mapM_ (\name -> readFile ("test-prepared-stg/site-fixtures" </> name ++ ".hs")
      >>= writeFile (dir </> name ++ ".hs"))
    ["TypedPreparationOwner", "TypedPreparationPlain", "TypedPreparationEntry"]
  pure (dir </> "TypedPreparationEntry.hs")

preparedFixtureOwner :: String -> PreparedPipelineResult -> PreparedModule
preparedFixtureOwner name result = case filter
    ((== name) . moduleNameString . moduleName . pmModule) (pprModules result) of
  [owner] -> owner
  _ -> error ("missing prepared fixture owner " ++ name)

finalizedFixtureOwner :: String -> PreparedPipelineResult -> FinalizedModule
finalizedFixtureOwner name result = case Map.lookup (mkModuleName name) (pprFinalizedModules result) of
  Just owner -> owner
  Nothing -> error ("missing finalized fixture owner " ++ name)

recoveredFixtureInput :: String -> PreparedPipelineResult -> RecoveredModuleInput
recoveredFixtureInput name result = case
    [ms_location summary | ModuleNode _ summary <- mgModSummaries'
      (hsc_mod_graph (prHscEnv (pprPipelineResult result)))
    , ms_mod_name summary == mkModuleName name] of
  [location] -> RecoveredModuleInput (cg_module guts) location (cg_tycons guts) (cg_binds guts)
    (Map.fromList [(varName binder,binder) | binder <- bindersOfBinds (cg_binds guts)])
  _ -> error ("missing defining fixture location " ++ name)
  where guts = finalizedTidyGuts (finalizedFixtureOwner name result)

verifyRecoveredTypedPreparation :: FilePath -> IO ()
verifyRecoveredTypedPreparation dir = do
  source <- writeTypedPreparationFixture dir
  result <- runPipelineSelected PreparedStg source [dir]
  let env = prHscEnv (pprPipelineResult result)
      owner = preparedFixtureOwner "TypedPreparationOwner" result
  assert (length (pmYieldSites owner) == 1 && null (pmSiteRejections owner))
    "fresh valid control lacks its issued typed site"
  recovered <- prepareRecoveredModule env (recoveredFixtureInput "TypedPreparationOwner" result)
  assert (pmCoverage recovered == ExactBodySubset && preparedUsesSiteAuthority recovered
      && length (pmYieldSites recovered) == 1 && null (pmSiteRejections recovered)
      && not (null (pmPreparedSites recovered)))
    "recovered typed Core bypassed site/carrier elaboration"
  let replaced = recovered : filter
        ((/= pmModule recovered) . pmModule) (pprModules result)
  assertProjects "recovered unrelated entry" (projectModulesWithAux replaced "TypedPreparationOwner" "unrelated" [] mempty)
  assertProjects "recovered issued receive" (projectModulesWithAux replaced "TypedPreparationOwner" "answer" [] mempty)
  case projectEntry result "Tidepool.Actor" "receive" mempty of
    Left Projection.UnelaboratedCompilerIntrinsic{} -> pure ()
    other -> fail ("emitted unsited intrinsic definition escaped: "
      ++ either show (const "projected executable") other)
  assertProjects "unused intrinsic definition is omitted"
    (projectEntry result "TypedPreparationOwner" "unrelated" mempty)
  assertProjects "same occurrence in another original owner is ordinary code"
    (projectEntry result "TypedPreparationPlain" "receive" mempty)
  let context = Projection.ProjectionContext
        { Projection.projectionProfile = "ghc-9.12-prepared-stg"
        , Projection.projectionToolchain = "ghc-9.12.2"
        , Projection.projectionTarget =
            Schema.TargetDescriptor Schema.X86_64 Schema.LittleEndian 64 64 "sysv64" []
        , Projection.projectionRetainedGenerations = mempty
        , Projection.projectionCurrentOriginals = mempty
        , Projection.projectionEntry = Schema.SymbolIdentity "main" "TypedPreparationOwner"
            "value" "unrelated" Nothing
        , Projection.projectionAuxiliaryRoots = []
        , Projection.projectionFormattingAuthority = Nothing
        , Projection.projectionTimeAuthority = Nothing
        , Projection.projectionJsonAuthority = Nothing
        , Projection.projectionTextUnit = Nothing
        }
      interfaces = Map.map (hm_iface . finalizedHomeModInfo) (pprFinalizedModules result)
      products = Projection.projectOriginalHomeModuleProducts env interfaces context mempty
        (pprModules result)
      intrinsicRefused omission = case Projection.omittedOriginalReason omission of
        Projection.ProjectionFailed Projection.UnelaboratedCompilerIntrinsic{} -> True
        _ -> False
  assert (any (any intrinsicRefused . snd) (Projection.preparedModuleProductOmissions products))
    "original product emission did not omit its unelaborated intrinsic definition"

verifyTypedPreparationCacheLifetime :: FilePath -> IO ()
verifyTypedPreparationCacheLifetime dir = do
  source <- writeTypedPreparationFixture dir
  withResidentPipelineSelected [dir] $ \compile -> do
    cold <- compile PreparedStg mempty GeneralCompile Nothing source [] Nothing
    warm <- compile PreparedStg mempty GeneralCompile Nothing source [] Nothing
    let plain = preparedFixtureOwner "TypedPreparationPlain"
        sited = preparedFixtureOwner "TypedPreparationOwner"
        finalized = finalizedTidyGuts . finalizedFixtureOwner "TypedPreparationOwner"
        identity :: a -> IO (StableName a)
        identity value = evaluate value >>= makeStableName
    assert (not (preparedUsesSiteAuthority (plain cold)) && preparedUsesSiteAuthority (sited cold))
      "owning typed census misclassified cache controls"
    coldPlain <- identity (plain cold)
    warmPlain <- identity (plain warm)
    coldSite <- identity (sited cold)
    warmSite <- identity (sited warm)
    coldCore <- identity (finalized cold)
    warmCore <- identity (finalized warm)
    assert (coldPlain == warmPlain) "intrinsic-free prepared memo was rebuilt"
    assert (coldSite == warmSite) "unchanged site authority rebuilt its prepared memo"
    assert (coldCore == warmCore) "unchanged authority replayed stable finalization"
    nextCell <- writeTypedPreparationNextCell dir
    next <- compile PreparedStg mempty GeneralCompile Nothing nextCell [] Nothing
    nextSite <- identity (sited next)
    assert (coldSite == nextSite) "an unrelated new cell invalidated original site authority"
    assert (pmYieldSites (sited cold) == pmYieldSites (sited next)
        && preparedSiteDependenciesEquivalent (sited cold) (sited next))
      "new-cell reuse changed compiler-issued site identities or consumed facts"
    owners <- newOwnerInterfaceCache
    bodies <- newPreparedBodyCache
    let env = prHscEnv (pprPipelineResult cold)
        input name = recoveredFixtureInput name cold
    mapM_ (\name -> do
        let recovered = input name
            iface = hm_iface (finalizedHomeModInfo (finalizedFixtureOwner name cold))
        context <- issueOwnerInterfaceContext (recoveredModule recovered) (mi_iface_hash (mi_final_exts iface))
          (recoveredLocation recovered) (recoveredTyCons recovered) (recoveredEntries recovered)
        cacheOwnerInterface owners (recoveredModule recovered) context)
      ["TypedPreparationPlain", "TypedPreparationOwner"]
    request <- newPreparedBodyPreparer env owners bodies
    nextRequest <- newPreparedBodyPreparer env owners bodies
    let prepare current name = let recovered = input name in
          current (recoveredModule recovered) (recoveredBindings recovered)
            >>= either (fail . show) pure
    stable <- prepare request "TypedPreparationPlain" >>= identity
    stableAgain <- prepare nextRequest "TypedPreparationPlain" >>= identity
    scoped <- prepare request "TypedPreparationOwner" >>= identity
    scopedAgain <- prepare request "TypedPreparationOwner" >>= identity
    nextScoped <- prepare nextRequest "TypedPreparationOwner" >>= identity
    assert (stable == stableAgain) "intrinsic-free recovered body lost stable reuse"
    assert (scoped == scopedAgain && scoped == nextScoped)
      "recovered original with unchanged site authority lost cross-request reuse"

writeTypedPreparationNextCell :: FilePath -> IO FilePath
writeTypedPreparationNextCell dir = do
  let path = dir </> "TypedPreparationNextCell.hs"
  readFile "test-prepared-stg/site-fixtures/TypedPreparationNextCell.hs" >>= writeFile path
  pure path

-- The resolver sees real defining interfaces in each history. A prior product
-- remains held while later compilation publishes a changed authority context.
-- No interface fingerprint, declaring Id, or dependency observation is forged.
verifySiteDependencyHistories :: FilePath -> IO ()
verifySiteDependencyHistories dir = do
  let presentDir = dir </> "present"
      absentDir = dir </> "absent"
  createDirectory presentDir
  createDirectory absentDir
  source <- writeTypedPreparationFixture presentDir
  withResidentPipelineSelected [presentDir] $ \compile -> do
    first <- compile PreparedStg mempty GeneralCompile Nothing source [] Nothing
    let original = preparedFixtureOwner "TypedPreparationOwner" first
    assert (length (pmYieldSites original) == 1 && null (pmSiteRejections original))
      "dependency history lacks its genuine typed-site positive control"
    assertMatch "original resolved authority" first original True
    nextCell <- writeTypedPreparationNextCell presentDir
    -- This interface is visible but contributes no authority query to answer.
    appendFile (presentDir </> "TypedPreparationPlain.hs")
      "\nunrelatedVersion :: Int\nunrelatedVersion = 23\n"
    unrelated <- compile PreparedStg mempty GeneralCompile Nothing nextCell [] Nothing
    assertMatch "unqueried sibling module growth" unrelated original True
    let actorPath = presentDir </> "Tidepool" </> "Actor.hs"
    originalActor <- BS.readFile actorPath
    appendFile actorPath "\nsiblingVersion :: Int\nsiblingVersion = 29\n"
    changedSibling <- compile PreparedStg mempty GeneralCompile Nothing nextCell [] Nothing
    assertMatch "selected sibling original interface replacement" changedSibling original False
    assert (not (preparedSiteDependenciesEquivalent original
        (preparedFixtureOwner "TypedPreparationOwner" changedSibling)))
      "changed authority collapsed distinct retained alternatives"
    BS.writeFile actorPath originalActor
    restored <- compile PreparedStg mempty GeneralCompile Nothing source [] Nothing
    assertMatch "restored original sibling authority" restored original True
    let carrierPath = presentDir </> "Tidepool" </> "Internal" </> "RequestSite.hs"
    carrier <- TextIO.readFile carrierPath
    let replacedCarrier = Text.replace "(RequestSite, requestSiteIdentity)"
          "(RequestSite, requestSiteIdentity, replacementVersion)" carrier
    assert (replacedCarrier /= carrier) "carrier replacement control did not alter the authentic export"
    writeFile carrierPath (Text.unpack replacedCarrier
      ++ "\nreplacementVersion :: Int\nreplacementVersion = 31\n")
    changedCarrier <- compile PreparedStg mempty GeneralCompile Nothing source [] Nothing
    assertMatch "globally queried nominal owner replacement" changedCarrier original False
    assert (length (pmYieldSites (preparedFixtureOwner "TypedPreparationOwner" changedCarrier)) == 1)
      "changed nominal owner was refused instead of being freshly prepared"
    -- Keep authentic old EPS declarations beside the authentic new home
    -- interface, but withhold that home's typed details. This is missing load
    -- evidence, not evidence of absence and not authority for the old object.
    fork <- forkExactContext (prHscEnv (pprPipelineResult changedCarrier))
    let originalHome = finalizedHomeModInfo (finalizedFixtureOwner "Tidepool.Internal.RequestSite" first)
        currentHome = finalizedHomeModInfo (finalizedFixtureOwner "Tidepool.Internal.RequestSite" changedCarrier)
        carrierOwner = mi_module (hm_iface originalHome)
        ExternalUnitCache externalCell = ue_eps (hsc_unit_env fork)
        partialHome = currentHome
          { hm_details = (hm_details currentHome) { md_types = emptyTypeEnv } }
    modifyIORef' externalCell $ \external -> external
      { eps_PTE = plusTypeEnv (md_types (hm_details originalHome)) (eps_PTE external)
      , eps_PIT = extendModuleEnv (eps_PIT external) carrierOwner (hm_iface originalHome) }
    let partial = hscUpdateHPT (\homes -> addToHpt homes (moduleName carrierOwner) partialHome) fork
    unavailable <- prepareRecoveredModule partial (recoveredFixtureInput "TypedPreparationOwner" changedCarrier)
    environment <- resolvePreparedSiteEnvironment partial
    assert (null (pmYieldSites unavailable)
        && any (isInfixOf "missing RequestSite type authority" . srMessage) (pmSiteRejections unavailable)
        && not (preparedSiteDependenciesMatch environment Map.empty unavailable)
        && not (preparedSiteDependenciesEquivalent unavailable unavailable))
      "partial current home details authorized an older imported declaration"
  absentSource <- writeTypedPreparationFixture absentDir
  readFile "test-prepared-stg/site-fixtures/TypedPreparationActorWithoutSibling.hs" >>=
    writeFile (absentDir </> "Tidepool" </> "Actor.hs")
  withResidentPipelineSelected [absentDir] $ \compile -> do
    missing <- compile PreparedStg mempty GeneralCompile Nothing absentSource [] Nothing
    let rejected = preparedFixtureOwner "TypedPreparationOwner" missing
    assert (null (pmYieldSites rejected)
        && any (isInfixOf "missing generated site-aware sibling" . srMessage) (pmSiteRejections rejected))
      "absent sibling control did not retain the typed preparation rejection"
    assertMatch "proven absent original sibling" missing rejected True
    readFile "test-prepared-stg/site-fixtures/TypedPreparationActor.hs" >>=
      writeFile (absentDir </> "Tidepool" </> "Actor.hs")
    available <- compile PreparedStg mempty GeneralCompile Nothing absentSource [] Nothing
    assertMatch "absent sibling becomes present" available rejected False
    let issued = preparedFixtureOwner "TypedPreparationOwner" available
    assert (length (pmYieldSites issued) == 1 && null (pmSiteRejections issued))
      "newly available original sibling did not receive fresh typed preparation"
  where
    assertMatch label result prepared expected = do
      environment <- resolvePreparedSiteEnvironment (prHscEnv (pprPipelineResult result))
      let siblings = Map.unions
            [resolvePreparedSiblings (cg_binds (finalizedTidyGuts finalized))
            | finalized <- Map.elems (pprFinalizedModules result)]
      assert (preparedSiteDependenciesMatch environment siblings prepared == expected)
        (label ++ ": wrong preparation dependency match")

withCaseScratch :: String -> (FilePath -> IO a) -> IO a
withCaseScratch name action = bracket temporary removePathForcibly action
  where
    temporary = do
      tmp <- getTemporaryDirectory
      (work, handle) <- openTempFile tmp ("tidepool-" ++ name)
      hClose handle
      removeFile work
      createDirectory work
      pure work

fullMain :: IO ()
fullMain = do
  let expectedSites =
        [ ("serve", DeliverLiveReentry, SelectedAnswer)
        , ("request", DeliverExitCellFill, SelectedAnswer)
        , ("requestWithProgress", DeliverExitCellFill, SelectedAnswer)
        ]
      actualSites =
        [ (vsName spec, vsDelivery spec, vsWireSource spec)
        | spec <- sitedVerbs
        , vsName spec `elem` map (\(name, _, _) -> name) expectedSites
        ]
  assert (sort actualSites == sort expectedSites)
    ("prepared delivery strategy drift: " ++ show actualSites)
  assert (all ((/= "requestWith") . vsName) sitedVerbs)
    "obsolete requestWith remains in the typed-site registry"
  let normalized = stripNospecSpine
        (Var nospecId, [Type boolTy, Var nospecId, Type boolTy])
  assert (case normalized of
    (Var _, [Type ty]) -> eqType ty boolTy
    _ -> False) "nospec normalization discarded a remaining type argument"
  withCaseScratch "prepared-stg-pipeline" $ \dir -> do
      verifyCompilerReuse dir
      verifyPreparedScope dir
      let dep = dir </> "Dep.hs"
          effectsDir = dir </> "Tidepool" </> "Effects"
          effects = dir </> "Tidepool" </> "Actor.hs"
          unfoldDir = dir </> "Tidepool" </> "Actors"
          unfold = unfoldDir </> "Unfold.hs"
          replyDir = dir </> "Tidepool" </> "Agent" </> "Reply"
          replyInternal = replyDir </> "Internal.hs"
          currentRequestTarget = dir </> "CurrentRequestSiteExpr.hs"
          dormantRequestTarget = dir </> "DormantRequestExpr.hs"
          hiddenRequestTool = dir </> "HiddenRequestTool.hs"
          activeRequestTarget = dir </> "ActiveRequestExpr.hs"
          laterRequestTarget = dir </> "LaterRequestSiteExpr.hs"
          changedRequestTarget = dir </> "ChangedRequestSiteExpr.hs"
          siteTarget = dir </> "SiteExpr.hs"
          partialChildTarget = dir </> "PartialChildExpr.hs"
          polySiteTarget = dir </> "PolySiteExpr.hs"
          target = dir </> "Expr.hs"
          validTarget = unlines
            [ "module Expr where"
            , "import Dep"
            , "data Packed = Packed !Int !Char"
            , "embeddedNul :: String"
            , "embeddedNul = \"left\\0right\""
            , "result :: Int"
            , "result = case Packed 41 'x' of Packed n _ -> helper n + length embeddedNul"
            ]
      writeRequestSiteFixture dir
      createDirectoryIfMissing True effectsDir
      createDirectoryIfMissing True unfoldDir
      createDirectoryIfMissing True replyDir
      let originalDep = unlines
            [ "module Dep where"
            , "helper :: Int -> Int"
            , "helper x = x + 1"
            ]
      writeFile dep originalDep
      writeFile effects (unlines
        [ "{-# LANGUAGE ExplicitForAll #-}"
        , "{-# LANGUAGE TypeApplications #-}"
        , "{-# LANGUAGE DataKinds #-}"
        , "module Tidepool.Actor where"
        , "import Tidepool.Internal.RequestSite (RequestSite)"
        , "{-# OPAQUE receive #-}"
        , "receive :: forall a. String -> Maybe a"
        , "receive _ = Nothing"
        , "{-# OPAQUE receiveSited #-}"
        , "receiveSited :: forall a. RequestSite '[] a -> String -> Maybe a"
        , "receiveSited _ _ = Nothing"
        ])
      writeFile unfold (unlines
        [ "{-# LANGUAGE ExplicitForAll #-}"
        , "{-# LANGUAGE DataKinds #-}"
        , "module Tidepool.Actors.Unfold where"
        , "import Tidepool.Internal.RequestSite (RequestSite)"
        , "import Data.Kind (Type)"
        , "import Tidepool.Agent.Reply.Internal (ResponseResult)"
        , "keepResponseResultAuthority :: Maybe (ResponseResult Bool)"
        , "keepResponseResultAuthority = Nothing"
        , "{-# OPAQUE child #-}"
        , "child :: forall result (child :: Type) input (parent :: Type). input -> Maybe result"
        , "child _ = Nothing"
        , "{-# OPAQUE childSited #-}"
        , "childSited :: forall result (child :: Type) input (parent :: Type). RequestSite '[input] result -> input -> Maybe result"
        , "childSited _ _ = Nothing"
        ])
      writeFile replyInternal (unlines
        [ "{-# LANGUAGE ExplicitForAll #-}"
        , "{-# LANGUAGE KindSignatures #-}"
        , "{-# LANGUAGE DataKinds #-}"
        , "module Tidepool.Agent.Reply.Internal where"
        , "import Tidepool.Internal.RequestSite (RequestSite)"
        , "import Data.Kind (Type)"
        , "data ResponseResult a = ResponseResult a"
        , "data RequestScope (input :: Type) (result :: Type) = RequestScope"
        , "data Eff (effs :: Type) (value :: Type) = Eff value"
        , "{-# OPAQUE currentRequest #-}"
        , "currentRequest :: forall input result effs. Eff effs (RequestScope input result)"
        , "currentRequest = currentRequestSited (error \"unavailable carrier\")"
        , "{-# OPAQUE currentRequestSited #-}"
        , "currentRequestSited :: forall input result effs. RequestSite '[input, result, ResponseResult result] (RequestScope input result) -> Eff effs (RequestScope input result)"
        , "currentRequestSited _ = Eff RequestScope"
        ])
      writeFile currentRequestTarget (unlines
        [ "{-# LANGUAGE TypeApplications #-}"
        , "module CurrentRequestSiteExpr where"
        , "import Tidepool.Agent.Reply.Internal"
        , "typedRequest :: Eff Bool (RequestScope Int Char)"
        , "typedRequest = currentRequest @Int @Char @Bool"
        ])
      writeFile laterRequestTarget (unlines
        [ "{-# LANGUAGE TypeApplications #-}"
        , "module LaterRequestSiteExpr where"
        , "import Tidepool.Agent.Reply.Internal"
        , "typedRequest :: Eff Bool (RequestScope Int Char)"
        , "typedRequest = currentRequest @Int @Char @Bool"
        ])
      writeFile hiddenRequestTool (unlines
        [ "{-# LANGUAGE TypeApplications #-}"
        , "module HiddenRequestTool (tool) where"
        , "import Tidepool.Agent.Reply.Internal"
        , "tool :: Eff Bool (RequestScope Int Char)"
        , "tool = currentRequest @Int @Char @Bool"
        ])
      writeFile dormantRequestTarget (unlines
        [ "module DormantRequestExpr where"
        , "import HiddenRequestTool ()"
        , "unrelated :: Int"
        , "unrelated = 1"
        ])
      writeFile activeRequestTarget (unlines
        [ "module ActiveRequestExpr where"
        , "import HiddenRequestTool (tool)"
        , "active = tool"
        ])
      writeFile changedRequestTarget (unlines
        [ "{-# LANGUAGE TypeApplications #-}"
        , "module ChangedRequestSiteExpr where"
        , "import Tidepool.Agent.Reply.Internal"
        , "typedRequest :: Eff Bool (RequestScope Int Char)"
        , "typedRequest = currentRequest @Int @Char @Bool"
        , "marker :: Int"
        , "marker = generationMarker"
        ])
      verifyPreparedPrivateImports
      withCaseScratch "constructor-representations" verifyConstructorRepresentations
      verifyJsonDependencyAuthority dir
      writeFile target validTarget
      writeFile siteTarget (unlines
        [ "{-# LANGUAGE TypeApplications #-}"
        , "module SiteExpr where"
        , "import Tidepool.Actor"
        , "typedSite :: Maybe Bool"
        , "typedSite = receive @Bool \"prepared\""
        ])
      writeFile polySiteTarget (unlines
        [ "{-# LANGUAGE RankNTypes #-}"
        , "{-# LANGUAGE ScopedTypeVariables #-}"
        , "{-# LANGUAGE TypeApplications #-}"
        , "module PolySiteExpr where"
        , "import Tidepool.Actor"
        , "polyHelper :: forall a. String -> Maybe a"
        -- Fully applied to a computed argument, so the simplifier cannot
        -- eta-reduce it to a partial, unelaborated verb occurrence.
        , "polyHelper label = receive @a (label ++ \"!\")"
        , "{-# OPAQUE polyHelper #-}"
        , "polyNested :: forall a. Bool -> String -> Maybe a"
        , "polyNested flag label = if flag then receive @a label else Nothing"
        , "{-# NOINLINE polyNested #-}"
        , "unrelated :: Int"
        , "unrelated = 42"
        -- A verb passed as a value has no site of its own.
        , "applyVerb :: (forall a. String -> Maybe a) -> Maybe Bool"
        , "applyVerb verb = verb \"first-class\""
        , "{-# NOINLINE applyVerb #-}"
        , "firstClass :: Maybe Bool"
        , "firstClass = applyVerb receive"
        , "usesHelper :: Maybe Bool"
        , "usesHelper = polyHelper @Bool \"helper\""
        , "usesNested :: Maybe Bool"
        , "usesNested = polyNested @Bool True \"nested\""
        ])
      writeFile partialChildTarget (unlines
        [ "{-# LANGUAGE TypeApplications #-}"
        , "module PartialChildExpr where"
        , "import Tidepool.Actors.Unfold"
        , "partialChild :: Char -> Maybe Bool"
        , "partialChild = child @Bool @Int @Char @String"
        ])

      direct <- runPipelineSelected PreparedStg target [dir]
      assertProductionFacts direct
      let directShape = preparedShape direct
      assert (map fst directShape == ["Dep", "Expr"])
        ("direct prepared modules lost context: " ++ show directShape)
      siteDirect <- runPipelineSelected PreparedStg siteTarget [dir]
      currentRequestDirect <- runPipelineSelected PreparedStg currentRequestTarget [dir]
      laterRequestDirect <- runPipelineSelected PreparedStg laterRequestTarget [dir]
      let currentRequestEvidence = preparedEvidence "CurrentRequestSiteExpr" currentRequestDirect
          laterRequestEvidence = preparedEvidence "LaterRequestSiteExpr" laterRequestDirect
      assert (case currentRequestEvidence of
                (inventory, [site]) -> "currentRequestSited" `isInfixOf` inventory
                  && ysOrigin site == "CurrentRequestSiteExpr.typedRequest"
                  && map stType (ysInputs site) == ["Int", "Char", "ResponseResult Char"]
                _ -> False)
        ("currentRequest must resolve its exact generated sibling: "
          ++ show currentRequestEvidence
          ++ "; rejections=" ++ show [srMessage rejection
            | prepared <- pprModules currentRequestDirect
            , rejection <- pmSiteRejections prepared])
      let (siteInventory, directSites) = preparedEvidence "SiteExpr" siteDirect
      assert (case filter ((== "SiteExpr.typedSite") . ysOrigin) directSites of
                [site] -> "receiveSited" `isInfixOf` siteInventory
                  && show (ysSite site) `isInfixOf` siteInventory
                _ -> False)
        "typed site was not elaborated before preparation"
      partialChildDirect <- runPipelineSelected PreparedStg partialChildTarget [dir]
      assertWireSite "direct value-partial child site" Schema.ExitCellFill "ResponseResult"
        (projectEntry partialChildDirect "PartialChildExpr" "partialChild" mempty)
      assert (case snd (preparedEvidence "PartialChildExpr" partialChildDirect) of
        [site] -> stType (ysAnswer site) == "Bool" && map stType (ysInputs site) == ["Char"]
        _ -> False) "partial child must retain its concrete result and input types"
      -- Open result types reject only when executable projection reaches them.
      polyDirect <- runPipelineSelected PreparedStg polySiteTarget [dir]
      assertProjects "unrelated top beside a polymorphic site helper"
        (projectEntry polyDirect "PolySiteExpr" "unrelated" mempty)
      assertSiteRejection "reachable polymorphic site helper" "result type is unresolved"
        (projectEntry polyDirect "PolySiteExpr" "usesHelper" mempty)
      assertSiteRejection "reachable polymorphic site under a prepared case" "result type is unresolved"
        (projectEntry polyDirect "PolySiteExpr" "usesNested" mempty)
      assertProjects "retained polymorphic site helper is linked, not executed"
        (projectEntry polyDirect "PolySiteExpr" "usesHelper"
          (Map.singleton (Schema.SymbolIdentity "main" "PolySiteExpr" "value" "polyHelper" Nothing) 1))
      assertSiteRejection "verb used as a first-class value" "result type is unresolved"
        (projectEntry polyDirect "PolySiteExpr" "firstClass" mempty)
      withResidentPipelineSelected [dir] $ \compileSite -> do
        requestCold <- compileSite PreparedStg mempty GeneralCompile Nothing currentRequestTarget [] Nothing
        assert (preparedEvidence "CurrentRequestSiteExpr" requestCold == currentRequestEvidence)
          "resident cold currentRequest site differs from direct"
        dormantRequest <- compileSite PreparedStg mempty GeneralCompile Nothing dormantRequestTarget [] Nothing
        assert (all ((/= "HiddenRequestTool") . fst) (preparedShape dormantRequest))
          "dormant import unexpectedly reached the hidden request tool"
        activeRequest <- compileSite PreparedStg mempty GeneralCompile Nothing activeRequestTarget [] Nothing
        let hiddenModules = [prepared | prepared <- pprModules activeRequest
              , moduleNameString (moduleName (pmModule prepared)) == "HiddenRequestTool"]
        assert (case hiddenModules of
                  [hidden] -> null (pmSiteRejections hidden)
                  _ -> False)
          ("cached nonreachable tool retained a missing-sibling site rejection: "
            ++ show [srMessage rejection | hidden <- hiddenModules
              , rejection <- pmSiteRejections hidden])
        siteCold <- compileSite PreparedStg mempty GeneralCompile Nothing siteTarget [] Nothing
        siteWarm <- compileSite PreparedStg mempty GeneralCompile Nothing siteTarget [] Nothing
        assert (preparedEvidence "SiteExpr" siteCold == (siteInventory, directSites))
          "resident cold typed-site evidence differs from direct"
        assert (preparedEvidence "SiteExpr" siteWarm == (siteInventory, directSites))
          "resident warm typed-site evidence differs from direct"
        partialChildResident <- compileSite PreparedStg mempty GeneralCompile Nothing partialChildTarget [] Nothing
        assertProjects "resident value-partial child site"
          (projectEntry partialChildResident "PartialChildExpr" "partialChild" mempty)
        siteRecovered <- compileSite PreparedStg mempty GeneralCompile Nothing siteTarget [] Nothing
        assert (preparedEvidence "SiteExpr" siteRecovered == (siteInventory, directSites))
          "resident compiler did not recover after a partial recognized site"
        requestLater <- compileSite PreparedStg mempty GeneralCompile Nothing laterRequestTarget [] Nothing
        assert (preparedEvidence "LaterRequestSiteExpr" requestLater == laterRequestEvidence)
          "fresh consumer lost currentRequest sibling after another target"
        appendFile replyInternal (unlines
          [ "{-# OPAQUE generationMarker #-}"
          , "generationMarker :: Int"
          , "generationMarker = 1"
          ])
        requestChanged <- compileSite PreparedStg mempty GeneralCompile Nothing changedRequestTarget [] Nothing
        let (changedInventory, changedSites) = preparedEvidence "ChangedRequestSiteExpr" requestChanged
        assert ("generationMarker" `isInfixOf` changedInventory
          && "currentRequestSited" `isInfixOf` changedInventory
          && length changedSites == 1)
          ("changed sibling provider source was not recompiled for a fresh consumer: "
            ++ show ("generationMarker" `isInfixOf` changedInventory,
              "currentRequestSited" `isInfixOf` changedInventory,
              length changedSites))

      -- Use the production forall/dictionary shape with an open effect row.
      readFile "test-prepared-stg/site-fixtures/Core.hs" >>= writeFile effects
      let constrainedTarget = dir </> "ConstrainedSites.hs"
      readFile "test-prepared-stg/site-fixtures/ConstrainedSites.hs" >>= writeFile constrainedTarget
      constrained <- runPipelineSelected PreparedStg constrainedTarget [dir]
      let (_, constrainedSites) = preparedEvidence "ConstrainedSites" constrained
      assert (length constrainedSites == 5 && all ((/= 0) . ysSite) constrainedSites)
        ("partial, open-row, higher-order and nospec-wrapped constrained sites must each elaborate once: "
          ++ show constrainedSites)
      mapM_ (\entry -> assertProjects ("constrained " ++ entry)
        (projectEntry constrained "ConstrainedSites" entry mempty))
        ["partial", "wrapped", "higherOrder", "openTail", "openEta", "unrelated"]
      assertSiteRejection "unresolved constrained partial site" "result type is unresolved"
        (projectEntry constrained "ConstrainedSites" "unresolved" mempty)
      writeFile dep (unlines
        [ "{-# LANGUAGE CPP #-}"
        , "module Dep where"
        , "#include \"Value.h\""
        , "helper :: Int -> Int"
        , "helper x = x + VALUE"
        ])
      let header = dir </> "Value.h"
      writeFile header "#define VALUE 1\n"
      withResidentPipelineSelected [dir] $ \compileCpp -> do
        before <- compileCpp PreparedStg mempty GeneralCompile Nothing target [] Nothing
        warm <- compileCpp PreparedStg mempty GeneralCompile Nothing target [] Nothing
        assert (preparedEvidence "Dep" before == preparedEvidence "Dep" warm)
          "unchanged CPP module changed its prepared result"
        writeFile header "#define VALUE 2\n"
        after <- compileCpp PreparedStg mempty GeneralCompile Nothing target [] Nothing
        assert (preparedEvidence "Dep" before /= preparedEvidence "Dep" after)
          "resident memo reused stale Core after an included header changed"
      writeFile dep originalDep

      writeFile target "module Expr where\nresult =\n"
      expectAuthoredParseFailure "direct prepared" target $
        runPipelineSelected PreparedStg target [dir]
      writeFile target validTarget

      withResidentPipelineSelected [dir] $ \compile -> do
        cold <- compile PreparedStg mempty GeneralCompile Nothing target [] Nothing
        warm <- compile PreparedStg mempty GeneralCompile Nothing target [] Nothing
        assert (preparedShape cold == directShape)
          "resident cold prepared output differs from direct output"
        assert (preparedShape warm == directShape)
          ("resident warm prepared output did not retain module results: "
            ++ show (preparedShape warm) ++ " /= " ++ show directShape)
        assert (allPreparedEvidence cold == allPreparedEvidence direct)
          "resident cold prepared facts differ from direct output"
        assert (allPreparedEvidence warm == allPreparedEvidence direct)
          "resident warm prepared facts differ from direct output"

        writeFile target "module Expr where\nresult =\n"
        expectAuthoredParseFailure "resident prepared" target $
          compile PreparedStg mempty GeneralCompile Nothing target [] Nothing
        writeFile target validTarget
        recovered <- compile PreparedStg mempty GeneralCompile Nothing target [] Nothing
        assert (preparedShape recovered == directShape)
          "resident compiler did not recover after a request-local failure"

writeRequestSiteFixture :: FilePath -> IO ()
writeRequestSiteFixture dir = do
  createDirectoryIfMissing True (dir </> "Tidepool" </> "Internal")
  readFile "lib/Tidepool/Internal/RequestSite.hs" >>=
    writeFile (dir </> "Tidepool" </> "Internal" </> "RequestSite.hs")
