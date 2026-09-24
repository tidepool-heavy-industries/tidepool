{-# LANGUAGE LambdaCase #-}

module Main where

import Control.Monad (forM_, unless, when)
import Control.Exception (SomeException, bracket, finally, throwIO, try)
import Control.Monad.IO.Class (liftIO)
import Data.IORef (newIORef, modifyIORef', readIORef)
import Data.List (isInfixOf, isPrefixOf, isSuffixOf, tails)
import Data.Char (isDigit)
import GHC
import GHC.Builtin.Types (intTy)
import GHC.Types.Name.Occurrence (mkVarOcc)
import GHC.Driver.Session (parseDynamicFilePragma)
import GHC.Parser.Header (getOptions)
import GHC.Driver.Config.Parser (initParserOpts)
import GHC.Data.StringBuffer (stringToStringBuffer)
import GHC.Types.SourceError (SourceError)
import Tidepool.Binders
import Tidepool.ExtractUtil (getLibdir)
import Tidepool.GhcPipeline
import Tidepool.ExtractRequest (InspectionRequest(..))
import Tidepool.Introspection (InfoEntry(..), InspectionResult(..), runInspection)
import Tidepool.DependencyEvidence
import Tidepool.Session
  ( Generation(..), SessionModule(..), SessionModuleKind(..), SessionScope(..)
  , mkThinSessionIface, writeSessionIface )
import Tidepool.Timing
  ( InterfaceStage(..), InterfaceReuse(..), measureModuleInterface )
import System.Directory
  ( getTemporaryDirectory, createDirectory, createDirectoryIfMissing
  , removeFile, removeDirectoryRecursive )
import System.FilePath ((</>))
import System.IO (openTempFile, hClose, hFlush, readFile', stderr)
import GHC.IO.Handle (hDuplicate, hDuplicateTo)
import System.Environment (getArgs, lookupEnv, setEnv, unsetEnv)

main :: IO ()
main = do
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
  interfaceMeasurementDiagnostics
  getArgs >>= \case
    [] -> pure ()
    ["--metadata"] -> metadataCompilation
    ["--prepared-session"] -> preparedSessionLeafCompilation
    ["--dependency-evidence"] -> dependencyEvidenceCompilation
    ["--untracked-compile-time"] -> untrackedCompileTimeCompilation
    ["--validation-memo"] -> validationMemoCompilation
    ["--path-insensitive-witness"] -> pathInsensitiveWitnessCompilation
    ["--structural-display", effectsRoot] -> structuralDisplayCompilation effectsRoot
    _ -> fail "expected --metadata, --prepared-session, --dependency-evidence, --untracked-compile-time, --validation-memo, --path-insensitive-witness, or --structural-display EFFECTS_INCLUDE"

untrackedCompileTimeCompilation :: IO ()
untrackedCompileTimeCompilation = bracket temporary removeDirectoryRecursive $ \root -> do
  let dependency = root </> "QuasiQuoteDependency.hs"
      templateDependency = root </> "TemplateDependency.hs"
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
  direct <- runPipelineSelected PreparedStg target [root]
  let evidence = pprDependencies direct
  when (dependencyCacheSafe evidence || dependencySelectionComplete evidence) $
    fail "QuasiQuotes source produced complete dependency evidence"
  previousTiming <- lookupEnv "TIDEPOOL_TIMING"
  previousMemoTrace <- lookupEnv "TIDEPOOL_MEMO_TRACE"
  setEnv "TIDEPOOL_TIMING" "1"
  setEnv "TIDEPOOL_MEMO_TRACE" "1"
  (withResidentPipelineSelected [root] $ \compile -> do
      _ <- compile PreparedStg mempty GeneralCompile Nothing target [] Nothing
      (_, warmLog) <- captureStderr root "quasiquote-warm" $
        compile PreparedStg mempty GeneralCompile Nothing target [] Nothing
      assertContains "QuasiQuotes source remains conservatively uncacheable"
        "tidepool-memo-miss module=QuasiQuoteDependency reason=untracked-compile-time-execution"
        warmLog
      assertContains "TemplateHaskell source remains conservatively uncacheable"
        "tidepool-memo-miss module=TemplateDependency reason=untracked-compile-time-execution"
        warmLog
      -- A dependency module whose only compile-time execution is an
      -- allowlisted, pure quasiquoter reuses the memo on the next
      -- identical compile (the target itself, 'LabelUser', is always
      -- freshly recompiled -- see 'validationMemoCompilation' -- so it is
      -- 'LabelDependency', not 'LabelUser', whose memo status this checks).
      _ <- compile PreparedStg mempty GeneralCompile Nothing labelUser [] Nothing
      (_, labelWarmLog) <- captureStderr root "label-warm" $
        compile PreparedStg mempty GeneralCompile Nothing labelUser [] Nothing
      when ("tidepool-memo-miss module=LabelDependency" `isInfixOf` labelWarmLog) $
        fail ("a dependency using only [label|...|] missed the memo: " ++ labelWarmLog)
      -- A dependency using a quasiquoter this resolver cannot place on the
      -- allowlist (here: locally defined, so no import brings it into
      -- scope) stays conservatively uncacheable, same as raw QuasiQuotes —
      -- same short TIDEPOOL_TIMING reason — but TIDEPOOL_MEMO_TRACE still
      -- honestly names the unresolved quoter it actually saw.
      _ <- compile PreparedStg mempty GeneralCompile Nothing localQuoteUser [] Nothing
      (_, localWarmLog) <- captureStderr root "local-quote-warm" $
        compile PreparedStg mempty GeneralCompile Nothing localQuoteUser [] Nothing
      assertContains "an unlisted (locally-defined) quasiquoter remains conservatively uncacheable"
        "tidepool-memo-miss module=LocalQuoteDependency reason=untracked-compile-time-execution"
        localWarmLog
      assertContains "the trace honestly reports the unresolved quoter, not a false allowlist hit"
        "tidepool-memo-trace-miss" localWarmLog
      assertContains "the trace honestly reports the unresolved quoter, not a false allowlist hit"
        "quasiquotes=untracked:LocalQuoteQuoter.myqq" localWarmLog)
    `finally` do
      maybe (unsetEnv "TIDEPOOL_TIMING") (setEnv "TIDEPOOL_TIMING") previousTiming
      maybe (unsetEnv "TIDEPOOL_MEMO_TRACE") (setEnv "TIDEPOOL_MEMO_TRACE") previousMemoTrace
  where
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
    [ "module WitnessTarget where"
    , "import qualified Data.Text as Text"
    , "import WitnessA (value)"
    , "result = (Text.length (Text.pack \"x\"), value)"
    ]
  prepared <- runPipelineSelected PreparedStg target [root]
  let evidence = pprDependencies prepared
      resolutions = dependencyResolutions evidence
      selectedPaths = [path | resolution <- resolutions
                            , Just path <- [dependencyResolutionSelected resolution]]
      packageWitnesses = [resolution | resolution <- resolutions
        , dependencyResolutionModule resolution == "Data.Text"]
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
      importer = workDir </> "Importer.hs"
      depContent =
        [ "module Dep where"
        , "value :: Int"
        , "value = 1"
        ]
      targetContent =
        [ "module Target where"
        , "import Dep (value)"
        , "result :: Int"
        , "result = value + 1"
        ]
  createDirectoryIfMissing True workDir
  createDirectoryIfMissing True rootA
  createDirectoryIfMissing True rootB
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
          compile PreparedStg mempty GeneralCompile Nothing importer [rootA] Nothing
        assertContains "cold compile resolves Dep from rootA"
          "tidepool-memo-miss module=Dep reason=absent" coldLog
        assertContains "cold compile resolves Target from rootA"
          "tidepool-memo-miss module=Target reason=dependency-miss:Dep" coldLog
        (_, warmLog) <- captureStderr root "path-insensitive-warm" $
          compile PreparedStg mempty GeneralCompile Nothing importer [rootB] Nothing
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

validationMemoCompilation :: IO ()
validationMemoCompilation = bracket temporary removeDirectoryRecursive $ \root -> do
  let base = root </> "WarmBase.hs"
      reexport = root </> "WarmReexport.hs"
      child = root </> "WarmChild.hs"
      target = root </> "WarmTarget.hs"
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
  previousTiming <- lookupEnv "TIDEPOOL_TIMING"
  setEnv "TIDEPOOL_TIMING" "1"
  (withResidentPipelineSelected [root] $ \compile -> do
      (_, coldLog) <- captureStderr root "validation-cold" $
        compile PreparedStg mempty GeneralCompile Nothing target [] Nothing
      assertContains "chain dependency witnesses are computed once per node"
        "tidepool-dependency-witness nodes=20 direct_edges=19 digest_computations=20"
        coldLog
      (_, warmLog) <- captureStderr root "validation-warm" $
        compile PreparedStg mempty GeneralCompile Nothing target [] Nothing
      forM_ ["WarmReexport", "WarmChild"] $ \name ->
        when (("tidepool-memo-miss module=" ++ name) `isInfixOf` warmLog) $
          fail ("unchanged validation-only module was recompiled: " ++ name)
      when ("tidepool-memo-miss module=WarmChain" `isInfixOf` warmLog) $
        fail "unchanged validation-only chain was recompiled"
      -- Since 3b5e38e76 ("fix: pair prepared home code with its compiler
      -- interfaces"), 'compileReachable' reruns 'compileFront' after the
      -- reachability pass so a module's prepared body sees the exact
      -- interfaces its own dependencies just registered, rather than the
      -- ones 'load'' saw. The evicted target is therefore front-compiled
      -- twice per warm cycle (once for the reachability walk, once for the
      -- real recompile) while core2core/prepare still run once.
      assertContains "warm compile prepares only its evicted target"
        "front_compiles=2 core_compiles=1 prepared_compiles=1" warmLog)
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
preparedSessionLeafCompilation :: IO ()
preparedSessionLeafCompilation = bracket temporary removeDirectoryRecursive $ \root -> do
  let scopeRoot = root </> "session"
      valueModule = SessionModule ValMod (Generation 1)
      seed = root </> "SessionSeed.hs"
      target = root </> "PreparedSessionLeaf.hs"
      scope = SessionScope scopeRoot [valueModule]
  writeFile seed "module SessionSeed where\nseed = 1 :: Int\n"
  seeded <- runPipelineSelected PreparedStg seed [root]
  let environment = prHscEnv (pprPipelineResult seeded)
  iface <- mkThinSessionIface environment valueModule [(mkVarOcc "prior", intTy)]
  writeSessionIface environment scopeRoot valueModule iface
  writeFile target $ unlines
    [ "module PreparedSessionLeaf where"
    , "import Tidepool.Session.Val.G1 (prior)"
    , "__result = prior + 1"
    ]
  previousTiming <- lookupEnv "TIDEPOOL_TIMING"
  setEnv "TIDEPOOL_TIMING" "1"
  (withResidentPipelineSelectedRequests [root] (const (pure ())) $ \runRequest -> do
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
        fail "prepared session leaf constructed an unused target interface")
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

requestMemoLifecycle :: FilePath -> IO ()
requestMemoLifecycle root = do
  let dependencyDir = root </> "Tidepool" </> "Session" </> "Lib"
      dependencyPath = dependencyDir </> "G1.hs"
      targetPath = root </> "MemoTarget.hs"
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
  previousTiming <- lookupEnv "TIDEPOOL_TIMING"
  setEnv "TIDEPOOL_TIMING" "1"
  (withResidentPipelineSelectedRequests [root] (const (pure ())) $ \runRequest -> do
      let compile purpose compiler = compiler PreparedStg mempty purpose Nothing targetPath [root] Nothing
          sessionMiss = "tidepool-memo-miss module=Tidepool.Session.Lib.G1"
          absentSession = sessionMiss ++ " reason=absent"
          targetMiss = "tidepool-memo-miss module=MemoTarget"
      runRequest $ \compiler -> do
        (_, coldLog) <- captureStderr root "memo-cold" (compile GeneralCompile compiler)
        assertContains "cold request compiles the session dependency" absentSession coldLog
        assertContains "cold request compiles its target" targetMiss coldLog
        (_, warmLog) <- captureStderr root "memo-warm" (compile LookupTypeCompile compiler)
        unless (not (sessionMiss `isInfixOf` warmLog)) $
          fail "unchanged session dependency was not reused within one worker request"
        assertContains "internal compile evicts its purpose-sensitive target" targetMiss warmLog
        writeFile dependencyPath invalidDependency
        (changed, changedLog) <- captureStderr root "memo-changed"
          (try (compile GeneralCompile compiler) :: IO (Either SourceError PreparedPipelineResult))
        case changed of
          Left _ -> pure ()
          Right _ -> fail "changed invalid session dependency reused a stale memo entry"
        assertContains "source-sensitive memo invalidation" sessionMiss changedLog
      writeFile dependencyPath validDependency
      (_, nextRequestLog) <- captureStderr root "memo-next-request"
        (runRequest $ \compiler -> compile GeneralCompile compiler)
      assertContains "normal request exit evicts session dependencies" absentSession nextRequestLog
      failedRequest <- try (captureStderr root "memo-exception" $ runRequest $ \compiler -> do
        _ <- compile GeneralCompile compiler
        throwIO (userError "request failure after compile"))
        :: IO (Either SomeException (PipelineResult, String))
      case failedRequest of
        Left _ -> pure ()
        Right _ -> fail "exception cleanup probe unexpectedly succeeded"
      (_, afterExceptionLog) <- captureStderr root "memo-after-exception"
        (runRequest $ \compiler -> compile GeneralCompile compiler)
      assertContains "exceptional request exit evicts session dependencies" absentSession afterExceptionLog)
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
