import Tidepool.Worktree (workspaceFor)
Right coordinatorTree <- createWorktree
  (fromRef (GitRef (renderGitOid sourceHead)) coordinatorName)
Right coordinatorWorkspace <- workspaceFor coordinatorTree
let flowDefinition = R.withWorkspace coordinatorWorkspace
      (reviewFlow me task
        (defaultReviewFlowPolicy { flowRepairLimit = limit, flowSourcePlan = sourcePlan }) worker)
flow <- R.start flowDefinition
