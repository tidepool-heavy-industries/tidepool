```haskell
data DynamicPolicy = DynamicPolicy Int
data DynamicProtocol result where
  AddDynamic :: Int -> DynamicProtocol Int
```
```haskell
complete
  ((let
      chooseDynamic :: Int -> Eff (ReadOnlyEffects DynamicProtocol) DynamicPolicy
      chooseDynamic seed = deliberate "Choose the dynamic base." seed
      definition = ActorDefinition
        { label = "model-authored"
        , effectProfile = ReadOnly
        , initialization = chooseDynamic
        , behavior = \_ (DynamicPolicy base) ->
            (receive (\(AddDynamic delta) -> pure (base + delta, (base +)))
              :: Eff (ReadOnlyEffects DynamicProtocol) (Int -> Int))
        , visibleToChild = ["DynamicPolicy", "DynamicProtocol"]
        , onShutdown = const (pure ())
        }
    in do
      ref <- startActor definition 40
      answer <- call ref (AddDynamic 2)
      outcome <- awaitExit ref
      case outcome of
        Completed applyDynamic -> pure (answer + applyDynamic 1)
        _ -> pure (-1)) :: Eff ActorEffects Int)
```
