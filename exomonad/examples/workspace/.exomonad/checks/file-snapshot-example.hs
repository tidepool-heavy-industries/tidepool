import qualified Tidepool.File as File
import qualified Tidepool.Effects.Authored as Files
import qualified Data.Text as Text
import Tidepool.Records (WriteOutcome(..))
let target = ".exomonad/state/notes/file-snapshot-example.md"
Right () <- Files.writeFile target "original λ\n"
Right observed <- File.readSnapshot target
snapshotDisplay <- display observed
Right written <- File.writeSnapshot observed (File.snapshotText observed <> "new\n")
Right conflict <- File.writeSnapshot observed "stale"
Right actual <- Files.readFile target
display (case (written, conflict) of
  (Written {}, WriteConflict {}) -> actual == "original λ\nnew\n" && Text.isPrefixOf "/" (File.snapshotPath observed)
  _ -> False)
