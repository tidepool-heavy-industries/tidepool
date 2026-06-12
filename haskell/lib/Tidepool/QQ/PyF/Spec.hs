-- | The Python format-spec mini-language AST and parser.
--
-- == Provenance
--
-- The AST (`FormatMode`, `Padding`, `Precision`, `TypeFormat`,
-- `AlternateForm`, `SignMode`) and the spec grammar are vendored from
-- PyF-0.11.5.0 (@PyF.Internal.PythonSyntax@ and @PyF.Formatters@).
-- Copyright (c) 2017 Guillaume Bouchard. BSD-3-Clause. See @LICENSE-PyF@ in
-- this directory.
--
-- == Deviations from upstream PyF
--
-- * __Parsec-free.__ PyF's grammar is written with @Text.Parsec@; tidepool
--   forbids new cabal dependencies, so the grammar is re-implemented here as a
--   hand-rolled positional scanner over 'String'.  The accepted language and
--   the validation errors mirror PyF's @formatSpec@/@evalFlag@ exactly.
-- * __No GADT alignment.__ PyF's @AnyAlign@ is a kind-indexed GADT; we collapse
--   it to a plain 'Align' sum (the codegen does not need the kind index).
-- * __Literal width\/precision only.__ PyF allows nested replacement fields in
--   width and precision (@{x:{w}.{p}f}@) via @ExprOrValue@; we accept only
--   literal integers there.  A nested field is a clean parse error.
-- * __Expression parsing lives elsewhere.__ This module parses only the part
--   after the @:@ (the format spec).  The expression before the @:@ is parsed
--   by "Tidepool.QQ.HsMeta.Parse" (the GHC-API parser we already vendor).
--
-- This module is compile-time only: it runs inside the GHC splice evaluator,
-- never on the Cranelift JIT.
module Tidepool.QQ.PyF.Spec
  ( FormatMode (..)
  , Padding (..)
  , Precision (..)
  , TypeFormat (..)
  , AlternateForm (..)
  , SignMode (..)
  , Align (..)
  , parseFormatSpec
  ) where

