import qualified Control.Monad.Freer as Freer

ordinaryToken <- pure (OrdinaryToken 35)
ordinaryFunction <- pure ordinaryRead
ordinaryAction <- pure (pure (ordinaryFunction ordinaryToken) :: Freer.Eff '[] Int)
display (ordinaryFunction ordinaryToken == 42 && Freer.run ordinaryAction == 42)
