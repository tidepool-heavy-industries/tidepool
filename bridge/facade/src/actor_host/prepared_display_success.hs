send (Core.DisplayWith ((0, 0, 0), "prepared startup output", [], False)
  ((\_ -> pure ()) :: Int -> M ())) >> pure ()
