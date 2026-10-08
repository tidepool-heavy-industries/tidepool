first <- pure (11 :: Int)
_ <- pure ()
_ <- pure ()
delayed <- pure (first + 7)
_ <- pure ()
later <- pure (delayed + first)
later
