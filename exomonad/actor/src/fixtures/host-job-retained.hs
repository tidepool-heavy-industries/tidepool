import qualified Tidepool.Command.Types as OriginalJob
jobProof <- case ({first}, {second}) of
  (OriginalJob.Job a, OriginalJob.Job b) ->
    if a == "job one" && b == "job two" && jobFirstProof == 41
      then pure (42 :: Int)
      else error "wrong retained Job payload"
