{-# LANGUAGE MagicHash #-}
{-# OPTIONS_GHC -Wno-incomplete-patterns #-}

module WiredInErrorProjection where

import GHC.Exts (Addr#)
import GHC.Internal.Control.Exception.Base qualified as Runtime

data Shadow = Shadow { patError :: Int }

{-# NOINLINE patternPartial #-}
patternPartial :: [Int] -> Int
patternPartial (value : _) = value

{-# NOINLINE applyListFunction #-}
applyListFunction :: ([Int] -> Int) -> Int
applyListFunction function = function [41]

{-# NOINLINE barePattern #-}
barePattern :: Int
barePattern = applyListFunction patternPartial

data ErrorBox = ErrorBox (Addr# -> Int)

{-# NOINLINE makeErrorBox #-}
makeErrorBox :: (Addr# -> Int) -> ErrorBox
makeErrorBox = ErrorBox

{-# NOINLINE applyError #-}
applyError :: (Addr# -> Int) -> Int
applyError failure = failure "wired-in projection"#

{-# NOINLINE bareWired #-}
bareWired :: ErrorBox
bareWired = makeErrorBox Runtime.patError

{-# NOINLINE transitiveWired #-}
transitiveWired :: Int
transitiveWired = applyError Runtime.patError

{-# NOINLINE papWired #-}
papWired :: Addr# -> Int
papWired = Runtime.patError

{-# NOINLINE addPartial #-}
addPartial :: Int -> [Int] -> Int
addPartial offset values = offset + patternPartial values

{-# NOINLINE papPattern #-}
papPattern :: [Int] -> Int
papPattern = addPartial 1

{-# NOINLINE shadowedDefinition #-}
shadowedDefinition :: Int
shadowedDefinition =
  let patError = 42
  in patError

{-# NOINLINE shadowedField #-}
shadowedField :: Int
shadowedField = patError (Shadow 42)
