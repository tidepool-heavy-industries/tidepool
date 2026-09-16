module RetainedUnrelatedLibrary where

-- | A home library module that defines no retained identity, compiled in the
-- same load as 'ImportProducerExposed'. 'RetainedPluginTest' requires that a
-- change of the retained set never recompiles it: the withholding pass can
-- only change a module's own retained definitions.
libraryTotal :: [Int] -> Int
libraryTotal = foldr (+) 0

libraryLabel :: Int -> String
libraryLabel n = "total " ++ show (libraryTotal [1 .. n])
