```haskell
complete ((\n -> pure (offset + twice (+ 1) n)) :: Int -> Eff ActorEffects Int)
```
```haskell
error "completion must stop the suffix"
```
