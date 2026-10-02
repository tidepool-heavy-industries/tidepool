module Main (main) where

import Control.Exception (SomeException, bracket, evaluate, try)
import Control.Monad (unless)
import Data.ByteString qualified as BS
import Data.List (isInfixOf, nub, sort)
import Data.String (fromString)
import Data.Set qualified as Set
import Data.Text qualified as Text
import GHC (moduleNameString)
import GHC.Builtin.Types (boolTy)
import GHC.Core (Expr(..), bindersOf, flattenBinds)
import GHC.Core.DataCon (dataConName, dataConRepArgTys)
import GHC.Core.DataCon qualified as DC
import GHC.Core.TyCon (PromDataConInfo(NoPromInfo))
import GHC.Core.TyCon qualified as TC
import GHC.Core.Type (mkTyConApp)
import GHC.Builtin.Types.Prim (wordPrimTy)
import GHC.Core.FVs (exprSomeFreeVarsList)
import GHC.Core.TyCo.Rep (Scaled(..), Type(TyConApp))
import GHC.Types.Id (idName)
import GHC.Types.Name (nameOccName, setNameUnique)
import GHC.Types.Unique (mkUnique)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.Id.Make (nospecId)
import GHC.Types.RepType (typePrimRep_maybe)
import GHC.Core.TyCo.Compare (eqType)
import Tidepool.SiteClassifier
  ( SiteFailure(..), classifySiteOccurrence, isNospecVar, stripNospecSpine )
import GHC.Unit.Types (moduleName)
import System.Directory
  ( createDirectoryIfMissing, getTemporaryDirectory, removePathForcibly )
import System.FilePath ((</>))
import System.Environment (getArgs)
import Tidepool.GhcPipeline
  ( PipelineSelection(..), PreparedPipelineResult(..), CompilePurpose(..)
  , PipelineResult(..), runPipelineSelected, withResidentPipelineSelected )
import Tidepool.PreparedStg (PreparedModule(..))
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
import ModuleProductRoundtripTest (verifyModuleProductInterfaceRoundtrip)
import TypeEvidenceChecks (runTypeEvidenceChecks)
import Tidepool.PreparedJson (JsonAuthority, resolveJsonAuthority)

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

projectEntryWithJsonAuthority :: JsonAuthority -> PreparedPipelineResult
  -> String -> String -> Either Projection.ProjectionError Schema.WireProgram
projectEntryWithJsonAuthority authority result modul entry =
  Projection.projectPreparedTarget (jsonProjectionContext authority modul entry) (pprModules result)

jsonProjectionContext :: JsonAuthority -> String -> String -> Projection.ProjectionContext
jsonProjectionContext authority modul entry = Projection.ProjectionContext
    { Projection.projectionProfile = "ghc-9.12-prepared-stg"
    , Projection.projectionToolchain = "ghc-9.12.2"
    , Projection.projectionTarget =
        Schema.TargetDescriptor Schema.X86_64 Schema.LittleEndian 64 64 "sysv64" []
    , Projection.projectionRetainedGenerations = mempty
    , Projection.projectionEntry = Schema.SymbolIdentity "main" (fromString modul)
        "value" (fromString entry) Nothing
    , Projection.projectionAuxiliaryRoots = []
    , Projection.projectionFormattingAuthority = Nothing
    , Projection.projectionTimeAuthority = Nothing
    , Projection.projectionJsonAuthority = Just authority
    , Projection.projectionTextUnit = Nothing
    }

