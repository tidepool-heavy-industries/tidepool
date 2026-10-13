displayFixture <- send (DisplayWith ((0, 0, 0), "preview A", [(1, "detail")], False)
  ((\_ -> do
      (identity, _, _) <- send DisplayExpansionInputWith
      _ <- send (DisplayWith (identity, "detail A", [], False)
        ((\_ -> pure ()) :: Int -> Eff '[Console] ()))
      pure ()) :: Int -> Eff '[Console] ()))
