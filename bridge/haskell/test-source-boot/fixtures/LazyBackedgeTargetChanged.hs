module LazyBackedgeTarget where
import LazyBackedgeDependency

data Token = Token Bool
__result :: Int
__result = observeToken (Token True)
