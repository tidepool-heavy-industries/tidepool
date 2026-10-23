{-# OPTIONS_GHC -Wmissing-fields #-}
module Prior where
data Old = Old { oldHead :: Int, oldTail :: Int }
{-# NOINLINE old #-}
{-# LINE 1 "<cell>" #-}
old = Old { oldHead = 1 }
