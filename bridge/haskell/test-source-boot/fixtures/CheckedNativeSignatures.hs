{-# LANGUAGE DataKinds, KindSignatures, PolyKinds, RankNTypes #-}
module CheckedNativeSignatures where

import CheckedNativeTypeOwner (Payload)
import Data.Proxy (Proxy)

__tidepool_cell_pin_0_nominal :: Payload Int
__tidepool_cell_pin_0_nominal = undefined

__tidepool_cell_pin_1_polymorphic :: forall a. a -> a
__tidepool_cell_pin_1_polymorphic = id

__tidepool_cell_pin_2_kinded :: forall k (a :: k). Proxy a -> Proxy a
__tidepool_cell_pin_2_kinded = id

__tidepool_cell_pin_3_rank :: (forall a. a -> a) -> Int
__tidepool_cell_pin_3_rank f = f 3

__tidepool_cell_pin_4_constraint :: forall a. Eq a => a -> a -> Bool
__tidepool_cell_pin_4_constraint = (==)

__tidepool_cell_pin_5_promoted :: Proxy ('Just 3)
__tidepool_cell_pin_5_promoted = undefined

__tidepool_cell_pin_6_tuple :: (Int, Bool)
__tidepool_cell_pin_6_tuple = (3, True)
