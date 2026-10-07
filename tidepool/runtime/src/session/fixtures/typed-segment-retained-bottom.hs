retainedBottom <- segmentRecord 1 >> pure (\() -> error "retained closure bottom" :: Int)
