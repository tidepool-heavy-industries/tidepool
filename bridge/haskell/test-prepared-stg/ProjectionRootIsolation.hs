module ProjectionRootIsolation where

plainEntry :: Int
plainEntry = 37
{-# NOINLINE plainEntry #-}

data ConcreteAnswer = ConcreteLeft | ConcreteRight

data PolymorphicAnswer a = PolymorphicAnswer a

concreteAnswer :: Int -> ConcreteAnswer
concreteAnswer value = concreteAnswer value
{-# NOINLINE concreteAnswer #-}

polymorphicAnswer :: forall a. Int -> PolymorphicAnswer a
polymorphicAnswer value = polymorphicAnswer value
{-# NOINLINE polymorphicAnswer #-}

concretePolymorphicArgument :: forall a. a -> ConcreteAnswer
concretePolymorphicArgument _ = concreteAnswer 0
{-# NOINLINE concretePolymorphicArgument #-}
