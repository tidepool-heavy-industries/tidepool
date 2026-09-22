{-# LANGUAGE ViewPatterns #-}

-- | This module is here to parse Haskell expression using the GHC Api
--
-- Vendored from ghc-hs-meta-0.1.5.0 (Language.Haskell.Meta.Parse).
-- Copyright (c) 2021 Zachary Wood; portions (c) 2017 Guillaume Bouchard (PyF).
-- BSD-3-Clause. See LICENSE and LICENSE-PyF in this directory. Renamed to the
-- Tidepool.QQ.HsMeta.* namespace; otherwise unmodified.
module Tidepool.QQ.HsMeta.Parse (parseExp, parseExpWithExts, parseExpWithFlags, parseHsExpr) where

import GHC.Parser.Errors.Ppr ()
import GHC.Parser.Annotation (LocatedA)
import GHC.Utils.Outputable

import GHC.Driver.Config.Parser (initParserOpts)

import GHC.Parser.PostProcess
import qualified GHC.Types.SrcLoc as SrcLoc
import GHC.Driver.Session
import GHC.Data.StringBuffer
import GHC.Parser.Lexer
import qualified GHC.Parser.Lexer as Lexer
import qualified GHC.Parser as Parser
import GHC.Data.FastString
import GHC.Types.SrcLoc

import GHC.Hs.Extension (GhcPs)

-- @HsExpr@ is available from GHC.Hs.Expr in all versions we support.
-- However, the goal of GHC is to split HsExpr into its own package, under
-- the namespace Language.Haskell.Syntax. The module split happened in 9.0,
-- but still in the ghc package.
import Language.Haskell.Syntax (HsExpr(..))

import Language.Haskell.TH (Extension(..))
import qualified Language.Haskell.TH.Syntax as TH

import qualified Tidepool.QQ.HsMeta.Settings as Settings
import Tidepool.QQ.HsMeta.Translate (toExp)

-- | Parse a Haskell expression from source code into a Template Haskell expression.
-- See @parseExpWithExts@ or @parseExpWithFlags@ for customizing with additional extensions and settings.
parseExp :: String -> Either (Int, Int, String) TH.Exp
parseExp = parseExpWithExts
    [ TypeApplications
    , OverloadedRecordDot
    , OverloadedLabels
    , OverloadedRecordUpdate
    ]

-- | Parse a Haskell expression from source code into a Template Haskell expression
-- using a given set of GHC extensions.
parseExpWithExts :: [Extension] -> String -> Either (Int, Int, String) TH.Exp
parseExpWithExts exts = parseExpWithFlags (Settings.baseDynFlags exts)

-- | Parse a Haskell expression from source code into a Template Haskell expression
-- using a given set of GHC DynFlags.
parseExpWithFlags :: DynFlags -> String -> Either (Int, Int, String) TH.Exp
parseExpWithFlags flags expStr = do
  hsExpr <- parseHsExpr flags expStr
  pure (toExp flags hsExpr)

-- | Run the GHC parser to parse a Haskell expression into a @HsExpr@.
parseHsExpr :: DynFlags -> String -> Either (Int, Int, String) (HsExpr GhcPs)
parseHsExpr dynFlags s =
  case runParser dynFlags s of
    POk _ locatedExpr ->
      let expr = SrcLoc.unLoc locatedExpr
       in Right
            expr

{- ORMOLU_DISABLE #-}
    PFailed PState{loc=SrcLoc.psRealLoc -> srcLoc, errors=errorMessages} ->
            let
                err = renderWithContext defaultSDocContext (ppr errorMessages)
                line = SrcLoc.srcLocLine srcLoc
                col = SrcLoc.srcLocCol srcLoc
            in Left (line, col, err)

-- From Language.Haskell.GhclibParserEx.GHC.Parser

parse :: P a -> String -> DynFlags -> ParseResult a
parse p str flags =
  Lexer.unP p parseState
  where
    location = mkRealSrcLoc (mkFastString "<string>") 1 1
    strBuffer = stringToStringBuffer str
    parseState =
      initParserState (initParserOpts flags) strBuffer location

runParser :: DynFlags -> String -> ParseResult (LocatedA (HsExpr GhcPs))
runParser flags str =
  case parse Parser.parseExpression str flags of
    POk s e -> unP (runPV (unECP e)) s
    PFailed ps -> PFailed ps
