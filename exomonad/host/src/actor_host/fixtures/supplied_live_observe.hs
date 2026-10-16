completed <- await (result suppliedJob)
display (case completed of { Right value -> value == (111 :: Int); _ -> False })
