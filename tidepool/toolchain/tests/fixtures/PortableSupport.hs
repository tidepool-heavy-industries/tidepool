module PortableSupport where

support :: Int -> Int
support n = n + 2
{-# NOINLINE support #-}
