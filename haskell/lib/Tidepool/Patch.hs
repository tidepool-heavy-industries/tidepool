{-# LANGUAGE OverloadedStrings #-}

-- | Unified-diff patches as a pure, JIT-safe core: parse, render, invert,
-- validate, and apply with /context-is-truth/ semantics.  Line numbers in
-- @\@\@@ headers are treated as HINTS — application locates each hunk by
-- matching its context\/deletion lines as a contiguous sublist, and reports
-- the drift between the hint and where the hunk actually landed.
--
-- This module is deliberately lens-free and template-haskell-free (it imports
-- only boot packages plus "Tidepool.Aeson.Value"), so the @--all-closed@
-- extract session can load it and the Cranelift JIT can run every function:
-- all recursion is tail- or constructor-guarded, all parsing is range-compare
-- + 'Data.Char.ord' (never @read@\/@isDigit@), and 'Either' is threaded by
-- explicit @case@ rather than its 'Monad' instance.
--
-- The quasi-quoter "Tidepool.QQ.Patch" emits these constructors; the verb
-- module @Diff@ ('.tidepool\/lib\/Diff.hs') wraps 'applyFilePatch' in the @Fs@
-- effect.
module Tidepool.Patch
  ( -- * Types
    HunkLine (..)
  , Hunk (..)
  , FilePatch (..)
  , Patch
  , HunkResult (..)
  , Conflict (..)
  , ConflictKind (..)
    -- * Derived hunk views
  , hunkOldLen
  , hunkNewLen
  , hunkOldSide
  , hunkNewSide
    -- * Parse / render / transform
  , parsePatch
  , renderPatch
  , invertPatch
  , validatePatch
  , applyFilePatch
  ) where

import Prelude
import Data.Char (ord)
import Data.List (isPrefixOf, stripPrefix)
import Data.Text (Text)
import qualified Data.Text as T

import Tidepool.Aeson.Value (Pair, ToJSON (..), object, (.=))

-- ---------------------------------------------------------------------------
-- Types
-- ---------------------------------------------------------------------------

-- | One line of a hunk body, tagged by side.
data HunkLine
  = Ctx Text   -- ^ a context line (present on both sides)
  | Del Text   -- ^ a deletion (old side only)
  | Ins Text   -- ^ an insertion (new side only)
  deriving Eq

-- | A single hunk.  @hOldStart@\/@hNewStart@ are the 1-based start lines from
-- the @\@\@@ header — HINTS for application, not authority.  The body is the
-- truth; line counts are DERIVED ('hunkOldLen'\/'hunkNewLen'), and the header
-- counts are CHECKED against the body at parse time, then discarded.
data Hunk = Hunk
  { hOldStart :: Int
  , hNewStart :: Int
  , hBody     :: [HunkLine]
  } deriving Eq

-- | All hunks for one file.  @fpCreate@ marks a @--- \/dev\/null@ creation
-- patch (exactly one all-insertion hunk).
data FilePatch = FilePatch
  { fpPath   :: Text
  , fpCreate :: Bool
  , fpHunks  :: [Hunk]
  } deriving Eq

-- | A whole patch: an ordered list of per-file patches.
type Patch = [FilePatch]

-- | Where a hunk actually landed.  @hrLine@ is the 1-based original-file line
-- the hunk's old side started at; @hrDrift@ is @hrLine - hOldStart@ (positive
-- if the file had grown above the hunk, negative if it had shrunk).
data HunkResult = HunkResult
  { hrLine  :: Int
  , hrDrift :: Int
  } deriving Eq

