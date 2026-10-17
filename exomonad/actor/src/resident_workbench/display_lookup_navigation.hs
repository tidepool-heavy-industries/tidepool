shown <- do
  let example = LookupApi.LookupExample "main.hs" "network access at runtime" ["setup.hs"] (T.replicate 3000 "α\r\n" <> "last-main-marker") (Just "main-digest") Nothing [LookupApi.LookupSource "setup.hs" (Just "setup-digest") (Just "complete-setup-marker\n") Nothing]
      entry = LookupApi.LookupEntry "operation" (Just "Owner") "operation :: Text" LookupApi.LookupValue LookupApi.LookupAvailable LookupApi.LookupModuleExport LookupApi.LookupExact Nothing (Just "exact-owner-contract-marker") (Just example)
      captured = LookupApi.LookupBatch [LookupApi.LookupResult "operation" (LookupApi.LookupFound [entry] False)] [] "original-view" Nothing "original-capture" (LookupApi.LookupSearchReceipt "original-view" LookupApi.LookupSearchCaptured [] [] (LookupApi.LookupDiscoveryNotRun LookupApi.LookupSearchUnavailable))
      select payload =
        let token = if T.null payload then "q" else T.take 1 payload
            fragments = LazyText.fromChunks ["α\r", "\n🙂" <> token, "b" <> token <> "b\n", payload]
            (before, after) = LazyText.breakOn (LazyText.fromStrict (token <> "b")) fragments
            validFragments = LazyText.length fragments == fromIntegral (9 + T.length payload)
              && LazyText.toStrict (LazyText.take 4 (LazyText.drop 1 fragments)) == "\r\n🙂" <> token
              && LazyText.toStrict before == "α\r\n🙂"
              && LazyText.toStrict after == token <> "b" <> token <> "b\n" <> payload
        in if not validFragments then error "retained fragment navigation changed Unicode or cross-chunk matching"
          else case Json.eitherDecode payload of
            Left issue -> TextLeaf (issue)
            Right selections -> displayTree (Nav.selectLookupMany captured selections)
  displaySelectable (Nav.lookupIndexTree captured) select
