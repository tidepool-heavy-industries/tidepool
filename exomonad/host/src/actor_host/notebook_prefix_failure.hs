actorsBeforeFailure <- listAgents
privatePrefixHelper :: Int -> Int
privatePrefixHelper value = value + 1
let prefixGetter = privatePrefixHelper
prefixValue <- pure (prefixGetter 40)
Just impossible <- pure (Nothing :: Maybe Int)
tailValue <- pure (42 :: Int)
