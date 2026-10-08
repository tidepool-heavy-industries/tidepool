completed <- await (result immediateJob)
display (case completed of { Right value -> value == (131 :: Int); _ -> False })
