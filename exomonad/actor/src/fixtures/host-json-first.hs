import qualified Tidepool.Aeson as OriginalJson
firstJsonProof <-
  if seen == OriginalJson.object [("greeting", OriginalJson.String "hi")]
      && echoed == seen
    then pure (41 :: Int)
    else error "wrong first JSON payload"
data UnrelatedJsonPublication = UnrelatedJsonPublication