-- | Why a file patch could not apply.  At most one is reported per file
-- (application stops the file's scan at the first conflict).
data ConflictKind
  = MissingFile               -- ^ a non-creation patch, but the file is absent
  | FileExists                -- ^ a creation patch, but the file already exists with different content
  | NoMatch [Text] [Text]     -- ^ the hunk's old side was not found; carries (expected, actual-window)
  | Ambiguous [Int]           -- ^ the old side matched at 2+ places; carries the candidate 1-based lines
  | AlreadyApplied Int        -- ^ the old side was absent but the new side is already present, at this line
  deriving Eq

-- | A located conflict: which file, which hunk (0-based), and the kind.
data Conflict = Conflict
  { cFile   :: Text
  , cHunkIx :: Int
  , cKind   :: ConflictKind
  } deriving Eq

-- ---------------------------------------------------------------------------
-- ToJSON (Conflict) — the wire shape the Diff verbs surface
-- ---------------------------------------------------------------------------

instance ToJSON Conflict where
  toJSON (Conflict file ix kind) =
    object (("file" .= file) : ("hunk" .= ix) : kindFields kind)

kindFields :: ConflictKind -> [Pair]
kindFields MissingFile         = [ "kind" .= ("missing-file" :: Text) ]
kindFields FileExists          = [ "kind" .= ("file-exists" :: Text) ]
kindFields (NoMatch expd act)  = [ "kind" .= ("no-match" :: Text), "expected" .= expd, "actual" .= act ]
kindFields (Ambiguous cands)   = [ "kind" .= ("ambiguous" :: Text), "candidates" .= cands ]
kindFields (AlreadyApplied ln) = [ "kind" .= ("already-applied" :: Text), "line" .= ln ]

-- ---------------------------------------------------------------------------
-- Derived hunk views
-- ---------------------------------------------------------------------------

-- | The old side: context + deletion lines, in order.
hunkOldSide :: Hunk -> [Text]
hunkOldSide h = go (hBody h)
  where
    go [] = []
    go (Ctx t : rest) = t : go rest
    go (Del t : rest) = t : go rest
    go (Ins _ : rest) = go rest

-- | The new side: context + insertion lines, in order.
hunkNewSide :: Hunk -> [Text]
hunkNewSide h = go (hBody h)
  where
    go [] = []
    go (Ctx t : rest) = t : go rest
    go (Ins t : rest) = t : go rest
    go (Del _ : rest) = go rest

-- | Number of old-side lines (context + deletions).
hunkOldLen :: Hunk -> Int
hunkOldLen = length . hunkOldSide

-- | Number of new-side lines (context + insertions).
hunkNewLen :: Hunk -> Int
hunkNewLen = length . hunkNewSide

-- ---------------------------------------------------------------------------
-- Small shared helpers
-- ---------------------------------------------------------------------------

showT :: Int -> Text
showT = T.pack . show

perr :: Int -> Text -> Text
perr n msg = "parsePatch: line " <> showT n <> ": " <> msg

-- The line parser works in @String@ space throughout: the tree-walking
-- interpreter represents an empty 'Text' (a @T.pack ""@) as a bare
-- @LitString []@ and then chokes on @T.null@\/@T.uncons@\/@T.unpack@ of it, so
-- we only pack to 'Text' at the leaves (paths and line content), where list
-- 'null' has already settled emptiness.

isBlankLineS :: String -> Bool
isBlankLineS = all (\c -> c == ' ' || c == '\t')   -- @all _ [] = True@: empty is blank

-- | Tolerated git metadata lines between\/around file sections.
isToleratedS :: String -> Bool
isToleratedS l =
     "diff --git"    `isPrefixOf` l
  || "index "        `isPrefixOf` l
  || "new file mode" `isPrefixOf` l
  || "old mode"      `isPrefixOf` l
  || "new mode"      `isPrefixOf` l
  || "similarity"    `isPrefixOf` l
  || "rename "       `isPrefixOf` l
  || "copy "         `isPrefixOf` l

-- | Strip a leading @a\/@ or @.\/@ from a @---@ path.
stripMinusS :: String -> String
stripMinusS p = case stripPrefix "a/" p of
  Just r  -> r
  Nothing -> case stripPrefix "./" p of
    Just r  -> r
    Nothing -> p

-- | Strip a leading @b\/@ or @.\/@ from a @+++@ path.
stripPlusS :: String -> String
stripPlusS p = case stripPrefix "b/" p of
  Just r  -> r
  Nothing -> case stripPrefix "./" p of
    Just r  -> r
    Nothing -> p

-- | Split a 'String' on newlines, keeping empty segments (empty lines and the
-- trailing-newline phantom are meaningful and 'null'-checkable as lists).
splitLinesS :: String -> [String]
splitLinesS s = case break (== '\n') s of
  (a, [])       -> [a]
  (a, _ : rest) -> a : splitLinesS rest

numberLines :: Int -> [String] -> [(Int, String)]
numberLines _ []       = []
numberLines n (x : xs) = (n, x) : numberLines (n + 1) xs

-- | Split a 'Text' on a single character in @String@ space, repacking each
-- segment (used by the application engine over file content). Kept here because
-- 'T.splitOn' yields empty segments the interpreter mis-represents.
splitOnChar :: Char -> Text -> [Text]
splitOnChar c t = map T.pack (go (T.unpack t))
  where
    go s = case break (== c) s of
      (a, [])       -> [a]
      (a, _ : rest) -> a : go rest

-- | Join with newlines in @String@ space (the inverse of @splitOnChar '\n'@,
-- and empty-segment-safe where @T.intercalate@ would concat an empty 'Text').
joinNL :: [Text] -> Text
joinNL ts = T.pack (go (map T.unpack ts))
  where
    go []       = []
    go [s]      = s
    go (s : ss) = s ++ "\n" ++ go ss

-- ---------------------------------------------------------------------------
-- Parser  (Either threaded by explicit case — never via its Monad instance;
-- lines stay in String space, packing to Text only at the leaves)
-- ---------------------------------------------------------------------------

-- | Parse unified-diff text into a 'Patch', or fail with a positioned message.
-- Runs 'validatePatch' as the final step, so a successful parse is always a
-- well-formed patch.
parsePatch :: Text -> Either Text Patch
parsePatch input =
  case parseFiles (numberLines 1 (splitLinesS (T.unpack input))) of
    Left e    -> Left e
    Right fps -> case validatePatch fps of
      Left e   -> Left e
      Right () -> Right fps

-- | The file-seeking loop: skip blanks and tolerated metadata, reject prose,
-- begin a file at each @---@.
parseFiles :: [(Int, String)] -> Either Text [FilePatch]
parseFiles = go []
  where
    go acc [] = Right (reverse acc)
    go acc ((n, l) : rest)
      | isBlankLineS l = go acc rest
      | "deleted file mode" `isPrefixOf` l =
          Left (perr n "file deletion unsupported (Fs has no delete op)")
      | isToleratedS l = go acc rest
      | "--- " `isPrefixOf` l || l == "---" =
          case parseFile n l rest of
            Left e            -> Left e
            Right (fp, rest') -> go (fp : acc) rest'
      | otherwise =
          Left (perr n ("unexpected line; expected a file header ('--- ' or 'diff --git'): " <> T.pack l))

-- | Parse one file section: the @---@ line is given; consume @+++@ then hunks.
parseFile :: Int -> String -> [(Int, String)] -> Either Text (FilePatch, [(Int, String)])
parseFile nMinus lMinus rest =
  let oldRaw = drop 4 lMinus               -- after "--- "
      create = oldRaw == "/dev/null"
  in case rest of
       [] -> Left (perr nMinus "expected a '+++' line after '---'")
       ((nPlus, lPlus) : rest1)
         | "+++ " `isPrefixOf` lPlus || lPlus == "+++" ->
             let newRaw = drop 4 lPlus
             in if newRaw == "/dev/null"
                  then Left (perr nPlus "file deletion unsupported (Fs has no delete op)")
                  else
                    let newPathS = stripPlusS newRaw
                        oldPathS = stripMinusS oldRaw
                        pathOk
                          | create               = Right newPathS
                          | oldPathS == newPathS = Right newPathS
                          | otherwise            = Left (perr nPlus "renames unsupported in v1 (--- and +++ paths differ)")
                    in case pathOk of
                         Left e -> Left e
                         Right pathS
                           | null pathS -> Left (perr nPlus "empty file path")
                           | otherwise  ->
                               case parseHunks rest1 [] of
                                 Left e            -> Left e
                                 Right (hs, rest2) -> Right (FilePatch (T.pack pathS) create hs, rest2)
         | otherwise -> Left (perr nPlus "expected a '+++ <path>' line after '--- <path>'")

-- | Collect consecutive @\@\@@ hunks; stop (returning control) at the first
-- non-hunk line.
parseHunks :: [(Int, String)] -> [Hunk] -> Either Text ([Hunk], [(Int, String)])
parseHunks ls acc = case ls of
  [] -> Right (reverse acc, ls)
  ((n, l) : rest)
    | "@@" `isPrefixOf` l ->
        case parseHunkHeader n l rest of
          Left e           -> Left e
          Right (h, rest') -> parseHunks rest' (h : acc)
    | otherwise -> Right (reverse acc, ls)

parseHunkHeader :: Int -> String -> [(Int, String)] -> Either Text (Hunk, [(Int, String)])
parseHunkHeader n l rest = case parseAtAt n l of
  Left e -> Left e
  Right (os, oc, ns, nc) ->
    case parseBodyN n oc nc rest [] of
      Left e           -> Left e
      Right (body, r') -> Right (Hunk os ns body, r')

-- | Parse @\@\@ -A +B \@\@ section@ into (oldStart, oldCount, newStart,
-- newCount).  Counts default to 1 when omitted; the trailing section is
-- ignored.
parseAtAt :: Int -> String -> Either Text (Int, Int, Int, Int)
parseAtAt n l = case stripPrefix "@@" l of
  Nothing    -> Left (perr n "hunk header must start with '@@'")
  Just body0 -> case beforeStr "@@" body0 of
    Nothing  -> Left (perr n "hunk header missing the closing '@@'")
    Just mid -> case spaceTokensS mid of
      (oldTok : newTok : _) ->
        case parseRange n '-' oldTok of
          Left e -> Left e
          Right (os, oc) -> case parseRange n '+' newTok of
            Left e -> Left e
            Right (ns, nc) -> Right (os, oc, ns, nc)
      _ -> Left (perr n "hunk header must carry '-OLD +NEW' ranges")

-- | The text before the first occurrence of @needle@, or 'Nothing' if absent.
beforeStr :: String -> String -> Maybe String
beforeStr needle = go []
  where
    go acc s
      | needle `isPrefixOf` s = Just (reverse acc)
      | otherwise = case s of
          []       -> Nothing
          (c : cs) -> go (c : acc) cs

-- | Whitespace-separated NON-empty tokens (drops empties in String space —
-- never produces a packed-empty 'Text').
spaceTokensS :: String -> [String]
spaceTokensS s = case dropWhile (== ' ') s of
  [] -> []
  s' -> let (w, rest) = break (== ' ') s' in w : spaceTokensS rest

-- | Parse a @-A[,B]@ \/ @+A[,B]@ range; the leading char must be the sign.
parseRange :: Int -> Char -> String -> Either Text (Int, Int)
parseRange n sign tok = case tok of
  (c : r) | c == sign ->
    case splitCommaS r of
      [s]     -> case parseNat n s of
        Left e      -> Left e
        Right start -> Right (start, 1)
      [s, c'] -> case parseNat n s of
        Left e      -> Left e
        Right start -> case parseNat n c' of
          Left e    -> Left e
          Right cnt -> Right (start, cnt)
      _ -> Left (perr n "malformed hunk range")
  _ -> Left (perr n ("expected '" <> T.singleton sign <> "' to start a hunk range"))

splitCommaS :: String -> [String]
splitCommaS s = case break (== ',') s of
  (a, [])       -> [a]
  (a, _ : rest) -> a : splitCommaS rest

-- | Parse a non-negative integer (range-compare + 'ord', the parseIntM shape).
parseNat :: Int -> String -> Either Text Int
parseNat n s
  | not (null s) && all isDigitC s =
      Right (foldl' (\acc c -> acc * 10 + (ord c - ord '0')) 0 s)
  | otherwise = Left (perr n ("expected a number in the hunk header, got '" <> T.pack s <> "'"))
  where
    isDigitC c = c >= '0' && c <= '9'

-- | Consume exactly @oldRem@ old-side and @newRem@ new-side body lines, by
-- count.  The header counts are the truth for how many lines to read; reaching
-- a non-body line (or EOF) before the counts are met is a LOUD mismatch.  A
-- bare empty line is @Ctx ""@; a @\\ No newline@ marker is skipped.  When both
-- counts hit zero, the remaining lines (including the trailing split phantom)
-- are handed back untouched.
parseBodyN :: Int -> Int -> Int -> [(Int, String)] -> [HunkLine] -> Either Text ([HunkLine], [(Int, String)])
parseBodyN n oldRem newRem ls acc
  | oldRem == 0 && newRem == 0 = Right (reverse acc, dropNoNewline ls)
  | otherwise = case ls of
      [] -> Left (perr n "hunk body ended early (header line counts exceed the body)")
      ((ln, l) : rest)
        | not (null l) && "\\" `isPrefixOf` l -> parseBodyN n oldRem newRem rest acc  -- "\ No newline at end of file"
        | otherwise -> case classify l of
            Right (CCtx, c) ->
              if oldRem > 0 && newRem > 0
                then parseBodyN n (oldRem - 1) (newRem - 1) rest (Ctx (T.pack c) : acc)
                else Left (perr ln "context line exceeds the hunk's line counts")
            Right (CDel, c) ->
              if oldRem > 0
                then parseBodyN n (oldRem - 1) newRem rest (Del (T.pack c) : acc)
                else Left (perr ln "deletion line exceeds the hunk's old line count")
            Right (CIns, c) ->
              if newRem > 0
                then parseBodyN n oldRem (newRem - 1) rest (Ins (T.pack c) : acc)
                else Left (perr ln "insertion line exceeds the hunk's new line count")
            Left _ -> Left (perr ln "hunk body ended early (header line counts exceed the body)")

data Side = CCtx | CDel | CIns

-- | Classify a raw body line into (side, content), or 'Left' if it is not a
-- body line.  A bare empty line is a context line carrying the empty string.
classify :: String -> Either () (Side, String)
classify []        = Right (CCtx, [])
classify (c : r) = case c of
  ' ' -> Right (CCtx, r)
  '-' -> Right (CDel, r)
  '+' -> Right (CIns, r)
  _   -> Left ()

dropNoNewline :: [(Int, String)] -> [(Int, String)]
dropNoNewline ((_, l) : rest) | not (null l) && "\\" `isPrefixOf` l = rest
dropNoNewline ls = ls

-- ---------------------------------------------------------------------------
-- Validation
-- ---------------------------------------------------------------------------

-- | Structural sanity, run as the final step of 'parsePatch':
--
--   * the patch is non-empty;
--   * every path is non-empty, relative, and free of @..@ components;
--   * no two file patches touch the same path;
--   * non-creation hunks have a non-empty old side;
--   * a creation patch is exactly one all-insertion hunk;
--   * within a file, @hOldStart@ is strictly increasing AND the hunks do not
--     overlap (@start_k + oldLen_k <= start_{k+1}@).
validatePatch :: Patch -> Either Text ()
validatePatch fps
  | null fps  = Left "validatePatch: empty patch (no file sections found)"
  | otherwise = case eachE validateFile fps of
      Left e   -> Left e
      Right () -> checkDupPaths (map fpPath fps)

-- | Short-circuiting traversal for @a -> Either e ()@ (avoids relying on the
-- 'Either' 'Monad' instance on the JIT).
eachE :: (a -> Either e ()) -> [a] -> Either e ()
eachE _ []       = Right ()
eachE f (x : xs) = case f x of
  Left e   -> Left e
  Right () -> eachE f xs

validateFile :: FilePatch -> Either Text ()
validateFile fp = case checkPath (fpPath fp) of
  Left e   -> Left e
  Right () ->
    if fpCreate fp
      then case fpHunks fp of
        [h] -> if allIns (hBody h)
                 then Right ()
                 else Left ("validatePatch: creation patch for '" <> fpPath fp <> "' must be all-insertion")
        _   -> Left ("validatePatch: creation patch for '" <> fpPath fp <> "' must have exactly one hunk")
      else case eachE (checkNonEmptyOld (fpPath fp)) (fpHunks fp) of
        Left e   -> Left e
        Right () -> checkOrder (fpPath fp) (fpHunks fp)

allIns :: [HunkLine] -> Bool
allIns []           = True
allIns (Ins _ : xs) = allIns xs
allIns _            = False

-- | Path sanity in String space (the path is non-empty by construction —
-- 'parseFile' rejects empties before packing — so 'T.unpack' here is safe).
checkPath :: Text -> Either Text ()
checkPath p
  | null s                 = Left "validatePatch: empty file path"
  | "/" `isPrefixOf` s     = Left ("validatePatch: absolute path not allowed: '" <> p <> "'")
  | hasDotDot (splitSlash s) = Left ("validatePatch: '..' component not allowed in path: '" <> p <> "'")
  | otherwise              = Right ()
  where
    s = T.unpack p
    hasDotDot []         = False
    hasDotDot (c : rest) = c == ".." || hasDotDot rest
    splitSlash str = case break (== '/') str of
      (a, [])       -> [a]
      (a, _ : rest) -> a : splitSlash rest

checkNonEmptyOld :: Text -> Hunk -> Either Text ()
checkNonEmptyOld path h
  | hunkOldLen h == 0 =
      Left ("validatePatch: non-creation hunk in '" <> path <> "' has an empty old side (no context or deletion to locate it)")
  | otherwise = Right ()

checkOrder :: Text -> [Hunk] -> Either Text ()
checkOrder _ []       = Right ()
checkOrder _ [_]      = Right ()
checkOrder path (a : b : rest)
  | hOldStart b <= hOldStart a =
      Left ("validatePatch: hunks in '" <> path <> "' are out of order (starts must strictly increase)")
  | hOldStart a + hunkOldLen a > hOldStart b =
      Left ("validatePatch: hunks in '" <> path <> "' overlap")
  | otherwise = checkOrder path (b : rest)

checkDupPaths :: [Text] -> Either Text ()
checkDupPaths = go []
  where
    go _ [] = Right ()
    go seen (p : ps)
      | p `elem` seen = Left ("validatePatch: duplicate file path: '" <> p <> "'")
      | otherwise     = go (p : seen) ps

-- ---------------------------------------------------------------------------
-- Render  (parsePatch . renderPatch == Right, property-tested)
-- ---------------------------------------------------------------------------

-- | Render a 'Patch' back to canonical unified-diff text: @--- a\/path@ (or
-- @--- \/dev\/null@ for a creation), @+++ b\/path@, an @\@\@@ header with
-- explicit recomputed counts, and prefixed, newline-terminated body lines.
renderPatch :: Patch -> Text
renderPatch fps = T.concat (map renderFile fps)

renderFile :: FilePatch -> Text
renderFile fp =
  let path      = fpPath fp
      minusLine = if fpCreate fp then "--- /dev/null\n" else "--- a/" <> path <> "\n"
      plusLine  = "+++ b/" <> path <> "\n"
  in T.concat (minusLine : plusLine : map renderHunk (fpHunks fp))

renderHunk :: Hunk -> Text
renderHunk h =
  let header = "@@ -" <> showT (hOldStart h) <> "," <> showT (hunkOldLen h)
            <> " +"   <> showT (hNewStart h) <> "," <> showT (hunkNewLen h) <> " @@\n"
  in header <> T.concat (map renderLine (hBody h))

-- | Render a body line in @String@ space (empty-content-safe — a bare context
-- line @Ctx ""@ renders as @" \n"@ without an empty-'Text' concat).
renderLine :: HunkLine -> Text
renderLine (Ctx t) = T.pack (' ' : T.unpack t ++ "\n")
renderLine (Del t) = T.pack ('-' : T.unpack t ++ "\n")
renderLine (Ins t) = T.pack ('+' : T.unpack t ++ "\n")

-- ---------------------------------------------------------------------------
-- Invert
-- ---------------------------------------------------------------------------

-- | Swap insertions and deletions and the old\/new starts.  A creation patch
-- cannot be inverted (it would become a deletion, which @Fs@ cannot express).
invertPatch :: Patch -> Either Text Patch
invertPatch fps
  | any fpCreate fps = Left "cannot invert a file-creation patch (deletion unsupported: Fs has no delete)"
  | otherwise        = Right (map invFile fps)
  where
    invFile fp = FilePatch (fpPath fp) (fpCreate fp) (map invHunk (fpHunks fp))
    invHunk h  = Hunk (hNewStart h) (hOldStart h) (map invLine (hBody h))
    invLine (Ctx t) = Ctx t
    invLine (Del t) = Ins t
    invLine (Ins t) = Del t

-- ---------------------------------------------------------------------------
-- Application engine (pure)
-- ---------------------------------------------------------------------------

-- | Apply one file's hunks to its (maybe-absent) current content.  On success
-- returns the new content and where each hunk landed.  On failure returns at
-- most one 'Conflict' (the scan stops the file at the first one).
applyFilePatch :: FilePatch -> Maybe Text -> Either [Conflict] (Text, [HunkResult])
applyFilePatch fp mc =
  if fpCreate fp
    then applyCreation fp mc
    else case mc of
      Nothing      -> Left [Conflict (fpPath fp) 0 MissingFile]
      Just content ->
        let origLines = splitOnChar '\n' content
        in goHunks (fpPath fp) origLines origLines 1 (indexFrom 0 (fpHunks fp)) [] []

applyCreation :: FilePatch -> Maybe Text -> Either [Conflict] (Text, [HunkResult])
applyCreation fp mc =
  let content = creationContent fp
  in case mc of
       Nothing -> Right (content, [HunkResult 1 0])
       Just c  -> if c == content
                    then Left [Conflict (fpPath fp) 0 (AlreadyApplied 1)]
                    else Left [Conflict (fpPath fp) 0 FileExists]

-- | Created files are newline-terminated per line (no original to mirror).
creationContent :: FilePatch -> Text
creationContent fp = case fpHunks fp of
  (h : _) -> T.pack (concatMap (\l -> T.unpack l ++ "\n") (hunkNewSide h))
  []      -> T.empty

indexFrom :: Int -> [a] -> [(Int, a)]
indexFrom _ []       = []
indexFrom i (x : xs) = (i, x) : indexFrom (i + 1) xs

-- | Forward cursor over the original lines.  @origLines@ is the full file (for
-- the NoMatch window); @remaining@ is the unconsumed tail; @pos@ is the
-- 1-based line of @remaining@'s head; @outAcc@ is the reversed output.
goHunks
  :: Text -> [Text] -> [Text] -> Int -> [(Int, Hunk)] -> [Text] -> [HunkResult]
  -> Either [Conflict] (Text, [HunkResult])
goHunks _ _ remaining _ [] outAcc resAcc =
  Right (joinNL (reverse outAcc ++ remaining), reverse resAcc)
goHunks path origLines remaining pos ((ix, h) : hs) outAcc resAcc =
  let oldSide = hunkOldSide h
      newSide = hunkNewSide h
  in case sublistOffsets oldSide remaining of
       [k] ->
         let before    = take k remaining
             after     = drop (k + length oldSide) remaining
             startLine = pos + k
             hr        = HunkResult startLine (startLine - hOldStart h)
             outAcc'   = revOnto newSide (revOnto before outAcc)
             pos'      = pos + k + length oldSide
         in goHunks path origLines after pos' hs outAcc' (hr : resAcc)
       [] ->
         case sublistOffsets newSide remaining of
           (k' : _) -> Left [Conflict path ix (AlreadyApplied (pos + k'))]
           []       -> Left [Conflict path ix (NoMatch oldSide (windowAt (hOldStart h) (length oldSide) origLines))]
       ks -> Left [Conflict path ix (Ambiguous (map (\k -> pos + k) ks))]

-- | Prepend @xs@ (in order) onto a reversed accumulator.
revOnto :: [a] -> [a] -> [a]
revOnto []       acc = acc
revOnto (x : xs) acc = revOnto xs (x : acc)

-- | All 0-based offsets where @needle@ occurs as a contiguous sublist of
-- @hay@.
sublistOffsets :: [Text] -> [Text] -> [Int]
sublistOffsets needle hay
  | nlen == 0 = []
  | otherwise = go 0 hay
  where
    nlen = length needle
    hlen = length hay
    go k h
      | k + nlen > hlen = []
      | otherwise =
          let here = if take nlen h == needle then [k] else []
          in here ++ go (k + 1) (tailOr h)
    tailOr []       = []
    tailOr (_ : xs) = xs

-- | The old-len-line window at the (1-based) hint position, clamped.
windowAt :: Int -> Int -> [Text] -> [Text]
windowAt hint oldLen origLines = take oldLen (drop (hint - 1) origLines)
