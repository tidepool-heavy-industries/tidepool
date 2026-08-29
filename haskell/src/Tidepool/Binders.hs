{-# LANGUAGE LambdaCase #-}

-- | Binder-name extraction for the turn/classify lanes.
--
-- Given Haskell source, parse it with GHC's own parser (NO typecheck) and
-- report the binders declarations introduce, or classify a single statement
-- as bind\/expr\/decl. The Rust runtime never parses Haskell itself; these
-- GHC-sourced names and verdicts are the only source.
module Tidepool.Binders
  ( ExportItem(..)
  , extractBindersNamed
  , exportItemName
  , declItems
    -- * Statement binders (session-eval bind-vs-expr classification)
  , TurnKind(..)
  , turnKindWireName
  , parseTurnKind
  , StmtBinders(..)
  , extractStmtBinders
  , classifyBlock
  , renderVerdictsJson
    -- * Turn-mode template selection (--turn)
  , TemplateSelector(..)
  , templateSelectorForVerdict
  , templateSelectorWireName
    -- * Turn-mode rich result (--turn)
  , TurnOut(..)
  , BoundBinder(..)
  , renderBoundBinderJson
  , renderAskJson
  ) where

import GHC
import GHC.Hs
  ( HsDecl(..), TyClDecl(..), HsDataDefn(..), ConDecl(..)
  , Sig(..), LSig, hsmodDecls )
import GHC.Hs.Expr (StmtLR(..))
import GHC.Hs.Utils (collectHsBindBinders, collectLStmtBinders, CollectFlag(..))
import GHC.Driver.Session (DynFlags, importPaths, xopt_set)
import GHC.LanguageExtensions (Extension(..))
import GHC.Parser (parseStatement, parseDeclaration)
import qualified GHC.Parser (parseModule)
import GHC.Parser.Lexer (ParseResult(..), unP, initParserState)
import GHC.Driver.Config.Parser (initParserOpts)
import GHC.Data.StringBuffer (stringToStringBuffer)
import GHC.Data.FastString (mkFastString)
import GHC.Types.SrcLoc (mkRealSrcLoc)
import GHC.Types.Name.Reader (RdrName, rdrNameOcc)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.SrcLoc (unLoc)
import Control.Exception (evaluate)
import Control.Monad.IO.Class (liftIO)
import Data.Foldable (toList)
import Data.List (intercalate, foldl', nub)
import Data.Maybe (catMaybes, isJust)
import Data.Text (Text)
import qualified Data.Text as T
import Data.Word (Word64)
import Tidepool.ExtractUtil (getLibdir)
import Tidepool.Json (jsonString)
import Tidepool.Timing (timeSection, emitPhase)

-- | A binder a declaration introduces.
--
-- 'EValue' is a function/value binder. 'EType' is a type/data head with its
-- data constructor children, so it can render as @Foo(..)@ for both export and
-- @hiding@. 'EClass' is a typeclass head with its method names, so it renders
-- as @Class(..)@ (required for instances to see the methods).
data ExportItem
  = EValue String
  | EType String [String]
  | EClass String [String]
  deriving (Eq, Show)

-- | Parse @path@ (with @includes@ on the search path) and collect the binders
-- of its top-level declarations as structured 'ExportItem's, selecting the
-- module summary by an EXACT match on @expectedModuleName@. Used by
-- @--turn@'s decl path: the caller controls the name of the scratch module it
-- just spliced and wrote (via 'Main.extractModuleName' on the spliced
-- source), so it can demand exactly that summary. A missing match is a
-- caller wiring bug — the module just written is not the module GHC parsed —
-- and fails loudly rather than silently returning a different module's
-- binders. Parse-only: never typechecks, so a declaration that references
-- not-yet-defined names still yields its binders.
extractBindersNamed :: FilePath -> [FilePath] -> String -> IO [ExportItem]
extractBindersNamed path includes expectedModuleName = do
  libdir <- getLibdir
  runGhc (Just libdir) $ do
    dflags <- getSessionDynFlags
    _ <- setSessionDynFlags dflags { importPaths = importPaths dflags ++ includes }
    target <- guessTarget path Nothing Nothing
    setTargets [target]
    _ <- depanal [] False
    graph <- getModuleGraph
    case filter isExpected (mgModSummaries graph) of
      (chosen : _) -> do
        pm <- parseModule chosen
        let decls = hsmodDecls (unLoc (pm_parsed_source pm))
        pure (concatMap declItems decls)
      [] -> liftIO (ioError (userError
              ("extractBindersNamed: no module named " ++ expectedModuleName
                ++ " in the parsed module graph")))
  where
    isExpected ms = moduleNameString (moduleName (ms_mod ms)) == expectedModuleName

-- | The head name an 'ExportItem' introduces — the binder for 'EValue', the
-- type/class head for 'EType'\/'EClass'. Used to derive a @--turn@ decl
-- verdict's binders from its harvested 'declItems' when the verdict itself
-- carries none (see 'TDecl's doc).
exportItemName :: ExportItem -> String
exportItemName (EValue n)   = n
exportItemName (EType n _)  = n
exportItemName (EClass n _) = n

-- | The binders one top-level declaration introduces.
declItems :: LHsDecl GhcPs -> [ExportItem]
declItems ldecl = case unLoc ldecl of
  ValD _ bind -> map (EValue . occStr) (collectHsBindBinders CollNoDictBinders bind)
  TyClD _ tcd -> tyClItems tcd
  _           -> []

tyClItems :: TyClDecl GhcPs -> [ExportItem]
tyClItems = \case
  DataDecl { tcdLName = n, tcdDataDefn = defn } ->
    [ EType (occStr (unLoc n)) (conNames defn) ]
  SynDecl  { tcdLName = n } -> [ EType (occStr (unLoc n)) [] ]
  ClassDecl{ tcdLName = n, tcdSigs = sigs } -> [ EClass (occStr (unLoc n)) (classMethodNames sigs) ]
  FamDecl  { tcdFam = FamilyDecl { fdLName = n } } -> [ EType (occStr (unLoc n)) [] ]
  _ -> []

-- | Method names of a class declaration (parse-only: uses 'ClassOpSig' from
-- 'tcdSigs', not the typechecked 'classMethods').
classMethodNames :: [LSig GhcPs] -> [String]
classMethodNames sigs =
  nub [ occStr (unLoc nm)
      | lsig <- sigs
      , ClassOpSig _ _ ns _ <- [unLoc lsig]
      , nm <- ns ]

-- | Data constructor names of a data/newtype definition.
conNames :: HsDataDefn GhcPs -> [String]
conNames defn = concatMap (conDeclNames . unLoc) (toList (dd_cons defn))

conDeclNames :: ConDecl GhcPs -> [String]
conDeclNames = \case
  ConDeclH98  { con_name  = n  } -> [ occStr (unLoc n) ]
  ConDeclGADT { con_names = ns } -> map (occStr . unLoc) (toList ns)

occStr :: RdrName -> String
occStr = occNameString . rdrNameOcc

--------------------------------------------------------------------------------
-- Statement binders — the session-eval bind-vs-expr signal
--------------------------------------------------------------------------------

-- | The three mutually-exclusive shapes a session-eval turn classifies to
-- (GHC-sourced) — the verdict's wire contract. Mirrors Rust's own 'TurnKind'
-- (@tidepool-runtime/src/session/turn.rs@) exactly; kept a plain 3-value type
-- here (not a refinement of template selection — see 'TemplateSelector' for
-- that) for the same reason Rust keeps them separate.
data TurnKind = KDecl | KBind | KExpr
  deriving (Eq, Show)

-- | The wire-name string every JSON verdict's @kind@ field and the
-- @--turn-verdict@ CLI argument both use. Mirrors Rust's
-- @turn_kind_wire_name@ byte for byte.
turnKindWireName :: TurnKind -> String
turnKindWireName KDecl = "decl"
turnKindWireName KBind = "bind"
turnKindWireName KExpr = "expr"

-- | Parse a wire-name string back into a 'TurnKind' — used by
-- @--turn-verdict@, where the Rust caller forwards its own already-classified
-- 'TurnKind' verbatim (via 'turnKindWireName'), so an unrecognized string
-- here means version skew. Fails loudly rather than silently defaulting,
-- mirroring Rust's own @parse_one_verdict@ read of the extract's classify
-- JSON, which rejects an unrecognized @kind@ the same way.
parseTurnKind :: String -> TurnKind
parseTurnKind "decl" = KDecl
parseTurnKind "bind" = KBind
parseTurnKind "expr" = KExpr
parseTurnKind s      = error ("unrecognized turn kind: " ++ s)

-- | The result of classifying one session-eval turn. @sbKind@ is 'KBind' when
-- the turn statement introduces binders (@x <- e@ / @let x = e@), 'KExpr' for
-- a bare expression (@BodyStmt@). @sbBinders@ are the bound names (GHC-sourced),
-- empty for an expr turn. The Rust runtime picks the wrap template + the bind
-- path from this signal — it never parses Haskell itself.
data StmtBinders = StmtBinders
  { sbKind    :: TurnKind
  , sbBinders :: [String]
  } deriving (Eq, Show)

-- | Which wrapper template a verdict selects — a refinement of 'TurnKind': a
-- 'KBind' verdict maps to one of two distinct template shapes depending on
-- whether it actually binds a name (a discarding bind, @_ <- e@, runs for
-- effect and discards, so it needs its own wrapper). Mirrors Rust's own
-- 'TemplateSelector' (@tidepool-runtime/src/session/turn.rs@).
data TemplateSelector = SDecl | SBind | SBindDiscard | SExpr
  deriving (Eq, Show)

-- | Compute the selector a verdict maps to — total over every 'TurnKind',
-- mirroring Rust's @TemplateSelector::for_verdict@.
templateSelectorForVerdict :: TurnKind -> [String] -> TemplateSelector
templateSelectorForVerdict KDecl _       = SDecl
templateSelectorForVerdict KBind []      = SBindDiscard
templateSelectorForVerdict KBind (_ : _) = SBind
templateSelectorForVerdict KExpr _       = SExpr

-- | The wire-name string the extract's @--turn-template <kind>=<file>@ keys
-- its template lookup on. Mirrors Rust's @TemplateSelector::wire_name@ byte
-- for byte.
templateSelectorWireName :: TemplateSelector -> String
templateSelectorWireName SDecl        = "decl"
templateSelectorWireName SBind        = "bind"
templateSelectorWireName SBindDiscard = "binddiscard"
templateSelectorWireName SExpr        = "expr"

-- | Classify @src@ against an already-obtained 'DynFlags' with GHC's own
-- parser (parse-only, no typecheck), letting GHC be the single authority for
-- the decl/bind/expr split (the Rust runtime never parses Haskell itself).
-- Session-independent: takes no session action itself, so both
-- 'extractStmtBinders' (one src, one session) and 'classifyBlock' (N srcs,
-- one session) can share it — they cannot fork the verdict a src gets
-- because there is exactly one place the parse happens.
--
-- DECLARATION CONTEXT FIRST, then statement context. A top-level declaration
-- (@f x = e@, a signature @f :: T@, a bare @x = 5@) parses as a decl but fails
-- as a statement. Trying the declaration parse first also disambiguates
-- the genuinely two-faced @f :: T@: in decl
-- context GHC reads it as a signature (@"decl"@), not an annotated expression.
-- Order of precedence:
--
--   * parses as a top-level declaration → @"decl"@ + the declared name(s).
--   * else parses as a statement: @BindStmt@/@LetStmt@ → @"bind"@ + bound
--     names ('collectLStmtBinders'); @BodyStmt@ (a bare expression) → @"expr"@.
--   * else (both fail) → @"expr"@ (the runtime recompiles through the
--     bare-expression path, where GHC re-parses and reports the real error).
classifyWithFlags :: DynFlags -> String -> StmtBinders
classifyWithFlags dflags0 src = classifyTurn declRes stmtRes modRes
  where
    dflags = foldl' xopt_set dflags0 stmtExtensions
    popts  = initParserOpts dflags
    loc    = mkRealSrcLoc (mkFastString "<turn>") 1 1
    buf    = stringToStringBuffer src
    -- Fresh parser state per attempt (the StringBuffer is immutable, so it
    -- is safe to reuse; the mutable lexer state is not).
    declRes = unP parseDeclaration (initParserState popts buf loc)
    stmtRes = unP parseStatement   (initParserState popts buf loc)
    modRes  = unP GHC.Parser.parseModule (initParserState popts buf loc)

-- | Boot a GHC session and classify one turn's source. A SUBSTEP, not a whole
-- timed unit: it emits no phases of its own — a phase's owner has to be whatever knows it
-- is a whole unit, and both surviving callers ('runTurnMode''s classify
-- substep, and the block-classify unit wrapping a batch of these) time it
-- themselves. The 'evaluate' force stays: the caller's phase measures wall
-- clock around this call, and an unforced thunk would let that phase measure
-- nothing.
extractStmtBinders :: String -> IO StmtBinders
extractStmtBinders src = do
  libdir <- getLibdir
  runGhc (Just libdir) $ do
    dflags <- getSessionDynFlags
    liftIO (evaluate (classifyWithFlags dflags src))

-- | Classify a whole BLOCK of turns in one process: boot exactly ONE GHC
-- session (unlike N calls to 'extractStmtBinders', which would boot N), then
-- run 'classifyWithFlags' once per item against that single session's
-- 'DynFlags'. This IS a whole timed unit — the @--classify@ CLI mode — so unlike
-- the substep it DOES take the timing flag and emit its own phases:
-- @startup@ around 'getLibdir', @ghc_session@ around 'getSessionDynFlags',
-- and @classify@ around the N forced parses (one phase for the whole batch,
-- not one per item). No @typecheck@ phase — that name was always a misnomer
-- for a step that runs no typecheck, and it leaves with the unit that
-- originated it rather than being carried forward here.
classifyBlock :: Bool -> [String] -> IO [StmtBinders]
classifyBlock timing srcs = do
  (libdir, startupMs) <- timeSection getLibdir
  emitPhase timing "startup" startupMs
  runGhc (Just libdir) $ do
    (dflags, sessionMs) <- timeSection getSessionDynFlags
    liftIO (emitPhase timing "ghc_session" sessionMs)
    (sbs, classifyMs) <- timeSection (liftIO (mapM (evaluate . classifyWithFlags dflags) srcs))
    liftIO (emitPhase timing "classify" classifyMs)
    pure sbs

-- | Combine the declaration- and statement-context parses into one verdict.
-- Neither context alone is sufficient: a bare @sq 7@ parses (spuriously) as a
-- top-level declaration — an implicit expression-splice, no binder — and a bare
-- signature @sq :: T@ parses as BOTH a signature-declaration and an annotated
-- expression. The precedence below is grounded in the syntax that is actually
-- unambiguous:
--
--   1. @<-@ / top-level @let@ are bind-only markers → @"bind"@.
--   2. A signature (@SigD@) is a declaration (this is how the two-faced
--      @sq :: T@ is resolved — decl, not annotated expr).
--   3. A value/function binding that actually BINDS A NAME (@ValD@ with ≥1
--      harvested binder) → @"decl"@ (@sq x = e@, @x = 5@, @(a,b) = p@).
--   4. A VALID BARE EXPRESSION (@BodyStmt@) → @"expr"@. This runs BEFORE the
--      other-decl catch-all so that @sq 7@ / @filter p xs@ / @pure e@ — which
--      @parseDeclaration@ spuriously accepts as a binder-less splice — classify
--      as expressions, not declarations.
--   5. Any remaining parsed declaration (a non-expression decl the lexical
--      classifier upstream didn't catch, e.g. @deriving instance …@) → @"decl"@.
--   6. Both parses failed → @"expr"@; the runtime recompiles through the
--      bare-expression path and GHC reports the real error loudly.
classifyTurn
  :: ParseResult (LHsDecl GhcPs)
  -> ParseResult (LStmt GhcPs (LHsExpr GhcPs))
  -> ParseResult (Located (HsModule GhcPs))
  -> StmtBinders
classifyTurn declRes stmtRes modRes
  | POk _ lstmt <- stmtRes, isBindStmt lstmt =
      StmtBinders KBind (map occStr (collectLStmtBinders CollNoDictBinders lstmt))
  | POk _ ldecl <- declRes, Just sb <- declNameVerdict ldecl = sb
  -- MULTI-DECLARATION items (a sig + its equation, mutually-referencing
  -- equations — one turn, several top-level decls). `parseDeclaration` and
  -- `parseStatement` are SINGLE-item parsers, so before this rule such items
  -- fell all the way to the "expr" fallback, the runtime wrapped them in a
  -- do-block, and the second line's `=` was a parse error — a regression the
  -- old decl-first try-cascade masked and the verdict fast-path exposed. A
  -- headerless decl sequence is a valid module, so the module parse is the
  -- authority for this shape. Gated on >= 2 decls (single-item verdicts keep
  -- their existing rules above/below, byte for byte) and on EVERY decl
  -- declaring a name (a trailing bare call parses as a zero-binder splice
  -- and must keep poisoning nothing — the item is then not a decl batch).
  | POk _ lmod <- modRes
  , decls <- hsmodDecls (unLoc lmod)
  , length decls >= 2
  , verdicts <- map declNameVerdict decls
  , all isJust verdicts =
      StmtBinders KDecl (nub (concatMap sbBinders (catMaybes verdicts)))
  | POk _ _ <- stmtRes = StmtBinders KExpr []
  | POk _ _ <- declRes = StmtBinders KDecl []
  | otherwise = StmtBinders KExpr []

-- | Whether a parsed statement introduces binders (@BindStmt@/@LetStmt@) rather
-- than being a bare expression (@BodyStmt@).
isBindStmt :: LStmt GhcPs (LHsExpr GhcPs) -> Bool
isBindStmt lstmt = case unLoc lstmt of
  BodyStmt{} -> False
  _          -> True

-- | A NAME-DECLARING declaration's verdict, or 'Nothing' if this parsed
-- \"declaration\" declares no name (a zero-binder @ValD@ — a bare application
-- @parseDeclaration@ over-accepts as an implicit splice). A signature (@SigD@)
-- always qualifies, as does a TYPE/CLASS declaration (@TyClD@ — @data@/
-- @newtype@/@type@/@class@): it declares a TYPE name, contributing no VALUE
-- binders (the define path re-derives real exports GHC-side), but it is
-- unambiguously a declaration — without this arm, a model turn defining a
-- block of @data@ types classified as an EXPRESSION and died on a parse
-- error, with the retry prompt then teaching the model that declarations
-- are forbidden (companion dogfood, 2026-08-13). Everything else defers to
-- the caller's expr/other-decl precedence.
declNameVerdict :: LHsDecl GhcPs -> Maybe StmtBinders
declNameVerdict ldecl = case unLoc ldecl of
  SigD _ sig  -> Just (StmtBinders KDecl (sigBinders sig))
  TyClD _ _   -> Just (StmtBinders KDecl [])
  ValD _ bind -> case map occStr (collectHsBindBinders CollNoDictBinders bind) of
    []    -> Nothing
    names -> Just (StmtBinders KDecl names)
  _           -> Nothing

-- | The names a signature declares (@f, g :: T@ → @["f","g"]@). Only the
-- name-bearing signature forms matter for a session turn.
sigBinders :: Sig GhcPs -> [String]
sigBinders (TypeSig _ names _)      = map (occStr . unLoc) names
sigBinders (ClassOpSig _ _ names _) = map (occStr . unLoc) names
sigBinders _                        = []

-- | Language extensions enabled for the parse-only statement classify. Broad
-- enough to cover the surface the eval template accepts (lambda-case, tuple
-- sections, block arguments, …) so a real turn classifies instead of failing to
-- parse and silently falling back to @"expr"@.
stmtExtensions :: [Extension]
stmtExtensions =
  [ LambdaCase, TupleSections, BlockArguments, MultiWayIf
  , OverloadedStrings, ScopedTypeVariables, TypeApplications
  , BangPatterns, ViewPatterns, OverloadedRecordDot
  -- QuasiQuotes: parse-only — a `[fmt|…|]` splice inside a bind turn must
  -- CLASSIFY as a bind (the quote is one token to the parser; nothing runs).
  -- Without it, `x <- … [fmt|…|] …` failed classification and fell through
  -- to the expression path ("parse error on input `<-'") — found live in the
  -- kata sweep, 2026-07-02.
  , QuasiQuotes
  ]

-- | The @--classify@ CLI contract: one verdict per positional file, in argv
-- order. @kind@ and @binders@ are verbatim the same shape a single
-- 'StmtBinders' always rendered.
--
-- > {"verdicts":[{"kind":"bind","binders":["x"]},
-- >              {"kind":"decl","binders":["sq"]},
-- >              {"kind":"expr","binders":[]}]}
renderVerdictsJson :: [StmtBinders] -> String
renderVerdictsJson sbs =
  "{\"verdicts\":[" ++ intercalate "," (map renderVerdict sbs) ++ "]}"
  where
    renderVerdict (StmtBinders kind binders) =
      "{\"kind\":" ++ jsonString (turnKindWireName kind)
        ++ ",\"binders\":[" ++ intercalate "," (map jsonString binders) ++ "]}"

--------------------------------------------------------------------------------
-- Turn-mode rich result (--turn) — a tagged variant over the verdict
--------------------------------------------------------------------------------

-- | One bound-value record from a BIND turn: the mint'd 'stableVarId', the
-- thin session iface module it was written under, its closure/data tier, and
-- its rendered type. Shared by the binder sidecar and the 'Bind' variant.
data BoundBinder = BoundBinder
  { bbName        :: String
  , bbVarId       :: Word64
  , bbModule      :: String
  , bbTier        :: String
  , bbTypeDisplay :: String
  } deriving (Eq, Show)

-- | The rich result of a @--turn@ run. 'TDecl' never compiles — its
-- 'toDeclItems' come from a whole-module parse ('extractBindersNamed' over
-- the @--turn@ decl turn's own spliced scratch module), not this module's
-- statement parse, because a decl-batch caller (@--turn-verdict decl@ over N
-- declarations joined into one module) has no single statement to parse.
-- 'TBind'/'TExpr'
-- carry what the selected template variant actually compiled to. The
-- wire-visible tag (the CBOR encoder) is @"Decl"@\/@"Bind"@\/@"Expr"@
-- regardless of these constructor names.
--
-- 'TDecl's @toBinders@ is UNRELIABLE as a verbatim echo of the supplied
-- verdict: a decl-batch verdict (@--turn-verdict decl@, no @:name,name…@
-- suffix) carries an empty binder list, so 'runTurnMode' DERIVES
-- @toBinders@ from 'toDeclItems'' head names ('exportItemName') whenever the
-- verdict itself supplies none. @toBinders@ is therefore never a bare echo
-- of the verdict on the decl path — a 'TurnOut' consumer should read it as
-- "the binders this turn introduces", not as "what the verdict said".
data TurnOut
  = TDecl
      { toBinders   :: [Text]
      , toDeclItems :: [ExportItem]
      }
  | TBind
      { toBinders       :: [Text]
      , toVariant       :: Int
      , toBoundBinders  :: [BoundBinder]
      , toAsks          :: [(Word64, Text, [Text])]
      , toWrappedSource :: Text
      }
  | TExpr
      { toVariant       :: Int
      , toAsks          :: [(Word64, Text, [Text])]
      , toWrappedSource :: Text
      }
  deriving (Eq, Show)

-- | One 'BoundBinder' as JSON. @varId@ is a decimal string because JSON
-- numbers cannot represent every u64 exactly.
renderBoundBinderJson :: BoundBinder -> String
renderBoundBinderJson (BoundBinder name varid modul tier tdisp) =
  "{\"name\":" ++ jsonString name
    ++ ",\"varId\":" ++ jsonString (show varid)
    ++ ",\"module\":" ++ jsonString modul
    ++ ",\"tier\":" ++ jsonString tier
    ++ ",\"typeDisplay\":" ++ jsonString tdisp ++ "}"

-- | One runLLMTurn/runLLMTurnFork @{site, type, modules}@ triple as JSON —
-- the @asks.json@ sidecar shape. @modules@ (added alongside @site@/@type@,
-- never optional — an extract that doesn't emit it is stale, and the Rust
-- reader fails loud on the missing key rather than silently falling back to
-- no modules) is the defining-module set 'Tidepool.Translate.modulesOfType'
-- resolved for @type@'s mentioned tycons.
renderAskJson :: (Word64, Text, [Text]) -> String
renderAskJson (site, ty, modules) =
  "{\"site\":" ++ show site
    ++ ",\"type\":" ++ jsonString (T.unpack ty)
    ++ ",\"modules\":[" ++ intercalate "," (map (jsonString . T.unpack) modules) ++ "]}"
