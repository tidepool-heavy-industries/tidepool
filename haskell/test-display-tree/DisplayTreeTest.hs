{-# LANGUAGE OverloadedStrings #-}

module Main where

import Control.Exception (evaluate)
import Control.Monad (unless)
import Data.Text (Text)
import Tidepool.Inspection.Tree

main :: IO ()
main = do
  punctuationAndChildrenRespectBudget
  pagesCoverTheExactTree
  infiniteLeavesAreProductive
  exhaustedBudgetDoesNotForceTheNextField
  legacyLeavesReportUnavailableDetail
  applicationsParenthesizeOnlyAboveApplicationPrecedence

applicationsParenthesizeOnlyAboveApplicationPrecedence :: IO ()
applicationsParenthesizeOnlyAboveApplicationPrecedence = do
  let application = Concat [TextLeaf "Just ", TextLeaf "5"]
  assertEqual "an application at precedence 10 is bare" "Just 5"
    (renderAll 64 (precedenceParens 10 application))
  assertEqual "an application argument is parenthesized" "(Just 5)"
    (renderAll 64 (precedenceParens 11 application))

punctuationAndChildrenRespectBudget :: IO ()
punctuationAndChildrenRespectBudget = do
  let tree = treeParts "(" ")" [TextLeaf "alpha", TextLeaf "beta"]
      (rendered, remainder, unavailable) = renderTree 8 tree
  assertEqual "punctuation and children consume one shared budget" "(alpha,\n" rendered
  assertTrue "tree retains the unrendered child and closing punctuation" (hasRemainder remainder)
  assertEqual "ordinary tree detail remains available" False unavailable

pagesCoverTheExactTree :: IO ()
pagesCoverTheExactTree = do
  let tree = treeParts "{" "}" [TextLeaf "left", TextLeaf "right", TextLeaf "tail"]
      expected = "{left,\nright,\ntail}"
  assertEqual "successive pages concatenate without gaps or duplicates" expected
    (renderAll 4 tree)

infiniteLeavesAreProductive :: IO ()
infiniteLeavesAreProductive = do
  let (rendered, remainder, unavailable) = renderTree 5 (StringLeaf (repeat 'x'))
  assertEqual "infinite string leaf yields its bounded prefix" "xxxxx" rendered
  assertTrue "infinite string leaf retains a next page" (hasRemainder remainder)
  assertEqual "infinite string leaf has recoverable detail" False unavailable

exhaustedBudgetDoesNotForceTheNextField :: IO ()
exhaustedBudgetDoesNotForceTheNextField = do
  rendered <- evaluate $ case renderTree 2 (Concat [TextLeaf "ok", undefined]) of
    (text, _, _) -> text
  assertEqual "budget exhaustion stops before the undefined next field" "ok" rendered

legacyLeavesReportUnavailableDetail :: IO ()
legacyLeavesReportUnavailableDetail = do
  let legacy budget
        | budget == 3 = ("abc", True)
        | otherwise = error "legacy leaf received the wrong remaining budget"
      (legacyText, legacyRemainder, legacyUnavailable) = renderTree 3 (LegacyLeaf legacy)
      (textText, textRemainder, textUnavailable) = renderTree 3 (TextLeaf "abcdef")
  assertEqual "legacy leaf receives the remaining budget" "abc" legacyText
  assertEqual "legacy omission has no fabricated continuation" False (hasRemainder legacyRemainder)
  assertEqual "legacy omission is explicitly unavailable" True legacyUnavailable
  assertEqual "ordinary text has the same visible prefix" "abc" textText
  assertTrue "ordinary text retains the real suffix" (hasRemainder textRemainder)
  assertEqual "ordinary text is recoverable" False textUnavailable

renderAll :: Int -> DisplayTree -> Text
renderAll budget = go
  where
    go tree = case renderTree budget tree of
      (rendered, Nothing, _) -> rendered
      (rendered, Just remainder, unavailable)
        | unavailable -> error "unexpected unavailable detail in an ordinary tree"
        | otherwise -> rendered <> go remainder

hasRemainder :: Maybe DisplayTree -> Bool
hasRemainder Nothing = False
hasRemainder (Just _) = True

assertEqual :: (Eq a, Show a) => String -> a -> a -> IO ()
assertEqual label expected actual =
  unless (expected == actual) $
    fail (label ++ ": expected " ++ show expected ++ ", got " ++ show actual)

assertTrue :: String -> Bool -> IO ()
assertTrue label condition = unless condition (fail label)
