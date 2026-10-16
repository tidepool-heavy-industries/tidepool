import qualified Examples.ModelCallWorkflow as ModelExample
-- No ambient conversation, source or tools are included. Put the actual bounded
-- evidence in this input; a path alone does not make its contents available.
let input = "Intent: decide whether to investigate the retry loop. Evidence: one retained test failed after three retries; no implementation or timing trace is supplied." :: Text
result <- ModelExample.synchronousLuna input
display (ModelExample.report result)
-- Sol variant: use ModelExample.synchronousSol input in this notebook.
-- Ordinary asynchronous programs can use ModelExample.luna or ModelExample.sol.
-- This is a fresh invocation when run: it does not reuse Luna's response.
result
