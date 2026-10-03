displayFixture <- send (DisplayWith ((0, 0, 0), "preview", [(1, "detail")], False)
  ((\_ -> send (Print "unauthorized callback output")) :: Int -> M ()))
