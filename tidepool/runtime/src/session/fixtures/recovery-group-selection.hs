module Lib where

early :: () -> Int
early () = 37
{-# NOINLINE early #-}

late :: () -> Int
late () = early () + 1
{-# NOINLINE late #-}

isolated :: () -> Int
isolated () = 91
{-# NOINLINE isolated #-}
