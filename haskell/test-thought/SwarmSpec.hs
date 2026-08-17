{-# LANGUAGE OverloadedStrings #-}

-- | Property coverage for "Tidepool.Swarm"'s 'Cycles' budget currency
-- (operator's type-level review, 2026-08-17): the conservation law
-- 'splitAllowance' is supposed to hold by construction, checked against many
-- random inputs rather than the one scenario the dev-tree regression pins.
module SwarmSpec (properties) where

import Test.QuickCheck

import Tidepool.Swarm (cyclesToInt, mkCycles, splitAllowance)

-- | The conservation law itself: nothing 'splitAllowance' hands out was ever
-- minted from nothing — the sum of every child's share plus what the parent
-- kept never exceeds the input allowance.
prop_splitAllowanceConserves :: Property
prop_splitAllowanceConserves =
  forAll (choose (0, 1000)) $ \inputN ->
    forAll (choose (0, 1000)) $ \reservationN ->
      forAll (choose (-2, 20)) $ \n ->
        let (kept, parts) = splitAllowance (mkCycles inputN) (mkCycles reservationN) n
            grandTotal = cyclesToInt kept + sum (map cyclesToInt parts)
         in counterexample
              ( "input=" <> show inputN <> " reservation=" <> show reservationN <> " n=" <> show n
                  <> " kept=" <> show (cyclesToInt kept) <> " parts=" <> show (map cyclesToInt parts)
              )
              (grandTotal <= inputN)

-- | Every value 'splitAllowance' returns is itself non-negative — 'Cycles'
-- own invariant, pinned here at the one call site authorized to mint a split.
prop_splitAllowanceSharesNonNegative :: Property
prop_splitAllowanceSharesNonNegative =
  forAll (choose (0, 1000)) $ \inputN ->
    forAll (choose (0, 1000)) $ \reservationN ->
      forAll (choose (-2, 20)) $ \n ->
        let (kept, parts) = splitAllowance (mkCycles inputN) (mkCycles reservationN) n
         in cyclesToInt kept >= 0 .&&. conjoin [counterexample (show p) (p >= 0) | p <- map cyclesToInt parts]

-- | Zero or fewer children: nothing is divided out, so the whole input stays
-- kept rather than a bogus per-child figure for zero recipients.
prop_splitAllowanceNoChildrenKeepsAll :: Property
prop_splitAllowanceNoChildrenKeepsAll =
  forAll (choose (0, 1000)) $ \inputN ->
    forAll (choose (0, 1000)) $ \reservationN ->
      forAll (choose (-5, 0)) $ \n ->
        let (kept, parts) = splitAllowance (mkCycles inputN) (mkCycles reservationN) n
         in null parts .&&. cyclesToInt kept === inputN

-- | Every child gets exactly the same share (the floor of what remained
-- after the reservation, divided evenly) — 'splitAllowance' never favors one
-- child over another.
prop_splitAllowanceSharesAreUniform :: Property
prop_splitAllowanceSharesAreUniform =
  forAll (choose (0, 1000)) $ \inputN ->
    forAll (choose (0, 1000)) $ \reservationN ->
      forAll (choose (1, 20)) $ \n ->
        let (_, parts) = splitAllowance (mkCycles inputN) (mkCycles reservationN) n
         in case parts of
              (p : rest) -> conjoin [counterexample (show parts) (q === p) | q <- rest]
              [] -> counterexample "n >= 1 must produce at least one share" False

properties :: [(String, Property)]
properties =
  [ ("splitAllowance conserves (sum of parts + kept <= input)", prop_splitAllowanceConserves)
  , ("splitAllowance shares and kept are never negative", prop_splitAllowanceSharesNonNegative)
  , ("splitAllowance with no children keeps the whole input", prop_splitAllowanceNoChildrenKeepsAll)
  , ("splitAllowance divides evenly among children", prop_splitAllowanceSharesAreUniform)
  ]
