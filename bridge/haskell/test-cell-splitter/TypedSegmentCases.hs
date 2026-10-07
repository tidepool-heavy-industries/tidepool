module TypedSegmentCases (typedSegmentRewriteSemantics, typedSegmentNativePreparation) where

import Control.Exception (SomeException, bracket, evaluate, fromException, try)
import Control.Monad (forM, forM_, unless, void)
import Control.Monad.IO.Class (liftIO)
import Data.List (isPrefixOf)
import Data.Maybe (isJust)
import qualified Data.Set as Set
import GHC
import GHC.Core (bindersOfBinds)
import GHC.Driver.Session (PackageDBFlag(..), PkgDbRef(..))
import GHC.Builtin.Types (intTy)
import GHC.Core.TyCo.Compare (eqType)
import GHC.Types.Name (getOccString, nameModule_maybe)
import GHC.Types.Var (varName)
import qualified GHC.Stg.Syntax as Stg
import GHC.Types.SourceError (SourceError)
import GHC.Tc.Utils.TcType (tcSplitSigmaTy)
import GHC.Utils.Outputable (ppr, showSDocUnsafe)
import System.Directory
  ( createDirectory, getTemporaryDirectory, removeDirectoryRecursive, removeFile )
import System.FilePath ((</>))
import System.IO (hClose, openTempFile)
import Tidepool.Binders
import Tidepool.ExtractUtil (getLibdir)
import Tidepool.DiagJson (diagsFromSourceError)
import Tidepool.GhcPipeline
import Tidepool.PreparedStg (pmModule, pmBindings, pmOriginalTopNames)
import Tidepool.SessionArtifacts
import Tidepool.Test.Runner (requiredInput)
import Tidepool.TurnSource (preambleDefaultDeclaration)
import qualified Tidepool.TypedSegment as TypedSegment
import Tidepool.TypedSegment.Source (rewriteParsedSegmentRoot)
import Unsafe.Coerce (unsafeCoerce)

