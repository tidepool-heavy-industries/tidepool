displayReopenedProof <-
  case PriorDisplay.displayWith 256 UnrelatedTextPublication of
    (rendered, False) | rendered == "UnrelatedTextPublication" -> pure (43 :: Int)
    _ -> error "published Display did not retain its exact native graph"
