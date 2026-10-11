_ <- do
  Right response <- J.ask1 (J.rawState (String "hold ancestor output until successor preparation"))
    (J.choice "Release the retained cell?"
      (J.alt #yes "Release" () J..| J.alt #no "Reject" ()))
  True <- pure (J.handle (J.answers response) (#yes (\_ -> True) J..| #no (\_ -> False)))
  display (40 + 2 :: Int)