import Data.Char (isDigit)
import Data.List (foldl')

-- | Sign handling (PyF @SignMode@).
data SignMode
  = Plus   -- ^ @+@ — show @+@ for non-negative, @-@ for negative.
  | Minus  -- ^ @-@ (default) — show @-@ for negative only.
  | Space  -- ^ a space — show a space for non-negative, @-@ for negative.
  deriving (Show, Eq)

-- | Alignment (PyF @AlignMode@ / @AnyAlign@, collapsed to a plain sum).
data Align
  = AlignLeft    -- ^ @<@
  | AlignRight   -- ^ @>@
  | AlignCenter  -- ^ @^@
  | AlignInside  -- ^ @=@ — pad between the sign and the digits.
  deriving (Show, Eq)

-- | Whether the @#@ alternate form is requested.
data AlternateForm = AlternateForm | NormalForm
  deriving (Show, Eq)

-- | Floating-point / string precision (the @.N@ field).
data Precision
  = PrecisionDefault
  | Precision Int
  deriving (Show, Eq)

-- | Padding: width plus optional (fill, align).  'PaddingDefault' means no
-- width was given, in which case alignment is irrelevant (PyF semantics).
data Padding
  = PaddingDefault
  | Padding Int (Maybe (Maybe Char, Align))
  deriving (Show, Eq)

-- | The presentation type (PyF @TypeFormat@).  Carries the precision, sign and
-- alternate-form fields that survived validation.
data TypeFormat
  = DefaultF Precision SignMode          -- ^ no explicit type
  | BinaryF AlternateForm SignMode       -- ^ @b@
  | CharacterF                           -- ^ @c@
  | DecimalF SignMode                    -- ^ @d@
  | ExponentialF Precision AlternateForm SignMode      -- ^ @e@
  | ExponentialCapsF Precision AlternateForm SignMode  -- ^ @E@
  | FixedF Precision AlternateForm SignMode            -- ^ @f@
  | FixedCapsF Precision AlternateForm SignMode        -- ^ @F@
  | GeneralF Precision AlternateForm SignMode          -- ^ @g@
  | GeneralCapsF Precision AlternateForm SignMode      -- ^ @G@
  | OctalF AlternateForm SignMode        -- ^ @o@
  | StringF Precision                    -- ^ @s@
  | HexF AlternateForm SignMode          -- ^ @x@
  | HexCapsF AlternateForm SignMode      -- ^ @X@
  | PercentF Precision AlternateForm SignMode          -- ^ @%@
  deriving (Show, Eq)

-- | A fully-parsed format spec: padding, presentation type and an optional
-- grouping char (@_@ or @,@).
data FormatMode = FormatMode Padding TypeFormat (Maybe Char)
  deriving (Show, Eq)

-- | Parse the text after a @:@ into a 'FormatMode'.  @Left@ carries a
-- human-readable message naming the offending field (these become compile
-- errors in the quoter).
--
-- Grammar (PyF, after Python):
--
-- @
-- format_spec ::= [[fill]align][sign][#][0][width][grouping][.precision][type]
-- @
parseFormatSpec :: String -> Either String FormatMode
parseFormatSpec s0 =
  let (alM, s1)   = pAlignment s0
      (sgnM, s2)  = pSign s1
      (alt, s3)   = pAlt s2
      (zero, s4)  = pZero s3
      (wM, s5)    = pWidth s4
      (grpM, s6)  = pGrouping s5
   in do
        (prec, s7)  <- pPrecision s6
        (mFlag, s8) <- pType s7
        if not (null s8)
          then Left ("trailing characters " ++ show s8)
          else do
            let al      = overrideAlignmentIfZero zero alM
                padding = case wM of
                  Just w  -> Padding w al
                  Nothing -> PaddingDefault
            case mFlag of
              Nothing  -> Right (FormatMode padding (DefaultF prec (defSign sgnM)) grpM)
              Just flg -> do
                tf <- evalFlag flg padding grpM prec alt sgnM
                Right (FormatMode padding tf grpM)

------------------------------------------------------------------------
-- Positional field scanners
------------------------------------------------------------------------

-- | @[[fill]align]@.  The two-char (fill+align) form is tried first, exactly
-- as PyF's @try@ does, so @*>@ is (fill @*@, align @>@) while @>@ alone is
-- just (align @>@).
pAlignment :: String -> (Maybe (Maybe Char, Align), String)
pAlignment s = case s of
  (c1 : c2 : rest) | Just a <- toAlign c2 -> (Just (Just c1, a), rest)
  (c1 : rest)      | Just a <- toAlign c1 -> (Just (Nothing, a), rest)
  _                                       -> (Nothing, s)

toAlign :: Char -> Maybe Align
toAlign '<' = Just AlignLeft
toAlign '>' = Just AlignRight
toAlign '^' = Just AlignCenter
toAlign '=' = Just AlignInside
toAlign _   = Nothing

pSign :: String -> (Maybe SignMode, String)
pSign ('+' : r) = (Just Plus, r)
pSign ('-' : r) = (Just Minus, r)
pSign (' ' : r) = (Just Space, r)
pSign s         = (Nothing, s)

pAlt :: String -> (AlternateForm, String)
pAlt ('#' : r) = (AlternateForm, r)
pAlt s         = (NormalForm, s)

pZero :: String -> (Bool, String)
pZero ('0' : r) = (True, r)
pZero s         = (False, s)

pWidth :: String -> (Maybe Int, String)
pWidth s = case span isDigit s of
  ("", _) -> (Nothing, s)
  (ds, r) -> (Just (readDigits ds), r)

pGrouping :: String -> (Maybe Char, String)
pGrouping ('_' : r) = (Just '_', r)
pGrouping (',' : r) = (Just ',', r)
pGrouping s         = (Nothing, s)

pPrecision :: String -> Either String (Precision, String)
pPrecision ('.' : r) = case span isDigit r of
  ("", _)  -> Left "a '.' must be followed by a precision (one or more digits)"
  (ds, r') -> Right (Precision (readDigits ds), r')
pPrecision s = Right (PrecisionDefault, s)

pType :: String -> Either String (Maybe Char, String)
pType []      = Right (Nothing, [])
pType (c : r)
  | c `elem` typeChars = Right (Just c, r)
  | otherwise          = Left ("unknown format type " ++ show c ++ ". " ++ errgGn)
  where typeChars = "bcdeEfFgGnosxX%" :: String

readDigits :: String -> Int
readDigits = foldl' (\acc c -> acc * 10 + (fromEnum c - fromEnum '0')) 0

------------------------------------------------------------------------
-- Flag → TypeFormat, with PyF's compatibility validation
------------------------------------------------------------------------

defSign :: Maybe SignMode -> SignMode
defSign Nothing  = Minus
defSign (Just s) = s

-- | PyF's @overrideAlignmentIfZero@: the @0@ flag forces zero-fill with
-- inside-alignment unless the user already gave an alignment.
overrideAlignmentIfZero :: Bool -> Maybe (Maybe Char, Align) -> Maybe (Maybe Char, Align)
overrideAlignmentIfZero True Nothing             = Just (Just '0', AlignInside)
overrideAlignmentIfZero True (Just (Nothing, a)) = Just (Just '0', a)
overrideAlignmentIfZero _    v                   = v

-- | Build a 'TypeFormat' from the type char, applying PyF's compatibility
-- validation (which fields are legal for which type).  The type char is always
-- one of @typeChars@ (checked by 'pType').
evalFlag
  :: Char -> Padding -> Maybe Char -> Precision -> AlternateForm -> Maybe SignMode
  -> Either String TypeFormat
evalFlag 'b' _ _ prec alt s = failIfPrec prec (BinaryF alt (defSign s))
evalFlag 'c' _ _ prec alt s = failIfS s =<< failIfPrec prec =<< failIfAlt alt CharacterF
evalFlag 'd' _ _ prec alt s = failIfPrec prec =<< failIfAlt alt (DecimalF (defSign s))
evalFlag 'e' _ _ prec alt s = Right (ExponentialF prec alt (defSign s))
evalFlag 'E' _ _ prec alt s = Right (ExponentialCapsF prec alt (defSign s))
evalFlag 'f' _ _ prec alt s = Right (FixedF prec alt (defSign s))
evalFlag 'F' _ _ prec alt s = Right (FixedCapsF prec alt (defSign s))
evalFlag 'g' _ _ prec alt s = Right (GeneralF prec alt (defSign s))
evalFlag 'G' _ _ prec alt s = Right (GeneralCapsF prec alt (defSign s))
evalFlag 'n' _ _ _ _ _      = Left ("format type 'n' (locale-aware number) is not supported. " ++ errgGn)
evalFlag 'o' _ _ prec alt s = failIfPrec prec (OctalF alt (defSign s))
evalFlag 's' pad grp prec alt s =
  failIfGrouping grp =<< failIfInsidePadding pad =<< failIfS s =<< failIfAlt alt (StringF prec)
evalFlag 'x' _ _ prec alt s = failIfPrec prec (HexF alt (defSign s))
evalFlag 'X' _ _ prec alt s = failIfPrec prec (HexCapsF alt (defSign s))
evalFlag '%' _ _ prec alt s = Right (PercentF prec alt (defSign s))
evalFlag c   _ _ _    _   _ = Left ("unknown format type " ++ show c ++ ". " ++ errgGn)

errgGn :: String
errgGn = "Use one of {'b','c','d','e','E','f','F','g','G','n','o','s','x','X','%'}."

failIfPrec :: Precision -> TypeFormat -> Either String TypeFormat
failIfPrec PrecisionDefault t = Right t
failIfPrec (Precision _) _ =
  Left "this type is incompatible with a precision (.N); use one of {'e','E','f','F','g','G','s','%'} or drop the precision"

failIfAlt :: AlternateForm -> TypeFormat -> Either String TypeFormat
failIfAlt NormalForm t = Right t
failIfAlt AlternateForm _ =
  Left "this type is incompatible with the alternate form (#); use one of {'b','o','x','X','e','E','f','F','g','G','%'} or drop the #"

failIfS :: Maybe SignMode -> TypeFormat -> Either String TypeFormat
failIfS Nothing t = Right t
failIfS (Just _) _ =
  Left "this type is incompatible with a sign field (+, - or space)"

failIfGrouping :: Maybe Char -> TypeFormat -> Either String TypeFormat
failIfGrouping Nothing t  = Right t
failIfGrouping (Just _) _ = Left "the string type ('s') is incompatible with grouping (_ or ,)"

failIfInsidePadding :: Padding -> TypeFormat -> Either String TypeFormat
failIfInsidePadding (Padding _ (Just (_, AlignInside))) _ =
  Left "the string type ('s') is incompatible with inside padding (=)"
failIfInsidePadding _ t = Right t
