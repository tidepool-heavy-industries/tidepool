historicalOld <- pure historicalRead
historicalNew <- pure HistoricalOriginalG.g
historicalResult <- if historicalOld == (41 :: Int) && historicalNew == (99 :: Int)
  then pure (historicalOld * 1000 + historicalNew)
  else error "historical Original followed the current implementation"
