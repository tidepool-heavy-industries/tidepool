{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
module Probe where

import Tidepool.Prelude hiding (error)
import Tidepool.Effects

t1 :: M [Text]
t1 = do
  (_, _, e) <- run "echo hi 1>&2"
  pure (lines e)

t2 :: M [Text]
t2 = do
  (_, _, e) <- run "echo hi 1>&2"
  pure (filter (\l -> "h" `isPrefixOf` l) (lines e))

t3 :: M [Text]
t3 = do
  (_, _, e) <- run "echo hi 1>&2"
  pure (filter (\l -> "h" `isPrefixOf` l || "w" `isPrefixOf` l) (lines e))

t4 :: [Text]
t4 = filter (\l -> "h" `isPrefixOf` l) ["hi", "wo"]

t5 :: M [Text]
t5 = do
  (_, _, e) <- run "echo hi 1>&2"
  pure (filter (\l -> len l > 1) (lines e))

t6 :: M [Bool]
t6 = do
  (_, _, e) <- run "echo hi 1>&2"
  pure (map (\l -> "h" `isPrefixOf` l) (lines e))

t7 :: M [Text]
t7 = do
  (c, o, e) <- run "echo hi 1>&2"
  pure (filter (\l -> len l > 1) (lines e) <> [pack (show c), o])

t8 :: M [Text]
t8 = do
  _ <- run "true"
  pure (filter (\l -> len l > 1) ["hi", "a"])
