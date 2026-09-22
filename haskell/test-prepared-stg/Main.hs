module Main (main) where

import Control.Exception (SomeException, bracket, evaluate, try)
import Control.Monad (unless)
import Data.List (isInfixOf, nub, sort)
import Data.String (fromString)
import Data.Text qualified as Text
import GHC (moduleNameString)
import GHC.Builtin.Types (boolTy)
import GHC.Core (Expr(..), bindersOf, flattenBinds)
import GHC.Core.DataCon (dataConName, dataConRepArgTys)
import GHC.Core.DataCon qualified as DC
import GHC.Core.TyCon (PromDataConInfo(NoPromInfo))
import GHC.Builtin.Types.Prim (wordPrimTy)
import GHC.Core.FVs (exprSomeFreeVarsList)
import GHC.Core.TyCo.Rep (Scaled(..))
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
import Tidepool.ExecutionIR
  ( LiteralInventory(..), PreparedFact(..), PreparedInventory(..), PreparedSupport(..), inventoryPreparedModule
  , renderPreparedInventory )
import Tidepool.PreparedSites (buildYieldSite, lookupPreparedVerb, resolvePreparedSiblings)
import RetainedPluginTest (verifyCompilerReuse)
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
  Projection.projectPreparedTarget context (pprModules result)
 where
  context = Projection.ProjectionContext
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

-- | TH's bytecode provisioning must not change a constructor declared by an
-- unchanged home module.  The graph checks run after metadata preparation and
-- before projection or execution; the executable checks then compare the
-- declarations a shared prepared machine receives from ordinary and quoted
-- source.
verifyConstructorRepresentations :: FilePath -> IO ()
verifyConstructorRepresentations dir = do
  let strictOwned = dir </> "StrictOwned.hs"
      strictPlain = dir </> "StrictPlainMetadata.hs"
      strictQuoted = dir </> "StrictQuotedMetadata.hs"
      scientificPlain = dir </> "ScientificPlain.hs"
      scientificQuoted = dir </> "ScientificQuoted.hs"
      scientificMetadataPlain = dir </> "ScientificMetadataPlain.hs"
      scientificMetadataQuoted = dir </> "ScientificMetadataQuoted.hs"
  writeFile strictOwned (unlines
    [ "{-# OPTIONS_GHC -O0 #-}"
    , "module StrictOwned where"
    , "data Automatic = Automatic !Int"
    , "data NoUnpack = NoUnpack {-# NOUNPACK #-} !Int"
    , "data ExplicitUnpack = ExplicitUnpack {-# UNPACK #-} !Int"
    ])
  writeFile strictPlain (strictMetadataSource "StrictPlainMetadata" False)
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
  strictPlainResult <- runPipelineSelected PreparedStg strictPlain [dir, "lib"]
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
  scientificMetadataSource modul quoted = unlines $
    [ "{-# LANGUAGE TypeApplications #-}" ] ++ quasiquoteLanguage quoted ++
    [ "module " ++ modul ++ " where"
    , "import Tidepool.Aeson.Value (Value)"
    , "import Tidepool.Effects.Core"
    ] ++ quasiquoteBindings quoted ++
    [ "result :: Maybe Value"
    , "result = runLLMTurn @Value \"scientific\""
    ]
  quasiquoteLanguage False = []
  quasiquoteLanguage True = ["{-# LANGUAGE QuasiQuotes #-}"]
  quasiquoteBindings False = []
  quasiquoteBindings True =
    [ "import Tidepool.Aeson.Value (Value)"
    , "import Tidepool.QQ (j)"
    , "quotedValue :: Value"
    , "quotedValue = [j|42|]"
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
      rejects pair = case project pair of
        Left (Projection.InvalidPreparedIdentity detail) ->
          assert ("conflicting physical declarations" `Text.isInfixOf` detail)
            ("unexpected constructor rejection: " ++ show detail)
        outcome -> ioError (userError ("conflicting constructor evidence was not rejected: "
          ++ either show (const "accepted") outcome))
  assertProjects "identical constructor provenance"
    (project [original, clone otherName fields runtimeFields])
  mapM_ (\changed -> do
    rejects [original, changed]
    rejects [changed, original])
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
  let expectedSites =
        [ ("runLLMTurnFork", DeliverHostAnswer, InvocationAnswer)
        , ("runLLMTurnFanout", DeliverHostAnswer, InvocationAnswers)
        , ("forkCata", DeliverHostAnswer, ListAnswer)
        , ("serve", DeliverLiveReentry, SelectedAnswer)
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
      let dep = dir </> "Dep.hs"
          effectsDir = dir </> "Tidepool" </> "Effects"
          effects = effectsDir </> "Core.hs"
          unfoldDir = dir </> "Tidepool" </> "Actors"
          unfold = unfoldDir </> "Unfold.hs"
          replyDir = dir </> "Tidepool" </> "Agent" </> "Reply"
          replyInternal = replyDir </> "Internal.hs"
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
        [ "module Tidepool.Agent.Reply.Internal where"
        , "data ResponseResult a = ResponseResult a"
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
