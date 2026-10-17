shown <- do
  let fragments = LazyText.fromChunks ["α\r", "\n🙂ab", "aba\n"]
      (before, after) = LazyText.breakOn "baba" fragments
      validFragments = LazyText.length fragments == 10
        && LazyText.toStrict (LazyText.take 4 (LazyText.drop 1 fragments)) == "\r\n🙂a"
        && LazyText.toStrict before == "α\r\n🙂a"
        && LazyText.toStrict after == "baba\n"
  if validFragments then pure () else error "retained fragment navigation changed Unicode or cross-chunk matching"
  let example = LookupApi.LookupExample "main.hs" "network access at runtime" ["setup.hs"] (T.replicate 3000 "α\r\n" <> "last-main-marker") (Just "main-digest") Nothing [LookupApi.LookupSource "setup.hs" (Just "setup-digest") (Just "complete-setup-marker\n") Nothing]
      entry = LookupApi.LookupEntry "operation" (Just "Owner") "operation :: Text" LookupApi.LookupValue LookupApi.LookupAvailable LookupApi.LookupModuleExport LookupApi.LookupExact Nothing (Just "exact-owner-contract-marker") (Just example)
      captured = LookupApi.LookupBatch [LookupApi.LookupResult "operation" (LookupApi.LookupFound [entry] False)] [] "original-view" Nothing "original-capture" (LookupApi.LookupSearchReceipt "original-view" LookupApi.LookupSearchCaptured [] [] (LookupApi.LookupDiscoveryNotRun LookupApi.LookupSearchUnavailable))
      select payload = case Json.eitherDecode payload of
        Left issue -> TextLeaf (issue)
        Right selections -> displayTree (Nav.selectLookupMany captured selections)
  displaySelectable (Nav.lookupIndexTree captured) select
