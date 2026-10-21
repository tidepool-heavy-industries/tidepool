{-# LANGUAGE NoImplicitPrelude #-}
module SourceDomainD where
import SourceDomainSupport (Node(..), support)
{-# NOINLINE d1 #-}
d1 :: Node
d1 = Node support
{-# NOINLINE d2 #-}
d2 :: Node
d2 = Node (Node support)
