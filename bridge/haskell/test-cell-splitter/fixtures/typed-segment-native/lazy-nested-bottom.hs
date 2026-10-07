answer <- pure (let unused = error "unused nested bottom" :: Int in (7 :: Int))