-- The owning selector performs the actual whole-source frontend, typed
-- extraction, real thin-interface hydration, simplify and prepared STG.
-- Execution of those item roots belongs to the runtime semantic suite.
typedSegmentNativePreparation :: IO ()
typedSegmentNativePreparation = bracket temporary removeDirectoryRecursive $ \root -> do
  effects <- requiredInput "TIDEPOOL_TEST_EFFECTS_DIR"
  prelude <- requiredInput "TIDEPOOL_PRELUDE_DIR"
  libdir <- getLibdir
  flags <- runGhc (Just libdir) getSessionDynFlags
  let fixtures = "test-cell-splitter/fixtures/typed-segment-native"
      includes = [root, fixtures, effects, prelude]
  withResidentPipelineSelectedRequests includes $ \runRequest ->
    forM_ (zip [0 :: Int ..] preparationCases) $ \(index, (name, expected)) -> do
      body <- readFile (fixtures </> name ++ ".hs")
      let template = preparationTemplate ("TypedPrepared" ++ show index)
      sourcePlan <- analyzeOrderedCellWithFlags flags template body >>= either (fail . show) pure
      let slots = [(ordinal, fromIntegral (index * 100 + ordinal + 1), observation ordinal item)
            | (ordinal, item) <- zip [0 ..] (cellPlanItems sourcePlan)]
          observation ordinal item = case sbKind (cellAnalysisVerdict item) of
            KExpr -> Just ("__typedObservation" ++ show ordinal)
            _ -> Nothing
      source <- either fail pure (prepareTypedSegmentSource template sourcePlan (replicate 64 'c') slots)
      let path = root </> "TypedPrepared" ++ show index ++ ".hs"
          plan = preparedTypedSegmentPlan source
          prepare environment admissions segment = do
            batch <- prepareTypedSegmentSessionBindings environment admissions segment
              (root </> "stage" ++ show index)
            pure (typedSegmentSessionEnvironment batch, typedSegmentSessionGlobals batch,
              typedSegmentSessionInterfaces batch)
      writeFile path (preparedTypedSegmentSource source)
      result <- try (runRequest (pure ()) $ \compiler -> compiler
        (WithTypedSegmentPreparation prepare (PreparedSegmentProducts plan Nothing))
        mempty (TypedSegmentCompile plan (preparedTypedSegmentOperations source) GeneralCompile) Nothing path includes Nothing)
          :: IO (Either SomeException PreparedSegmentProductsResult)
      case (expected, result) of
        (PrepareAccepted, Right products) -> do
          let segment = preparedSegmentCaptures products
              issued = bindersOfBinds (TypedSegment.typedSegmentRoots segment)
          owner <- case Set.toList (Set.fromList [owner
              | identifier <- issued, Just owner <- [nameModule_maybe (varName identifier)]]) of
            [owner] -> pure owner
            _ -> fail (name ++ ": issued item/ABI roots do not have one actual module owner")
          target <- case [modul | modul <- pprModules (preparedSegmentProducts products), pmModule modul == owner] of
            [modul] -> pure modul
            _ -> fail (name ++ ": actual prepared target module is absent or ambiguous")
          let emitted = Set.fromList [varName identifier
                | (Stg.StgTopLifted binding, _) <- pmBindings target
                , identifier <- case binding of
                    Stg.StgNonRec identifier _ -> [identifier]
                    Stg.StgRec bindings -> map fst bindings]
          forM_ issued $ \identifier -> unless
              (varName identifier `Set.member` pmOriginalTopNames target
                && varName identifier `Set.member` emitted)
            (fail (name ++ ": an actual issued item/ABI entry was lost during prepared lowering"))
          -- Generalization/defaulting controls look at the GHC-issued value
          -- type, never a type moved from the enclosing action's quantifiers.
          let captures = concatMap TypedSegment.typedItemCaptures
                (TypedSegment.typedSegmentItems (preparedSegmentCaptures products))
              capture binder = case [TypedSegment.typedCaptureType value | value <- captures
                  , getOccString (TypedSegment.typedCaptureIdentifier value) == binder] of
                [value] -> pure value
                _ -> fail (name ++ ": missing unique semantic witness " ++ binder)
          case name of
            "numeric-action-mr" -> capture "number" >>= assertInt name
            "numeric-action-nomr" -> capture "number" >>= assertInt name
            "read-later" -> capture "number" >>= assertInt name
            "num-scalar-nomr" -> do
              ty <- capture "number"
              let (variables, predicates, _) = tcSplitSigmaTy ty
              unless (not (null variables) && not (null predicates) && isClosureType ty)
                (fail "numeric let lost its genuine dictionary-taking value sigma")
            _ -> pure ()
        (PrepareTypedRefusal, Left exception)
          | Just failure <- fromException exception, captureRefusal failure -> pure ()
        (PrepareSourceRefusal, Left exception)
          | Just sourceError <- fromException exception ->
              unless (not (null (diagsFromSourceError (sourceError :: SourceError))))
                (fail (name ++ ": GHC source refusal has no diagnostics"))
        (_, Left exception) -> fail (name ++ ": unexpected compiler failure " ++ show exception)
        (_, Right _) -> fail (name ++ ": expected compiler refusal was accepted")
  putStrLn ("native typed preparation: " ++ show (length preparationCases) ++ " fixture requests")
  where
    assertInt name ty = unless (eqType ty intTy) (fail (name ++ ": actual capture is not Int"))
    captureRefusal failure = case (failure :: TypedSegment.TypedSegmentFailure) of
      TypedSegment.UnresolvedItemType _ -> True
      TypedSegment.OpenCaptureType _ _ -> True
      TypedSegment.OpenItemCore _ _ -> True
      _ -> False

data PreparationExpected = PrepareAccepted | PrepareTypedRefusal | PrepareSourceRefusal

