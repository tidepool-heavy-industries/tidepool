{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleInstances #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE UndecidableInstances #-}

module Main where

import Test.Tasty.QuickCheck (testProperty, NonNegative(..))
import Tidepool.Source
import Tidepool.Text.Navigation
import FormViewTest (formViewTests)
import Tidepool.Test.Runner (TestTree, runTests, testCase, testGroup)

import Control.Exception (evaluate)
import Control.Monad (unless)
import Data.Text (Text)
import qualified Data.Text as Text
import Tidepool.Inspection.Display (Display (displayTree), genericDisplayTree, GDisplay, Rep)
import GHC.Generics (Generic)
import Tidepool.Inspection.Tree

main :: IO ()
main = runTests tests

tests :: TestTree
tests = testGroup "display-tree"
  [ testProperty "retained literal locations agree with independent String suffix oracle" literalSearchProperty
  , testCase "retainedTextPreservesNewlinesAndBoundsHugeLine" retainedTextPreservesNewlinesAndBoundsHugeLine
  , testProperty "source windows agree with independent String line addresses" sourceWindowProperty
  , testCase "sourceWindowsRetainRevisionAndLineAddresses" sourceWindowsRetainRevisionAndLineAddresses
  , testCase "noticedDisplaysStayBoundedAndRetainDetail" noticedDisplaysStayBoundedAndRetainDetail
  , formViewTests
  , testCase "punctuationAndChildrenRespectBudget" punctuationAndChildrenRespectBudget
  , testCase "pagesCoverTheExactTree" pagesCoverTheExactTree
  , testCase "infiniteLeavesAreProductive" infiniteLeavesAreProductive
  , testCase "exhaustedBudgetDoesNotForceTheNextField" exhaustedBudgetDoesNotForceTheNextField
  , testCase "legacyLeavesReportUnavailableDetail" legacyLeavesReportUnavailableDetail
  , testCase "applicationsParenthesizeOnlyAboveApplicationPrecedence" applicationsParenthesizeOnlyAboveApplicationPrecedence
  , testCase "compactCompoundValuesStayOnOneLine" compactCompoundValuesStayOnOneLine
  , testCase "wideCompoundValuesBreakOnlyWhenNeeded" wideCompoundValuesBreakOnlyWhenNeeded
  , testCase "textUsesEscapedStringLiterals" textUsesEscapedStringLiterals
  , testCase "topLevelTextKeepsLineBreaks" topLevelTextKeepsLineBreaks
  , testCase "nonAsciiTextIsNotNumericallyEscaped" nonAsciiTextIsNotNumericallyEscaped
  , testCase "controlCharactersAreEscaped" controlCharactersAreEscaped
  , testCase "independentSiblingsKeepTheirKeys" independentSiblingsKeepTheirKeys
  , testCase "collapsedFieldsRemainLazy" collapsedFieldsRemainLazy
  , testCase "infiniteSequencesHaveBoundedFrontiers" infiniteSequencesHaveBoundedFrontiers
  , testCase "continuedFrontierLabelsStayBounded" continuedFrontierLabelsStayBounded
  , testCase "smallSequenceGrantsAdvanceTheirActualSuffix" smallSequenceGrantsAdvanceTheirActualSuffix
  , testCase "unsupportedFieldsRemainOpaque" unsupportedFieldsRemainOpaque
  , testCase "structuralGenericUsesFieldNames" structuralGenericUsesFieldNames
  , testCase "recursiveGenericDisplayIsProductive" recursiveGenericDisplayIsProductive
  , testCase "nestedInfiniteStringsAreProductive" nestedInfiniteStringsAreProductive
  , testCase "collapsedSequenceTailRemainsLazy" collapsedSequenceTailRemainsLazy
  , testCase "shortGroupBudgetDoesNotForceFields" shortGroupBudgetDoesNotForceFields
  , testCase "exactBudgetRetainsUnknownSuffixes" exactBudgetRetainsUnknownSuffixes
  , testCase "tinyGrantsRetainConstructorNames" tinyGrantsRetainConstructorNames
  , testCase "tinyConstructorFieldsAdvanceTheirActualSuffix" tinyConstructorFieldsAdvanceTheirActualSuffix
  , testCase "oversizedConstructorNamesMakeProgressWithoutHidingFields" oversizedConstructorNamesMakeProgressWithoutHidingFields
  , testCase "oversizedFieldNamesRetainTheirSuffixAndValue" oversizedFieldNamesRetainTheirSuffixAndValue
  , testCase "unavailableDetailDoesNotReplaceSupportedText" unavailableDetailDoesNotReplaceSupportedText
  ]

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
  -- Top-level String shares this unquoted rendering through 'rawString'.
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

independentSiblingsKeepTheirKeys :: IO ()
independentSiblingsKeepTheirKeys = do
  let state = newDisplayState 1024 (Constructor "Pair" [("left", StringLeaf (replicate 300 'l')), ("right", StringLeaf (replicate 300 'r'))])
  case displayStateKeys state of
    [(left, _), (right, _)] -> case expandDisplayState 1024 left state of
      Just detail -> do
        assertEqual "only selected suffix is rendered" (mconcat (replicate 172 "l")) (displayStateText detail)
        assertEqual "right keeps its independent key" [right] (map fst (displayStateKeys detail))
        assertEqual "consumed key cannot be expanded again" Nothing
          (fmap displayStateText (expandDisplayState 1024 left detail))
      Nothing -> fail "left key was not retained"
    _ -> fail "large sibling fields did not receive independent keys"

collapsedFieldsRemainLazy :: IO ()
collapsedFieldsRemainLazy = do
  value <- evaluate (displayStateText (newDisplayState 1024
    (Constructor "Outer" [("inner", Constructor "Inner" [("hidden", undefined)])])))
  assertEqual "collapsed depth does not force hidden field" "Outer {inner = Inner {hidden = …}…}" value

infiniteSequencesHaveBoundedFrontiers :: IO ()
infiniteSequencesHaveBoundedFrontiers = do
  let state = newDisplayState 1024 (Sequence "[" "]" (repeat (TextLeaf "1")))
  assertEqual "bounded sequence preview" "[1, 1, 1, 1, 1, 1, 1, 1…]" (displayStateText state)
  assertEqual "one retained sequence remainder" 1 (length (displayStateKeys state))

smallSequenceGrantsAdvanceTheirActualSuffix :: IO ()
smallSequenceGrantsAdvanceTheirActualSuffix = do
  tiny <- collect 1 8 (newDisplayState 1 (Sequence "[" "]" [TextLeaf "1"]))
  assertEqual "a tiny sequence grant advances rather than repeating its opening" "[1]" tiny
  let opening = Text.replicate 100 "o"
      closing = Text.replicate 100 "c"
  wide <- collect 8 40 (newDisplayState 8 (Sequence opening closing [TextLeaf "1"]))
  assertEqual "oversized sequence framing retains its actual suffix" (opening <> "1" <> closing) wide
  let lazyState = newDisplayState 1 (Sequence "[" "]" undefined)
  assertEqual "an opening-only page does not inspect the child spine" "[" (displayStateText lazyState)
  where
    collect budget remaining state
      | remaining <= (0 :: Int) = fail "sequence framing did not make progress"
      | otherwise = case displayStateKeys state of
          [] -> pure (displayStateText state)
          [(key, _)] -> do
            detail <- maybe (fail "sequence cursor was lost") pure (expandDisplayState budget key state)
            suffix <- collect budget (remaining - 1) detail
            pure (displayStateText state <> suffix)
          _ -> fail "exceptional sequence framing acquired unrelated keys"

continuedFrontierLabelsStayBounded :: IO ()
continuedFrontierLabelsStayBounded = do
  let sequenceState = newDisplayState 1024 (Sequence "[" "]" (replicate 10000 (TextLeaf "1")))
      chain = Constructor "Node" [("next", chain)]
  pages <- finish 0 sequenceState
  assertEqual "all finite sequence pages remain expandable" 1251 pages
  follow 1500 (newDisplayState 1024 chain)
  where
    assertLabels state = assertTrue "frontier metadata stays small across pages"
      (all ((<= 16) . Text.length . snd) (displayStateKeys state))
    finish count state = do
      assertLabels state
      case displayStateKeys state of
        [] -> pure (count + 1 :: Int)
        [(key, _)] -> requireExpansion key state >>= finish (count + 1)
        _ -> fail "a scalar sequence acquired unrelated expansion keys"
    follow count state = do
      assertLabels state
      if count <= (0 :: Int) then pure () else case displayStateKeys state of
        [(key, _)] -> requireExpansion key state >>= follow (count - 1)
        _ -> fail "recursive field lost its single frontier"
    requireExpansion key state = maybe (fail "frontier cursor was lost") pure
      (expandDisplayState 1024 key state)

newtype Unsupported = Unsupported Int

data GenericRecord = GenericRecord { supported :: Int, unsupported :: Unsupported, callback :: Int -> Int }
  deriving Generic

instance Display GenericRecord where
  displayTree = genericDisplayTree

unsupportedFieldsRemainOpaque :: IO ()
unsupportedFieldsRemainOpaque = do
  assertEqual "opaque fallback never evaluates its value" "<opaque>"
    (renderAll 64 (displayTree (undefined :: Unsupported)))
  assertEqual "function rendering never evaluates its closure" "<function>"
    (renderAll 64 (displayTree (undefined :: Int -> Int)))

structuralGenericUsesFieldNames :: IO ()
structuralGenericUsesFieldNames =
  assertEqual "Generic selects primitives and opaque unsupported fields"
    "GenericRecord {supported = 7, unsupported = <opaque>, callback = <function>}"
    (displayStateText (newDisplayState 1024 (displayTree (GenericRecord 7 undefined undefined))))

data Recursive = End | Link Int Recursive deriving Generic

instance GDisplay (Rep Recursive) => Display Recursive where
  displayTree = genericDisplayTree

recursiveGenericDisplayIsProductive :: IO ()
recursiveGenericDisplayIsProductive = do
  let recursive = Link 1 recursive
      state = newDisplayState 1024 (displayTree recursive)
  assertEqual "recursive generic tree previews only bounded depth"
    "Link {1 = 1, 2 = Link {1 = …, 2 = …}…}" (displayStateText state)

nestedInfiniteStringsAreProductive :: IO ()
nestedInfiniteStringsAreProductive = do
  let state = newDisplayState 32 (displayTree (Just (repeat 'x')))
  assertEqual "nested infinite String gives a bounded prefix" 32 (Text.length (displayStateText state))
  assertEqual "nested infinite String retains its suffix" 1 (length (displayStateKeys state))
  assertEqual "lazy String literals preserve escaping" "\"a\\\"b\\\\c\\nd\""
    (renderAll 64 (literalString "a\"b\\c\nd"))

collapsedSequenceTailRemainsLazy :: IO ()
collapsedSequenceTailRemainsLazy = do
  rendered <- evaluate (displayStateText (newDisplayState 1024
    (Sequence "[" "]" (replicate 8 (TextLeaf "1") ++ undefined))))
  assertEqual "sequence cap does not force the collapsed tail" "[1, 1, 1, 1, 1, 1, 1, 1…]" rendered

shortGroupBudgetDoesNotForceFields :: IO ()
shortGroupBudgetDoesNotForceFields = do
  rendered <- evaluate $ case renderTree 1 (treeParts "(" ")" [undefined]) of
    (value, _, _) -> value
  assertEqual "layout lookahead stays within the page allowance" "(" rendered

exactBudgetRetainsUnknownSuffixes :: IO ()
exactBudgetRetainsUnknownSuffixes = do
  let stringState = newDisplayState 2 (StringLeaf ('o' : 'k' : undefined))
      concatState = newDisplayState 2 (Concat (TextLeaf "ok" : undefined))
      fieldsState = newDisplayState 9 (Constructor "T" (("x", TextLeaf "") : undefined))
  assertEqual "exact String page does not inspect the next character" "ok" (displayStateText stringState)
  assertEqual "exact String page keeps the unseen suffix" 1 (length (displayStateKeys stringState))
  assertEqual "exact Concat page does not inspect the next child spine" "ok" (displayStateText concatState)
  assertEqual "exact Concat page keeps the unseen children" 1 (length (displayStateKeys concatState))
  assertEqual "constructor cap does not inspect the collapsed field spine" "T {x = …}" (displayStateText fieldsState)
  assertEqual "constructor cap retains the collapsed field spine" 2 (length (displayStateKeys fieldsState))

tinyGrantsRetainConstructorNames :: IO ()
tinyGrantsRetainConstructorNames = do
  let atom = newDisplayState 1 (Constructor "Nothing" [])
      record = newDisplayState 1 (Constructor "Outer" [("value", TextLeaf "7")])
  assertEqual "nullary constructor preview respects the tiny allowance" "N" (displayStateText atom)
  case displayStateKeys atom of
    [(key, _)] -> assertEqual "nullary constructor suffix remains available" (Just "othing")
      (fmap displayStateText (expandDisplayState 64 key atom))
    _ -> fail "nullary constructor suffix was lost"
  assertEqual "compound constructor preview respects the tiny allowance" "O" (displayStateText record)
  case displayStateKeys record of
    [(nameKey, _), (fieldsKey, _)] -> do
      assertEqual "compound constructor retains the actual name suffix" (Just "uter")
        (fmap displayStateText (expandDisplayState 64 nameKey record))
      assertEqual "compound constructor fields remain independently available" (Just " {value = 7}")
        (fmap displayStateText (expandDisplayState 64 fieldsKey record))
    _ -> fail "compound constructor envelope was lost"

tinyConstructorFieldsAdvanceTheirActualSuffix :: IO ()
tinyConstructorFieldsAdvanceTheirActualSuffix = do
  let state = newDisplayState 1 (Constructor "C" [("x", TextLeaf "1")])
      lazyState = newDisplayState 1 (Constructor "C" [("x", undefined)])
  case displayStateKeys state of
    [(fieldsKey, _)] -> do
      fields <- requireExpansion fieldsKey state
      suffix <- collect 12 fields
      assertEqual "tiny constructor fields advance instead of retaining themselves" "C {x = 1}" (displayStateText state <> suffix)
    _ -> fail "tiny constructor fields were not retained"
  case displayStateKeys lazyState of
    [(fieldsKey, _)] -> do
      fields <- requireExpansion fieldsKey lazyState
      assertEqual "a tiny fields opening does not force its value" " " (displayStateText fields)
    _ -> fail "lazy constructor fields were not retained"
  where
    collect remaining state
      | remaining <= (0 :: Int) = fail "constructor fields did not make progress"
      | otherwise = case displayStateKeys state of
          [] -> pure (displayStateText state)
          [(key, _)] -> do
            detail <- requireExpansion key state
            suffix <- collect (remaining - 1) detail
            pure (displayStateText state <> suffix)
          _ -> fail "tiny constructor cursor acquired unrelated keys"
    requireExpansion key state = maybe (fail "constructor fields cursor was lost") pure
      (expandDisplayState 1 key state)

oversizedConstructorNamesMakeProgressWithoutHidingFields :: IO ()
oversizedConstructorNamesMakeProgressWithoutHidingFields = do
  let name = Text.replicate (8192 * 2 + 7) "x"
      state = newDisplayState 8192 (Constructor name [("left", TextLeaf "A"), ("right", TextLeaf "B")])
  assertEqual "the first constructor name page is bounded" (Text.take 8192 name) (displayStateText state)
  case displayStateKeys state of
    [(nameKey, _), (fieldsKey, _)] -> do
      fields <- requireExpansion fieldsKey state
      assertEqual "fields can be reached before the constructor name finishes" " {left = A, right = B}" (displayStateText fields)
      middle <- requireExpansion nameKey fields
      assertEqual "the constructor name cursor advances" (Text.replicate 8192 "x") (displayStateText middle)
      case displayStateKeys middle of
        [(lastKey, _)] -> do
          final <- requireExpansion lastKey middle
          assertEqual "the final constructor name suffix is exact" "xxxxxxx" (displayStateText final)
          assertEqual "the constructor name finishes at the capped grant" [] (displayStateKeys final)
        _ -> fail "constructor name suffix was lost or repeated"
    _ -> fail "constructor name and fields must have separate cursors"
  where
    requireExpansion key state = maybe (fail "constructor cursor was lost") pure
      (expandDisplayState 8192 key state)

oversizedFieldNamesRetainTheirSuffixAndValue :: IO ()
oversizedFieldNamesRetainTheirSuffixAndValue = do
  let field = Text.replicate 100 "x"
      state = newDisplayState 32 (Constructor "Outer" [(field, TextLeaf "A"), ("right", TextLeaf "B")])
      visiblePrefix = "Outer {" <> Text.take 23 field <> "}"
  assertEqual "a field prefix stays within its page allowance" visiblePrefix (displayStateText state)
  case displayStateKeys state of
    [(nameKey, _), (valueKey, _), (restKey, _)] -> do
      value <- requireExpansion valueKey state
      assertEqual "the field value is reachable before its name finishes" "A" (displayStateText value)
      suffix <- requireExpansion nameKey value
      assertEqual "the true field-name suffix is retained" (Text.drop 23 field <> " = ") (displayStateText suffix)
      siblings <- requireExpansion restKey suffix
      assertEqual "following fields remain independently available" " {right = B}" (displayStateText siblings)
    _ -> fail "a clipped field prefix lost its name, value, or siblings"
  let lazyState = newDisplayState 32 (Constructor "Outer" [(field, undefined)])
  _ <- evaluate (Text.length (displayStateText lazyState))
  assertEqual "a clipped field prefix does not force its value" 3 (length (displayStateKeys lazyState))
  where
    requireExpansion key state = maybe (fail "field cursor was lost") pure
      (expandDisplayState 256 key state)

unavailableDetailDoesNotReplaceSupportedText :: IO ()
unavailableDetailDoesNotReplaceSupportedText = do
  let state = newDisplayState 10 (Concat [LegacyLeaf (\_ -> ("old", True)), TextLeaf "visible"])
  assertEqual "supported payload stays visible beside legacy unavailable detail" "oldvisible" (displayStateText state)
  assertEqual "legacy unavailability is separate from the visible payload" True (displayStateUnavailable state)
  case displayStateKeys state of
    [(key, _)] -> case expandDisplayState 64 key state of
      Just detail -> assertEqual "unavailable detail survives later sibling expansion" True (displayStateUnavailable detail)
      Nothing -> fail "supported tree suffix was lost"
    _ -> fail "lazy exact-boundary suffix was not retained"

sourceWindowsRetainRevisionAndLineAddresses :: IO ()
sourceWindowsRetainRevisionAndLineAddresses = do
  let captured = ("original-revision", "α\n\nλ\n終\n")
  assertEqual "numbered windows retain identity and empty lines"
    (SourceWindow "original-revision" [(2,""),(3,"λ")]) (sourceLines 2 2 captured)
  assertEqual "successive windows cover the same captured revision"
    [(1,"α"),(2,""),(3,"λ"),(4,"終")]
    (concatMap (sourceWindowLines . (\start -> sourceLines start 2 captured)) [1,3])
  assertEqual "past-end is empty" [] (sourceWindowLines (sourceLines 10 2 captured))
  assertEqual "nonpositive count is empty" [] (sourceWindowLines (sourceLines 1 (-1) captured))
  assertEqual "start clamps to one" [(1,"α")] (sourceWindowLines (sourceLines 0 1 captured))

noticedDisplaysStayBoundedAndRetainDetail :: IO ()
noticedDisplaysStayBoundedAndRetainDetail = do
  let original = Text.replicate 200 "λ"
      initial = newDisplayStateWithNotice 80 (TextLeaf original)
  assertTrue "explicit truncation marker" ("[truncated;" `Text.isInfixOf` displayStateText initial)
  assertTrue "marker counts within allowance" (Text.length (displayStateText initial) <= 80)
  case displayStateKeys initial of
    [(key,_)] -> case expandDisplayStateWithNotice 1024 key initial of
      Just detail -> assertEqual "retained Unicode suffix is exact" original
        (Text.takeWhile (/= '[') (displayStateText initial) <> displayStateText detail)
      Nothing -> error "missing expansion"
    _ -> error "missing retained suffix"
  let unsupported = newDisplayStateWithNotice 128 (Concat [LegacyLeaf (\_ -> ("old", True)), TextLeaf "visible"])
  assertTrue "unavailable detail is explicit" ("unavailable" `Text.isInfixOf` displayStateText unsupported)
  assertTrue "supported detail survives unavailable marker" ("oldvisible" `Text.isPrefixOf` displayStateText unsupported)
  mapM_ (\budget -> assertTrue "all budgets stay bounded"
    (all (\tree -> Text.length (displayStateText (newDisplayStateWithNotice budget tree)) <= max 0 budget &&
                   let (value,_,_) = renderTreeWithNotice budget tree in Text.length value <= max 0 budget)
      [TextLeaf original, LegacyLeaf (\_ -> ("old", True)), Constructor "Pair" [("long", TextLeaf original)]])) [-1..100]

sourceWindowProperty :: String -> NonNegative Int -> NonNegative Int -> Bool
sourceWindowProperty source (NonNegative startInput) (NonNegative countInput) =
  let start = 1 + startInput `mod` 30
      count = countInput `mod` 30
      expected = take count (drop (start - 1) (zip [1..] (lines source)))
      SourceWindow revision actual = sourceLines start count ("original", Text.pack source)
  in revision == "original" && [(number, Text.unpack body) | (number, body) <- actual] == expected


literalSearchProperty :: String -> NonNegative Int -> Bool
literalSearchProperty raw (NonNegative radius) =
  let source = Text.pack raw
      needle = Text.pack (take 3 raw)
      suffixes [] = [[]]
      suffixes xs@(_:rest) = xs : suffixes rest
      begins [] _ = True
      begins _ [] = False
      begins (a:as) (b:bs) = a == b && begins as bs
      expected = [(n, n + length (Text.unpack needle)) | (n, rest) <- zip [0..] (suffixes (Text.unpack source)), begins (Text.unpack needle) rest]
  in case literalSearch "captured" (radius `mod` 16) needle source of
       Left EmptyNeedle -> Text.null needle
       Left _ -> False
       Right matches -> occurrenceLocations matches == expected
         && occurrenceCount matches == length expected
         && all (\slice -> sliceIdentity slice == "captured"
           && sliceText slice == Text.take (sliceEnd slice - sliceStart slice) (Text.drop (sliceStart slice) source)
           && Text.length (sliceText slice) <= Text.length needle + 2 * (radius `mod` 16)) (occurrenceContexts matches)

retainedTextPreservesNewlinesAndBoundsHugeLine :: IO ()
retainedTextPreservesNewlinesAndBoundsHugeLine = do
  let source = "α\r\n\n🙂last\n"
  assertEqual "line concatenation preserves exact text" source (Text.concat (retainedLines source))
  assertEqual "CRLF retained" (Right (TextSlice "id" 0 3 "α\r\n" False True)) (retainedWindow "id" 1 1 source)
  assertEqual "invalid windows refuse" (Left InvalidLineWindow) (retainedWindow "id" 0 1 source)
  assertEqual "out of range refuses" (Left LineOutsideCapture) (retainedWindow "id" 4 4 source)
  assertEqual "empty needle refuses" (Left EmptyNeedle) (literalSearch "id" 5 "" source)
  let huge = Text.replicate 100000 "x" <> "🙂🙂🙂" <> Text.replicate 100000 "x"
  case literalSearch "huge" 2 "🙂🙂" huge of
    Left issue -> fail (show issue)
    Right matches -> do
      assertEqual "overlapping unicode matches" [(100000,100002),(100001,100003)] (occurrenceLocations matches)
      assertTrue "contexts bounded independently of line size" (all ((<= 6) . Text.length . sliceText) (occurrenceContexts matches))
