ActorDefinition
  { label = "invalid-selected-writer"
  , effectProfile = Selected (knownEffects @'[])
  , initialization = \() -> pure ()
  , behavior = \() () ->
      (writeFile "forbidden.txt" "must not typecheck" >> pure ()
        :: Eff '[ActorLocal Maybe] ())
  , onShutdown = const (pure ())
  }
  `seq` pure ()
