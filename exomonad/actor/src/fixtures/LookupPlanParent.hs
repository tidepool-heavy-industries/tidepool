{-# LANGUAGE PatternSynonyms #-}
module LookupPlanParent (Choice, pattern Choice, pattern ConstructorOnly) where

-- Distinct type and constructor namespaces, with hidden constructor parents.
-- Ordinary compact browsing must still expose both names independently.
data Choice = ChoiceValue
data HiddenChoice = Choice
data HiddenConstructor = ConstructorOnly