preparationCases :: [(String, PreparationExpected)]
preparationCases =
  [(name, PrepareAccepted) | name <-
    [ "let-poly", "num-mr", "num-nomr", "num-scalar-nomr"
    , "numeric-action-mr", "numeric-action-nomr", "read-later", "dependent-later"
    , "nondefaultable-let", "rank-n-value", "explicit-poly", "phantom-let"
    , "scoped-equality", "nested-existential", "patterns", "strict-patterns"
    , "lazy-nested-bottom", "applied-original", "substitution-shadow", "bare-bottom"
    , "constrained-open-let", "zero-let-lazy", "zero-let-bang", "zero-let-strict"
    , "strict-closure-let", "wildcard-action", "bang-wildcard-action"
    , "refutable-failure", "authored-helper-alias" ]]
  ++ [(name, PrepareTypedRefusal) | name <-
    [ "unresolved-action", "unresolved-dependent"
    , "unresolved-phantom-action", "open-let", "cross-item-existential" ]]
  ++ [(name, PrepareSourceRefusal) | name <- ["wrong-row", "wrong-observation-row", "unresolved-read"]]

preparationTemplate :: String -> String
preparationTemplate owner = unlines
  [ "{-# LANGUAGE GHC2024, NamedDefaults, ScopedTypeVariables, TypeApplications, BangPatterns, UndecidableInstances, ExtendedDefaultRules #-}"
  , "{{CELL_PRAGMAS}}"
  , "module " ++ owner ++ " where"
  , "import Control.Monad.Freer (Eff)"
  , "import Data.Proxy"
  , "import Data.Typeable"
  , "import Data.Text (Text)"
  , "import qualified GHC.TypeError as TidepoolWorkbenchTypeError"
  , "import TypedSegmentSupport"
  , "import Tidepool.Effects.Core ()"
  , "{{CELL_IMPORTS}}"
  , preambleDefaultDeclaration
  , "default Applicative (Eff '[])"
  , "default Monad (Eff '[])"
  , "class TidepoolCellPure value"
  , "instance {-# OVERLAPPABLE #-} TidepoolCellPure value"
  , "instance {-# OVERLAPPING #-} TidepoolWorkbenchTypeError.Unsatisfiable ('TidepoolWorkbenchTypeError.Text \"an Eff action must use the current workbench effect row\") => TidepoolCellPure (Eff effects value)"
  , "class TidepoolCellExpression value where { __tidepoolCellExpression :: value -> Eff '[] () }"
  , "instance {-# OVERLAPPING #-} (effects ~ '[]) => TidepoolCellExpression (Eff effects value) where { __tidepoolCellExpression action = action >> pure () }"
  , "instance {-# OVERLAPPABLE #-} TidepoolCellPure value => TidepoolCellExpression value where { __tidepoolCellExpression _ = pure () }"
  , "{{CELL_DECLS}}"
  , "__tidepool_cell_check :: Eff '[] ()"
  , "__tidepool_cell_check = do {"
  , "{{CELL_BODY}}"
  , "; pure () }"
  ]

