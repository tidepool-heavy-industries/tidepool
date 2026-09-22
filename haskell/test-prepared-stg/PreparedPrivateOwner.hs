module PreparedPrivateOwner (answer) where

privateIncrement :: Int -> Int
privateIncrement value = value + 1

answer :: Int -> Int
answer = privateIncrement
