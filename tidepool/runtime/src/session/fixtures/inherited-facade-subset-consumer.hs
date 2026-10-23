{-# LANGUAGE TypeFamilies #-}
module InheritedFacadeConsumer where
import DECLARATION_FACADE_MODULE

__prepared :: Int
__prepared = keptNumber (Kept (41 :: KeptPayload Bool))
