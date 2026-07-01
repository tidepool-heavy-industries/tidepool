{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, OverloadedRecordDot #-}
module Probe where

import Tidepool.Prelude hiding (error)
import Tidepool.Effects
import qualified Tidepool.Data.Text as T

t1 :: M [Text]
t1 = do
  p <- run "echo hi 1>&2"
  pure (lines p.stderr)

t2 :: M [Text]
t2 = do
  p <- run "echo hi 1>&2"
  pure (filter (\l -> "h" `isPrefixOf` l) (lines p.stderr))

t3 :: M [Text]
t3 = do
  p <- run "echo hi 1>&2"
  pure (filter (\l -> "h" `isPrefixOf` l || "w" `isPrefixOf` l) (lines p.stderr))

t4 :: [Text]
t4 = filter (\l -> "h" `isPrefixOf` l) ["hi", "wo"]

t5 :: M [Text]
t5 = do
  p <- run "echo hi 1>&2"
  pure (filter (\l -> len l > 1) (lines p.stderr))

t6 :: M [Bool]
t6 = do
  p <- run "echo hi 1>&2"
  pure (map (\l -> "h" `isPrefixOf` l) (lines p.stderr))

t7 :: M [Text]
t7 = do
  p <- run "echo hi 1>&2"
  pure (filter (\l -> len l > 1) (lines p.stderr) <> [pack (show p.exitCode), p.stdout])

t8 :: M [Text]
t8 = do
  _ <- run "true"
  pure (filter (\l -> len l > 1) ["hi", "a"])

t9 :: M Int
t9 = do
  src <- readFile ".tidepool/lib/Tables.hs"
  let (_, b) = T.breakOn "countTable" src
  pure (if isNull b then 0 else 1)

t10 :: M Int
t10 = do
  src <- readFile ".tidepool/lib/Tables.hs"
  pure (len (sdrop 3 src))

occ2 :: Text -> Text -> Int
occ2 needle hay =
  let (_, rest) = T.breakOn needle hay
  in if isNull rest
       then 0
       else let (_, rest2) = T.breakOn needle (sdrop (len needle) rest)
            in if isNull rest2 then 1 else 2

t11 :: M Int
t11 = do
  src <- readFile ".tidepool/lib/Tables.hs"
  pure (occ2 "countTable" src)
