resA <- investigate "/home/inanna/dev/shoal-evals/tui-test-app" "f726882" ["src/panels/"] "bash ./check.sh" (101 :: Int) rawA
case resA of
  (sym, leaf, keyed, verdict, found) -> object
    [ "symbol" .= sym
    , "locations" .= [ object ["id" .= k, "why" .= w, "at" .= l] | (k, w, l) <- keyed ]
    , "groups" .= either (\e -> String (T.pack (show e))) (toJSON . J.answers) verdict
    , "sites" .= either (\e -> String (T.pack (show e))) (toJSON . J.answers) found ]