-- | 'projectEntry' plus a set of auxiliary root occurrences in the same
-- module (mirroring the fixed entries beside a turn's resume entry): admitted
-- the same way as a turn's own auxiliary roots, so a
-- fixture can assert that an auxiliary root's own result type is interned
-- even when the selected entry never otherwise reaches it.
projectEntryWithAux :: PreparedPipelineResult -> String -> String -> [String]
  -> Map.Map Schema.SymbolIdentity Word -> Either Projection.ProjectionError Schema.WireProgram
projectEntryWithAux result modul entry auxEntries retained =
  Projection.projectPreparedTarget context (pprModules result)
  where
    context = Projection.ProjectionContext
      { Projection.projectionProfile = "ghc-9.12-prepared-stg"
      , Projection.projectionToolchain = "ghc-9.12.2"
      , Projection.projectionTarget =
          Schema.TargetDescriptor Schema.X86_64 Schema.LittleEndian 64 64 "sysv64" []
      , Projection.projectionRetainedGenerations = Map.map fromIntegral retained
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
  case projectEntryWithJsonAuthority authority shadowedEither
      "JsonEitherAuthorityContract" "result" of
    Left (Projection.UnsupportedPreparedShape detail)
      | "InvalidJsonType" `Text.isInfixOf` detail -> pure ()
    outcome -> ioError (userError
      ("JSON authority admitted a home-shadowed Either dependency: "
        ++ either show (const "projected") outcome))

verifyJsonLayoutDemand :: JsonAuthority -> PreparedPipelineResult -> IO ()
verifyJsonLayoutDemand authority result = do
  let context = jsonProjectionContext authority "JsonAuthorityContract" "unrelated"
      plain = context { Projection.projectionJsonAuthority = Nothing }
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
      program <- require entry (projectEntryWithJsonAuthority authority result "JsonAuthorityContract" entry)
      assert (Schema.programJsonLayout program /= Nothing)
        (entry ++ ": omitted required JSON layout"))
    ["result", "encodeOnly", "decodeOnly", "hostValue", "polymorphicValue"]
  mapM_ (\entry -> do
      program <- require entry (projectEntryWithJsonAuthority authority result "JsonAuthorityContract" entry)
      assert (any isJsonOperation (Schema.programOperations program))
        (entry ++ ": intrinsic regression fixture lacks JSON operations")) ["encodeOnly", "decodeOnly"]
  host <- require "host carrier" (projectEntryWithJsonAuthority authority result "JsonAuthorityContract" "hostValue")
  assert (not (any isJsonOperation (Schema.programOperations host)))
    "host carrier regression fixture unexpectedly uses JSON operations"
  let nestedContext = context { Projection.projectionAuxiliaryRoots =
        [Schema.SymbolIdentity "main" "JsonAuthorityContract" "value" "nestedValue" Nothing] }
  nested <- require "nested JSON evidence" (project nestedContext (pprModules result))
  assert (Schema.programJsonLayout nested /= Nothing)
    "nested JSON type evidence omitted authenticated roles"
  assert (not (null (Schema.programTypes nested))
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
  writeFile (dir </> "Tidepool" </> "Effects" </> "Core.hs") (unlines
    [ "module Tidepool.Effects.Core where"
    , "{-# OPAQUE runLLMTurn #-}"
    , "runLLMTurn :: forall answer. String -> Maybe answer"
    , "runLLMTurn _ = Nothing"
    , "{-# OPAQUE runLLMTurnSited #-}"
    , "runLLMTurnSited :: forall answer. Int -> String -> Maybe answer"
    , "runLLMTurnSited _ _ = Nothing"
    ])
  let source = dir </> "JsonNominalAnswer.hs"
  writeFile source (unlines
    [ "{-# LANGUAGE TypeApplications #-}"
    , "module JsonNominalAnswer where"
    , "import Tidepool.Aeson.Value (Value)"
    , "import Tidepool.Effects.Core"
    , "answer :: Maybe Value"
    , "answer = runLLMTurn @Value \"json\""
    ])
  result <- runPipelineSelected PreparedStg source [dir, "lib"]
  authority <- resolveJsonAuthority (prHscEnv (pprPipelineResult result))
    >>= maybe (ioError (userError "nominal JSON fixture lacks authority")) pure
  let valueTyCons = [ tc | prepared <- pprModules result
        , TypePolicy.DataG _ tc _ _ <- TypePolicy.tgNodes (pmTypeGraph prepared)
        , occNameString (nameOccName (TC.tyConName tc)) == "Value" ]
  valueTyCon <- case valueTyCons of
    tc : _ -> pure tc
    [] -> ioError (userError "nominal JSON fixture lacks Value type evidence")
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
      replace prepared = prepared { pmTypeGraph = TypePolicy.TypeGraph
        [ case node of
            TypePolicy.DataG ty tc args rows | tc == valueTyCon ->
              TypePolicy.DataG ty tc args [(clone con, children) | (con, children) <- rows]
            _ -> node
        | node <- TypePolicy.tgNodes (pmTypeGraph prepared) ] }
      context = jsonProjectionContext authority "JsonNominalAnswer" "answer"
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

verifyProjectionInterning :: FilePath -> FilePath -> IO ()
verifyProjectionInterning dir output = do
  vertical <- runPipelineSelected PreparedStg "test-prepared-stg/M3Vertical.hs"
    ["test-prepared-stg"]
  let context = Projection.ProjectionContext
        { Projection.projectionProfile = "ghc-9.12-prepared-stg"
        , Projection.projectionToolchain = "ghc-9.12.2"
        , Projection.projectionTarget =
            Schema.TargetDescriptor Schema.X86_64 Schema.LittleEndian 64 64 "sysv64" []
        , Projection.projectionRetainedGenerations = mempty
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
  BS.writeFile output (encodeWireProgram program)
  putStrLn ("interned table sizes (signatures/globals/constructors/operations): "
    ++ show (length (Schema.programSignatures program), length (Schema.programGlobals program),
      length (Schema.programConstructors program), length (Schema.programOperations program)))
  strictPlain <- writePlainConstructorEvidenceFixture dir
  strictPlainResult <- runPipelineSelected PreparedStg strictPlain [dir, "test/prepared-stg", "lib"]
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
  [ "{-# LANGUAGE TypeApplications #-}" ] ++ quasiquoteLanguage quoted ++
  [ "module " ++ modul ++ " where"
  , "import StrictOwned"
  , "import Tidepool.Effects.Core"
  ] ++ quasiquoteBindings quoted ++
  [ "automatic :: Maybe Automatic"
  , "automatic = runLLMTurn @Automatic \"automatic\""
  , "noUnpack :: Maybe NoUnpack"
  , "noUnpack = runLLMTurn @NoUnpack \"nounpack\""
  , "explicitUnpack :: Maybe ExplicitUnpack"
  , "explicitUnpack = runLLMTurn @ExplicitUnpack \"unpack\""
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
  strictPlainResult <- runPipelineSelected PreparedStg strictPlain [dir, "test/prepared-stg", "lib"]
  strictQuotedResult <- runPipelineSelected PreparedStg strictQuoted [dir, "lib"]
  scientificPlainResult <- runPipelineSelected PreparedStg scientificPlain [dir, "lib"]
  scientificQuotedResult <- runPipelineSelected PreparedStg scientificQuoted [dir, "lib"]
  scientificMetadataPlainResult <- runPipelineSelected PreparedStg scientificMetadataPlain [dir, "lib"]
  scientificMetadataQuotedResult <- runPipelineSelected PreparedStg scientificMetadataQuoted [dir, "lib"]
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
    [ "{-# LANGUAGE TypeApplications #-}" ] ++ quasiquoteLanguage quoted ++
    [ "module " ++ modul ++ " where"
    , "import Tidepool.Aeson.Value (Value)"
    , "import Tidepool.Effects.Core"
    ] ++ quasiquoteBindings quoted ++
    [ "result :: Maybe Value"
    , "result = runLLMTurn @Value \"scientific\""
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
    , TypePolicy.DataG _ _ _ rows <- TypePolicy.tgNodes (pmTypeGraph prepared)
    , (constructor, _) <- rows
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
        , TypePolicy.DataG _ _ _ rows <- TypePolicy.tgNodes (pmTypeGraph prepared)
        , (con, _) <- rows, occNameString (nameOccName (dataConName con)) == "NoUnpack" ]
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
      replace pair prepared = prepared { pmTypeGraph = TypePolicy.TypeGraph
        [ case node of
            TypePolicy.DataG ty tc args rows -> TypePolicy.DataG ty tc args
              (concatMap (\row@(con, children) -> if con == original
                then [(first, children) | first <- pair] else [row]) rows)
            _ -> node
        | node <- TypePolicy.tgNodes (pmTypeGraph prepared) ] }
      project pair = projectEntry
        (result { pprModules = map (replace pair) (pprModules result) })
        "StrictPlainMetadata" "noUnpack" mempty
      target = case filter ((== "StrictPlainMetadata")
            . moduleNameString . moduleName . pmModule) (pprModules result) of
        [prepared] -> prepared
        prepared -> error ("constructor collision fixture expected one target module, got "
          ++ show (map (moduleNameString . moduleName . pmModule) prepared))
      replaceNominal occurrence replacement prepared = prepared { pmTypeGraph = TypePolicy.TypeGraph
        [ case node of
            TypePolicy.DataG ty tc args rows -> TypePolicy.DataG ty tc args
              [ (if occNameString (nameOccName (dataConName con)) == occurrence
                  then replacement else con, children)
              | (con, children) <- rows ]
            _ -> node
        | node <- TypePolicy.tgNodes (pmTypeGraph prepared) ] }
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
            owners = map (idName . psOwner) selected
            ownsSelected (binding, _) = any ((`elem` owners) . idName)
              (Projection.topBinders binding)
        in (replaceNominal "NoUnpack" replacement target)
          { pmBindings = filter ownsSelected (pmBindings target)
          , pmPreparedSites = rooted
          , pmSiteRejections = []
          }
      otherModules = filter ((/= "StrictPlainMetadata")
        . moduleNameString . moduleName . pmModule) (pprModules result)
      projectAcrossGraphs first second = Projection.projectPrepared context
        (otherModules
          ++ [ graphModule "noUnpack" first
             , graphModule "automatic" second
             ])
      context = Projection.ProjectionContext
        { Projection.projectionProfile = "ghc-9.12-prepared-stg"
        , Projection.projectionToolchain = "ghc-9.12.2"
        , Projection.projectionTarget =
            Schema.TargetDescriptor Schema.X86_64 Schema.LittleEndian 64 64 "sysv64" []
        , Projection.projectionRetainedGenerations = mempty
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
      , Schema.TypeNodeId raw <- Schema.siteWire site
      , node : _ <- drop (fromIntegral raw) (Schema.programTypes program)
      , Schema.TypeData identity _ _ <- node
      , Schema.symbolOccurrence identity == fromString family -> pure ()
    sites -> ioError (userError (label ++ ": unexpected site evidence " ++ show sites
      ++ " in " ++ show (Schema.programTypes program)))

expectFailure :: String -> IO result -> IO ()
expectFailure label action = do
  outcome <- try (action >> pure ()) :: IO (Either SomeException ())
  case outcome of
    Left _ -> pure ()
    Right _ -> ioError (userError (label ++ ": expected compiler failure"))

expectFailureContaining :: String -> String -> IO result -> IO ()
expectFailureContaining label needle action = do
  outcome <- try (action >> pure ()) :: IO (Either SomeException ())
  case outcome of
    Left failure -> assert (needle `isInfixOf` show failure)
      (label ++ ": unexpected failure: " ++ show failure)
    Right _ -> ioError (userError (label ++ ": expected compiler failure"))

main :: IO ()
main = do
  args <- getArgs
  case args of
    ["--json-authority"] -> do
      tmp <- getTemporaryDirectory
      let work = tmp </> "tidepool-json-authority-test"
      bracket
        (removePathForcibly work >> createDirectoryIfMissing True work >> pure work)
        removePathForcibly
        verifyJsonDependencyAuthority
    ["--retained-scope"] -> do
      tmp <- getTemporaryDirectory
      let work = tmp </> "tidepool-retained-scope-test"
      bracket
        (removePathForcibly work >> createDirectoryIfMissing True work >> pure work)
        removePathForcibly
        $ \dir -> verifyCompilerReuse dir >> verifyPreparedScope dir
    ["--projection-interning", output] -> do
      tmp <- getTemporaryDirectory
      let work = tmp </> "tidepool-projection-interning-test"
      bracket
        (removePathForcibly work >> createDirectoryIfMissing True work >> pure work)
        removePathForcibly
        (\dir -> verifyProjectionInterning dir output)
    ["--module-product-roundtrip"] -> do
      tmp <- getTemporaryDirectory
      let work = tmp </> "tidepool-module-product-roundtrip-test"
      bracket
        (removePathForcibly work >> createDirectoryIfMissing True work >> pure work)
        removePathForcibly
        verifyModuleProductInterfaceRoundtrip
    [] -> fullMain
    _ -> ioError (userError ("unknown test arguments: " ++ show args))

fullMain :: IO ()
fullMain = do
  let expectedSites =
        [ ("runLLMTurnFork", DeliverHostAnswer, InvocationAnswer)
        , ("runLLMTurnFanout", DeliverHostAnswer, InvocationAnswers)
        , ("forkCata", DeliverHostAnswer, ListAnswer)
        , ("serve", DeliverLiveReentry, SelectedAnswer)
        , ("request", DeliverExitCellFill, ResponseResultEvidence)
        , ("requestWithProgress", DeliverExitCellFill, ResponseResultEvidence)
        , ("finalize", DeliverTerminalCapture, SelectedAnswer)
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
  tmp <- getTemporaryDirectory
  let work = tmp </> "tidepool-prepared-stg-pipeline-test"
  bracket
    (removePathForcibly work >> createDirectoryIfMissing True work >> pure work)
    removePathForcibly
    $ \dir -> do
      verifyCompilerReuse dir
      verifyPreparedScope dir
      let dep = dir </> "Dep.hs"
          effectsDir = dir </> "Tidepool" </> "Effects"
          effects = effectsDir </> "Core.hs"
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
        , "module Tidepool.Effects.Core where"
        , "data InvocationExit = InvocationExit"
        , "{-# OPAQUE runLLMTurn #-}"
        , "runLLMTurn :: forall a. String -> Maybe a"
        , "runLLMTurn _ = Nothing"
        , "{-# OPAQUE runLLMTurnSited #-}"
        , "runLLMTurnSited :: forall a. Int -> String -> Maybe a"
        , "runLLMTurnSited _ _ = Nothing"
        , "{-# OPAQUE runLLMTurnFork #-}"
        , "runLLMTurnFork :: forall a. String -> Maybe (Either InvocationExit a)"
        , "runLLMTurnFork _ = Nothing"
        , "{-# OPAQUE runLLMTurnForkSited #-}"
        , "runLLMTurnForkSited :: forall a. Int -> String -> Maybe (Either InvocationExit a)"
        , "runLLMTurnForkSited _ _ = Nothing"
        , "{-# OPAQUE runLLMTurnFanout #-}"
        , "runLLMTurnFanout :: forall a. [String] -> Maybe [Either InvocationExit a]"
        , "runLLMTurnFanout _ = Nothing"
        , "{-# OPAQUE runLLMTurnFanoutSited #-}"
        , "runLLMTurnFanoutSited :: forall a. Int -> [String] -> Maybe [Either InvocationExit a]"
        , "runLLMTurnFanoutSited _ _ = Nothing"
        , "{-# OPAQUE forkAllSited #-}"
        , "forkAllSited :: forall a. Int -> String -> Maybe [a]"
        , "forkAllSited _ _ = Nothing"
        , "keepForkAllSited :: Maybe [Bool]"
        , "keepForkAllSited = forkAllSited @Bool 0 \"\""
        ])
      writeFile unfold (unlines
        [ "{-# LANGUAGE ExplicitForAll #-}"
        , "module Tidepool.Actors.Unfold where"
        , "import Data.Kind (Type)"
        , "import Tidepool.Agent.Reply.Internal (ResponseResult)"
        , "keepResponseResultAuthority :: Maybe (ResponseResult Bool)"
        , "keepResponseResultAuthority = Nothing"
        , "{-# OPAQUE child #-}"
        , "child :: forall result (child :: Type) input (parent :: Type). input -> Maybe result"
        , "child _ = Nothing"
        , "{-# OPAQUE childSited #-}"
        , "childSited :: forall result (child :: Type) input (parent :: Type). Int -> input -> Maybe result"
        , "childSited _ _ = Nothing"
        ])
      writeFile replyInternal (unlines
        [ "{-# LANGUAGE ExplicitForAll #-}"
        , "{-# LANGUAGE KindSignatures #-}"
        , "module Tidepool.Agent.Reply.Internal where"
        , "import Data.Kind (Type)"
        , "data ResponseResult a = ResponseResult a"
        , "data RequestScope (input :: Type) (result :: Type) = RequestScope"
        , "data Eff (effs :: Type) (value :: Type) = Eff value"
        , "{-# OPAQUE currentRequest #-}"
        , "currentRequest :: forall input result effs. Eff effs (RequestScope input result)"
        , "currentRequest = currentRequestSited (-1)"
        , "{-# OPAQUE currentRequestSited #-}"
        , "currentRequestSited :: forall input result effs. Int -> Eff effs (RequestScope input result)"
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
      runTypeEvidenceChecks dir
        (\result entry -> projectEntry result "TypeEvidence" entry mempty)
        (\result entry auxEntries ->
          projectEntryWithAux result "TypeEvidence" entry auxEntries mempty)
      verifyPreparedPrivateImports
      verifyConstructorRepresentations dir
      verifyJsonDependencyAuthority dir
      writeFile target validTarget
      writeFile siteTarget (unlines
        [ "{-# LANGUAGE TypeApplications #-}"
        , "module SiteExpr where"
        , "import Tidepool.Effects.Core"
        , "typedSite :: Maybe Bool"
        , "typedSite = runLLMTurn @Bool \"prepared\""
        , "forkedSite :: Maybe (Either InvocationExit Bool)"
        , "forkedSite = runLLMTurnFork @Bool \"prepared\""
        , "fanoutSite :: Maybe [Either InvocationExit Bool]"
        , "fanoutSite = runLLMTurnFanout @Bool [\"prepared\"]"
        ])
      writeFile polySiteTarget (unlines
        [ "{-# LANGUAGE RankNTypes #-}"
        , "{-# LANGUAGE ScopedTypeVariables #-}"
        , "{-# LANGUAGE TypeApplications #-}"
        , "module PolySiteExpr where"
        , "import Tidepool.Effects.Core"
        , "polyHelper :: forall a. String -> Maybe a"
        -- Fully applied to a computed argument, so the simplifier cannot
        -- eta-reduce it to a partial, unelaborated verb occurrence.
        , "polyHelper label = runLLMTurn @a (label ++ \"!\")"
        , "{-# OPAQUE polyHelper #-}"
        , "polyNested :: forall a. Bool -> String -> Maybe a"
        , "polyNested flag label = if flag then runLLMTurn @a label else Nothing"
        , "{-# NOINLINE polyNested #-}"
        , "unrelated :: Int"
        , "unrelated = 42"
        -- A verb passed as a value has no site of its own.
        , "applyVerb :: (forall a. String -> Maybe a) -> Maybe Bool"
        , "applyVerb verb = verb \"first-class\""
        , "{-# NOINLINE applyVerb #-}"
        , "firstClass :: Maybe Bool"
        , "firstClass = applyVerb runLLMTurn"
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
                _ -> False)
        ("currentRequest must resolve its exact generated sibling: "
          ++ show currentRequestEvidence
          ++ "; rejections=" ++ show [srMessage rejection
            | prepared <- pprModules currentRequestDirect
            , rejection <- pmSiteRejections prepared])
      let (siteInventory, directSites) = preparedEvidence "SiteExpr" siteDirect
      assert (case filter ((== "SiteExpr.typedSite") . ysOrigin) directSites of
                [site] -> "runLLMTurnSited" `isInfixOf` siteInventory
                  && show (ysSite site) `isInfixOf` siteInventory
                _ -> False)
        "typed site was not elaborated before preparation"
      assertWireSite "forked answer wrapper" Schema.HostAnswer "Either"
        (projectEntry siteDirect "SiteExpr" "forkedSite" mempty)
      assertWireSite "fanout answer wrapper" Schema.HostAnswer "List"
        (projectEntry siteDirect "SiteExpr" "fanoutSite" mempty)
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
      let forkAllSpec = case filter ((== "forkAll") . vsName) sitedVerbs of
            [spec] -> spec
            _ -> error "missing forkAll VerbSpec"
          listSite = buildYieldSite forkAllSpec "ListSiteExpr.listSite" 0 boolTy []
      assert (stType (ysAnswer listSite) == "[Bool]")
        "list-answer site did not preserve shared legacy answer identity"

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
      expectFailure "direct prepared" $
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
        expectFailure "resident prepared" $
          compile PreparedStg mempty GeneralCompile Nothing target [] Nothing
        writeFile target validTarget
        recovered <- compile PreparedStg mempty GeneralCompile Nothing target [] Nothing
        assert (preparedShape recovered == directShape)
          "resident compiler did not recover after a request-local failure"
