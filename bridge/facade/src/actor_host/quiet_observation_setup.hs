data LargeDelivery = LargeDelivery { candidateRevision :: String, testedRevision :: String, checkEvidence :: [String], limitations :: [String] } deriving Show
delivery = LargeDelivery "candidate-9828" "tested-6c6c" (replicate 400 "verified exact tree with focused checks") ["LIMITATION-MUST-REMAIN-AVAILABLE"]
worker <- startAgent (readonlyAgent "quiet-observation-worker")
let observationLabel = [label|quiet-observation|]
answer <- request @LargeDelivery worker (assignment observationLabel ())
let readyLabel = "large-delivery-ready" :: WatchLabel
ready <- watch readyLabel (awaitResponse answer)
