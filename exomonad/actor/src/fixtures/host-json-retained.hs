secondSeen <- pure input
secondJsonProof <-
  if seen == OriginalJson.object [("greeting", OriginalJson.String "hi")]
      && secondSeen == OriginalJson.object
        [("greeting", OriginalJson.String "again")
        ,("count", OriginalJson.Number (OriginalJson.scientific 42 0))
        ]
      && firstJsonProof == 41
    then pure (42 :: Int)
    else error "wrong retained JSON payload"
