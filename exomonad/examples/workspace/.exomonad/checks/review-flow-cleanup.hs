-- Run after reading the terminal review and retaining reviewer interviews.
-- The coordinator owns the reviewer fork groups and retains the exact receipts.
cleanup <- R.call (reviewCleanup (R.client flow)) ReviewCleanupOnce
stateAfterCleanup <- R.call (reviewSnapshot (R.client flow)) ()
inspectFull (show (flowStage stateAfterCleanup, flowCleanupResult stateAfterCleanup))
