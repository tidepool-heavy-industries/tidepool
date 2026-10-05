{-# LANGUAGE TypeApplications #-}
module TypedPreparationOwner where
import Tidepool.Actor (receive)
{-# OPAQUE answer #-}
answer :: Maybe Bool
answer = receive @Bool "prepared"
unrelated :: Int
unrelated = 42