-- This executes the production parsed-source transformation, separately
-- from item extraction and runtime publication. Both programs run with an
-- independent IO trace interpreter; fixed expected outcomes distinguish a
-- shared error in the original and rewritten execution from equivalence.
typedSegmentRewriteSemantics :: IO ()
typedSegmentRewriteSemantics = bracket temporary removeDirectoryRecursive $ \root -> do
  effects <- requiredInput "TIDEPOOL_TEST_EFFECTS_DIR"
  prelude <- requiredInput "TIDEPOOL_PRELUDE_DIR"
  libdir <- getLibdir
  let fixtures = "test-cell-splitter/fixtures/typed-segment-native"
      includes = [root, fixtures, "test-cell-splitter/fixtures/typed-segment-oracle", effects, prelude]
  runGhc (Just libdir) $ do
    flags <- getSessionDynFlags
    void (setSessionDynFlags flags
      { backend = interpreterBackend, ghcLink = LinkInMemory, importPaths = includes
      , packageDBFlags = [PackageDB GlobalPkgDb, ClearPackageDBs] })
    sources <- liftIO $ forM (zip [0 :: Int ..] differentialCases) $ \(index, control) -> do
      body <- readFile (fixtures </> differentialFixture control ++ ".hs")
      let ordinary = "TypedOrdinary" ++ show index
          rewritten = "TypedRewritten" ++ show index
          (pragmas, statements) = span (isPrefixOf "{-# LANGUAGE") (lines body)
          source = unlines pragmas ++ differentialBefore control
            ++ unlines statements ++ differentialAfter control
          template = oracleTemplate ordinary
      parsed <- analyzeOrderedCellWithFlags flags template source >>= either (fail . show) pure
      let slots = [(ordinal, fromIntegral (ordinal + 1), Nothing)
            | ordinal <- [0 .. length (cellPlanItems parsed) - 1]]
      prepared <- either fail pure
        (prepareTypedSegmentSource template parsed (replicate 64 'b') slots)
      let original = preparedTypedSegmentSource prepared
          sourcePath = root </> ordinary ++ ".hs"
      writeFile sourcePath (original ++ oracleEntry (preparedTypedSegmentPlan prepared))
      pure (control, ordinary, rewritten, sourcePath, prepared)
    originalTargets <- mapM (\(_, _, _, path, _) -> guessTarget path Nothing Nothing) sources
    setTargets originalTargets
    void (depanal [] False)
    rewrittenPaths <- forM sources $ \(_, ordinary, rewritten, _, prepared) -> do
      summary <- getModSummary (mkModuleName ordinary)
      parsed <- parseModule summary
      transformed <- liftIO (rewriteParsedSegmentRoot (preparedTypedSegmentOperations prepared)
        (preparedTypedSegmentPlan prepared) parsed)
      -- Preserve the actual source pragmas: ParsedModule's printed AST does
      -- not include the header's LANGUAGE directives. Only the module owner
      -- changes so both programs can be loaded in the same interpreter.
      let pragmas = unlines (takeWhile (not . isPrefixOf "module ")
            (lines (preparedTypedSegmentSource prepared)))
          rendered = unlines [if ("module " ++ ordinary ++ " ") `isPrefixOf` line
            then "module " ++ rewritten ++ " where" else line
            | line <- lines (showSDocUnsafe (ppr (pm_parsed_source transformed)))]
          path = root </> rewritten ++ ".hs"
      liftIO (writeFile path (pragmas ++ rendered))
      pure path
    rewrittenTargets <- mapM (\path -> guessTarget path Nothing Nothing) rewrittenPaths
    setTargets (originalTargets ++ rewrittenTargets)
    loaded <- load LoadAllTargets
    case loaded of Failed -> liftIO (fail "differential oracle programs failed to load"); Succeeded -> pure ()
    forM_ sources $ \(control, ordinary, rewritten, _, _) -> do
      original <- execute ordinary
      transformed <- execute rewritten
      liftIO $ do
        let expected = (differentialTrace control, differentialFailure control)
            outcome (values, exception) = (values, isJust exception)
        unless (outcome original == expected)
          (fail (differentialFixture control ++ ": ordinary GHC result " ++ show original ++ " /= " ++ show expected))
        unless (outcome transformed == expected)
          (fail (differentialFixture control ++ ": production rewrite result " ++ show transformed ++ " /= " ++ show expected))
        unless (original == transformed)
          (fail (differentialFixture control ++ ": production rewrite changed the complete language exception message/category"))
    liftIO (putStrLn ("production rewrite differential: " ++ show (length sources)
      ++ " histories, " ++ show (2 * length sources) ++ " executed programs"))
  where
    execute owner = do
      setContext [IIDecl (simpleImportDecl (mkModuleName owner))]
      value <- compileExpr "(oracleRun :: IO ([Int], Maybe (Either String String)))"
      result <- liftIO (unsafeCoerce value :: IO ([Int], Maybe (Either String String)))
      -- Finish the shared base-type result before changing interpreter scope.
      void (liftIO (evaluate (length (show result))))
      pure result

