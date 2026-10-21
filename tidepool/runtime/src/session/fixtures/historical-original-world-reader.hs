import qualified HistoricalOriginalG

{-# NOINLINE historicalRead #-}
historicalRead :: Int
historicalRead = HistoricalOriginalG.g

historicalReaderResult <- pure historicalRead
