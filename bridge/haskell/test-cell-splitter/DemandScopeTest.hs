module DemandScopeTest (demandScopeChecks) where

import Control.Exception (bracket)
import Control.Monad (forM_, unless, foldM)
import Control.Monad.IO.Class (liftIO)
import Data.List (sort)
import qualified Data.Map.Strict as Map
import qualified Data.ByteString as BS
import GHC
import GHC.Driver.Backend (noBackend)
import GHC.Types.Name (nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import System.Directory
  ( createDirectory, getTemporaryDirectory, removeDirectoryRecursive, removeFile )
import System.FilePath ((</>))
import System.IO (hClose, openTempFile)
import Tidepool.Binders (sourceModuleFlags)
import Tidepool.ExactHydration
import Tidepool.ExtractUtil (getLibdir)
import Tidepool.TypedSegment.Source
import Tidepool.TypedSegment.Types

-- These exercise the production demand owner with ordinary data as bindings.
-- World/checked-value authority is exercised separately by the real service
-- consumer; lexical scope tests must not fabricate that authority.
demandScopeChecks :: IO ()
demandScopeChecks = do
  libdir <- getLibdir
  temporary <- getTemporaryDirectory
  let acquire = do
        (path,handle) <- openTempFile temporary "tidepool-demand-scope"
        hClose handle
        removeFile path
        createDirectory path
        pure path
  bracket acquire removeDirectoryRecursive $ \directory ->
    runGhc (Just libdir) $ do
      initial <- getSessionDynFlags
      forM_ controls $ \(name,root,aliases,expected) -> do
        source <- liftIO $ case name of
          "DeclarationModule" -> pure (unlines
            ["module DeclarationModule where", "data Box = Box Int", "refer = old"])
          "QualifiedOperand" -> pure (unlines
            ["module QualifiedOperand where", "import qualified Data.Version as Operand", "root = Operand.makeVersion"])
          "SameLabelOperand" -> pure (unlines
            ["module SameLabelOperand where", "import qualified Data.Version as Operand"
            ,"root = (input, input, Operand.makeVersion, Operand.makeVersion)"])
          "UnusedHelperOperand" -> pure (unlines
            ["module UnusedHelperOperand where", "import qualified Data.Version as Operand"
            ,"helper = Operand.makeVersion", "root = ()"])
          "UnusedOperand" -> pure (unlines
            ["module UnusedOperand where", "import qualified Data.Version as Operand", "root = ()"])
          "ShadowOperand" -> pure (unlines
            ["module ShadowOperand where", "import qualified Data.Version as Operand"
            ,"root = let versionBranch value = value in versionBranch ()"])
          "MalformedModule" -> pure (unlines
            ["module MalformedModule where", "root = ("])
          "DeclarationQuote" -> pure (unlines
            ["{-# LANGUAGE QuasiQuotes #-}", "module DeclarationQuote where"
            ,"import qualified MissingDemandQuoteInterface as NeverLoad"
            ,"[doesNotExist| refuse before rename |]"])
          _ -> readFile ("test-cell-splitter/fixtures/demand-scope" </> name ++ ".hs")
        actual <- liftIO (sourceModuleFlags initial source)
          >>= either (liftIO . fail . show) pure
        _ <- setSessionDynFlags actual {backend=noBackend,ghcLink=NoLink}
        actualOperand <- if name `elem` ["QualifiedOperand","SameLabelOperand","UnusedHelperOperand","UnusedOperand","ShadowOperand"] then do
          owner <- findModule (mkModuleName "Data.Version") Nothing
          information <- getModuleInfo owner >>= maybe (liftIO (fail "missing actual Data.Version interface")) pure
          case [exported | exported <- modInfoExports information,occNameString (nameOccName exported) == "makeVersion"] of
            [exported] -> pure (Just exported)
            _ -> liftIO (fail "operand fixture lacks its unique actual exported Name")
          else pure Nothing
        environment <- getSession
        liftIO $ do
          let path = directory </> name ++ ".hs"
              bindings = Map.fromList (zip aliases [0 :: Int ..])
          writeFile path source
          recipe <- generatedCheckingTemplateRecipe actual (CheckedTemplateImports [] [])
            source source path name >>= either fail pure
          captured <- captureGeneratedScaffoldSource recipe path >>= either fail pure
          let addOperand scope = case actualOperand of
                Nothing -> pure scope
                Just exported -> either (fail . show) pure (associateSegmentDemandName exported "input" 999 scope)
          initialScope <- newSegmentDemandScope environment bindings
          scope <- addOperand initialScope
          forM_ actualOperand $ \exported -> do
            case associateSegmentDemandName exported "other" 1000 scope of
              Left ConflictingDemandName -> pure ()
              _ -> fail "conflicting exact Name association was accepted"
            case associateSegmentDemandName exported "input" 999 scope of
              Left ConflictingDemandName -> pure ()
              _ -> fail "duplicate exact Name association was accepted"
          case fmap prepareModuleDemand (parseCapturedGeneratedImports actual captured) of
            Left diagnostic
              | expected == SyntaxRefusal && capturedGeneratedScaffoldPath captured == path
              , diagnostic `elem` ["GHC could not parse the captured generated root"
                  ,"GHC reported errors in the captured generated root"] ->
                  putStrLn (name ++ ": " ++ diagnostic)
            Left diagnostic -> fail (name ++ ": wrong capture parsing refusal " ++ diagnostic)
            Right (Left (ModuleDemandQuasiQuoteRejected actualPath quotes))
              | expected == QuoteRefusal && actualPath == path && quotes == ["doesNotExist"] -> pure ()
            Right (Left failure) -> fail (name ++ ": wrong demand refusal " ++ show failure)
            Right (Right _) | expected `elem` [SyntaxRefusal,QuoteRefusal] ->
              fail (name ++ ": refused source passed the parser gate")
            Right (Right parsed) -> do
              let sealedPath = capturedGeneratedScaffoldPath captured
                  sealedBytes = capturedGeneratedScaffoldBytes captured
              unless (moduleDemandMatchesSource parsed sealedPath sealedBytes
                  && not (moduleDemandMatchesSource parsed (sealedPath ++ ".other") sealedBytes)
                  && not (moduleDemandMatchesSource parsed sealedPath (sealedBytes <> BS.singleton 0)))
                (fail (name ++ ": quote proof lost its exact source identity"))
              demanded <- moduleInputOccurrences environment scope parsed
              if name == "ScopeControl" then do
                let makePlan checker = either (fail . show) pure
                      (typedSegmentPlan (replicate 64 '0') checker
                        [TypedItemPlan 0 "item" 1 (LetItem "marker" [])])
                plan <- makePlan root
                typed <- either (fail . show) pure
                  (prepareSegmentDemand plan (moduleDemandCapturedImports parsed))
                typedDemand <- typedSegmentInputOccurrences environment scope typed
                unless (typedDemand == demanded
                    && moduleDemandMatchesSource (segmentModuleDemand typed) sealedPath sealedBytes
                    && segmentDemandMatchesSource typed sealedPath sealedBytes)
                  (fail "typed root wrapper changed its module proof or demand")
                wrong <- makePlan "missingTypedCheckerRoot"
                case prepareSegmentDemand wrong (moduleDemandCapturedImports parsed) of
                  Left UnprovedRootAbstraction -> pure ()
                  _ -> fail "typed root wrapper admitted a plan without its unique checker root"
              else pure ()
              case (expected,demanded) of
                (Bindings names,Right selected) | map fst selected == names -> pure ()
                (RenameRefusal,Left (InputDemandRenameRejected diagnostic))
                  | not (null diagnostic) -> putStrLn (name ++ ": " ++ diagnostic)
                _ -> fail (name ++ ": wrong demand or refusal phase: " ++ show demanded)
              case demanded of
                Right selected | name == "SameLabelOperand" ->
                  unless (sort selected == [("input",0),("input",999)])
                    (fail "distinct resolved Names sharing a label lost their original binding payloads")
                _ -> pure ()
              _ <- foldM (\(previous,previousBindings) changes -> do
                updated <- updateSegmentDemandScope environment changes previous
                let nextBindings = Map.mapMaybe id (Map.union (Map.fromList changes)
                      (Map.map Just previousBindings))
                rebuilt <- newSegmentDemandScope environment nextBindings >>= addOperand
                incrementalDemand <- moduleInputOccurrences environment updated parsed
                rebuiltDemand <- moduleInputOccurrences environment rebuilt parsed
                unless (incrementalDemand == rebuiltDemand)
                  (fail (name ++ ": alias delta differs from a fresh GHC scope"))
                pure (updated,nextBindings)) (scope,bindings)
                  [ [("old",Just 100)]
                  , [("id",Just 101),("old",Nothing)]
                  , [("old",Just 102),("pure",Just 103)]
                  , [("id",Nothing),("pure",Nothing),("old",Nothing)] ]
              forM_ demanded $ \selected -> forM_ selected $ \(alias,binding) ->
                unless (Map.lookup alias bindings == Just binding || (actualOperand /= Nothing && alias == "input" && binding == 999))
                  (fail (name ++ ": lost exact alias/binding association"))
          writeFile path (source ++ "\nchanged = ()\n")
          stale <- captureGeneratedScaffoldSource recipe path
          case stale of
            Left _ -> pure ()
            Right _ -> fail (name ++ ": source drift was accepted")
      observationContributionChecks directory initial
      liftIO (putStrLn ("demand scope: " ++ show (length controls) ++ " controls passed"))

-- Actual rendered operands must remain inside their single compiler-owned
-- declaration slot. LINE pragmas cannot move a physical suffix into custody.
observationContributionChecks :: FilePath -> DynFlags -> Ghc ()
observationContributionChecks directory flags = liftIO $ do
  let prefix = "module ObservationContribution where\n-- \955 \26085\nprotected = ()\n"
      inert = "(let observation = observation in observation)"
      typeHeader = "\n__tidepool_type_search_query :: " :: String
      valueHeader = "__tidepool_type_search_query = "
      expressionHeader = "\n__tidepool_inspect_0 = (\n"
      typeCase operand accepted =
        (ObservationTypeOperand (length prefix + length typeHeader + length operand + 1),
          typeHeader ++ operand ++ "\n" ++ valueHeader ++ inert ++ "\n",accepted)
      expressionCase operand accepted =
        (ObservationExpressionOperand,expressionHeader ++ unlines (map ("  " ++) (lines operand)) ++ "  )\n",accepted)
      cases =
        [ typeCase "Int" True
        , typeCase "Int\ninjected = True" False
        , typeCase "Int\n{-# LINE 1 \"trusted\" #-}\ninjected = True" False
        , typeCase "Int\ninstance Show Int where" False
        , typeCase "Int\n__tidepool_type_search_query = 1" False
        , expressionCase "let local = (1 :: Int) in local" True
        , expressionCase "() ); injected = True; closer = (" False
        , expressionCase "() ); {-# LINE 1 \"trusted\" #-} injected = True; closer = (" False
        ]
  forM_ (zip [0 :: Int ..] cases) $ \(ordinal,(mode,suffix,accepted)) -> do
    let source = prefix ++ suffix
        name = "ObservationContribution"
        path = directory </> name ++ ".hs"
    writeFile path source
    recipe <- generatedCheckingTemplateRecipe flags (CheckedTemplateImports [] [])
      prefix source path name >>= either fail pure
    captured <- captureGeneratedScaffoldSource recipe path >>= either fail pure
    capturedImports <- either (fail . ("operand capture " ++)) pure
      (parseCapturedGeneratedImports flags captured)
    parsed <- either (fail . (("operand control " ++ show ordinal ++ " must parse before its contribution check: ") ++) . show) pure
      (prepareModuleDemand capturedImports)
    let actual = validateObservationContribution (length prefix) mode [] parsed
    unless (case actual of Right () -> accepted; Left _ -> not accepted)
      (fail ("wrong observation contribution result " ++ show ordinal ++ ": " ++ show actual))
  putStrLn ("observation contribution: " ++ show (length cases) ++ " physical-boundary controls passed")

data ExpectedDemand = Bindings [String] | SyntaxRefusal | QuoteRefusal | RenameRefusal
  deriving (Eq, Show)

controls :: [(String,String,[String],ExpectedDemand)]
controls =
  [ ("ScopeControl","__tidepool_segment_check",["old","id","pure","field"],Bindings ["id","old","pure"])
  , ("ImportedMethod","root",["old"],Bindings ["old"])
  , ("LocalShadow","root",["old"],Bindings [])
  , ("DuplicateFields","root",["field","old"],Bindings [])
  , ("ClassShadow","root",["old"],Bindings [])
  , ("ImportedRecord","root",["old"],Bindings ["old"])
  , ("LocalRecord","root",["old","field"],Bindings ["old"])
  , ("ImportedFieldShadow","root",["old"],Bindings ["old"])
  , ("ParseOnlyQuoteFlag","root",["old"],Bindings [])
  , ("QualifiedField","root",["versionBranch","old"],Bindings ["old"])
  , ("AliasNamespace","root",["versionBranch"],Bindings [])
  , ("AliasSynonyms","root",["first","second"],Bindings ["first","second"])
  , ("RenameError","root",["old"],RenameRefusal)
  , ("MissingImport","root",["old"],RenameRefusal)
  , ("MalformedModule","root",[],SyntaxRefusal)
  , ("ActualQuote","root",[],QuoteRefusal)
  , ("QualifiedOperand","root",[],Bindings ["input"])
  , ("SameLabelOperand","root",["input"],Bindings ["input","input"])
  , ("UnusedHelperOperand","root",[],Bindings ["input"])
  , ("UnusedOperand","root",[],Bindings [])
  , ("ShadowOperand","root",[],Bindings [])
  , ("DeclarationModule","",["old"],Bindings ["old"])
  , ("DeclarationQuote","",[],QuoteRefusal)
  ]
