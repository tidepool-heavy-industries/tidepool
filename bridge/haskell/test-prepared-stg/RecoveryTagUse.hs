module RecoveryTagUse where
import RecoveryTagFixture
use :: (StrictLeaf, LazyLeaf, StrictFunction, StrictFunction, StrictFunction,
        Int -> Maybe Int, Leaf, Leaf)
use = (strictLeaf, lazyLeaf, strictFunction, unknownStrictFunction,
       indirectStrictFunction, ordinaryCall, leaf, unknownLeaf)
