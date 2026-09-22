let (temporal, visual) = workers
let rowFor height sample = let center = (height-1) `div` 2 in center - (sample*center `quot` 2000)
let overlapExamples = [(h,a,b,rowFor h a,rowFor h b) | (h,a,b) <- [(9,0,499),(9,500,999),(3,-1999,1999),(1,-2000,2000)]]
overlapExamples
let churn = sum [1..10000 :: Int]
churn
contextBeforeReply <- actorContext
let usageView c = (contextActorPath c, contextFirstUsage c, contextLatestUsage c)
usageView contextBeforeReply
