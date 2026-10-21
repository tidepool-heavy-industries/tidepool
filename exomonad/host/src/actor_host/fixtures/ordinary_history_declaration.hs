data OrdinaryToken = OrdinaryToken Int

ordinaryRead :: OrdinaryToken -> Int
ordinaryRead (OrdinaryToken value) = value + ordinarySeed
