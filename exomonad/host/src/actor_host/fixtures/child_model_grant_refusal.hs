import Tidepool.Actors.Exomonad
import qualified Tidepool.Effects.Core as Core
import qualified Tidepool.Worktree as Wt
import qualified Project.Supplied as Supplied
import qualified Exomonad.Models as Models
import qualified Tidepool.Model.Selection as Selection

let fixtureOpenAI key options = Selection.openAI Models.catalogIdentity key options
Right attachmentTree <- send (Core.WorktreeAllocationCreate (Wt.fromCurrentRepository "child-attachment-directory"))
Right attachmentWorkspace <- Wt.workspaceFor attachmentTree
let attachmentOptions = defaultSpawnOptions currentModel (Supplied.actualSpec 101 "The caller supplied this distinctive probe.")
attachmentFailure <- spawnSubagent (FreshCtx "This child must refuse its ungranted model without activating inference.") (ExistingWorkspace attachmentWorkspace) (attachmentOptions { spawnLabel = Just "ungranted-model-child", spawnModel = fixtureOpenAI "ungranted-model" (Selection.OpenAIOptions Selection.OpenAILow) })
display (case attachmentFailure of { Left (SpawnRefused _) -> True; _ -> False })
