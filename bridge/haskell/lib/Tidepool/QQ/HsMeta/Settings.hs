{-# LANGUAGE PatternSynonyms #-}
{-# LANGUAGE TupleSections #-}
{-# LANGUAGE ViewPatterns #-}
{-# OPTIONS_GHC -Wno-missing-fields -Wno-name-shadowing -Wno-unused-imports #-}

-- | Settings needed for running the GHC Parser.
--
-- Vendored from ghc-hs-meta-0.1.5.0 (Language.Haskell.Meta.Settings).
-- Copyright (c) 2021 Zachary Wood; portions (c) 2017 Guillaume Bouchard (PyF).
-- BSD-3-Clause. See LICENSE and LICENSE-PyF in this directory. Renamed to the
-- Tidepool.QQ.HsMeta.* namespace; otherwise unmodified.
module Tidepool.QQ.HsMeta.Settings (baseDynFlags) where

{- ORMOLU_DISABLE -}

import GHC.Settings.Config
import GHC.Driver.Session
import GHC.Utils.Fingerprint
import GHC.Platform
import GHC.Settings

import GHC.Hs

import GHC.Driver.Config

import GHC.Parser.PostProcess
import GHC.Driver.Session
import GHC.Data.StringBuffer
import GHC.Parser.Lexer
import qualified GHC.Parser.Lexer as Lexer
import qualified GHC.Parser as Parser
import GHC.Data.FastString
import GHC.Types.SrcLoc
import GHC.Driver.Backpack.Syntax
import GHC.Unit.Info
import GHC.Types.Name.Reader


import Data.Data hiding (Fixity)

import GHC.Hs

import GHC.Types.Fixity
import GHC.Types.SourceText

import GHC.Types.Name.Reader
import GHC.Types.Name
import GHC.Types.SrcLoc

import qualified Language.Haskell.TH.Syntax as GhcTH
import Data.Maybe

fakeSettings :: Settings
fakeSettings = Settings
  { sGhcNameVersion=ghcNameVersion
  , sFileSettings=fileSettings
  , sTargetPlatform=platform
  , sPlatformMisc=platformMisc
  , sToolSettings=toolSettings
  }
  where
    toolSettings = ToolSettings {
      toolSettings_opt_P_fingerprint=fingerprint0
      }
    fileSettings = FileSettings {}
    platformMisc = PlatformMisc {}
    ghcNameVersion =
      GhcNameVersion{ghcNameVersion_programName="ghc"
                    ,ghcNameVersion_projectVersion=cProjectVersion
                    }
    platform =
      Platform{
    -- It doesn't matter what values we write here as these fields are
    -- not referenced for our purposes. However the fields are strict
    -- so we must say something.
        platformByteOrder=LittleEndian
      , platformHasGnuNonexecStack=True
      , platformHasIdentDirective=False
      , platformHasSubsectionsViaSymbols=False
      , platformIsCrossCompiling=False
      , platformLeadingUnderscore=False
      , platformTablesNextToCode=False
      , platform_constants=platformConstants
      ,

        platformWordSize=PW8
      , platformArchOS=ArchOS {archOS_arch=ArchUnknown, archOS_OS=OSUnknown}

      , platformHasLibm = False
      , platformUnregisterised=True
      }
    platformConstants = Nothing

applyFakeLlvmConfig :: a -> a
applyFakeLlvmConfig = id

baseDynFlags :: [GhcTH.Extension] -> DynFlags
baseDynFlags exts =
  let enable = GhcTH.TemplateHaskellQuotes : exts
   in foldl xopt_set (applyFakeLlvmConfig $ defaultDynFlags fakeSettings) enable
