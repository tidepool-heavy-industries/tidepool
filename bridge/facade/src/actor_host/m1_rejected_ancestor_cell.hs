_ <- let waitForRelease = do
           Right state <- readFile "ancestor-output-release"
           if state == "pending"
             then sleep (milliseconds 10) >> waitForRelease
             else do
               True <- pure (state == "release")
               pure ()
     in do
       waitForRelease
       display (40 + 2 :: Int)
