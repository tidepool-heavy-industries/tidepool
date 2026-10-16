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
    replacementSourceFailureEntry :: Int -> Eff '[ActorKernel, ActorLocal Maybe] Int
    replacementSourceFailureEntry state = do
      send ActorReadyWith
      send @(ActorLocal Maybe) (ActorCheckpointWith 0 state)
      next <- send @(ActorLocal Maybe) (ActorReceiveStatefulWith 0 handler)
      send @(ActorLocal Maybe) (ActorCheckpointWith 0 (next :: Int))
      error "controlled failure after staged source checkpoint"
      where
        handler :: forall result. Maybe result -> Eff '[ActorKernel, ActorLocal Maybe] ()
        handler (Just reply) = do
          send (ActorReplyWith 0 reply)
          send (ActorContinueWith 0 (99 :: Int))
        handler Nothing = error "source mapper did not deliver its mailbox value"
    replacementLifecycleMapper :: Int -> Eff '[ActorKernel, ActorLocal Maybe] ()
    replacementLifecycleMapper _ = do
      _ <- send ActorLifecycleInputWith
      send (ActorReplyWith 0 (Just ()))
