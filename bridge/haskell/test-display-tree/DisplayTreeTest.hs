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
  compactCompoundValuesStayOnOneLine
  wideCompoundValuesBreakOnlyWhenNeeded
  textUsesEscapedStringLiterals
  topLevelTextKeepsLineBreaks
  nonAsciiTextIsNotNumericallyEscaped
  controlCharactersAreEscaped

applicationsParenthesizeOnlyAboveApplicationPrecedence :: IO ()
applicationsParenthesizeOnlyAboveApplicationPrecedence = do
  let application = Concat [TextLeaf "Just ", TextLeaf "5"]
  assertEqual "an application at precedence 10 is bare" "Just 5"
    (renderAll 64 (precedenceParens 10 application))
  assertEqual "an application argument is parenthesized" "(Just 5)"
    (renderAll 64 (precedenceParens 11 application))

compactCompoundValuesStayOnOneLine :: IO ()
compactCompoundValuesStayOnOneLine = do
  let value = treeParts "Right [" "]"
        [treeParts "(" ")" [literalText "fib.py", TextLeaf "0.96", TextLeaf "0.17"],
         treeParts "(" ")" [literalText "notes.md", TextLeaf "0.27", TextLeaf "0.75"]]
  assertEqual "the saved tuple-list result stays compact"
    "Right [(\"fib.py\", 0.96, 0.17), (\"notes.md\", 0.27, 0.75)]"
    (renderAll 512 value)

wideCompoundValuesBreakOnlyWhenNeeded :: IO ()
wideCompoundValuesBreakOnlyWhenNeeded = do
  let longValue = mconcat (replicate 25 "long")
      value = treeParts "[" "]" [TextLeaf "first", TextLeaf longValue]
  assertEqual "long collections keep line breaks between elements"
    ("[first,\n" <> longValue <> "]") (renderAll 512 value)

textUsesEscapedStringLiterals :: IO ()
textUsesEscapedStringLiterals =
  assertEqual "quotes, backslashes and newlines are escaped"
    "\"a\\\"b\\\\c\\nd\"" (renderAll 64 (literalText "a\"b\\c\nd"))

topLevelTextKeepsLineBreaks :: IO ()
topLevelTextKeepsLineBreaks = do
  -- Top-level String shares this same unquoted rendering: the 'WorkbenchDisplay'
  -- and 'FullDisplay' [Char] instances convert to Text and call 'rawText'.
  assertEqual "standalone text is raw" ("first\nsecond", False)
    (rawText 64 "first\nsecond")
  assertEqual "standalone text remains bounded" ("first", True)
    (rawText 5 "first\nsecond")
  assertEqual "text nested in a pair is quoted" "(\"first\\nsecond\", 1)"
    (renderAll 64 (treeParts "(" ")" [literalText "first\nsecond", TextLeaf "1"]))

nonAsciiTextIsNotNumericallyEscaped :: IO ()
nonAsciiTextIsNotNumericallyEscaped = do
  assertEqual "printable non-ASCII passes through literalText unescaped"
    "\"\955-calculus\""
    (renderAll 64 (literalText "\955-calculus"))
  assertEqual "printable non-ASCII in a standalone value stays raw"
    ("\955-calculus", False)
    (rawText 64 "\955-calculus")

controlCharactersAreEscaped :: IO ()
controlCharactersAreEscaped = do
  assertEqual "tabs are escaped in nested text"
    "\"a\\tb\""
    (renderAll 64 (literalText "a\tb"))
  assertEqual "other control characters use a numeric escape"
    "\"a\\x1;b\""
    (renderAll 64 (literalText "a\x01\&b"))

punctuationAndChildrenRespectBudget :: IO ()
punctuationAndChildrenRespectBudget = do
  let tree = treeParts "(" ")" [TextLeaf "alpha", TextLeaf "beta"]
      (rendered, remainder, unavailable) = renderTree 8 tree
  assertEqual "punctuation and children consume one shared budget" "(alpha, " rendered
  assertTrue "tree retains the unrendered child and closing punctuation" (hasRemainder remainder)
  assertEqual "ordinary tree detail remains available" False unavailable

pagesCoverTheExactTree :: IO ()
pagesCoverTheExactTree = do
  let tree = treeParts "{" "}" [TextLeaf "left", TextLeaf "right", TextLeaf "tail"]
      expected = "{left, right, tail}"
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
