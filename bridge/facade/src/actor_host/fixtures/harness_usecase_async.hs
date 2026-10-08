let perfDelayed = (do { sleep (milliseconds 100); value <- perfAction; pure (value + 36) } :: Eff effects Int)
value <- perfDelayed
_ <- display value
