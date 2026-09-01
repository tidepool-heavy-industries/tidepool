let deadDefinition :: ActorDefinition () Maybe ()
    deadDefinition =
      ActorDefinition
        { label = "dead-callee"
        , effectProfile = ReadOnly
        , initialization = pure
        , behavior = \_ initial ->
            (pure initial :: Eff (ReadOnlyEffects Maybe) ())
        , visibleToChild = []
        , onShutdown = const (pure ())
        }
in do
    dead <- startActor deadDefinition ()
    call dead (Nothing :: Maybe Int)
