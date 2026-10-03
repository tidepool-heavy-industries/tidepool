module QuasiQuoteOccurrencesTest
  ( quasiQuoteOccurrenceChecks, quasiQuoteOccurrenceChecksWith ) where

import Control.Monad (unless)
import Control.Monad.IO.Class (liftIO)
import Data.Data (Data, cast, gmapQ)
import GHC (GhcPs, HsUntypedSplice(..), getSessionDynFlags, runGhc, unLoc)
import GHC.Data.FastString (mkFastString)
import GHC.Data.StringBuffer (stringToStringBuffer)
import GHC.Driver.Config.Parser (initParserOpts)
import GHC.Driver.Session (DynFlags, xopt_set, xopt_unset)
import GHC.LanguageExtensions (Extension(..))
import GHC.Parser qualified as Parser
import GHC.Parser.Lexer (ParseResult(..), initParserState, unP)
import GHC.Types.Name.Reader (RdrName)
import GHC.Types.SrcLoc (mkRealSrcLoc)
import GHC.Utils.Outputable (ppr, showSDocUnsafe)
import Tidepool.ExtractUtil (getLibdir)
import Tidepool.QuasiQuoteOccurrences (quasiQuoteOccurrences)

-- The reference walks every Data child, as the previous everything/mkQ query
-- did. Compare exact ordered occurrences, not sets or rendered diagnostics.
referenceOccurrences :: Data value => value -> [RdrName]
referenceOccurrences value = selected ++ concat (gmapQ referenceOccurrences value)
  where
    selected = case cast value :: Maybe (HsUntypedSplice GhcPs) of
      Just (HsQuasiQuote _ name _) -> [name]
      _ -> []

quasiQuoteOccurrenceChecks :: IO ()
quasiQuoteOccurrenceChecks = do
  libdir <- getLibdir
  runGhc (Just libdir) $ getSessionDynFlags >>= liftIO . quasiQuoteOccurrenceChecksWith

quasiQuoteOccurrenceChecksWith :: DynFlags -> IO ()
quasiQuoteOccurrenceChecksWith initial = do
  quoted <- readFile "test-cell-splitter/fixtures/quasiquote-occurrences/contexts.hs"
  literal <- readFile "test-cell-splitter/fixtures/quasiquote-occurrences/literals.hs"
  let disabled = xopt_unset initial QuasiQuotes
      enabled = foldl xopt_set initial [QuasiQuotes, TemplateHaskell, TemplateHaskellQuotes]
      cases =
        [ ("quote contexts", enabled, quoted,
            ["firstQ", "againQ", "againQ", "Q.qualifiedQ", "patternQ",
             "typeQ", "declarationQ", "nestedQ", "spliceQ"])
        , ("quote-like literals and comments", enabled, literal, [])
        , ("syntax disabled", disabled, literal, [])
        , ("large literal leaf", enabled,
            "module LargeLiteral where\nvalue = \"" ++ replicate 100000 'x' ++ "\"\n", [])
        ]
  mapM_ (\(label, flags, source, expected) -> do
    let state = initParserState (initParserOpts flags)
          (stringToStringBuffer source) (mkRealSrcLoc (mkFastString label) 1 1)
    case unP Parser.parseModule state of
      PFailed _ -> fail (label ++ ": parser rejected fixture")
      POk _ parsed -> do
        let actual = quasiQuoteOccurrences flags parsed
        unless (actual == referenceOccurrences (unLoc parsed)) $
          fail (label ++ ": shared traversal changed occurrence order or multiplicity")
        unless (map (showSDocUnsafe . ppr) actual == expected) $
          fail (label ++ ": unexpected quasiquote inventory")) cases
  putStrLn "quasiquote occurrences: 4 parsed AST equivalence cases passed"
