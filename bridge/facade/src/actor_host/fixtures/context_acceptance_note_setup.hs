import qualified Tidepool.Agent.Context as C
import Control.Lens (over, toListOf)

do
  current <- C.getContext
  let note = C.Text Nothing C.Assistant "independent-note-roundtrip" []
  C.putContext (over C.contextBlocks (<> [note, note]) current)
  pure True
