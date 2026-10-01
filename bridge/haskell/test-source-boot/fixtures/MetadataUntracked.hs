{-# LANGUAGE TemplateHaskell #-}
module MetadataUntracked where

import Language.Haskell.TH.Syntax (addDependentFile)

$(addDependentFile "test-source-boot/fixtures/MetadataTarget.hs" >> pure [])

value :: Int
value = 42