data DifferentialCase = DifferentialCase
  { differentialFixture :: String
  , differentialBefore :: String
  , differentialAfter :: String
  , differentialTrace :: [Int]
  , differentialFailure :: Bool
  }

differentialCases :: [DifferentialCase]
differentialCases =
  [ success "let-poly" "_ <- record intValue\n_ <- record (if boolValue then 1 else 0)\n_ <- record constantValue\n" [3, 1, 7]
  , success "num-mr" "_ <- record intValue\n_ <- record doubleValue\n" [3, 4]
  , success "num-nomr" "_ <- record intValue\n_ <- record doubleValue\n" [3, 4]
  , success "num-scalar-nomr" "_ <- record intValue\n_ <- record doubleValue\n" [9, 11]
  , success "read-later" "_ <- record answer\n" [8]
  , success "dependent-later" "_ <- record answer\n" [9]
  , success "patterns" "_ <- record answer\n" [134]
  , success "substitution-shadow" "_ <- record first\n_ <- record second\n" [4, 13]
  , success "lazy-nested-bottom" "_ <- record answer\n" [7]
  , success "nested-existential" "_ <- record (if name == \"Int\" then 41 else 0)\n" [41]
  , success "cross-item-existential" "_ <- record (if name == \"Int\" then 41 else 0)\n" [41]
  , success "rank-n-value" "_ <- record intValue\n_ <- record doubleValue\n" [9, 11]
  , success "nondefaultable-let" "_ <- record intValue\n_ <- record doubleValue\n" [17, 20]
  , success "scoped-equality" "_ <- record answer\n" [12]
  , success "zero-let-lazy" "_ <- record 2\n" [1, 2]
  , failure "zero-let-bang"
  , failure "zero-let-strict"
  , failure "strict-closure-let"
  , success "wildcard-action" "_ <- record 2\n" [1, 2]
  , failure "bang-wildcard-action"
  , failure "refutable-failure"
  , DifferentialCase "applied-original" "_ <- record 1\n" "_ <- record answer\n" [1] True
  , success "authored-helper-alias" "_ <- record answer\n" [9]
  ]
  where
    success name after values = DifferentialCase name (before name) after values False
    failure name = DifferentialCase name "_ <- record 1\n" "_ <- record 2\n" [1] True
    before name | name `elem` ["zero-let-lazy", "wildcard-action"] = "_ <- record 1\n"
                | otherwise = ""

oracleTemplate :: String -> String
oracleTemplate owner = unlines
  [ "{-# LANGUAGE GHC2024, NamedDefaults, ScopedTypeVariables, TypeApplications, BangPatterns, ExtendedDefaultRules #-}"
  , "{{CELL_PRAGMAS}}"
  , "module " ++ owner ++ " where"
  , "import Control.Monad.Freer (Eff)"
  , "import Data.Proxy"
  , "import Data.Typeable"
  , "import Data.Text (Text)"
  , "import TypedSegmentSupport"
  , "import TypedSegmentOracleSupport"
  , "import Tidepool.Effects.Core ()"
  , "{{CELL_IMPORTS}}"
  , preambleDefaultDeclaration
  , "default Applicative (Eff '[IO])"
  , "default Monad (Eff '[IO])"
  , "{{CELL_DECLS}}"
  , "__tidepool_cell_check :: Eff '[IO] ()"
  , "__tidepool_cell_check = do {"
  , "{{CELL_BODY}}"
  , "; pure () }"
  ]

oracleEntry :: TypedSegment.TypedSegmentPlan -> String
oracleEntry plan = "\noracleRun :: IO ([Int], Maybe (Either String String))\noracleRun = check "
  ++ TypedSegment.typedSegmentPlanRoot plan ++ "\n"

temporary :: IO FilePath
temporary = do
  tmp <- getTemporaryDirectory
  (path, handle) <- openTempFile tmp "typed-segment-differential"
  hClose handle
  removeFile path
  createDirectory path
  pure path
