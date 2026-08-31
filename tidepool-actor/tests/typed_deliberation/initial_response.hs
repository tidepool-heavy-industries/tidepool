```haskell
twice f x = f (f x)
```
```haskell
offset <- pure (twice (+ 1) 38)
```
```haskell
complete "wrong type"
```
```haskell
error "rejected completion must stop the suffix"
```
