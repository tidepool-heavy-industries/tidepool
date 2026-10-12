let replacementEntry :: Int -> Eff '[] Int
    replacementEntry state = pure (if state == 41 then 42 else error "replacement lost its predecessor checkpoint")
    replacementCheckpoint :: Int
    replacementCheckpoint = 41
    replacementReadyEntry :: Int -> Eff '[ActorKernel, ActorLocal Maybe] Int
    replacementReadyEntry state = do
      send ActorReadyWith
      send @(ActorLocal Maybe) (ActorCheckpointWith 0 state)
      next <- send @(ActorLocal Maybe) (ActorReceiveStatefulWith 0 handler)
      pure (maybe state id next)
      where
        handler :: forall result. Maybe result -> Eff '[ActorKernel, ActorLocal Maybe] ()
        handler _ = send (ActorContinueWith 0 (Nothing :: Maybe Int))
    predecessorEntry :: Int -> Eff '[ActorKernel, ActorLocal Maybe] Int
    predecessorEntry state = do
      send @(ActorLocal Maybe) (ActorCheckpointWith 7 state)
      next <- send @(ActorLocal Maybe) (ActorReceiveStatefulWith 7 handler)
      pure (maybe state id next)
      where
        handler :: forall result. Maybe result -> Eff '[ActorKernel, ActorLocal Maybe] ()
        handler _ = error "intentional predecessor handler failure"
    predecessorInput :: Maybe Int
    predecessorInput = Just 73
    predecessorInputValue :: Maybe Int -> Eff '[] Int
    predecessorInputValue input = pure (maybe (-1) id input)
