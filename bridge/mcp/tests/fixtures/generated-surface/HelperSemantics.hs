{-# LANGUAGE DataKinds, GADTs, OverloadedStrings #-}
module Main where
import Prelude
import Control.Monad.Freer (Eff, interpret, run)
import Tidepool.Aeson (Value, object, (.=))
import qualified Tidepool.Effects.Core as Core

entropy :: Core.Entropy a -> Eff '[] a
entropy Core.EntropySeed = pure 17

console :: Value -> Core.Console a -> Eff '[] a
console expected (Core.DisplayViewWith actual) | actual == expected = pure ()
console _ _ = error "generated display helper changed constructor or payload"

main :: IO ()
main = do
  if run (interpret entropy Core.entropySeed) == 17 then pure () else fail "handwritten entropy helper changed"
  let payload = object ["payload" .= (42 :: Int)]
  run (interpret (console payload) (Core.displayViewRaw payload)) `seq` pure ()
