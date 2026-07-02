{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DuplicateRecordFields, OverloadedRecordDot #-}
-- | Declarative small edits (line-range + anchor) that LOWER to a
-- "Tidepool.Patch" 'FilePatch' and ride the shipped atomic apply. This is the
-- non-pattern half of "apply this edit to this file": where a unified diff is
-- awkward to author (replace lines 10-15, insert after an anchor, edits that
-- don't need exact surrounding context), an 'Edit' names the change directly,
-- the engine resolves it against freshly-read file content, and 'genPatch'
-- turns the result into a CONTEXT-anchored diff — so it inherits every keystone
-- property of "Diff" for free: pre-flight ('planEdits'), all-or-nothing apply
-- ('applyEdits' delegates to 'Diff.apply'), conflict-as-data, and a rendered
-- review diff.
--
-- == Line-number safety (read the haddock before using ReplaceLines/InsertAt)
--
-- Line numbers are resolved against the content read in the SAME eval, then
-- baked into a context-anchored patch — so even deferred application matches by
-- CONTEXT, not by number, and an in-eval read+edit is safe. The footgun is
-- CROSS-eval: a line number captured in a prior eval may no longer point where
-- you think. For cross-eval edits, prefer the anchor ops ('ReplaceAnchor' /
-- 'InsertAfterAnchor' / 'InsertBeforeAnchor') — they are content-addressed and
-- carry their own uniqueness check (an ambiguous anchor is a conflict, not a
-- silent wrong edit). The line-number ops are the in-eval escape hatch, ideal
-- when you already hold numbers from a structural search (@grepGlob@ / @rsFn@ /
-- @hsDef@) run in the same eval.
--
-- == Conflicts are DATA, never thrown
--
-- The resolution phase reports 'EditConflict's (anchor missing\/ambiguous,
-- range out of bounds, edits that overlap) as a JSON array; 'applyEdits' writes
-- NOTHING when any resolution conflict fires (atomic across the whole edit
-- batch). After a clean resolution the patch flows through 'Diff.apply', so the
-- apply-phase conflict vocabulary ('Tidepool.Patch.Conflict') applies as usual.
-- Only a malformed @editsJ@ payload (missing\/ill-typed fields) is loud — that
-- is a caller bug, mirroring @patchJ@.
--
-- FOLLOW-UP (not in v1): the older "Patch" surgery verbs (@patchFile@,
-- @insertAfter@) could be re-expressed as 'Edit' producers to collapse the two
-- paradigms onto this one lowering; left untouched here to avoid changing the
-- shipped surface.
module Edit
  ( Edit (..)
  , EditConflict (..)
  , EditOutcome (..)
  , ApplyOutcome (..)
  , resolveEdits
  , planEdits
  , applyEdits
  , planEditsJ
  , editsJ
  ) where

import Tidepool.Prelude hiding (error)
import Tidepool.Effects
import Tidepool.Patch (Patch, FilePatch, genPatch, renderPatch)
import Diff (apply, DiffOutcome)

-- ---------------------------------------------------------------------------
-- Edit language
-- ---------------------------------------------------------------------------

-- | One declarative edit. All line numbers are 1-based; all anchors must match
-- exactly one line (a substring test). The replacement is a list of whole
-- lines (no trailing newlines — the engine handles line termination).
data Edit
  = ReplaceLines Int Int [Text]    -- ^ replace lines @[lo..hi]@ inclusive (@[]@ deletes)
  | InsertAt Int [Text]            -- ^ insert before line @n@ (@n = lineCount+1@ appends)
  | ReplaceAnchor Text [Text]      -- ^ replace the unique line CONTAINING the anchor
  | InsertAfterAnchor Text [Text]  -- ^ insert after the unique line containing the anchor
  | InsertBeforeAnchor Text [Text] -- ^ insert before the unique line containing the anchor

-- | A resolution-phase conflict, reported as data (never thrown).
data EditConflict
  = AnchorMissing Text             -- ^ anchor matched no line
  | AnchorAmbiguous Text [Int]     -- ^ anchor matched 2+ lines (1-based line numbers)
  | RangeOutOfBounds Int Int Int   -- ^ @lo hi lineCount@: a line range outside @[1..lineCount]@
  | EditsOverlap Int Int Int Int   -- ^ two resolved spans @lo1 hi1 lo2 hi2@ touch
  deriving (Eq, Show)

-- | Outcome of 'planEdits' (dry run). Documents the shapes the verb surfaced as
-- an opaque Value; ToJSON reproduces them exactly.
data EditOutcome
  = Applied { changed :: Bool, diff :: Text }  -- ^ resolves; @changed=False@ = no-op, else carries the review diff
  | Conflicts { conflicts :: [EditConflict] }  -- ^ resolution conflicts (nothing to apply)
  | NotFound { reason :: Text }                -- ^ the target file does not exist
  deriving (Eq, Show)

instance ToJSON EditOutcome where
  toJSON (Applied False _) = object [ "ok" .= True, "changed" .= False ]
  toJSON (Applied True d)  = object [ "ok" .= True, "changed" .= True, "diff" .= d ]
  toJSON (Conflicts cs)    = object [ "ok" .= False, "conflicts" .= map ecToValue cs ]
  toJSON (NotFound e)      = object [ "ok" .= False, "error" .= e ]

-- | Outcome of 'applyEdits'. The @changed=True@ branch delegates to
-- 'Diff.apply', so its shape is that verb's 'DiffOutcome' verbatim (surfaced
-- via 'ApplyDelegated'); the other branches use the @applied@ key.
data ApplyOutcome
  = ApplyNotFound Text            -- ^ @{applied:false,error}@
  | ApplyConflicts [EditConflict] -- ^ @{applied:false,conflicts}@
  | ApplyNoChange                 -- ^ @{applied:true,changed:false}@
  | ApplyDelegated DiffOutcome    -- ^ 'Diff.apply' result verbatim
  deriving (Eq, Show)

instance ToJSON ApplyOutcome where
  toJSON (ApplyNotFound e)   = object [ "applied" .= False, "error" .= e ]
  toJSON (ApplyConflicts cs) = object [ "applied" .= False, "conflicts" .= map ecToValue cs ]
  toJSON ApplyNoChange       = object [ "applied" .= True, "changed" .= False ]
  toJSON (ApplyDelegated d)  = toJSON d

-- | A resolved edit: replace 1-based old lines @[lo..hi]@ with @new@. A pure
-- insertion before line @k@ is the empty range @(k, k-1)@ (consumes no old
-- line), so splicing and overlap detection treat inserts and replaces alike.
type Span = (Int, Int, [Text])

-- ---------------------------------------------------------------------------
-- String-space line split/join (mirrors Tidepool.Patch's discipline: keep the
-- trailing-newline phantom so the round trip through genPatch is exact, and
-- never build an empty Text the tree-walker mis-represents).
-- ---------------------------------------------------------------------------

-- | Split Text on newlines; exact round-trip with joinNL (no phantom trailing entry).
splitNL :: Text -> [Text]
splitNL t = map pack (go (unpack t))
  where
    go s = case break (== '\n') s of
      (a, [])       -> [a]
      (a, _ : rest) -> a : go rest

-- | Join lines with newlines; exact round-trip with splitNL.
joinNL :: [Text] -> Text
joinNL ts = pack (go (map unpack ts))
  where
    go []       = []
    go [s]      = s
    go (s : ss) = s ++ "\n" ++ go ss

-- ---------------------------------------------------------------------------
-- Resolution: Edit -> Span (with conflicts as data)
-- ---------------------------------------------------------------------------

-- | Resolve a batch of edits against the file's lines into the spliced
-- candidate lines, or report ALL resolution conflicts. Conflicts are collected
-- in two phases: per-edit (anchor\/range) first, then pairwise overlap on the
-- resolved spans — so an overlap is only reported once every span resolved.
resolveEdits :: Text -> [Edit] -> Either [EditConflict] [Text]
resolveEdits src edits =
  let srcLines = splitNL src
  in case resolveAll srcLines edits of
       (c : cs, _)    -> Left (c : cs)
       ([], spans)    -> case overlapConflicts spans of
         (o : os) -> Left (o : os)
         []       -> Right (spliceLines srcLines (sortSpans spans))

-- | Resolve all edits in order, accumulating conflicts and resolved spans (both lists reversed).
resolveAll :: [Text] -> [Edit] -> ([EditConflict], [Span])
resolveAll srcLines = go [] []
  where
    go cs sp [] = (reverse cs, reverse sp)
    go cs sp (e : es) = case resolveEdit srcLines e of
      Left c  -> go (c : cs) sp es
      Right s -> go cs (s : sp) es

-- | Resolve one Edit against the source lines; Left = per-edit conflict, Right = Span.
resolveEdit :: [Text] -> Edit -> Either EditConflict Span
resolveEdit srcLines e = case e of
  ReplaceLines lo hi new
    | lo >= 1 && lo <= hi && hi <= n -> Right (lo, hi, new)
    | otherwise                      -> Left (RangeOutOfBounds lo hi n)
  InsertAt k new
    | k >= 1 && k <= n + 1 -> Right (k, k - 1, new)
    | otherwise            -> Left (RangeOutOfBounds k k n)
  ReplaceAnchor a new       -> withAnchor a (\i -> (i, i, new))
  InsertAfterAnchor a new   -> withAnchor a (\i -> (i + 1, i, new))
  InsertBeforeAnchor a new  -> withAnchor a (\i -> (i, i - 1, new))
  where
    n = length srcLines
    hits a = [ i | (i, l) <- zip [1 .. n] srcLines, a `isInfixOf` l ]
    withAnchor a mk = case hits a of
      [i] -> Right (mk i)
      []  -> Left (AnchorMissing a)
      is  -> Left (AnchorAmbiguous a is)

-- | Pairwise overlap over the resolved spans (original order). A replace span
-- is @lo <= hi@; an insertion is the empty range @lo > hi@ sitting in the gap
-- before line @lo@. Two claims conflict when they would make the splice
-- order-dependent.
overlapConflicts :: [Span] -> [EditConflict]
overlapConflicts spans =
  [ EditsOverlap lo1 hi1 lo2 hi2
  | (i, (lo1, hi1, _)) <- idx spans
  , (j, (lo2, hi2, _)) <- idx spans
  , i < j
  , spansConflict lo1 hi1 lo2 hi2
  ]
  where
    idx xs = zip [0 .. length xs - 1] xs

-- | True when two (lo,hi) spans conflict: overlapping ranges or inserts in the same gap.
spansConflict :: Int -> Int -> Int -> Int -> Bool
spansConflict lo1 hi1 lo2 hi2
  | ins1 && ins2 = lo1 == lo2                 -- two inserts in the same gap
  | ins1         = lo2 < lo1 && lo1 <= hi2    -- insert gap interior to a replaced block
  | ins2         = lo1 < lo2 && lo2 <= hi1
  | otherwise    = lo1 <= hi2 && lo2 <= hi1   -- two replaced ranges intersect
  where
    ins1 = hi1 < lo1
    ins2 = hi2 < lo2

-- | Sort spans by lo then hi; required ordering for spliceLines.
sortSpans :: [Span] -> [Span]
sortSpans = sortBy cmpSpan
  where
    cmpSpan (lo1, hi1, _) (lo2, hi2, _) = case compare lo1 lo2 of
      EQ -> compare hi1 hi2
      o  -> o

-- | Splice sorted, disjoint spans into the file's lines. @pos@ is the 1-based
-- next unconsumed line; for each span emit the gap before it unchanged, then
-- the replacement, then skip the replaced lines (an insertion skips none).
spliceLines :: [Text] -> [Span] -> [Text]
spliceLines = go 1
  where
    go _ remaining [] = remaining
    go pos remaining ((lo, hi, new) : rest) =
      let before = take (lo - pos) remaining
          after  = drop (hi - pos + 1) remaining
      in before ++ new ++ go (hi + 1) after rest

-- ---------------------------------------------------------------------------
-- Lowering to a patch + the shipped apply
-- ---------------------------------------------------------------------------

-- | Read a file's current content, or 'Nothing' if it does not exist.
readTarget :: Text -> M (Maybe Text)
readTarget path = do
  exists <- doesFileExist path
  if exists then fmap Just (readFile path) else pure Nothing

-- | Lower the edits to a context-anchored patch against the current file. The
-- candidate is diffed back to @src@ by 'genPatch', so the returned 'FilePatch'
-- carries 'genPatch's three-line context and round-trips through apply.
lowerEdits :: Text -> Text -> [Edit] -> Either [EditConflict] (Either Text FilePatch)
lowerEdits path src edits = case resolveEdits src edits of
  Left cs   -> Left cs
  Right cand -> Right (genPatch path src (joinNL cand))

-- | Dry run: report resolution conflicts, or the rendered review diff. No
-- writes. The @diff@ field is exactly the text 'applyEdits' would apply — feed
-- it to @Diff.applyDiff@ to commit a pre-approved edit.
planEdits :: Text -> [Edit] -> M EditOutcome
planEdits path edits = do
  mc <- readTarget path
  case mc of
    Nothing  -> pure (NotFound ("file not found: " <> path))
    Just src -> case lowerEdits path src edits of
      Left cs            -> pure (Conflicts cs)
      Right (Left _)     -> pure (Applied False "")
      Right (Right fp)   -> pure (Applied True (renderPatch [fp]))

-- | Resolve the edits and apply them ATOMICALLY: any resolution conflict (or a
-- conflict in the shipped apply) writes nothing. Delegates to 'Diff.apply', so
-- the success report is apply's (@{"applied":true,"files":[…]}@) and rollback
-- composes via the existing inverse machinery.
applyEdits :: Text -> [Edit] -> M ApplyOutcome
applyEdits path edits = do
  mc <- readTarget path
  case mc of
    Nothing  -> pure (ApplyNotFound ("file not found: " <> path))
    Just src -> case lowerEdits path src edits of
      Left cs           -> pure (ApplyConflicts cs)
      Right (Left _)    -> pure ApplyNoChange
      Right (Right fp)  -> ApplyDelegated <$> apply [fp]

-- | Serialize an EditConflict to a JSON Value for the wire format.
ecToValue :: EditConflict -> Value
ecToValue (AnchorMissing a)          = object [ "kind" .= ("anchor-missing" :: Text), "anchor" .= a ]
ecToValue (AnchorAmbiguous a is)     = object [ "kind" .= ("anchor-ambiguous" :: Text), "anchor" .= a, "lines" .= is ]
ecToValue (RangeOutOfBounds lo hi n) = object [ "kind" .= ("range-out-of-bounds" :: Text), "lo" .= lo, "hi" .= hi, "fileLines" .= n ]
ecToValue (EditsOverlap a b c d)     = object [ "kind" .= ("edits-overlap" :: Text), "a" .= [a, b], "b" .= [c, d] ]

-- ---------------------------------------------------------------------------
-- JSON front door (input lane): {file, edits: [{op, …}]}
-- ---------------------------------------------------------------------------

-- | 'applyEdits' from a JSON payload on the @input@ lane:
-- @{"file":"p","edits":[{"op":"replaceAnchor","anchor":"…","lines":["…"]}, …]}@.
-- Ops: @replaceLines@ (@lo@,@hi@,@lines@), @insertAt@ (@line@,@lines@),
-- @replaceAnchor@\/@insertAfterAnchor@\/@insertBeforeAnchor@ (@anchor@,@lines@).
-- A malformed payload is loud (caller bug); resolution conflicts are data.
editsJ :: Value -> M ApplyOutcome
editsJ v = withFileEdits v applyEdits

-- | 'planEdits' from the same JSON payload (dry run).
planEditsJ :: Value -> M EditOutcome
planEditsJ v = withFileEdits v planEdits

-- | Parse the file path and edit list from a JSON payload, then call the continuation.
withFileEdits :: Value -> (Text -> [Edit] -> M a) -> M a
withFileEdits v k = case getTxt v "file" of
  Nothing   -> error "editsJ: need a string 'file' field"
  Just path -> case parseEdits v of
    Left e      -> error e
    Right edits -> k path edits

-- | Parse the 'edits' array from an editsJ JSON payload; Left on missing or malformed array.
parseEdits :: Value -> Either Text [Edit]
parseEdits v = case getArr v "edits" of
  Nothing  -> Left "editsJ: need an array 'edits' field"
  Just arr -> mapEitherList parseEdit arr

-- | Parse one edit object from its 'op' field; Left on unknown op or missing fields.
parseEdit :: Value -> Either Text Edit
parseEdit v = txtField "op" `bindE` \op -> case op of
  "replaceLines"       -> intField "lo" `bindE` \lo -> intField "hi" `bindE` \hi -> linesField `bindE` \ls -> Right (ReplaceLines lo hi ls)
  "insertAt"           -> intField "line" `bindE` \k -> linesField `bindE` \ls -> Right (InsertAt k ls)
  "replaceAnchor"      -> txtField "anchor" `bindE` \a -> linesField `bindE` \ls -> Right (ReplaceAnchor a ls)
  "insertAfterAnchor"  -> txtField "anchor" `bindE` \a -> linesField `bindE` \ls -> Right (InsertAfterAnchor a ls)
  "insertBeforeAnchor" -> txtField "anchor" `bindE` \a -> linesField `bindE` \ls -> Right (InsertBeforeAnchor a ls)
  other                -> Left ("editsJ: unknown op '" <> other <> "'")
  where
    txtField k = case getTxt v k of { Just t -> Right t; Nothing -> Left ("editsJ: op needs string '" <> k <> "'") }
    intField k = case getInt v k of { Just i -> Right i; Nothing -> Left ("editsJ: op needs int '" <> k <> "'") }
    linesField = case getArr v "lines" of
      Nothing -> Left "editsJ: op needs array 'lines'"
      Just ls -> mapEitherList asTextE ls
    asTextE x = case asText x of { Just t -> Right t; Nothing -> Left "editsJ: 'lines' entries must be strings" }

-- | Extract a Text field from a JSON object; Nothing when absent or not a string.
-- (getTxt/getInt/getArr use explicit case chains, not the Maybe monad —
-- do-notation over Maybe pulls dictionary paths that are riskier on the JIT.)
getTxt :: Value -> Text -> Maybe Text
getTxt v k = case v ?. k of { Just x -> asText x; Nothing -> Nothing }

-- | Extract an Int field from a JSON object; Nothing when absent or not a number.
getInt :: Value -> Text -> Maybe Int
getInt v k = case v ?. k of { Just x -> asInt x; Nothing -> Nothing }

-- | Extract an array field from a JSON object; Nothing when absent or not an array.
getArr :: Value -> Text -> Maybe [Value]
getArr v k = case v ?. k of { Just x -> asArray x; Nothing -> Nothing }

-- | Either threaded by a hand-written bind (the JIT-safe pattern: a plain
-- function, never the Either Monad dictionary — see "Tidepool.Patch").
bindE :: Either Text a -> (a -> Either Text b) -> Either Text b
bindE (Left e)  _ = Left e
bindE (Right a) f = f a

-- | Map a fallible function over a list, stopping at the first Left.
mapEitherList :: (a -> Either Text b) -> [a] -> Either Text [b]
mapEitherList f = go []
  where
    go acc [] = Right (reverse acc)
    go acc (x : xs) = case f x of
      Left e  -> Left e
      Right y -> go (y : acc) xs
