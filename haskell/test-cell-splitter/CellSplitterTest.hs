{-# LANGUAGE LambdaCase #-}

module Main where

import Control.Monad (unless)
import Control.Monad.IO.Class (liftIO)
import Data.List (isInfixOf)
import GHC
import Tidepool.Binders
import Tidepool.ExtractUtil (getLibdir)

main :: IO ()
main = do
  libdir <- getLibdir
  runGhc (Just libdir) $ do
    flags <- getSessionDynFlags
    liftIO $ do
      lexicalIslands flags
      commentsPragmasAndLayout flags

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
