{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE DataKinds #-}
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
import qualified Tidepool.Lookup.Tools as Tools
import Tidepool.Agent.Contract (compileTools, AsServerT)
import Control.Monad.Freer (Eff)
import Tidepool.Effects.Core (Lookup, Console)

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
  [] "immutable-view" Nothing "fixture-capture"

assert :: String -> Bool -> IO ()
assert label value = unless value (fail label)
lookupNavigationTests :: TestTree
lookupNavigationTests = testGroup "retained lookup navigation"
  [testCase "hosted query and retained read compile through ordinary tool owner" $
     assert "compiled retained tool surface" (case compileTools (Tools.tools :: Tools.LookupTools (AsServerT (Eff '[Lookup, Console]))) of Right _ -> True; Left _ -> False)
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
