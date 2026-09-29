{-# LANGUAGE TypeFamilies #-}
module BadFamily where
import Joined
bad :: F Int -> Bool
bad x = x
