import qualified Tidepool.Command.Types as OriginalJob
jobFirstProof <- case {first} of
  OriginalJob.Job value ->
    if value == "job one"
      then pure (41 :: Int)
      else error "wrong first Job payload"
data UnrelatedJobPublication = UnrelatedJobPublication
jobPublication <- pure (17 :: Int)
