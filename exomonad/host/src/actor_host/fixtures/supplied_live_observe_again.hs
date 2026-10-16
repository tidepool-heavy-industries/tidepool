completed <- await (result suppliedSecondJob)
display (case completed of { Right value -> value == (121 :: Int); _ -> False })
