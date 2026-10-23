module LazyBackedgeTarget where
import LazyBackedgeDependency

data Token = Token Int
__result :: Int
__result = observeToken (Token 42)
