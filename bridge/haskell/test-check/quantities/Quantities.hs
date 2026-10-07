{-# LANGUAGE DataKinds, GADTs, OverloadedStrings #-}
module Main where

import Control.Monad (unless)
import Control.Exception (ErrorCall, evaluate, try)
import Control.Monad.Freer (Eff, interpret, run)
import Data.Either (isRight)
import Data.List (isInfixOf)
import Numeric.Natural (Natural)
import Tidepool.Agent.Contract (AgentSpec, AsyncHaskellTools, defaultAsyncWorkbenchSpec)
import qualified Tidepool.Actors.Spawn as Spawn
import qualified Tidepool.Duration as Duration
import qualified Tidepool.Effects.Core as Core

require :: String -> Bool -> IO ()
require label observed = unless observed (error label)

refusesDuration :: (Natural -> Duration.Duration) -> Natural -> IO Bool
refusesDuration constructor value = do
  outcome <- try (evaluate (constructor value == Duration.milliseconds 0)) :: IO (Either ErrorCall Bool)
  pure $ case outcome of
    Left failure -> "duration exceeds the runtime integer range" `isInfixOf` show failure
    Right _ -> False

limits :: Natural -> Natural -> Spawn.SpawnLimits
limits depth active = Spawn.SpawnLimits
  (either (error . show) id (Spawn.descendantDepth depth))
  (either (error . show) id (Spawn.activeDescendants active))

probeWire :: Maybe (Int, Int) -> Maybe Spawn.SpawnLimits -> Bool
probeWire expected requested = case run (interpret host admission) of
  Left (Spawn.SpawnRefused "observed limits") -> True
  _ -> False
  where
    options = (Spawn.defaultSpawnOptions
      (defaultAsyncWorkbenchSpec :: AgentSpec (AsyncHaskellTools '[]) '[]))
      { Spawn.spawnLimits = requested }
    admission = Spawn.spawnSubagent (Spawn.FreshCtx "idle") Spawn.SameDir options
    host :: Core.AgentLaunch a -> Eff '[] a
    host (Core.AgentLaunchSpawnWith _ _ _ _ _ _ _ _ _ actual)
      | actual == expected = pure (Left (Core.SpawnRefused "observed limits"))
      | otherwise = error "spawn limits changed at the generated boundary"
    host _ = error "unexpected launch operation"

main :: IO ()
main = do
  let accepted = [0..65535] :: [Natural]
      rejected = [65536, 65537, fromIntegral (maxBound :: Int) + 1, 2 ^ (100 :: Int)]
  require "every representable limit is accepted"
    (all (isRight . Spawn.descendantDepth) accepted && all (isRight . Spawn.activeDescendants) accepted)
  require "rejected quantities retain their exact value without truncation"
    (all (\value -> Spawn.descendantDepth value == Left (Spawn.DescendantDepthTooLarge value)
                && Spawn.activeDescendants value == Left (Spawn.ActiveDescendantsTooLarge value)) rejected)
  require "quantity order is monotonic over the whole admitted range"
    (and (zipWith (<) (map (either (error . show) id . Spawn.descendantDepth) accepted) (map (either (error . show) id . Spawn.descendantDepth) (tail accepted)))
      && and (zipWith (<) (map (either (error . show) id . Spawn.activeDescendants) accepted) (map (either (error . show) id . Spawn.activeDescendants) (tail accepted))))
  require "omitted limits preserve inheritance" (probeWire Nothing Nothing)
  require "zero caps stay explicit" (probeWire (Just (0, 0)) (Just (limits 0 0)))
  require "distinct dimensional caps keep their field order" (probeWire (Just (3, 17)) (Just (limits 3 17)))
  require "maximum caps cross the boundary unchanged" (probeWire (Just (65535, 65535)) (Just (limits 65535 65535)))
  let largest = fromIntegral (maxBound :: Int) :: Natural
      counts = [0, 1, 2, 59, 60, 61, 1000, 65535, largest `div` 60000,
                largest `div` 1000, largest - 1, largest]
      samples = [(constructor count, toInteger count * scale)
                | (constructor, scale) <- [(Duration.milliseconds, 1), (Duration.seconds, 1000), (Duration.minutes, 60000)]
                , count <- counts]
  require "duration equality and order follow exact elapsed magnitude"
    (and [ (left == right) == (leftMagnitude == rightMagnitude)
           && compare left right == compare leftMagnitude rightMagnitude
         | (left, leftMagnitude) <- samples, (right, rightMagnitude) <- samples])
  require "all zero units are the same immediate duration"
    (Duration.milliseconds 0 == Duration.seconds 0 && Duration.seconds 0 == Duration.minutes 0)
  require "equivalent units compare equal"
    (Duration.seconds 1 == Duration.milliseconds 1000 && Duration.minutes 1 == Duration.seconds 60)
  require "scaled large durations never wrap in comparisons"
    (Duration.minutes largest > Duration.seconds largest && Duration.seconds largest > Duration.milliseconds largest)
  let durations = map fst samples
  require "duration ordering obeys antisymmetry and equality consistency"
    (and [ (left <= right && right <= left) == (left == right)
           && (left <= right) == (not (right < left))
         | left <- durations, right <- durations])
  require "duration ordering is transitive across units and extreme counts"
    (and [ not (left <= middle && middle <= right) || left <= right
         | left <- durations, middle <- durations, right <- durations])
  require "duration transport constructor names and unit counts stay unchanged"
    (show (Duration.milliseconds 3) == "DurationMilliseconds 3"
      && show (Duration.seconds 3) == "DurationSeconds 3"
      && show (Duration.minutes 3) == "DurationMinutes 3")
  refused <- sequence
    [ refusesDuration constructor value
    | constructor <- [Duration.milliseconds, Duration.seconds, Duration.minutes]
    , value <- [largest + 1, 2 ^ (100 :: Int)] ]
  require "each duration unit retains its existing integer range refusal" (and refused)
