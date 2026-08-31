```haskell
data DriftStartup = DriftStartup Int
data DriftProtocol result where
  ReadDrift :: DriftProtocol Int
```
```haskell
complete
  ((let
      definition = ActorDefinition
        { label = "drifted-child"
        , effectProfile = ReadOnly
        , initialization = \seed -> deliberate "Capture the original startup type." seed
        , behavior = \_ (DriftStartup value) ->
            (receive (\ReadDrift -> pure (value, value))
              :: Eff (ReadOnlyEffects DriftProtocol) Int)
        , visibleToChild = ["DriftStartup", "DriftProtocol"]
        , onShutdown = const (pure ())
        } :: ActorDefinition Int DriftProtocol Int
    in do
      _ <- (deliberate "Redefine the selected startup head before start." ()
        :: Eff ActorEffects ())
      startActor definition 7
      pure 0) :: Eff ActorEffects Int)
```
