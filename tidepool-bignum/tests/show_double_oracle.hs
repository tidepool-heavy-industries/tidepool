import GHC.Float (castWord64ToDouble)

main :: IO ()
main = getContents >>= mapM_ (putStrLn . show . castWord64ToDouble . read) . lines
