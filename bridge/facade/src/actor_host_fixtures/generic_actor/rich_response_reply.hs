let plotRow h n = (h-1) `div` 2 - (n * ((h-1) `div` 2) `quot` 2000)
let overlaps = [(a,b,plotRow 9 a,plotRow 9 b) | (a,b) <- [(0,499),(500,999)]]
overlaps
respond (Report ["Different sample values can quantize to the same display row.", "Noise depends on elapsed time independently of sample motion."] ["Evaluated resident counterexamples without creating artifacts."] (Just "retained candidate evidence"))
