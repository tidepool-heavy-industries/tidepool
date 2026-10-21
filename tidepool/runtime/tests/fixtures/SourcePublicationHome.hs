{-# LANGUAGE NoImplicitPrelude #-}
module SourcePublicationHome where

data Node = Node Node

{-# NOINLINE first #-}
first :: Node
first = Node second

{-# NOINLINE second #-}
second :: Node
second = Node first

