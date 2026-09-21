{-# LANGUAGE LambdaCase #-}

module Main where

import Control.Monad (unless)
import Control.Exception (SomeException, bracket, finally, throwIO, try)
import Control.Monad.IO.Class (liftIO)
import Data.List (isInfixOf, isPrefixOf, tails)
import GHC
import GHC.Driver.Session (parseDynamicFilePragma)
import GHC.Parser.Header (getOptions)
import GHC.Driver.Config.Parser (initParserOpts)
import GHC.Data.StringBuffer (stringToStringBuffer)
import GHC.Types.SourceError (SourceError)
import Tidepool.Binders
import Tidepool.ExtractUtil (getLibdir)
import Tidepool.GhcPipeline
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
  getArgs >>= \case
    [] -> pure ()
    ["--metadata"] -> metadataCompilation
    ["--structural-display", effectsRoot] -> structuralDisplayCompilation effectsRoot
    _ -> fail "expected --metadata or --structural-display EFFECTS_INCLUDE"

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
  previousTiming <- lookupEnv "TIDEPOOL_TIMING"
  setEnv "TIDEPOOL_TIMING" "1"
  (withResidentPipelineSelectedRequests [root] $ \runRequest -> do
      (checked, output) <- captureStderr root "metadata-check" $
        runRequest $ \compiler ->
          compiler CheckedEnvironment mempty GeneralCompile Nothing target [root] Nothing
      assertContains "metadata captures the checked target's types" "Box Int"
        (show (crCapturedTypes checked))
      assertEqual "exactly one checked target" 1
        (length (filter (isInfixOf "tidepool-checked module=MetadataTarget target=True") (lines output)))
      unless (not ("tidepool-target phase=desugar" `isInfixOf` output || "phase=lowering " `isInfixOf` output)) $
        fail "metadata target entered the executable pipeline"
      writeFile dependency "module MetadataDependency where\nvalue = missingDependencyName\n"
      rejected <- try (runRequest $ \compiler ->
        compiler CheckedEnvironment mempty GeneralCompile Nothing target [root] Nothing)
        :: IO (Either SomeException CheckedEnvironmentResult)
      case rejected of
        Left _ -> pure ()
        Right _ -> fail "metadata reused an invalid dependency")
    `finally` maybe (unsetEnv "TIDEPOOL_TIMING") (setEnv "TIDEPOOL_TIMING") previousTiming
  where
    temporary = do
      parent <- getTemporaryDirectory
      (path, handle) <- openTempFile parent "tidepool-metadata"
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
  withResidentPipelineSelectedRequests includes $ \runRequest -> runRequest $ \compiler -> do
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
  (withResidentPipelineSelectedRequests [root] $ \runRequest -> do
      let compile purpose compiler = compiler CheckedEnvironment mempty purpose Nothing targetPath [root] Nothing
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
          (try (compile GeneralCompile compiler) :: IO (Either SourceError CheckedEnvironmentResult))
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
    other -> fail ("pragma-only plan: " ++ show other)
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
