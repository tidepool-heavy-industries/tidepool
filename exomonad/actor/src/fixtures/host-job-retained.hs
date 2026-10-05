import qualified Tidepool.Command.Types as OriginalJob
import qualified Data.Aeson as Aeson
import Tidepool.Worktree (DirtySummary)
jobSummaryProof :: Maybe DirtySummary
jobSummaryProof = Aeson.decode "{\"staged\":[],\"unstaged\":[],\"untracked\":[],\"ignoredExcluded\":0}"
jobProof <- case ({first}, {second}) of
  (OriginalJob.Job a, OriginalJob.Job b) ->
    case jobSummaryProof of
      Just _ | a == "job one" && b == "job two" && jobFirstProof == 41 -> pure (42 :: Int)
      _ -> error "wrong retained Job payload or JSON parser"
