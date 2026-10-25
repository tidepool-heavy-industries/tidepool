module LazyBackedgeRequest where
import LazyBackedgeTarget (Token)
import LazyBackedgeRelay (relayToken)

__result :: Int
__result = relayToken (undefined :: Token)
