{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE DataKinds #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE TypeOperators #-}
{-# LANGUAGE OverloadedStrings #-}
module LookupNavigationTest (lookupNavigationTests) where
import Prelude
import qualified Project.RetainedLookupExample as Example
import Control.Monad (unless)
import qualified Data.Text as T
import Test.Tasty.QuickCheck (testProperty, NonNegative(..))
import Tidepool.Test.Runner
import Tidepool.Lookup
import Tidepool.Lookup.Navigation
import Tidepool.Text.Navigation (TextSlice(..), TextSelectionIssue(..))
import qualified Tidepool.Lookup.Tools as Tools
import Tidepool.Agent.Contract (compileTools, AsServerT, CompiledTools(..))
import Tidepool.Aeson.Value (object, (.=))
import Control.Monad.Freer (Eff, run, interpret, reinterpret)
import Control.Monad.Freer.State (State, modify, runState)
import Control.Monad.Freer.Internal (handleRelay)
import Data.List (nubBy)
import Tidepool.Inspection (displayWith, displayTree, DisplayTree(..))
import Tidepool.Inspection.Tree (renderTree)
import Tidepool.Effects.Core (Lookup(..), Console(..))

entry :: T.Text -> LookupEntry
entry source = LookupEntry "same" (Just "Owner") "same :: Text" LookupValue LookupAvailable LookupModuleExport LookupExact Nothing (Just "owner contract")
  (Just (LookupExample "main.hs" "runtime network access" ["setup.hs"] source (Just "main-digest") Nothing
    [LookupSource "setup.hs" (Just "setup-digest") (Just "module Setup where\r\nsetup = 1\n") Nothing]))
batch :: T.Text -> LookupBatch
batch source = LookupBatch
  [LookupResult "same" (LookupFound [entry source, entry "other\n"] True),
   LookupResult "same" (LookupFound [entry "second-result"] False),
   LookupResult "missing" (LookupMissing ["missing"] []),
   LookupResult "error" (LookupRejected "compiler refused")]
  [] "immutable-view" Nothing "fixture-capture" (LookupSearchReceipt "immutable-view" LookupSearchCaptured [] [] (LookupDiscoveryNotRun LookupSearchUnavailable))

assert :: String -> Bool -> IO ()
assert label value = unless value (fail label)
lookupNavigationTests :: TestTree
lookupNavigationTests = testGroup "retained lookup navigation"
  [lookupPackingTests
  ,testCase "default lookup dispatch applies the display budget before later results" $ do
     tooling <- either (fail . show) pure (compileTools (Tools.tools :: Tools.LookupTools (AsServerT (Eff '[Lookup, Console]))))
     let original = (batch "source") {lookupResults =
           LookupResult "same" (LookupFound [entry "source"] False) : error "default tool forced an unrequested result"}
         lookupHost :: Lookup value -> Eff '[Console] value
         lookupHost (LookupRaw _) = pure original
         console :: Console value -> Eff '[State [T.Text]] value
         console DisplayAllowanceWith = pure 160
         console (DisplaySelectableWith (_, text, _, _) _) = modify (++ [text]) >> pure (7, 11, 13)
         console _ = error "unexpected default lookup console operation"
         observed = run $ runState [] $ reinterpret console $ interpret lookupHost $
           dispatch tooling "lookup" (object ["queries" .= (["same"] :: [T.Text])])
     case observed of
       (Right _, [text]) -> assert "ordinary dispatch publishes a bounded initial index"
         ("fixture-capture" `T.isInfixOf` text && "same" `T.isInfixOf` text && T.length text <= 160)
       _ -> fail "default lookup did not publish exactly one retained display"
  ,testCase "index display leaves unrequested result and setup tails untouched" $ do
     let first = (batch "source") {lookupResults =
           LookupResult "same" (LookupFound [entry "source"] False) : error "unrequested result tail"}
         complete = (batch "source") {lookupResults = [LookupResult "same" (LookupFound [entry "source"] False)]}
         (prefix, remaining, _) = renderTree 70 (lookupIndexTree first)
     assert "prefix agrees with the original complete index" (prefix == T.take 70 (referenceIndex complete))
     assert "result tail is retained" (case remaining of Just _ -> True; Nothing -> False)
     let setup = LookupSource "setup.hs" Nothing Nothing (Just "not captured")
         withSetup sources = (entry "source") {lookupExample = fmap (\example -> example {exampleSetup = sources}) (lookupExample (entry "source"))}
         single sources = complete {lookupResults = [LookupResult "same" (LookupFound [withSetup sources] False)]}
         one = referenceIndex (single [setup])
         allowance = T.length one - 1
         (setupPrefix, _, _) = renderTree allowance (lookupIndexTree (single (setup : error "unrequested setup tail")))
     assert "section ordinals stream without counting all setup entries" (setupPrefix == T.take allowance one)
  ,testProperty "index tree preserves complete text and bounded prefixes" $ \(NonNegative seed) (NonNegative budget) ->
     let original = batch (T.replicate (seed `mod` 9) "α🙂\r\n")
         captured = original {lookupResults = take (seed `mod` 12) (cycle (lookupResults original))}
         expected = referenceIndex captured
         allowance = budget `mod` (T.length expected + 1)
         (prefix, remaining, unavailable) = renderTree allowance (lookupIndexTree captured)
         suffix = case remaining of Nothing -> ""; Just tree -> let (text, _, _) = renderTree maxBound tree in text
         related = captured {lookupIdentity = "related-capture", lookupResults = reverse (lookupResults captured)}
         retained = retainLookupCapture captured (Just related)
     in renderLookupIndex captured == expected && prefix == T.take allowance expected
       && prefix <> suffix == expected && not unavailable
       && renderLookupCaptureIndex retained == "PrimaryLookup\n" <> expected <> "\nRelatedLookup\n" <> referenceIndex related
  ,testProperty "numbered display preserves exact lines and every retained prefix" $ \(NonNegative seed) (NonNegative budget) ->
     let rows = take (seed `mod` 12) (cycle ["α\r\n", "\n", "🙂line\n"])
         final = if even seed then "last🙂" else ""
         source = T.concat rows <> final
         expected = T.pack (concat [show number ++ ": " ++ T.unpack line
           | (number, line) <- zip [1 :: Int ..] (rows ++ [final | not (T.null final)])])
     in case selectLookup (batch source) (LookupSelection (LookupAddress 1 1 Example) Nothing) of
       Left _ -> False
       Right excerpt -> case displayTree excerpt of
         Constructor _ fields -> case lookup "text" fields of
           Nothing -> False
           Just tree ->
             let allowance = budget `mod` (T.length expected + 1)
                 (prefix, remaining, unavailable) = renderTree allowance tree
                 suffix = case remaining of Nothing -> ""; Just rest -> let (text, _, _) = renderTree maxBound rest in text
             in prefix == T.take allowance expected && prefix <> suffix == expected && not unavailable
         _ -> False
  ,testCase "numbered display preserves captured text and usable continuations" $ do
     let captured = retainLookupCapture (batch "first\r\n\r\nλ\nlast") Nothing
         selection = LookupCaptureSelection PrimaryLookup
           (LookupSelection (LookupAddress 1 1 Example) (Just (LookupLines 2 3)))
     case selectLookupCapture captured selection of
       Left issue -> fail (show issue)
       Right value -> do
         let (rendered, _) = displayWith 65536 value
         assert "line labels retain captured addresses" ("2: " `T.isInfixOf` rendered && "3: λ" `T.isInfixOf` rendered)
         assert "semantic text remains exact" (excerptText (captureExcerpt value) == "\r\nλ\n")
         assert "continuations remain directly reusable" (map (fmap (excerptText . captureExcerpt))
           (selectLookupCaptureMany captured (captureContinuations value)) == [Right "first\r\n", Right "last"])
  ,testCase "literal search counts overlaps and bounded contexts retain reusable exact ranges" $ do
     let source = "🙂ααα\r\n" <> T.replicate 20000 "x" <> "αα"
         primary = batch "primary"
         related = (batch source) {lookupIdentity = "related-capture"}
         captured = retainLookupCapture primary (Just related)
         query = LookupLiteralQuery RelatedLookup (LookupAddress 1 1 Example) "αα" 1 2
     case searchLookupCapture captured query of
       Left issue -> fail (show issue)
       Right found -> do
         let matches = literalMatches found
         assert "all overlaps counted but contexts bounded" (literalCount found == 3 && literalOmitted found == 1
           && length matches == 2 && all ((<= 4) . T.length . sliceText . literalMatchContext) matches)
         assert "issuing provenance retained" (literalBatchIdentity found == "related-capture"
           && literalView found == "immutable-view" && literalSourceIdentity found == Just "main-digest")
         assert "Unicode scalar match offsets" (map literalMatchSelection matches ==
           [LookupCaptureTextSelection RelatedLookup (LookupAddress 1 1 Example) "related-capture" "immutable-view" 1 3,
            LookupCaptureTextSelection RelatedLookup (LookupAddress 1 1 Example) "related-capture" "immutable-view" 2 4])
         assert "exact match ranges replay in caller order" (map (fmap (sliceText . captureTextSlice))
           (selectLookupCaptureTextMany captured (map literalMatchSelection (reverse matches ++ matches))) == replicate 4 (Right "αα"))
         assert "contexts replay exactly" (map (fmap captureTextSlice) (selectLookupCaptureTextMany captured
           (map literalContextSelection matches)) == map (Right . literalMatchContext) matches)
         assert "pure example uses the same search" (Example.searchExample captured "primary" ==
           searchLookupCapture captured (LookupLiteralQuery PrimaryLookup (LookupAddress 1 1 Example) "primary" 40 4))
         assert "pure exact-match example replays same selections" (Example.exactMatches captured found ==
           selectLookupCaptureTextMany captured (map literalMatchSelection matches))
  ,testCase "literal search addresses reject unavailable stages sections needles and invalid ranges" $ do
     let primary = batch "α\r\n🙂"
         captured = retainLookupCapture primary Nothing
         at = LookupAddress 1 1 Example
         query part section needle radius limit = LookupLiteralQuery part section needle radius limit
         search = searchLookupCapture captured
         select start end = selectLookupCaptureText captured (LookupCaptureTextSelection PrimaryLookup at "fixture-capture" "immutable-view" start end)
     assert "empty literal refused" (search (query PrimaryLookup at "" 0 2) == Left (CaptureTextIssue EmptyNeedle))
     assert "missing stage remains explicit" (search (query RelatedLookup at "α" 0 2) == Left (CaptureNotRetained RelatedLookup))
     assert "unretained source remains explicit" (case search (query PrimaryLookup (LookupAddress 1 1 Source) "α" 0 2) of
       Left (CaptureSelectionIssue (SectionUnavailable _)) -> True; _ -> False)
     assert "invalid search budgets refused" (search (query PrimaryLookup at "α" (-1) 2) == Left (CaptureInvalidSearchBounds (-1) 2)
       && search (query PrimaryLookup at "α" 0 (-1)) == Left (CaptureInvalidSearchBounds 0 (-1)))
     assert "out-of-range character selections refuse rather than clamp" (select (-1) 1 == Left (CaptureInvalidTextRange (-1) 1 4)
       && select 2 1 == Left (CaptureInvalidTextRange 2 1 4)
       && select 0 5 == Left (CaptureInvalidTextRange 0 5 4))
     assert "CRLF selection preserves bytes as text" (fmap (sliceText . captureTextSlice) (select 1 3) == Right "\r\n")
     assert "complete example is exact captured source" (fmap (excerptText . captureExcerpt) (Example.completeExample captured) == Right "α\r\n🙂")
     assert "zero limit retains count and reports omissions" (case search (query PrimaryLookup at "α" 0 0) of
       Right found -> literalCount found == 1 && literalOmitted found == 1 && null (literalMatches found); _ -> False)
     assert "absent needle is a successful empty search" (case search (query PrimaryLookup at "absent" 0 1) of
       Right found -> literalCount found == 0 && literalOmitted found == 0 && null (literalMatches found); _ -> False)
     assert "empty text and terminal ranges remain exact" (fmap (sliceText . captureTextSlice) (select 4 4) == Right ""
       && fmap (sliceText . captureTextSlice) (selectLookupCaptureText (retainLookupCapture (batch "") Nothing)
         (LookupCaptureTextSelection PrimaryLookup at "fixture-capture" "immutable-view" 0 0)) == Right "")
  ,testCase "reusable literal locations refuse changed issuing batch or view" $ do
     let original = batch "original token"
         captured = retainLookupCapture original Nothing
         at = LookupAddress 1 1 Example
         replacement = original {lookupIdentity = "replacement-capture", lookupResults = lookupResults (batch "replacement token")}
         changedView = original {lookupView = "changed-view", lookupSearchReceipt =
           (lookupSearchReceipt original) {searchView = "changed-view"}}
     case searchLookupCapture captured (LookupLiteralQuery PrimaryLookup at "token" 2 1) of
       Left issue -> fail (show issue)
       Right found -> do
         let selections = literalSource found : concatMap (\hit ->
               [literalMatchSelection hit, literalContextSelection hit]) (literalMatches found)
         assert "locations retain the exact issuing facts" (all (\selection ->
           textBatchIdentity selection == lookupIdentity original && textView selection == lookupView original) selections)
         assert "same view with another batch refuses every issued location" (
           selectLookupCaptureTextMany (retainLookupCapture replacement Nothing) selections ==
           replicate (length selections) (Left (CaptureBatchMismatch "fixture-capture" "replacement-capture")))
         assert "same batch with another view refuses every issued location" (
           selectLookupCaptureTextMany (retainLookupCapture changedView Nothing) selections ==
           replicate (length selections) (Left (CaptureViewMismatch "immutable-view" "changed-view")))
         assert "original locations remain readable" (all (either (const False) (const True))
           (selectLookupCaptureTextMany captured selections))
  ,testProperty "literal search agrees with independent String overlap oracle and exact replay" $ \(NonNegative seed) (NonNegative budget) ->
     let alphabet = "α🙂\r\n"
         source = take (seed `mod` 100) (cycle alphabet)
         needle = take (1 + seed `mod` 5) (drop (seed `mod` 4) (cycle alphabet))
         starts = [n | n <- [0..length source - 1], take (length needle) (drop n source) == needle]
         limit = budget `mod` 9
         captured = retainLookupCapture (batch (T.pack source)) Nothing
         at = LookupAddress 1 1 Example
         query = LookupLiteralQuery PrimaryLookup at (T.pack needle) (seed `mod` 5) limit
     in case searchLookupCapture captured query of
       Left _ -> False
       Right found -> literalCount found == length starts && literalOmitted found == max 0 (length starts - limit)
         && map literalMatchSelection (literalMatches found) ==
           [LookupCaptureTextSelection PrimaryLookup at "fixture-capture" "immutable-view" n (n + length needle) | n <- take limit starts]
         && map (fmap (sliceText . captureTextSlice)) (Example.exactMatches captured found) ==
           replicate (min limit (length starts)) (Right (T.pack needle))
  ,testCase "hosted query and retained read compile through ordinary tool owner" $
     assert "compiled retained tool surface" (case compileTools (Tools.tools :: Tools.LookupTools (AsServerT (Eff '[Lookup, Console]))) of Right _ -> True; Left _ -> False)
  ,testCase "two-stage capture preserves issuing batches receipts and continuation addresses" $ do
     let primary = batch "primary\nremaining-primary\n"
         related = (batch "related\nremaining-related\n") {lookupIdentity = "related-capture"}
         captured = retainLookupCapture primary (Just related)
         selection = LookupCaptureSelection RelatedLookup (LookupSelection (LookupAddress 1 1 Example) (Just (LookupLines 1 1)))
     assert "native receipts stay separate" (lookupCaptureReceipts captured ==
       [(PrimaryLookup, lookupSearchReceipt primary), (RelatedLookup, lookupSearchReceipt related)])
     case selectLookupCapture captured selection of
       Left issue -> fail (show issue)
       Right excerpt -> do
         assert "issuing batch provenance" (excerptBatchIdentity (captureExcerpt excerpt) == "related-capture"
           && excerptView (captureExcerpt excerpt) == lookupView related
           && excerptText (captureExcerpt excerpt) == "related\n")
         assert "continuations target related batch" (captureContinuations excerpt ==
           [LookupCaptureSelection RelatedLookup (LookupSelection (LookupAddress 1 1 Example) (Just (LookupLines 2 2)))])
         assert "continuation returns exact remaining source" (case selectLookupCaptureMany captured (captureContinuations excerpt) of
           [Right suffix] -> excerptText (captureExcerpt suffix) == "remaining-related\n"
           _ -> False)
  ,testCase "related refusal never invalidates primary or conceals its own stage" $ do
     let primary = batch "retained-primary\n"
         receipt = lookupSearchReceipt primary
         stale = primary {lookupView = "changed-view", lookupResults = [], lookupIssue = Just "lookup compile view changed",
           lookupSearchReceipt = receipt {searchView = "changed-view", searchDisposition = LookupSearchNotExecuted,
             searchDiscovery = LookupDiscoveryNotRun LookupSearchStaleView}}
         invalidated = primary {lookupResults = [], lookupIssue = Just "lookup compile view changed",
           lookupSearchReceipt = receipt {searchDisposition = LookupSearchInvalidated (searchMatches receipt) (searchDiscovery receipt),
             searchMatches = [], searchDiscovery = LookupDiscoveryInvalidated}}
         unavailable = primary {lookupIssue = Just "compiler unavailable",
           lookupResults = [LookupResult "failed" (LookupRejected "compiler unavailable")]}
         selection part = LookupCaptureSelection part (LookupSelection (LookupAddress 1 1 Example) Nothing)
     mapM_ (\related -> do
       let captured = retainLookupCapture primary (Just related)
       assert "receipt stage and view unchanged" (lookupCaptureReceipts captured ==
         [(PrimaryLookup, receipt), (RelatedLookup, lookupSearchReceipt related)])
       assert "primary remains readable" (case selectLookupCapture captured (selection PrimaryLookup) of
         Right excerpt -> excerptText (captureExcerpt excerpt) == "retained-primary\n"
         _ -> False)
       assert "refused related not exposed as found entries" (all (\(part, hit) -> part == PrimaryLookup || hitEntry hit == Nothing) (lookupCaptureIndex captured))
       assert "search and character reads preserve the original refusal" (
         fmap (const ()) (searchLookupCapture captured
           (LookupLiteralQuery RelatedLookup (LookupAddress 1 1 Example) "retained" 1 2)) ==
           fmap (const ()) (selectLookupCapture captured (selection RelatedLookup))
         && fmap (const ()) (selectLookupCaptureText captured
           (LookupCaptureTextSelection RelatedLookup (LookupAddress 1 1 Example) "fixture-capture" "immutable-view" 0 1)) ==
           fmap (const ()) (selectLookupCapture captured (selection RelatedLookup)))
       assert "related selection refuses" (case selectLookupCapture captured (selection RelatedLookup) of
         Left (CaptureRefused RelatedLookup _ _) -> True
         Left (CaptureViewMismatch _ _) -> True
         Left (CaptureSelectionIssue (ResultRejected _)) -> True
         _ -> False)) [stale, invalidated, unavailable]
  ,testProperty "generated two-stage selections keep distinct issuing captures and replay order" $ \(NonNegative pick :: NonNegative Int) ->
     let primary = batch "primary-source"
         related = (batch "related-source") {lookupIdentity = "related-capture"}
         captured = retainLookupCapture primary (Just related)
         part = if even pick then PrimaryLookup else RelatedLookup
         other = if even pick then RelatedLookup else PrimaryLookup
         selection which = LookupCaptureSelection which (LookupSelection (LookupAddress 1 1 Example) Nothing)
         expected which = if which == PrimaryLookup then ("fixture-capture", "primary-source") else ("related-capture", "related-source")
     in case selectLookupCaptureMany captured (map selection [part,other,part]) of
       [Right first,Right second,Right repeated] ->
         first == repeated && map (\excerpt -> (excerptBatchIdentity (captureExcerpt excerpt), excerptText (captureExcerpt excerpt))) [first,second] == map expected [part,other]
       _ -> False
  ,testCase "revisit multiselect duplicate hits complete large example and setup" $ do
     let source = T.replicate 3000 "α\r\n" <> "last🙂"
         captured = batch source
         selections = [LookupSelection (LookupAddress 2 1 Example) Nothing,
                       LookupSelection (LookupAddress 1 2 Example) Nothing,
                       LookupSelection (LookupAddress 1 1 Example) Nothing,
                       LookupSelection (LookupAddress 1 1 (Setup 1)) Nothing]
         results = selectLookupMany captured selections
     assert "selection order and full source" (map (fmap excerptText) results == map Right ["second-result", "other\n", source, "module Setup where\r\nsetup = 1\n"])
     assert "compiled public example uses same pure selector" (Example.firstSignatureLine captured == selectLookup captured (LookupSelection (LookupAddress 1 1 Signature) (Just (LookupLines 1 1))))
     assert "repeat selection is immutable" (results == selectLookupMany captured selections)
     assert "duplicate queries retain distinct addresses" (map hitResult (lookupIndex captured) == [1,1,2,3,4])
     assert "explicit missing" (selectLookup captured (LookupSelection (LookupAddress 3 1 Signature) Nothing) == Left (ResultMissing ["missing"] []))
     assert "explicit source absence" (case selectLookup captured (LookupSelection (LookupAddress 1 1 Source) Nothing) of Left (SectionUnavailable _) -> True; _ -> False)
     let missingSetup = (entry source) {lookupExample = fmap (\example -> example {exampleSetup = [LookupSource "missing.hs" Nothing Nothing (Just "not found at capture")]}) (lookupExample (entry source))}
         partial = captured {lookupResults = [LookupResult "same" (LookupFound [missingSetup] False)]}
     assert "bundle cannot hide absent setup" (case selectLookup partial (LookupSelection (LookupAddress 1 1 Bundle) Nothing) of Right result -> not (null (excerptUnavailable result)); _ -> False)
  ,testProperty "generated windows preserve independent exact String lines and replay order" $ \(NonNegative first) (NonNegative count) ->
     let rows = ["α\r\n", "\n", "🙂three\n", "last"]
         source = T.concat rows
         start = 1 + first `mod` 4
         end = min 4 (start + count `mod` 4)
         selection = LookupSelection (LookupAddress 1 1 Example) (Just (LookupLines start end))
         captured = batch source
         expected = T.concat (take (end - start + 1) (drop (start - 1) rows))
     in case selectLookup captured selection of
       Left _ -> False
       Right result -> excerptText result == expected && excerptLines result == Just (LookupLines start end)
         && excerptSourceIdentity result == Just "main-digest" && excerptView result == "immutable-view"
         && selectLookupMany captured [selection, selection] == [Right result, Right result]
  ,testProperty "generated selection histories preserve order refusals and original capture" $ \(actions :: [Int]) ->
     let source = "α\r\n\n🙂three\nlast"
         captured = batch source
         action n = case n `mod` 8 of
           0 -> (LookupSelection (LookupAddress 1 1 Example) Nothing, Right source)
           1 -> (LookupSelection (LookupAddress 1 2 Example) Nothing, Right "other\n")
           2 -> (LookupSelection (LookupAddress 2 1 Example) Nothing, Right "second-result")
           3 -> (LookupSelection (LookupAddress 1 1 Contract) Nothing, Right "owner contract")
           4 -> (LookupSelection (LookupAddress 1 1 (Setup 1)) Nothing, Right "module Setup where\r\nsetup = 1\n")
           5 -> (LookupSelection (LookupAddress 0 1 Example) Nothing, Left InvalidResult)
           6 -> (LookupSelection (LookupAddress 1 1 Example) (Just (LookupLines 0 1)), Left InvalidLines)
           _ -> (LookupSelection (LookupAddress 3 1 Signature) Nothing, Left (ResultMissing ["missing"] []))
         history = map action (take 64 actions)
         observed = map (fmap excerptText) (selectLookupMany captured (map fst history))
     in observed == map snd history
       && observed == map (fmap excerptText) (selectLookupMany captured (map fst history))
  ]

-- The original eager formatter is an independent text oracle for the tree.
referenceIndex :: LookupBatch -> T.Text
referenceIndex captured = "Capture " <> lookupIdentity captured <> " (view " <> lookupView captured <> ")\n"
  <> T.intercalate "\n" [T.pack (show (hitResult hit)) <> maybe "" (("." <>) . T.pack . show) (hitEntry hit)
     <> " " <> hitQuery hit <> maybe "" (" — " <>) (hitName hit)
     <> maybe "" (\owner -> " (" <> owner <> ")") (hitModule hit)
     <> " [" <> hitStatus hit <> "] " <> T.pack (show (hitSections hit)) | hit <- lookupIndex captured]
  <> maybe "" ("\nLookup issue: " <>) (lookupIssue captured)


candidate :: Int -> T.Text -> LookupCandidate
candidate ordinal summary = LookupCandidate ("Fixture." <> T.pack (show ordinal)) ["root"] summary True Nothing

primary :: [LookupCandidate] -> LookupBatch
primary candidates = (batch "primary-source") {lookupCandidates = candidates}

observeLookup :: (LookupRequest -> LookupBatch) -> Eff '[Lookup] a -> ([LookupRequest], a)
observeLookup respond = run . handleRelay (\value -> pure ([], value))
  (\(LookupRaw request) resume -> do
    (requests, value) <- resume (respond request)
    pure (request : requests, value))

-- Independently recompute policy from original String metadata, never from
-- production assignments, candidateText, or packCandidates' output.
packingOracle :: [LookupCandidate] -> [Int]
packingOracle candidates = fit (8192 * 4) unique
  where
    unique = take 128 (nubBy (\(_, a) (_, b) -> candidateQuery a == candidateQuery b
      && candidateReference a == candidateReference b) (zip [1..] candidates))
    fit _ [] = []
    fit remaining ((ordinal, value) : rest) =
      let summary = take 2048 (T.unpack (candidateSummary value))
          cost = length (T.unpack (candidateQuery value)) + length ("\nFor queries: " :: String)
            + length (T.unpack (T.intercalate ", " (candidateOrigins value))) + 1 + length summary + 1024
      in if cost > remaining then fit remaining rest else ordinal : fit (remaining - cost) rest

lookupPackingTests :: TestTree
lookupPackingTests = testGroup "lookup evidence"
  [ testProperty "packing agrees with independent scalar oracle across duplicates and threshold histories" $ \(actions :: [Int]) ->
      let candidates = [candidate (n `mod` 150) (T.replicate (abs (n `mod` 4097)) "🙂") | n <- take 180 actions]
          packed = Tools.packCandidates "immutable-view" candidates
      in map Tools.packedOrdinal (Tools.packingCandidates packed) == packingOracle candidates
        && map Tools.assignmentOrdinal (Tools.packingAssignments packed) == [1..length candidates]
        && all (\value -> Tools.packedOriginal value == candidates !! (Tools.packedOrdinal value - 1)) (Tools.packingCandidates packed)
  , testCase "Unicode shortening duplicates budget omission and cap all have assignments" $ do
      let unicode = candidate 1 (T.replicate 3000 "🙂")
          oversized = (candidate 2 "brief") {candidateQuery = T.replicate 40000 "x"}
          values = [unicode, unicode, oversized, candidate 3 "brief"]
          packed = Tools.packCandidates "immutable-view" values
      assert "every candidate assigned" (map Tools.assignmentDecision (Tools.packingAssignments packed) ==
        [Tools.CandidatePacked (head (Tools.packingCandidates packed))
          (length (T.unpack (Tools.candidateText ((head values) {candidateSummary = T.replicate 2048 "🙂"}))) + 1024) True,
         Tools.CandidateDuplicate 1, Tools.CandidateBudgetOmitted (length (T.unpack (Tools.candidateText oversized)) + 1024),
         Tools.CandidatePacked (last (Tools.packingCandidates packed))
          (length (T.unpack (Tools.candidateText (last values))) + 1024) False])
      assert "full original metadata remains available" (Tools.packedOriginal (head (Tools.packingCandidates packed)) == unicode)
      let capped = Tools.packCandidates "immutable-view" [candidate n (T.replicate 40000 "x") | n <- [1..129]]
      assert "cap precedes aggregate budget" (Tools.assignmentDecision (last (Tools.packingAssignments capped)) == Tools.CandidateCapOmitted)
  , testCase "large capped pools retain one assignment per source with a bounded prefix inventory" $ do
      let values = [candidate n "brief" | n <- [1..10000]]
          prefix = Tools.packCandidates "immutable-view" (take 128 values)
          allRows = Tools.packCandidates "immutable-view" values
      assert "admitted pool and scoring budget unchanged after cap" (Tools.packingCandidates allRows == Tools.packingCandidates prefix
        && Tools.packingRemainingScalars allRows == Tools.packingRemainingScalars prefix)
      assert "all originals have a trace row" (map Tools.assignmentOrdinal (Tools.packingAssignments allRows) == [1..10000])
      assert "suffix is cap omitted without further deduplication" (all ((== Tools.CandidateCapOmitted) . Tools.assignmentDecision)
        (drop 128 (Tools.packingAssignments allRows)))
  , testCase "aggregate Unicode scalar threshold is inclusive with exact budget accounting" $ do
      let base = candidate 1 ""
          overhead = length (T.unpack (Tools.candidateText base)) + 1024 - 4
          exact = base {candidateOrigins = [T.replicate (8192 * 4 - overhead) "🙂"]}
          over = exact {candidateOrigins = map (<> "🙂") (candidateOrigins exact)}
          fits = Tools.packCandidates "immutable-view" [exact, candidate 2 "brief"]
          omitted = Tools.packCandidates "immutable-view" [over, candidate 2 "brief"]
      assert "exact threshold admitted" (map Tools.packedOrdinal (Tools.packingCandidates fits) == [1]
        && Tools.packingRemainingScalars fits == 0)
      assert "one scalar over omitted without crowding out later candidate" (map Tools.packedOrdinal (Tools.packingCandidates omitted) == [2])
  , testCase "same spelling namespace references stay distinct and expand exactly" $ do
      let typeReference = LookupReference "Owner" "T" LookupTypeNamespace
          constructorReference = LookupReference "Owner" "T" LookupConstructorNamespace
          values = [(candidate 1 "brief") {candidateQuery = "T", candidateReference = Just reference}
            | reference <- [typeReference, constructorReference]]
          original = primary values
          (requests, captured) = observeLookup (const original) (Tools.captureWith
            (\_ pool -> pure (Tools.SelectionSucceeded () (map Tools.packedIdentity pool))) (Tools.LookupArguments ["root"]))
      assert "exact canonical reference pair expanded with original view" (length requests == 2
        && lookupQueries (requests !! 1) == [] && lookupReferences (requests !! 1) == [typeReference, constructorReference]
        && lookupExpectedView (requests !! 1) == Just "immutable-view")
      assert "different namespaces have different candidate identities" (case Tools.captureSelected captured of
        [first,second] -> Tools.packedIdentity first /= Tools.packedIdentity second
        _ -> False)
  , testCase "failed skipped and successful empty selection remain distinct" $ do
      let original = primary [candidate 1 "brief"]
          capture select = observeLookup (const original) (Tools.captureWith select (Tools.LookupArguments ["root"]))
          (failedRequests, failed) = capture (\_ _ -> pure (Tools.SelectionFailed ("typed refusal" :: T.Text)))
          (emptyRequests, empty) = capture (\_ _ -> pure (Tools.SelectionSucceeded ("completed" :: T.Text) []))
          (skippedRequests, skipped) = observeLookup (const (primary []))
            (Tools.captureWith (\_ _ -> error "empty pool invoked selector" :: Eff '[Lookup] (Tools.SelectionOutcome T.Text)) (Tools.LookupArguments ["root"]))
          refused = original {lookupSearchReceipt = (lookupSearchReceipt original) {searchDisposition = LookupSearchNotExecuted}}
          (refusedRequests, unavailable) = observeLookup (const refused)
            (Tools.captureWith (\_ _ -> error "refused primary invoked selector" :: Eff '[Lookup] (Tools.SelectionOutcome T.Text)) (Tools.LookupArguments ["root"]))
      assert "all preserve original with no expansion" (map length [failedRequests, emptyRequests, skippedRequests, refusedRequests] == [1,1,1,1]
        && Tools.capturePrimary failed == original && Tools.capturePrimary empty == original)
      assert "typed failure retained" (case Tools.captureSelection failed of Tools.SelectionFailed "typed refusal" -> True; _ -> False)
      assert "successful empty retained" (case Tools.captureSelection empty of Tools.SelectionSucceeded "completed" [] -> True; _ -> False)
      assert "empty pool skipped" (case Tools.captureSelection skipped of Tools.SelectionSkipped Tools.NoCandidates -> True; _ -> False)
      assert "unavailable primary skipped with native refusal retained" (case Tools.captureSelection unavailable of
        Tools.SelectionSkipped Tools.PrimaryUnavailable -> Tools.capturePrimary unavailable == refused
        _ -> False)
  , testCase "selection is canonical capped and exactly view-fenced with no refetch on reporting" $ do
      let original = primary [candidate n (T.replicate 3000 "🙂") | n <- [1..6]]
          foreignId = Tools.packedIdentity (last (Tools.packingCandidates (Tools.packCandidates "immutable-view" [candidate n "brief" | n <- [1..7]])))
          choose _ pool = pure (Tools.SelectionSucceeded ("completed" :: T.Text)
            (foreignId : map Tools.packedIdentity pool ++ map Tools.packedIdentity pool))
          respond request = if lookupDiscover request then original else (batch "related-source") {lookupIdentity = "related-capture"}
          (requests, captured) = observeLookup respond (Tools.captureWith choose (Tools.LookupArguments ["root"]))
          retained = Tools.captureRetained captured
          section = LookupCaptureSelection PrimaryLookup (LookupSelection (LookupAddress 1 1 Example) Nothing)
          (report, _) = displayWith 65536 (Tools.captureSelection captured)
      assert "only selected exact originals expanded once" (length requests == 2 && lookupQueries (requests !! 1) == map candidateQuery (take 4 (lookupCandidates original))
        && lookupExpectedView (requests !! 1) == Just (lookupView original) && not (lookupDiscover (requests !! 1)) && lookupCandidateLimit (requests !! 1) == 0)
      assert "foreign duplicate and cap decisions inspectable" (map Tools.selectionDecision (Tools.captureSelectionAssignments captured) ==
        [Tools.SelectionUnknown, Tools.CandidateSelected, Tools.CandidateSelected, Tools.CandidateSelected, Tools.CandidateSelected,
         Tools.SelectionCapOmitted, Tools.SelectionCapOmitted, Tools.SelectionDuplicate, Tools.SelectionDuplicate,
         Tools.SelectionDuplicate, Tools.SelectionDuplicate, Tools.SelectionCapOmitted, Tools.SelectionCapOmitted])
      assert "full original candidates retained after bounded scoring" (all ((== 3000) . length . T.unpack . candidateSummary . Tools.packedOriginal) (Tools.captureSelected captured))
      assert "report and repeat navigation use retained facts" ("completed" `T.isInfixOf` report && map (fmap (excerptText . captureExcerpt)) (selectLookupCaptureMany retained [section,section]) == [Right "primary-source", Right "primary-source"])
      case Tools.captureExpansion captured of
        Tools.ExpansionRequested exact expanded -> assert "exact expansion evidence" (exact == requests !! 1 && lookupIdentity expanded == "related-capture")
        _ -> fail "missing expansion"
  , testProperty "cross-capture selection refuses stale views changed keys and reordered candidates" $ \(NonNegative index) ->
      let first = 1 + index `mod` 100
          values = [candidate first "brief", candidate (first + 1) "brief"]
          original = primary values
          idFrom view candidates = Tools.packedIdentity (head (Tools.packingCandidates (Tools.packCandidates view candidates)))
          variants = [idFrom "old-view" values, idFrom "immutable-view" (reverse values),
            idFrom "immutable-view" [candidate (first + 2) "brief"]]
          select identity _ _ = pure (Tools.SelectionSucceeded () [identity])
          runSelection identity = observeLookup (const original)
            (Tools.captureWith (select identity) (Tools.LookupArguments ["root"]))
          refused identity = let (requests, captured) = runSelection identity in length requests == 1
            && null (Tools.captureSelected captured)
            && map Tools.selectionDecision (Tools.captureSelectionAssignments captured) == [Tools.SelectionUnknown]
          (sameRequests, same) = runSelection (idFrom "immutable-view" values)
      in all refused variants && length sameRequests == 2
        && map (candidateQuery . Tools.packedOriginal) (Tools.captureSelected same) == [candidateQuery (head values)]
  , testCase "failed optional expansion preserves primary and typed stale refusal" $ do
      let original = primary [candidate 1 "brief"]
          stale = original {lookupView = "changed", lookupIssue = Just "stale", lookupResults = []}
          respond request = if lookupDiscover request then original else stale
          (_, captured) = observeLookup respond (Tools.captureWith (\_ pool -> pure
            (Tools.SelectionSucceeded () (map Tools.packedIdentity pool))) (Tools.LookupArguments ["root"]))
          retained = Tools.captureRetained captured
          section part = LookupCaptureSelection part (LookupSelection (LookupAddress 1 1 Example) Nothing)
      assert "primary preserved" (Tools.capturePrimary captured == original && case selectLookupCapture retained (section PrimaryLookup) of Right _ -> True; _ -> False)
      assert "stale refusal remains explicit" (selectLookupCapture retained (section RelatedLookup) == Left (CaptureViewMismatch "immutable-view" "changed"))
  ]
