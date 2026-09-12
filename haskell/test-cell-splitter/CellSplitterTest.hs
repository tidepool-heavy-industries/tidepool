{-# LANGUAGE LambdaCase #-}

module Main where

import Control.Monad (unless)
import Control.Monad.IO.Class (liftIO)
import Data.List (isInfixOf)
import GHC
import GHC.Driver.Session (parseDynamicFilePragma)
import GHC.Parser.Header (getOptions)
import GHC.Driver.Config.Parser (initParserOpts)
import GHC.Data.StringBuffer (stringToStringBuffer)
import Tidepool.Binders
import Tidepool.ExtractUtil (getLibdir)

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
  [ "{-# LANGUAGE LambdaCase, QuasiQuotes, MultilineStrings #-}"
  , "{{CELL_PRAGMAS}}"
  , "module CellCheck where"
  , "{{CELL_IMPORTS}}"
  , "{{CELL_DECLS}}"
  , "__tidepool_cell_check = do { {{CELL_BODY}} }"
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
    Right (CellSourcePlan _ [item]) -> do
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
    Right (CellSourcePlan _ [item]) -> do
      assertEqual "pragma-only kind" KDecl (sbKind (cellAnalysisVerdict item))
      assertEqual "pragma-only body" "" (cellAnalysisSource item)
    other -> fail ("pragma-only plan: " ++ show other)
  disabled <- analyzeCellWithFlags flags checkTemplate
    "{-# LANGUAGE NoQuasiQuotes #-}\nf = [bash|echo hello|]\n"
  case disabled of
    Right (CellSourcePlan _ [_, item]) ->
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
