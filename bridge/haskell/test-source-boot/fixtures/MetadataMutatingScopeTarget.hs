{-# LANGUAGE TemplateHaskell #-}
module MetadataMutatingScopeTarget where

import Language.Haskell.TH.Syntax (runIO)

$(do
    runIO (appendFile "{{SCOPE_MANIFEST}}" "\NUL")
    pure [])

__result :: Int
__result = 42
