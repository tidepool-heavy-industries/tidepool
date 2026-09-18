noiseHead :: Text -> Bool
noiseHead l = any (`T.isPrefixOf` l) ["warning: ignoring", "warning: build failed", "warning: unused manifest key"]

summaryHead :: Text -> Bool
summaryHead l = "error: could not compile" `T.isPrefixOf` l

isHead :: Text -> Bool
isHead l = ("error" `T.isPrefixOf` l || "warning" `T.isPrefixOf` l) && not (noiseHead l) && not (summaryHead l)

chunks :: [Text] -> [[Text]]
chunks [] = []
chunks (l:ls) | isHead l = let (body, rest) = L.break isHead ls in (l : body) : chunks rest
              | otherwise = chunks ls

diagChunks :: Text -> [[Text]]
diagChunks = chunks . T.lines

object [ "a_count" .= length (diagChunks rawA), "b_count" .= length (diagChunks rawB)
       , "a_heads" .= map head (diagChunks rawA), "b_heads" .= map head (diagChunks rawB)
       , "a_sizes" .= map length (diagChunks rawA), "b_sizes" .= map length (diagChunks rawB) ]
