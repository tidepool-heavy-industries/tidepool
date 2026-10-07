retainedBottom <- record 1 >> pure (\() -> error "retained closure bottom" :: Int)
