ActorDefinition
  { label = "valid-selected-empty"
  , effectProfile = Selected (knownEffects @'[])
  , initialization = \() -> pure ()
  , behavior = \() () -> (pure () :: Eff '[ActorLocal Maybe] ())
  , onShutdown = const (pure ())
  }
  `seq` pure ()
