{-# LANGUAGE NoImplicitPrelude #-}
module SourceDomainE where
import SourceDomainSupport (Node(..))
import qualified SourceDomainD as D
{-# NOINLINE e1 #-}
e1 :: Node
e1 = Node D.d1
{-# NOINLINE e2 #-}
e2 :: Node
e2 = Node D.d2
