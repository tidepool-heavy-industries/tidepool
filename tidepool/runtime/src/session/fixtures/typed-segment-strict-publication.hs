scalarBottom <- segmentRecord 1 >> pure (error "strict scalar publication" :: Int)
segmentRecord scalarBottom
