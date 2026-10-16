send (Core.DisplayWith ((0, 0, 0), "prepared startup output", [], False)
  ((\_ -> pure ()) :: Int -> Eff '[] ())) >> (error "failure after published display" :: Eff effects ())
