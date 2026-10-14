module ConstructorAdmissionSupport (Ticket, ticket) where

data Ticket = Ticket Int

{-# NOINLINE ticket #-}
ticket :: Ticket
ticket = Ticket 42
