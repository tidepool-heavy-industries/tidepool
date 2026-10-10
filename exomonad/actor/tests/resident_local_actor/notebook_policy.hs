(do
  attachAgent Nothing
  receive @() @Maybe $ \request -> case request of
    Just value -> pure (value, ())
    Nothing -> error "unused fixture mailbox") :: Eff ActorEffects ()
