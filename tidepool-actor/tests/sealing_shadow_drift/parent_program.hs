do
  childProgram <-
    (deliberate "Define an actor before shadowing one selected startup head." ()
      :: Eff ActorEffects (Eff ActorEffects Int))
  childProgram
