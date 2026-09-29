{-# LANGUAGE TypeFamilies #-}
module BadAssociated where
import Joined
bad :: Associated Bool -> Int
bad x = x
