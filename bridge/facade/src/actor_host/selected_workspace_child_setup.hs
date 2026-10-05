import qualified Exomonad.Contrib.Types as Types
import qualified Project.Work as Work

Right selectedTree <- boundWorktree
Right selectedState <- observeSubmission (worktreeId selectedTree)
let selectedTask = Work.task [label|typed-source|] "keep the original task" ["README.md"] "return the same source and obligation" (headOid (submittedHead selectedState))
selectedWorker <- unfold (batch "typed-source" "coding")
  (child @(Types.Outcome Types.Candidate)
    (withLifetime ActorOwned
      (withContext (selected Types.obligation)
        (Work.lunaTaskFrom [label|worker|] Medium currentCheckout selectedTask))))
display True
