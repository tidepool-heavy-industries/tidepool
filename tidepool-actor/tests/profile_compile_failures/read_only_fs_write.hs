let definition :: ActorDefinition () Maybe ()
    definition = ActorDefinition
      { label = "invalid-read-only-writer"
      , effectProfile = ReadOnly
      , initialization = \() -> pure ()
      , behavior = \() () ->
          (writeFile "forbidden.txt" "must not typecheck" >> pure ()
            :: Eff (ReadOnlyEffects Maybe) ())
      , onShutdown = const (pure ())
      }
in definition `seq` pure ()
