module Identity where

identity :: a -> a
identity x = x

apply :: (a -> b) -> a -> b
apply f x = f x

const' :: a -> b -> a
const' x _ = x
