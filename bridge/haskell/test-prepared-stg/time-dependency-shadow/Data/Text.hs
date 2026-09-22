{-# LANGUAGE PackageImports #-}

module Data.Text (Text, pack) where

import "text" Data.Text (Text)
import "text" Data.Text qualified as Real

pack :: String -> Text
pack value = Real.pack ("shadow:" ++ value)
