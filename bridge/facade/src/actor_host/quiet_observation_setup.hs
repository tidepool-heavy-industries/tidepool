import qualified Tidepool.Agent.Contract as A
data LargeDelivery = LargeDelivery { candidateRevision :: String, testedRevision :: String, checkEvidence :: [String], limitations :: [String] } deriving Show
delivery = LargeDelivery "candidate-9828" "tested-6c6c" (replicate 400 "verified exact tree with focused checks") ["LIMITATION-MUST-REMAIN-AVAILABLE"]
Right workerCapture <- checkpoint "typed worker fixture"
Right worker <- spawnSubagent (ForkCtx workerCapture) SameDir ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies])) { spawnLabel = Just "quiet-observation-worker" })
Right answer <- request @LargeDelivery worker () defaultRequestOptions
let readyLabel = Just "large-delivery-ready"
ready <- watch readyLabel (result answer)
