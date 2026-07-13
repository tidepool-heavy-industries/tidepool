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
  , module ExploreProbe
  , module Dev
  , module Tables
  , module Diff
  , module Edit
  , module Optics
  , module SelfTest
  , module Lsp
  , module LspGraph
  , module Churn
  , module Repo
  , module RustAudit
  , module RustSections
  ) where

import Schemes
import Explore
import ExploreProbe
import Dev
import Tables
import Diff
import Edit
import Optics
import SelfTest
import Lsp
import LspGraph
import Churn
import Repo
-- RustAudit's own 'tally'/'crateOf' collide with Schemes.tally and
-- Lsp.crateOf (both already in the facade); hidden here so RustAudit's
-- internal uses are untouched but the facade keeps one owner per name.
import RustAudit hiding (tally, crateOf)
import RustSections
