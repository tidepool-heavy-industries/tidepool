import Tidepool.Inspection (Display(..))
import qualified Tidepool.Inspection as TidepoolInspection

data NotebookCustom = NotebookCustom Int

instance Display NotebookCustom where
  displayTree _ = TidepoolInspection.TextLeaf "custom-display-wins"

NotebookCustom 7
