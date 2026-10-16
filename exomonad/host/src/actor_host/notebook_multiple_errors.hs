badDecl :: Int
badDecl = True
willNotRun <- pure (1 :: Int)
let badBind = ("wrong" :: Text) + (1 :: Int)
badBind
