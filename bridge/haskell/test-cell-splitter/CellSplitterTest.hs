{-# LANGUAGE LambdaCase #-}
{-# LANGUAGE OverloadedStrings #-}

module Main where

import Control.Monad (forM_, unless, when)
import Control.Exception (SomeException, bracket, finally, throwIO, try)
import Control.Monad.IO.Class (liftIO)
import Data.IORef (newIORef, modifyIORef', readIORef)
import Data.List (intercalate, isInfixOf, isPrefixOf, isSuffixOf, tails)
import Data.Char (isDigit)
import Data.Data (Data, Typeable, cast, gmapQ)
import qualified Data.Text as Text
import GHC
import GHC.Builtin.Types (intTy)
import GHC.Types.Name.Occurrence (mkVarOcc, occNameString)
import GHC.Types.Name (nameModule_maybe, nameOccName)
import GHC.Tc.Types (tcg_rn_decls)
import GHC.Types.SourceText (il_value)
import GHC.Types.Fixity (Fixity(..))
import GHC.Driver.Session (parseDynamicFilePragma)
import GHC.Driver.Env (hsc_HPT)
import GHC.Unit.Home.ModInfo (lookupHpt, hm_iface)
import GHC.Unit.Module.ModIface (mi_iface_hash)
import GHC.Parser.Header (getOptions)
import GHC.Driver.Config.Parser (initParserOpts)
import GHC.Data.StringBuffer (stringToStringBuffer)
import GHC.Types.SourceError (SourceError)
import Tidepool.Agent.Assignment.Internal (NameError (..), renderNameError)
import Tidepool.Binders
import Tidepool.TurnSource (spliceTemplate)
import Tidepool.SessionArtifacts (mkBoundBinders)
import Tidepool.DiagJson (Diag (..), DiagSeverity(..), DependencyLoadFailure(..))
import Tidepool.ExtractUtil (getLibdir)
import Tidepool.ExecutionSchema (SymbolIdentity(..))
import GHC.Utils.Outputable (ppr, showSDocUnsafe)
import Tidepool.PlannedDeclaration (hydratePlannedDeclarationInventory, transformProgramDeclarationImports)
import Tidepool.GhcPipeline
import Tidepool.ExtractRequest
  ( InspectionRequest(..), RequestField(..), WorkerRequest(..)
  , workerArgv, workerRequestFromArgv )
import Tidepool.Introspection (InfoEntry(..), InspectionResult(..), runInspection)
import Tidepool.DependencyEvidence
import Tidepool.Session
  ( Generation(..), SessionModule(..), SessionModuleKind(..), SessionScope(..)
  , mkThinSessionIface, writeSessionIface, injectSessionIface, renderSessionModule )
import Tidepool.PreparedStg (PreparedModule(..))
import UnreachableCompileTimeTest (unreachableCompileTimeCompilation)
import Tidepool.Timing
  ( InterfaceStage(..), InterfaceReuse(..), measureModuleInterface )
import System.Directory
  ( getTemporaryDirectory, createDirectory, createDirectoryIfMissing
  , removeFile, removeDirectoryRecursive
  , getPermissions, setPermissions, setOwnerExecutable )
import System.FilePath ((</>))
import System.IO (openTempFile, hClose, hFlush, readFile', stderr)
import GHC.IO.Handle (hDuplicate, hDuplicateTo)
import System.Environment (getArgs, lookupEnv, setEnv, unsetEnv)

main :: IO ()
main = getArgs >>= \case
  ["--check-source-request"] -> checkingSourceRequestRoundTrip >> putStrLn "checking source request: 1 passed"
  ["--ordered-segments"] -> orderedInferenceSegments >> putStrLn "ordered inference segments: 1 passed"
  ["--program-originals"] -> programOriginalImportsCompilation >> putStrLn "program original imports: 1 passed"
  ["--function-value-iface"] -> functionValueInterfaceCompilation >> putStrLn "function value interface: 1 passed"
  ["--session-fixities"] -> sessionFixitiesCompilation >> putStrLn "session fixities: 1 passed"
  ["--unreachable-compile-time"] -> unreachableCompileTimeCompilation >> putStrLn "unreachable compile-time: 1 passed"
  _ -> runAllTests

runAllTests :: IO ()
runAllTests = do
  certificationRequestValidation
  checkingSourceRequestRoundTrip
  libdir <- getLibdir
  runGhc (Just libdir) $ do
    flags <- getSessionDynFlags
    let (_, lexicalOptions) = getOptions (initParserOpts flags)
          (stringToStringBuffer
            "{-# LANGUAGE QuasiQuotes, MultilineStrings, LambdaCase #-}\n")
          "<cell-test>"
    (lexicalFlags, _, _) <- parseDynamicFilePragma flags lexicalOptions
    liftIO $ do
      lexicalIslands lexicalFlags
      commentsPragmasAndLayout lexicalFlags
      declarationsBecomeOneCellItem flags
      prologuePlans flags
      automaticGenericPlans flags
      noStandaloneDerivingLeavesCellUntouched flags
      danglingOperatorCells flags
      multilineLetPlacement flags
  orderedInferenceSegments
  interfaceMeasurementDiagnostics
  multilineLetCompilation
  renderNameErrorTeachesGroupPaths
  ambiguousOccurrenceHintCompilation
  getArgs >>= \case
    [] -> pure ()
    ["--metadata"] -> metadataCompilation
    ["--prepared-session"] -> preparedSessionLeafCompilation
    ["--dependency-evidence"] -> dependencyEvidenceCompilation
    ["--untracked-compile-time"] -> untrackedCompileTimeCompilation
    ["--validation-memo"] -> validationMemoCompilation
    ["--pin-imports"] -> pinnedTypeImportsCompilation
    ["--path-insensitive-witness"] -> pathInsensitiveWitnessCompilation
    ["--memo-lifecycle"] -> memoLifecycleCompilation
    ["--structural-display", effectsRoot] -> structuralDisplayCompilation effectsRoot
    _ -> fail "expected --metadata, --prepared-session, --dependency-evidence, --untracked-compile-time, --validation-memo, --pin-imports, --path-insensitive-witness, --memo-lifecycle, or --structural-display EFFECTS_INCLUDE"

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
  withResidentPipelineSelectedRequests [root] (const (pure ())) $ \runRequest -> do
    checked <- runRequest $ \compiler -> compiler CheckedEnvironment mempty GeneralCompile Nothing target [root] Nothing
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
    writeFile target (rendered ++ "\n__legacy = Tidepool.Session.Lib.G1.Old\n")
    _ <- runRequest $ \compiler -> compiler CheckedEnvironment mempty GeneralCompile Nothing target [root] Nothing
    writeFile target (rendered ++ "\n__legacy = Old\n")
    rejected <- try (runRequest $ \compiler -> compiler CheckedEnvironment mempty GeneralCompile Nothing target [root] Nothing)
      :: IO (Either SomeException CheckedEnvironmentResult)
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
        , ActivationPreview, CellFoldTurn, TargetModuleOnly
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
  withResidentPipelineSelectedRequests includes (const (pure ())) $ \runRequest ->
    runRequest $ \compiler -> forM_ cases $ \(name, source, binder) -> do
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
      -- A fixture standing in for a real, allowlisted quoter module
      -- ('bridge/haskell/lib/Tidepool/QQ/Label.hs'): 'pureQuasiQuoters'
      -- matches purely on qualified name
      -- ("Tidepool.QQ.Label.label"), so a small local module under that
      -- same name exercises the same resolution path without depending on
      -- the deployed stdlib tree.
      qqDir = root </> "Tidepool" </> "QQ"
      qqLabel = qqDir </> "Label.hs"
      -- The compile TARGET itself is always freshly recompiled every
      -- request ('validationMemoCompilation' asserts this directly); only
      -- a *dependency* module's memo entry is ever reused. So each
      -- quasiquoter case below needs its own dependency module, imported
      -- by a throwaway target, to actually observe a memo hit or miss on
      -- the quasiquoter-using module itself.
      labelDependency = root </> "LabelDependency.hs"
      labelUser = root </> "LabelUser.hs"
      -- GHC's own stage restriction forbids using a quasiquoter in the
      -- same module that defines it ("must be imported, not defined
      -- locally"), so the unlisted quoter lives in its own module,
      -- imported like any other -- exercising "imported, but not on the
      -- allowlist" rather than "no import at all provides it".
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
  -- Two dependencies standing in for the real workspace shape
  -- ('Project.Work'/'Project.Review' importing 'label' via the open
  -- 'Tidepool.Actors.Exomonad' import, which re-exports it from
  -- 'Tidepool.QQ.Label', alongside several other open imports of modules
  -- that do not export 'label'):
  --
  --  1. 'OpenImportDependency' imports the *defining* module,
  --     'Tidepool.QQ.Label', with no explicit import list, alongside a
  --     decoy open import that brings nothing relevant into scope.
  --     Resolution must pick the one open import whose interface actually
  --     exports 'label', not bail out as ambiguous just because more than
  --     one import decl has no explicit list.
  --  2. 'ReexportDependency' imports 'label' (with an explicit list, so
  --     import-list resolution alone would already succeed) from a
  --     *re-exporting* convenience module, 'LabelReexport'. Resolution must
  --     follow the re-export back to "Tidepool.QQ.Label.label" -- the
  --     allowlist's key -- not stop at "LabelReexport.label".
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
  let evidence = pprDependencies direct
  when (dependencyCacheSafe evidence || dependencySelectionComplete evidence) $
    fail "TemplateHaskell source produced complete dependency evidence"
  noQuotes <- runPipelineSelected PreparedStg noQuoteUser [root]
  assertComplete "QuasiQuotes enabled without occurrences" noQuotes
  cpp <- runPipelineSelected PreparedStg cppUser [root]
  assertIncomplete "CPP enabled" cpp
  preprocessed <- runPipelineSelected PreparedStg preprocessedUser [root]
  assertIncomplete "external preprocessor with an untracked input" preprocessed
  previousTiming <- lookupEnv "TIDEPOOL_TIMING"
  previousMemoTrace <- lookupEnv "TIDEPOOL_MEMO_TRACE"
  setEnv "TIDEPOOL_TIMING" "1"
  setEnv "TIDEPOOL_MEMO_TRACE" "1"
  (withResidentPipelineSelected [root] $ \compile -> do
      _ <- compile PreparedStg mempty GeneralCompile Nothing target [] Nothing
      (_, warmLog) <- captureStderr root "quasiquote-warm" $
        compile PreparedStg mempty GeneralCompile Nothing target [] Nothing
      when ("tidepool-memo-miss module=QuasiQuoteDependency" `isInfixOf` warmLog) $
        fail ("QuasiQuotes with no occurrences missed the memo: " ++ warmLog)
      assertContains "TemplateHaskell source remains conservatively uncacheable"
        "tidepool-memo-miss module=TemplateDependency reason=untracked-compile-time-execution"
        warmLog
      preprocessedCold <- compile PreparedStg mempty GeneralCompile Nothing preprocessedUser [] Nothing
      assertIncomplete "external preprocessor, fresh" preprocessedCold
      (preprocessedWarm, preprocessedWarmLog) <- captureStderr root "preprocessed-warm" $
        compile PreparedStg mempty GeneralCompile Nothing preprocessedUser [] Nothing
      assertIncomplete "external preprocessor, repeated" preprocessedWarm
      assertContains "external preprocessor is never memoized"
        "tidepool-memo-miss module=PreprocessedDependency reason=untracked-compile-time-execution"
        preprocessedWarmLog
      -- A dependency module whose only compile-time execution is an
      -- allowlisted, pure quasiquoter reuses the memo on the next
      -- identical compile (the target itself, 'LabelUser', is always
      -- freshly recompiled -- see 'validationMemoCompilation' -- so it is
      -- 'LabelDependency', not 'LabelUser', whose memo status this checks).
      labelCold <- compile PreparedStg mempty GeneralCompile Nothing labelUser [] Nothing
      assertComplete "allowlisted pure quasiquote, fresh" labelCold
      (labelWarm, labelWarmLog) <- captureStderr root "label-warm" $
        compile PreparedStg mempty GeneralCompile Nothing labelUser [] Nothing
      assertComplete "allowlisted pure quasiquote, memoized" labelWarm
      when ("tidepool-memo-miss module=LabelDependency" `isInfixOf` labelWarmLog) $
        fail ("a dependency using only [label|...|] missed the memo: " ++ labelWarmLog)
      -- Same guarantee, reached through an open (no-explicit-list) import of
      -- the defining module itself, with an unrelated open import present
      -- too -- the ambiguity the old import-list-only heuristic could not
      -- see through (every no-list import "brings everything into scope",
      -- so it bailed out as soon as more than one was present).
      openCold <- compile PreparedStg mempty GeneralCompile Nothing openImportUser [] Nothing
      assertComplete "allowlisted pure quasiquote via open import, fresh" openCold
      (openWarm, openImportWarmLog) <- captureStderr root "open-import-warm" $
        compile PreparedStg mempty GeneralCompile Nothing openImportUser [] Nothing
      assertComplete "allowlisted pure quasiquote via open import, memoized" openWarm
      when ("tidepool-memo-miss module=OpenImportDependency" `isInfixOf` openImportWarmLog) $
        fail ("a dependency using [label|...|] via an open import missed the memo: "
                ++ openImportWarmLog)
      -- Same guarantee again, reached through a module that re-exports the
      -- allowlisted quoter rather than defining it -- resolution must
      -- follow the re-export back to "Tidepool.QQ.Label.label", the
      -- allowlist's key, not stop at "LabelReexport.label".
      reexportCold <- compile PreparedStg mempty GeneralCompile Nothing reexportUser [] Nothing
      assertComplete "allowlisted pure quasiquote via reexport, fresh" reexportCold
      (reexportWarm, reexportWarmLog) <- captureStderr root "reexport-warm" $
        compile PreparedStg mempty GeneralCompile Nothing reexportUser [] Nothing
      assertComplete "allowlisted pure quasiquote via reexport, memoized" reexportWarm
      when ("tidepool-memo-miss module=ReexportDependency" `isInfixOf` reexportWarmLog) $
        fail ("a dependency using [label|...|] via a re-export missed the memo: "
                ++ reexportWarmLog)
      -- A dependency using a quasiquoter this resolver cannot place on the
      -- allowlist (here: locally defined, so no import brings it into
      -- scope) stays conservatively uncacheable, same as raw QuasiQuotes —
      -- same short TIDEPOOL_TIMING reason — but TIDEPOOL_MEMO_TRACE still
      -- honestly names the unresolved quoter it actually saw.
      localCold <- compile PreparedStg mempty GeneralCompile Nothing localQuoteUser [] Nothing
      assertIncomplete "unlisted quasiquote, fresh" localCold
      (localWarm, localWarmLog) <- captureStderr root "local-quote-warm" $
        compile PreparedStg mempty GeneralCompile Nothing localQuoteUser [] Nothing
      assertIncomplete "unlisted quasiquote, memoized" localWarm
      assertContains "an unlisted (locally-defined) quasiquoter remains conservatively uncacheable"
        "tidepool-memo-miss module=LocalQuoteDependency reason=untracked-compile-time-execution"
        localWarmLog
      assertContains "the trace honestly reports the unresolved quoter, not a false allowlist hit"
        "tidepool-memo-trace-miss" localWarmLog
      assertContains "the trace honestly reports the unresolved quoter, not a false allowlist hit"
        "quasiquotes=untracked:LocalQuoteQuoter.myqq" localWarmLog
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
        , "result = value"
        ]
      withDependentFile <- compile PreparedStg mempty GeneralCompile Nothing dependentUser [] Nothing
      assertIncomplete "allowlisted origin with a dependent file" withDependentFile)
    `finally` do
      maybe (unsetEnv "TIDEPOOL_TIMING") (setEnv "TIDEPOOL_TIMING") previousTiming
      maybe (unsetEnv "TIDEPOOL_MEMO_TRACE") (setEnv "TIDEPOOL_MEMO_TRACE") previousMemoTrace
  where
    assertComplete label result = do
      let evidence = pprDependencies result
      unless (dependencyCacheSafe evidence && dependencySelectionComplete evidence) $
        fail (label ++ " did not produce complete dependency evidence")
    assertIncomplete label result = do
      let evidence = pprDependencies result
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
  let evidence = pprDependencies prepared
      resolutions = dependencyResolutions evidence
      selectedPaths = [path | resolution <- resolutions
                            , Just path <- [dependencyResolutionSelected resolution]]
      packageWitnesses = [resolution | resolution <- resolutions
        , dependencyResolutionModule resolution == "Data.Text"]
      qualifiedWitnesses = [resolution | resolution <- resolutions
        , dependencyResolutionModule resolution == "Data.Map"]
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
      [resolution] -> "other:" `isPrefixOf` dependencyResolutionQualifier resolution
        && dependencyResolutionSelected resolution == Nothing
        && null (dependencyResolutionCandidates resolution)
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
  previousTiming <- lookupEnv "TIDEPOOL_TIMING"
  setEnv "TIDEPOOL_TIMING" "1"
  (withResidentPipelineSelectedRequests [workDir] (const (pure ())) $ \runRequest ->
      runRequest $ \compile -> do
        (_, coldLog) <- captureStderr root "path-insensitive-cold" $
          compile PreparedStg mempty GeneralCompile Nothing importer [rootA, sharedRoot] Nothing
        assertContains "cold compile resolves Dep from rootA"
          "tidepool-memo-miss module=Dep reason=absent" coldLog
        assertContains "cold compile resolves Target from rootA"
          "tidepool-memo-miss module=Target reason=dependency-miss:Dep" coldLog
        (warm, warmLog) <- captureStderr root "path-insensitive-warm" $
          compile PreparedStg mempty GeneralCompile Nothing importer [rootB, sharedRoot] Nothing
        let evidence = pprDependencies warm
            obsoletePaths = [path | resolution <- dependencyResolutions evidence
              , path <- dependencyResolutionCandidates resolution, rootA `isPrefixOf` path]
        unless (null obsoletePaths) $
          fail ("previous request roots leaked through shared module summary: " ++ show obsoletePaths)
        unless (dependencyCacheSafe evidence && dependencySelectionComplete evidence) $
          fail "changed import roots lost complete dependency evidence"
        when ("tidepool-memo-miss module=Target" `isInfixOf` warmLog) $
          fail ("byte-identical Target resolved from a different root missed the memo: " ++ warmLog)
        when ("tidepool-memo-miss module=Dep" `isInfixOf` warmLog) $
          fail ("byte-identical Dep resolved from a different root missed the memo: " ++ warmLog))
    `finally` maybe (unsetEnv "TIDEPOOL_TIMING") (setEnv "TIDEPOOL_TIMING") previousTiming
  where
    temporary = do
      parent <- getTemporaryDirectory
      (path, handle) <- openTempFile parent "tidepool-path-insensitive-witness"
      hClose handle
      removeFile path
      createDirectory path
      pure path

pinnedTypeImportsCompilation :: IO ()
pinnedTypeImportsCompilation = bracket temporary removeDirectoryRecursive $ \root -> do
  let alias = root </> "PinHandler.hs"
      checkedSource = root </> "PinCheck.hs"
      stagedSource = root </> "PinStage.hs"
  writeFile alias $ unlines
    [ "{-# LANGUAGE DataKinds, TypeOperators #-}"
    , "module PinHandler (Handler, Box(..)) where"
    , "import Control.Monad.Freer (Eff)"
    , "import qualified Control.Monad.Freer.State as S"
    , "type Handler a = Eff '[S.State Int] a"
    , "data Box = Box"
    ]
  writeFile checkedSource $ unlines
    [ "module PinCheck where"
    , "import PinHandler (Handler)"
    , "import qualified PinHandler as Alias"
    , "__cell = do { let { saved = (pure () :: Handler ()) };"
    , "  let { __tidepool_cell_pin_0_saved = saved };"
    , "  let { boxed = (1 :: Int, [Alias.Box]) };"
    , "  let { __tidepool_cell_pin_1_boxed = boxed }; pure () } :: IO ()"
    ]
  withResidentPipelineSelected [root] $ \compile -> do
    checked <- compile CheckedEnvironment mempty GeneralCompile Nothing checkedSource [] Nothing
    pin <- case filter ((== "__tidepool_cell_pin_0_saved") . checkedPinKey)
                 (crCheckedBinderPins checked) of
      [selected] -> pure selected
      _ -> fail "whole-cell check did not capture the Handler binding"
    unless ("Control.Monad.Freer.State" `elem` checkedPinImports pin) $
      fail ("expanded Handler type omitted its qualified State import: " ++ show pin)
    aliasPin <- case filter ((== "__tidepool_cell_pin_1_boxed") . checkedPinKey)
                      (crCheckedBinderPins checked) of
      [selected] -> pure selected
      _ -> fail "whole-cell check did not capture the alias-qualified binding"
    assertContains "authored import alias stays in the pinned type" "Alias.Box" (checkedPinType aliasPin)
    unless (null (checkedPinImports aliasPin)) $
      fail ("tuple/list/alias type added an unneeded module import: " ++ show aliasPin)
    writeFile stagedSource $ unlines $
      [ "{-# LANGUAGE DataKinds, TypeOperators #-}"
      , "module PinStage where"
      , "import PinHandler (Handler)"
      , "import qualified PinHandler as Alias"
      ] ++ map ("import qualified " ++) (checkedPinImports pin) ++
      [ "__result = do { let { saved = (pure () :: Handler ()) };"
      , "  let { boxed = (1 :: Int, [Alias.Box]) };"
      , "  pure ((saved :: " ++ checkedPinType pin ++ "),"
          ++ " (boxed :: " ++ checkedPinType aliasPin ++ ")) } :: IO (Handler (), (Int, [Alias.Box]))"
      ]
    _ <- compile CheckedEnvironment mempty GeneralCompile Nothing stagedSource [] Nothing
    pure ()
  where
    temporary = do
      parent <- getTemporaryDirectory
      (path, handle) <- openTempFile parent "tidepool-pin-imports"
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
  writeFile consumer "module MemoConsumer (result) where\nimport MemoProducer (value)\nresult = value + 1\n"
  writeFile memoTarget "module MemoTarget where\nimport MemoConsumer (result)\nfinal = result\n"
  previousTiming <- lookupEnv "TIDEPOOL_TIMING"
  setEnv "TIDEPOOL_TIMING" "1"
  (withResidentPipelineSelected [root] $ \compile -> do
      (cold, coldLog) <- captureStderr root "validation-cold" $
        compile PreparedStg mempty GeneralCompile Nothing target [] Nothing
      assertContains "chain dependency witnesses are computed once per node"
        "tidepool-dependency-witness nodes=20 direct_edges=19 digest_computations=20"
        coldLog
      (warm, warmLog) <- captureStderr root "validation-warm" $
        compile PreparedStg mempty GeneralCompile Nothing target [] Nothing
      let productShape result =
            (length (pprModules result), length (prBinds (pprPipelineResult result)))
      assertEqual "validation facts preserve the prepared target product"
        (productShape cold) (productShape warm)
      forM_ ["WarmReexport", "WarmChild"] $ \name ->
        when (("tidepool-memo-miss module=" ++ name) `isInfixOf` warmLog) $
          fail ("unchanged validation-only module was recompiled: " ++ name)
      when ("tidepool-memo-miss module=WarmChain" `isInfixOf` warmLog) $
        fail "unchanged validation-only chain was recompiled"
      -- The reachable target is checked against the exact prepared interfaces
      -- its dependencies registered. Its reachability and executable passes
      -- therefore each compile the front, while optimization and preparation
      -- run once.
      assertContains "warm compile prepares only its evicted target"
        "front_compiles=2 core_compiles=1 prepared_compiles=1" warmLog
      executableCold <- compile PreparedStg mempty GeneralCompile Nothing memoTarget [] Nothing
      (executableWarm, executableWarmLog) <- captureStderr root "executable-warm" $
        compile PreparedStg mempty GeneralCompile Nothing memoTarget [] Nothing
      assertEqual "complete memo product preserves its prepared output"
        (productShape executableCold) (productShape executableWarm)
      when ("tidepool-memo-miss module=MemoProducer" `isInfixOf` executableWarmLog ||
            "tidepool-memo-miss module=MemoConsumer" `isInfixOf` executableWarmLog) $
        fail "unchanged producer or consumer executable was recompiled"
      previousDrop <- lookupEnv "TIDEPOOL_TEST_DROP_MEMO_INTERFACE"
      (regenerated, forcedLog) <- (do
          setEnv "TIDEPOOL_TEST_DROP_MEMO_INTERFACE" "MemoProducer"
          captureStderr root "executable-interface-miss" (
            compile PreparedStg mempty GeneralCompile Nothing memoTarget [] Nothing))
        `finally` maybe (unsetEnv "TIDEPOOL_TEST_DROP_MEMO_INTERFACE")
                        (setEnv "TIDEPOOL_TEST_DROP_MEMO_INTERFACE") previousDrop
      assertContains "missing producer interface regenerates producer executable"
        "tidepool-memo-miss module=MemoProducer reason=required-interface-not-retained" forcedLog
      assertContains "regenerated producer invalidates cached consumer executable"
        "tidepool-memo-miss module=MemoConsumer reason=dependency-executable-regenerated" forcedLog
      assertEqual "interface recovery restores the paired prepared body"
        (productShape executableCold) (productShape regenerated))
    `finally` maybe (unsetEnv "TIDEPOOL_TIMING") (setEnv "TIDEPOOL_TIMING") previousTiming
  where
    temporary = do
      parent <- getTemporaryDirectory
      (path, handle) <- openTempFile parent "tidepool-validation-memo"
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
  evictions <- newIORef []
  previousTiming <- lookupEnv "TIDEPOOL_TIMING"
  setEnv "TIDEPOOL_TIMING" "1"
  (withResidentPipelineSelectedRequests [root] (\name -> modifyIORef' evictions (name :)) $ \runRequest -> do
      (checked, output) <- captureStderr root "metadata-check" $
        runRequest $ \compiler ->
          compiler CheckedEnvironment mempty GeneralCompile Nothing target [root] Nothing
      inspected <- runInspection
        (crHscEnv checked)
        (crTargetTcGblEnv checked)
        (crTargetRdrEnv checked)
        (crInspectionProbes checked)
        [InspectTypeOf "value", InspectModule "MetadataTarget" False]
      case inspected of
        [InspectionType "value" rendered _, InspectionBrowse "MetadataTarget" False entries] -> do
          assertContains "inspection resolves a local probe without a target HPT interface"
            "Box Int" rendered
          unless (any ((== "__tidepool_inspect_0") . infoName) entries) $
            fail "metadata inspection could not browse the checked target module"
        _ -> fail ("metadata inspection returned an unexpected result: " ++ show inspected)
      assertEqual "exactly one checked target" 1
        (length (filter (isInfixOf "tidepool-checked module=MetadataTarget target=True") (lines output)))
      assertContains "metadata leaf skips its unused HPT interface"
        "tidepool-checked-interface-elided module=MetadataTarget reason=no-later-home-importer"
        output
      when (any (isInfixOf "module=MetadataTarget")
            (filter (isPrefixOf "tidepool-timing-module-detail ") (lines output))) $
        fail "metadata leaf constructed an unused target interface"
      unless (not ("tidepool-target phase=desugar" `isInfixOf` output || "phase=lowering " `isInfixOf` output)) $
        fail "metadata target entered the executable pipeline"
      writeFile dependency "module MetadataDependency where\nvalue = missingDependencyName\n"
      rejected <- try (runRequest $ \compiler ->
        compiler CheckedEnvironment mempty GeneralCompile Nothing target [root] Nothing)
        :: IO (Either SomeException CheckedEnvironmentResult)
      case rejected of
        Left _ -> pure ()
        Right _ -> fail "metadata reused an invalid dependency"
      evicted <- readIORef evictions
      assertEqual "success and rejection each evict their target" 2 (length evicted))
    `finally` maybe (unsetEnv "TIDEPOOL_TIMING") (setEnv "TIDEPOOL_TIMING") previousTiming
  where
    temporary = do
      parent <- getTemporaryDirectory
      (path, handle) <- openTempFile parent "tidepool-metadata"
      hClose handle
      removeFile path
      createDirectory path
      pure path

-- A source-less value interface activates the session pipeline. Its target is
-- the final source consumer, so it must prepare successfully without creating
-- a registration interface solely for itself.
--
-- The ordinary request retains validation facts for its unused import. The
-- later every-module session request prepares that body when it needs it.
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
  previousTiming <- lookupEnv "TIDEPOOL_TIMING"
  setEnv "TIDEPOOL_TIMING" "1"
  (withResidentPipelineSelectedRequests [root] (const (pure ())) $ \runRequest -> do
      (ordinaryPrepared, ordinaryOutput) <- captureStderr root "prepared-session-ordinary" $
        runRequest $ \compiler ->
          compiler PreparedStg mempty GeneralCompile Nothing ordinary [root] Nothing
      unless (all ((/= "SessionUnreachable") . moduleNameString . moduleName . pmModule)
          (pprModules ordinaryPrepared)) $
        fail "ordinary request prepared its unreachable import"
      when ("memo_completion" `isInfixOf` ordinaryOutput) $
        fail "ordinary request performed speculative memo completion"
      (prepared, output) <- captureStderr root "prepared-session-leaf" $
        runRequest $ \compiler ->
          compiler PreparedStg mempty GeneralCompile (Just scope) target [root] Nothing
      when (null (pprModules prepared)) $
        fail "prepared session leaf produced no prepared module"
      assertContains "prepared session leaf skips its unused registration interface"
        "tidepool-prepared-interface-elided module=PreparedSessionLeaf reason=no-later-home-importer"
        output
      when (any (isInfixOf "module=PreparedSessionLeaf")
            (filter (isPrefixOf "tidepool-timing-module-detail ") (lines output))) $
        fail "prepared session leaf constructed an unused target interface"
      when ("tidepool-memo-miss module=SessionUnused" `isInfixOf` output) $
        fail ("session tier recompiled a module the ordinary request memoized: " ++ output)
      when ("tidepool-memo-miss module=SessionConsumer" `isInfixOf` output) $
        fail ("session tier recompiled a consumer the ordinary request memoized: " ++ output)
      assertContains "session tier prepares the previously validation-only import"
        "tidepool-memo-miss module=SessionUnreachable reason=executable-body-not-prepared" output
      unless (any ((== "SessionUnreachable") . moduleNameString . moduleName . pmModule)
          (pprModules prepared)) $
        fail "session tier omitted its required original body"
      previousDrop <- lookupEnv "TIDEPOOL_TEST_DROP_MEMO_INTERFACE"
      forcedLog <- (do
          setEnv "TIDEPOOL_TEST_DROP_MEMO_INTERFACE" "SessionUnused"
          snd <$> captureStderr root "prepared-session-interface-miss" (runRequest $ \compiler ->
            compiler PreparedStg mempty GeneralCompile (Just scope) target [root] Nothing))
        `finally` maybe (unsetEnv "TIDEPOOL_TEST_DROP_MEMO_INTERFACE")
                        (setEnv "TIDEPOOL_TEST_DROP_MEMO_INTERFACE") previousDrop
      assertContains "session tier regenerates producer with missing interface"
        "tidepool-memo-miss module=SessionUnused reason=required-interface-not-retained" forcedLog
      assertContains "session tier invalidates consumer after producer regeneration"
        "tidepool-memo-miss module=SessionConsumer reason=dependency-miss:SessionUnused" forcedLog)
    `finally` maybe (unsetEnv "TIDEPOOL_TIMING") (setEnv "TIDEPOOL_TIMING") previousTiming
  where
    temporary = do
      parent <- getTemporaryDirectory
      (path, handle) <- openTempFile parent "tidepool-prepared-session-leaf"
      hClose handle
      removeFile path
      createDirectory path
      pure path

structuralDisplayCompilation :: FilePath -> IO ()
structuralDisplayCompilation effectsRoot = bracket temporary removeDirectoryRecursive $ \root -> do
  requestMemoLifecycle root
  source <- readFile "test-cell-splitter/DisplayFields.cell.hs"
  plan <- analyzeCell template source >>= either (fail . renderCellSplitError) pure
  let includes = ["lib", "test-cell-splitter", effectsRoot]
  withResidentPipelineSelectedRequests includes (const (pure ())) $ \runRequest -> runRequest $ \compiler -> do
    let compile current = do
          rendered <- either fail pure (renderCellCheckSource template current)
          let path = root </> "CellCheck.hs"
          writeFile path rendered
          compiler CheckedEnvironment mempty GeneralCompile Nothing path includes Nothing
    (accepted, provisional) <- checkCellInstances compile plan
    assertEqual "resolved authored Display instances retained" False
      (any (`elem` map displayTargetName (cellPlanDisplayTargets accepted)) ["Custom", "Reexported"])
    assertEqual "resolved authored Generic instances retained" False
      (any (`elem` map genericDeclarationTarget (cellPlanGenericDeclarations accepted)) ["Authored", "Standalone", "Reexported"])
    assertEqual "unrelated qualified classes do not suppress generated instances" True
      ("ForeignClass" `elem` map displayTargetName (cellPlanDisplayTargets accepted)
        && "ForeignClass" `elem` map genericDeclarationTarget (cellPlanGenericDeclarations accepted))
    assertEqual "specialized custom instance preserves general structure" True
      ("Special" `elem` map displayTargetName (cellPlanDisplayTargets accepted))
    assertEqual "authored Show keeps its presentation" False
      ("Presented" `elem` map displayTargetName (cellPlanDisplayTargets accepted))
    assertEqual "authored Show keeps the automatic Generic" True
      ("Presented" `elem` map genericDeclarationTarget (cellPlanGenericDeclarations accepted))
    assertEqual "unsupported automatic Generic derivations omitted" False
      (any ((`elem` ["Poly", "HiddenPoly", "Unboxed"]) . genericDeclarationTarget) (cellPlanGenericDeclarations accepted))
    contextual <- cellDisplayDeclarations DisplayInstanceContexts provisional accepted
    assertContains "parameter context" "Display a) =>" contextual
    typed <- compile (installCellDisplayDeclarations contextual accepted)
    finalized <- cellDisplayDeclarations DisplayInstanceFields typed accepted
    assertContains "unsupported imported field is not evaluated"
      "displayTree (Fields __tidepoolDisplayField0 _ __tidepoolDisplayField2 __tidepoolDisplayField3)" finalized
    assertContains "unsupported field remains named" "unknown = " finalized
    assertContains "recursive field remains displayable" ".displayTree __tidepoolDisplayField0" finalized
    assertContains "custom instance field remains displayable" ".displayTree __tidepoolDisplayField2" finalized
    assertContains "function field uses its opaque Display instance"
      "displayTree (Functions __tidepoolDisplayField0)" finalized
    assertContains "positional field renders as an application argument"
      ".displayTreePrec 11 __tidepoolDisplayField0" finalized
    assertContains "applied constructor is parenthesized as an argument"
      ".precedenceParens __tidepoolPrecedence" finalized
    assertContains "infix constructor precedence pattern" "(:+:) {} -> " finalized
    assertContains "higher-kinded unsupported field is not evaluated" "displayTree (Higher _)" finalized
    assertContains "symbolic datatype instance head" ".Display ((:+:) a b)" finalized
    assertContains "rank-n field remains opaque" "displayTree (Poly _)" finalized
    assertContains "alias-hidden rank-n field remains opaque" "displayTree (HiddenPoly _)" finalized
    assertContains "unlifted unsupported field remains opaque" "displayTree (Unboxed _)" finalized
    _ <- compile (installCellDisplayDeclarations finalized accepted)
    invalidSource <- readFile "test-cell-splitter/ExplicitInvalidGeneric.cell.hs"
    invalidPlan <- analyzeCell template invalidSource >>= either (fail . renderCellSplitError) pure
    invalid <- try (checkCellInstances compile invalidPlan)
      :: IO (Either SourceError (CellSourcePlan, CheckedEnvironmentResult))
    case invalid of
      Left _ -> pure ()
      Right _ -> fail "explicit invalid Generic instance must remain a user error"
  where
    temporary = do
      parent <- getTemporaryDirectory
      (path, handle) <- openTempFile parent "tidepool-cell-display"
      hClose handle
      removeFile path
      createDirectory path
      pure path
    template = unlines
      [ "{-# LANGUAGE OverloadedStrings, DeriveGeneric, StandaloneDeriving, FlexibleInstances, FlexibleContexts, UndecidableInstances #-}"
      , "{{CELL_PRAGMAS}}"
      , "module CellCheck where"
      , "{{CELL_IMPORTS}}"
      , "{{CELL_DECLS}}"
      , "__tidepool_cell_check = do { {{CELL_BODY}} } :: Maybe ()"
      ]

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
  createDirectoryIfMissing True dependencyDir
  writeFile dependencyPath validDependency
  writeFile targetPath $ unlines
    [ "module MemoTarget where"
    , "import Tidepool.Session.Lib.G1 (dependency)"
    , "result :: Int"
    , "result = dependency + 1"
    ]
  writeFile otherTargetPath "module MemoOther where\nother :: Int\nother = 2\n"
  previousTiming <- lookupEnv "TIDEPOOL_TIMING"
  setEnv "TIDEPOOL_TIMING" "1"
  (withResidentPipelineSelectedRequests [root] (const (pure ())) $ \runRequest -> do
      -- Session entries are reusable only within one incarnation; an
      -- incarnation-less request never reuses one and never keeps one.
      let compileIn scope purpose compiler = compiler PreparedStg mempty purpose scope targetPath [root] Nothing
          compile = compileIn Nothing
          incarnate = Just (SessionScope root [] Nothing (Just "7"))
          sessionMiss = "tidepool-memo-miss module=Tidepool.Session.Lib.G1"
          absentSession = sessionMiss ++ " reason=absent"
          targetMiss = "tidepool-memo-miss module=MemoTarget"
      (_, anonymousLog) <- captureStderr root "memo-anonymous"
        (runRequest $ \compiler -> compile GeneralCompile compiler)
      assertContains "cold request compiles the session dependency" absentSession anonymousLog
      (_, nextRequestLog) <- captureStderr root "memo-next-request"
        (runRequest $ \compiler -> compile GeneralCompile compiler)
      assertContains "normal request exit evicts incarnation-less session entries"
        absentSession nextRequestLog
      failedRequest <- try (captureStderr root "memo-exception" $ runRequest $ \compiler -> do
        _ <- compile GeneralCompile compiler
        throwIO (userError "request failure after compile"))
        :: IO (Either SomeException (PipelineResult, String))
      case failedRequest of
        Left _ -> pure ()
        Right _ -> fail "exception cleanup probe unexpectedly succeeded"
      (_, afterExceptionLog) <- captureStderr root "memo-after-exception"
        (runRequest $ \compiler -> compile GeneralCompile compiler)
      assertContains "exceptional request exit evicts incarnation-less session entries"
        absentSession afterExceptionLog
      runRequest $ \compiler -> do
        (_, coldLog) <- captureStderr root "memo-cold" (compileIn incarnate GeneralCompile compiler)
        assertContains "incarnation's first request compiles the session dependency" absentSession coldLog
        assertContains "cold request compiles its target" targetMiss coldLog
        (_, warmLog) <- captureStderr root "memo-warm" (compileIn incarnate LookupTypeCompile compiler)
        when (sessionMiss `isInfixOf` warmLog) $
          fail ("unchanged session dependency was not reused within one worker request: " ++ warmLog)
        assertContains "internal compile evicts its purpose-sensitive target" targetMiss warmLog
        writeFile dependencyPath invalidDependency
        (changed, changedLog) <- captureStderr root "memo-changed"
          (try (compileIn incarnate GeneralCompile compiler) :: IO (Either DependencyLoadFailure PreparedPipelineResult))
        case changed of
          Left (DependencySourceFailure diagnostics) ->
            unless (any (\diagnostic -> dSeverity diagnostic == DiagError
                && dFile diagnostic == Just (dependencyPath, 3, 14, 3, 21)) diagnostics) $
              fail "changed invalid session dependency lost its exact source error"
          Left DependencyWorkerFailure -> fail "changed dependency became a worker failure"
          Right _ -> fail "changed invalid session dependency reused a stale memo entry"
        -- Failure in GHC's load barrier precedes the prepared-front memo log.
        assertContains "changed source was checked rather than a stale memo entry"
          "Variable not in scope: missing :: Int" changedLog
      writeFile dependencyPath validDependency
      (restored, _) <- captureStderr root "memo-incarnate-restored"
        (runRequest $ \compiler -> compileIn incarnate GeneralCompile compiler)
      -- A transaction with no incarnation (a lookup, a plain eval) must not
      -- discard another incarnation's session entries: in a live daemon one
      -- lands between most pairs of a session's transactions.
      _ <- captureStderr root "memo-anonymous-between"
        (runRequest $ \compiler ->
          compiler PreparedStg mempty GeneralCompile Nothing otherTargetPath [root] Nothing)
      (incarnateWarm, incarnateWarmLog) <- captureStderr root "memo-incarnate-warm"
        (runRequest $ \compiler -> compileIn incarnate GeneralCompile compiler)
      when (sessionMiss `isInfixOf` incarnateWarmLog) $
        fail ("an incarnation-less transaction evicted another incarnation's session entry: "
          ++ incarnateWarmLog)
      assertEqual "memo sanitization preserves complete prepared output"
        (length (pprModules restored), length (prBinds (pprPipelineResult restored)))
        (length (pprModules incarnateWarm), length (prBinds (pprPipelineResult incarnateWarm))))
    `finally` maybe (unsetEnv "TIDEPOOL_TIMING") (setEnv "TIDEPOOL_TIMING") previousTiming

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
-- through the typed dependency-load diagnostic path that failed GHC loads
-- render through (see @app/Main.hs@'s @reportDiags@) and checks the
-- rendered message names both qualified candidates and both fixes.
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
  withResidentPipelineSelectedRequests [root] (const (pure ())) $ \runRequest -> do
    rejected <- try (runRequest $ \compiler ->
        compiler CheckedEnvironment mempty GeneralCompile Nothing targetPath [root] Nothing)
      :: IO (Either DependencyLoadFailure CheckedEnvironmentResult)
    case rejected of
      Right _ -> fail "ambiguous candidateSummary occurrence unexpectedly compiled"
      Left DependencyWorkerFailure -> fail "ambiguous occurrence became a worker failure"
      Left (DependencySourceFailure diagnostics) -> do
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
