-- | Re-export facade — the module auto-imported into every eval. All
-- names below are in scope bare: pure generics from Schemes, effectful
-- vocabularies from the verb modules.
--
-- LAYERING (import direction is strict):
--   Schemes (pure generics)
--     -> verb modules (Explore/Dev/Optics/...: effectful vocabularies)
--       -> Library (this facade)
-- Verb modules import Schemes, never Library (re-export cycle). New
-- definitions go in Schemes (if pure-generic) or a verb module (if
-- effectful) — never here.
module Library
  ( module Schemes
  , module Explore
  , module Dev
  , module Tables
  , module Diff
  , module Edit
  , module Optics
  , module GotchaGuard
  , module SelfTest
  , module Lsp
  ) where

import Schemes
import Explore
import Dev
import Tables
import Diff
import Edit
import Optics
import GotchaGuard
import SelfTest
import Lsp
