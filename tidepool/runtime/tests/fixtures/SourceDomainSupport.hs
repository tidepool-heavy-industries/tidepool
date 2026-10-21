{-# LANGUAGE NoImplicitPrelude #-}
module SourceDomainSupport where

data Node = Node Node
{-# NOINLINE support #-}
support :: Node
support = Node support
