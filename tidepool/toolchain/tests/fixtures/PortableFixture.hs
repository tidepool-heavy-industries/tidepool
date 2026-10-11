module PortableFixture where

import PortableSupport (support)

local :: Int -> Int
local n = support n + 1
{-# NOINLINE local #-}

first :: Int
first = local 7
{-# NOINLINE first #-}

second :: Int
second = local 8
{-# NOINLINE second #-}
