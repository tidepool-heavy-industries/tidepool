heldRequest <- pure $! (receive @Json.Value @Maybe (\_ -> P.error "mailbox handler is not invoked") :: Eff '[ActorLocal Maybe] Json.Value)
