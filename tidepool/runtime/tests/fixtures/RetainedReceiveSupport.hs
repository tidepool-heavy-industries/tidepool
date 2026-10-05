module RetainedReceiveSupport where

{-# OPAQUE routeEven #-}
routeEven :: Int -> Bool
routeEven n = if n == 0 then True else routeOdd (n - 1)

{-# OPAQUE routeOdd #-}
routeOdd :: Int -> Bool
routeOdd n = if n == 0 then False else routeEven (n - 1)
