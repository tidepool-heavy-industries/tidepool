data LargeDelivery = LargeDelivery { candidateRevision :: String, testedRevision :: String, checkEvidence :: [String], limitations :: [String] } deriving Show
let delivery = LargeDelivery "candidate-9828" "tested-6c6c" (replicate 2000 "verified exact tree with focused checks") ["LIMITATION-MUST-REMAIN-AVAILABLE"]
data Costly = Costly Int
instance Show Costly where { show _ = error "custom Show failed before its first character" }
worker <- startAgent (readonlyAgent "quiet-observation-worker")
let Right label = requestLabel "quiet-observation"
answer <- request @LargeDelivery worker label delivery
let Right readyLabel = watchLabel "large-delivery-ready"
ready <- watch readyLabel (awaitResponse answer)
