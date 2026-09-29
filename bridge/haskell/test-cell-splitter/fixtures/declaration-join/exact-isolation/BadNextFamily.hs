{-# LANGUAGE TypeFamilies #-}
module BadNextFamily where
import Next
bad :: F Int -> Bool
bad value = value
