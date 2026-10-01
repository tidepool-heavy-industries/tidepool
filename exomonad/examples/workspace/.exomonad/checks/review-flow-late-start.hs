Right coordinatorTree <- createWorktree
  (fromRef (GitRef (renderGitOid sourceHead)) coordinatorName)
let flowDefinition = R.withWorktree (worktreeId coordinatorTree)
      (reviewFlow me task
        (defaultReviewFlowPolicy { flowRepairLimit = limit, flowSourcePlan = sourcePlan }) worker)
flow <- R.start flowDefinition
