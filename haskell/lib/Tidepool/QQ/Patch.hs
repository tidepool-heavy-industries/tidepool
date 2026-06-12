{-# LANGUAGE TemplateHaskellQuotes #-}
{-# LANGUAGE OverloadedStrings #-}

-- | The @[patch|...|]@ quasi-quoter: a unified-diff 'Tidepool.Patch.Patch'
-- literal (expression side) and a structural diff matcher (pattern side).
--
-- == Expression side
--
-- The quote body is parsed at COMPILE time by 'Tidepool.Patch.parsePatch'; on
-- success it expands to constructor applications over "Tidepool.Patch" (the
-- quote-bracket 'Language.Haskell.TH.Name's resolve to this module's imports,
-- so the splice site needs no extra import).  A parse error is a splice error.
--
-- Diff lines must be LEFT-ALIGNED in the quote (a body line's first column is
-- its @ \/ - \/ +@ prefix).  @|]@ cannot appear inside the body; for a diff
-- that must contain it, take the input lane (@parsePatch@ on a runtime 'Text').
--
-- == Pattern side (in scope — ships whole)
--
-- The scrutinee is a 'Data.Text.Text' (a runtime diff, e.g. an LLM's output).
-- The pattern desugars to a 'ViewPatterns' matcher that runs
-- 'Tidepool.Patch.parsePatch' on the scrutinee and structurally matches the
-- result.  The v1 hole envelope:
--
--   * @$var@ at a path position binds the file's 'Data.Text.Text' path;
--   * a bare @$var@ where the @\@\@@ hunks would go binds that file's @[Hunk]@;
--   * line-content holes @ $x@\/@-$x@\/@+$x@ bind the line's 'Data.Text.Text';
--   * a trailing @...@ after the last file allows extra trailing files
--     (without it, the file count must match exactly);
--   * everything else is a literal that must match exactly.
--
-- Duplicate binders are a compile error.  Line numbers in @\@\@@ headers are
-- hints and are NOT matched — only body shape and content are.
module Tidepool.QQ.Patch (patch) where

import Data.Char (isAlpha, isAlphaNum)
import Data.Text (Text)
import qualified Data.Text as T

import Language.Haskell.TH
  ( Exp (..)
  , Pat
  , Q
  , caseE
  , conP
  , integerL
  , lamE
  , letE
  , listE
  , listP
  , litE
  , match
  , mkName
  , newName
  , normalB
  , stringL
  , tupE
  , tupP
  , valD
  , varE
  , varP
  , viewP
  , wildP
  )
import Language.Haskell.TH.Quote (QuasiQuoter (..))

import Tidepool.Patch
  ( FilePatch (..)
  , Hunk (..)
  , HunkLine (..)
  , Patch
  , parsePatch
  )

-- | @[patch| --- a\/f \\n +++ b\/f \\n \@\@ ... \@\@ ... |]@
patch :: QuasiQuoter
patch = QuasiQuoter
  { quoteExp  = patchExp
  , quotePat  = patchPat
  , quoteType = \_ -> fail "[patch|…|] cannot be used in a type context"
  , quoteDec  = \_ -> fail "[patch|…|] cannot be used in a declaration context"
  }

stripLeadingNewline :: String -> String
stripLeadingNewline ('\n' : r) = r
stripLeadingNewline r          = r

-- ---------------------------------------------------------------------------
-- Expression side
-- ---------------------------------------------------------------------------

patchExp :: String -> Q Exp
patchExp s = case parsePatch (T.pack (stripLeadingNewline s)) of
  Left e  -> fail ("[patch|…|]: " ++ T.unpack e)
  Right p -> patchCodegen p

patchCodegen :: Patch -> Q Exp
patchCodegen p = listE (map fileCodegen p)

fileCodegen :: FilePatch -> Q Exp
fileCodegen (FilePatch path create hunks) =
  [| FilePatch (T.pack $(strE path)) $(boolE create) $(listE (map hunkCodegen hunks)) |]

hunkCodegen :: Hunk -> Q Exp
hunkCodegen (Hunk os ns body) =
  [| Hunk $(intE os) $(intE ns) $(listE (map lineCodegen body)) |]

lineCodegen :: HunkLine -> Q Exp
lineCodegen (Ctx t) = [| Ctx (T.pack $(strE t)) |]
lineCodegen (Del t) = [| Del (T.pack $(strE t)) |]
lineCodegen (Ins t) = [| Ins (T.pack $(strE t)) |]

strE :: Text -> Q Exp
strE t = litE (stringL (T.unpack t))

intE :: Int -> Q Exp
intE n = litE (integerL (fromIntegral n))

boolE :: Bool -> Q Exp
boolE True  = [| True |]
boolE False = [| False |]

-- ---------------------------------------------------------------------------
-- Pattern side: AST
-- ---------------------------------------------------------------------------

data PPath  = PLitPath Text | PVarPath String
data PHunks = PHunksVar String | PHunksList [PHunk]
newtype PHunk = PHunk { phBody :: [PLine] }
data PLine  = PLineLit HunkLine | PLineHole LineKind String
data LineKind = LCtx | LDel | LIns
data PFile  = PFile { pPath :: PPath, pCreate :: Bool, pHunks :: PHunks }

-- ---------------------------------------------------------------------------
-- Pattern side: parser (compile time; full GHC, no JIT constraints)
-- ---------------------------------------------------------------------------

parsePat :: String -> Either String ([PFile], Bool)
parsePat src = goFiles [] (lines (stripLeadingNewline src))
  where
    goFiles acc [] = Right (reverse acc, False)
    goFiles acc (l : ls)
      | trim l == "..." =
          if all isBlankStr ls
            then Right (reverse acc, True)
            else Left "'...' must be the last line"
      | isBlankStr l = goFiles acc ls
      | isToleratedStr l = goFiles acc ls
      | isPrefixStr "--- " l || l == "---" =
          case parseFilePat l ls of
            Left e          -> Left e
            Right (pf, ls') -> goFiles (pf : acc) ls'
      | otherwise = Left ("unexpected line: " ++ l)

parseFilePat :: String -> [String] -> Either String (PFile, [String])
parseFilePat lMinus ls =
  let oldRaw = drop 4 lMinus
      create = oldRaw == "/dev/null"
  in case ls of
       (lPlus : ls1)
         | isPrefixStr "+++ " lPlus || lPlus == "+++" ->
             let newRaw   = drop 4 lPlus
                 pathSpec = if create
                              then litOrVarPath stripPlusStr newRaw
                              else litOrVarPath stripMinusStr oldRaw
             in case parseHunksPat ls1 of
                  Left e            -> Left e
                  Right (phs, ls2)  -> Right (PFile pathSpec create phs, ls2)
         | otherwise -> Left "expected '+++ <path>' after '--- <path>'"
       [] -> Left "expected '+++ <path>' after '--- <path>'"

parseHunksPat :: [String] -> Either String (PHunks, [String])
parseHunksPat ls = case ls of
  (l : ls1)
    | Just x <- asHole l   -> Right (PHunksVar x, ls1)
    | isPrefixStr "@@" l    -> parseHunkList ls
  _ -> Right (PHunksList [], ls)

parseHunkList :: [String] -> Either String (PHunks, [String])
parseHunkList = goH []
  where
    goH acc (l : ls1)
      | isPrefixStr "@@" l =
          case parseBodyPat ls1 [] of
            Left e            -> Left e
            Right (body, ls2) -> goH (PHunk body : acc) ls2
    goH acc ls = Right (PHunksList (reverse acc), ls)

parseBodyPat :: [String] -> [PLine] -> Either String ([PLine], [String])
parseBodyPat ls acc = case ls of
  (l : ls1)
    | isPrefixStr "@@" l                  -> Right (reverse acc, ls)
    | isPrefixStr "--- " l || l == "---"   -> Right (reverse acc, ls)
    | trim l == "..."                      -> Right (reverse acc, ls)
    | otherwise -> case classifyPatLine l of
        Left e  -> Left e
        Right p -> parseBodyPat ls1 (p : acc)
  [] -> Right (reverse acc, [])

classifyPatLine :: String -> Either String PLine
classifyPatLine "" = Right (PLineLit (Ctx ""))
classifyPatLine (c : rest) = case c of
  ' ' -> Right (mkLine LCtx Ctx rest)
  '-' -> Right (mkLine LDel Del rest)
  '+' -> Right (mkLine LIns Ins rest)
  _   -> Left ("hunk body line must start with ' ', '-', or '+': " ++ (c : rest))
  where
    mkLine kind con content = case asHole content of
      Just x  -> PLineHole kind x
      Nothing -> PLineLit (con (T.pack content))

-- | A whole-line @$ident@ hole; 'Nothing' otherwise.
asHole :: String -> Maybe String
asHole ('$' : rest)
  | isIdent rest = Just rest
asHole _ = Nothing

isIdent :: String -> Bool
isIdent []       = False
isIdent (c : cs) = identStart c && all identTail cs

identStart :: Char -> Bool
identStart c = isAlpha c || c == '_'

identTail :: Char -> Bool
identTail c = isAlphaNum c || c == '_' || c == '\''

litOrVarPath :: (String -> String) -> String -> PPath
litOrVarPath strip raw = case asHole raw of
  Just x  -> PVarPath x
  Nothing -> PLitPath (T.pack (strip raw))

stripMinusStr :: String -> String
stripMinusStr s = case stripPrefixS "a/" s of
  Just r  -> r
  Nothing -> case stripPrefixS "./" s of { Just r -> r; Nothing -> s }

stripPlusStr :: String -> String
stripPlusStr s = case stripPrefixS "b/" s of
  Just r  -> r
  Nothing -> case stripPrefixS "./" s of { Just r -> r; Nothing -> s }

stripPrefixS :: String -> String -> Maybe String
stripPrefixS [] ys = Just ys
stripPrefixS _ [] = Nothing
stripPrefixS (x : xs) (y : ys)
  | x == y    = stripPrefixS xs ys
  | otherwise = Nothing

isPrefixStr :: String -> String -> Bool
isPrefixStr p s = case stripPrefixS p s of { Just _ -> True; Nothing -> False }

isBlankStr :: String -> Bool
isBlankStr = all (\c -> c == ' ' || c == '\t')

isToleratedStr :: String -> Bool
isToleratedStr l =
     isPrefixStr "diff --git" l
  || isPrefixStr "index " l
  || isPrefixStr "new file mode" l
  || isPrefixStr "old mode" l
  || isPrefixStr "new mode" l
  || isPrefixStr "similarity" l
  || isPrefixStr "rename " l
  || isPrefixStr "copy " l

trim :: String -> String
trim = f . f where f = reverse . dropWhile (\c -> c == ' ' || c == '\t')

-- ---------------------------------------------------------------------------
-- Pattern side: binders
-- ---------------------------------------------------------------------------

collectBindersFiles :: [PFile] -> [String]
collectBindersFiles = concatMap collectFile
  where
    collectFile pf = pathBinders (pPath pf) ++ hunksBinders (pHunks pf)
    pathBinders (PVarPath x) = [x]
    pathBinders _            = []
    hunksBinders (PHunksVar x)    = [x]
    hunksBinders (PHunksList phs) = concatMap (concatMap lineBinders . phBody) phs
    lineBinders (PLineHole _ x) = [x]
    lineBinders _               = []

findDup :: [String] -> Maybe String
findDup = go []
  where
    go _ [] = Nothing
    go seen (x : xs)
      | x `elem` seen = Just x
      | otherwise     = go (x : seen) xs

-- ---------------------------------------------------------------------------
-- Pattern side: matcher codegen
-- ---------------------------------------------------------------------------

-- | A pending match obligation: a scrutinee 'Exp' and what it must match.
data Ob
  = ObFiles  Exp [PFile] Bool
  | ObPath   Exp PPath
  | ObCreate Exp Bool
  | ObHunks  Exp PHunks
  | ObBody   Exp [PLine]
  | ObLine   Exp PLine

patchPat :: String -> Q Pat
patchPat s = case parsePat s of
  Left e -> fail ("[patch|…|] (pattern): " ++ e)
  Right (pfiles, ell) ->
    let binders = collectBindersFiles pfiles
    in case findDup binders of
         Just d  -> fail ("[patch|…|] (pattern): duplicate binder $" ++ d)
         Nothing -> do
           t <- newName "t"
           p <- newName "p"
           matchBody <- build binders [ObFiles (VarE p) pfiles ell]
           matcher <- lamE [varP t]
             (caseE [| parsePatch $(varE t) |]
                [ match (conP 'Right [varP p]) (normalB (pure matchBody)) []
                , match (conP 'Left [wildP])   (normalB nothingE)        [] ])
           viewP (pure matcher) (resultPat binders)

nothingE :: Q Exp
nothingE = [| Nothing |]

build :: [String] -> [Ob] -> Q Exp
build binders [] = finalJust binders
build binders (ob : rest) = case ob of
  ObFiles e pfiles ell -> matchFiles binders e pfiles ell rest
  ObPath e pp          -> matchPath binders e pp rest
  ObCreate e flag      -> matchCreate binders e flag rest
  ObHunks e ph         -> matchHunks binders e ph rest
  ObBody e plines      -> matchBodyLines binders e plines rest
  ObLine e pl          -> matchLine binders e pl rest

matchFiles :: [String] -> Exp -> [PFile] -> Bool -> [Ob] -> Q Exp
matchFiles binders e pfiles ell rest = do
  names <- mapM (const (newName "f")) pfiles
  let pat | ell       = foldr (\nm acc -> conP '(:) [varP nm, acc]) wildP names
          | otherwise = listP (map varP names)
      obsFor (nm, pf) =
        [ ObPath   (AppE (VarE 'fpPath)   (VarE nm)) (pPath pf)
        , ObCreate (AppE (VarE 'fpCreate) (VarE nm)) (pCreate pf)
        , ObHunks  (AppE (VarE 'fpHunks)  (VarE nm)) (pHunks pf)
        ]
      newObs = concatMap obsFor (zip names pfiles)
  caseE (pure e)
    [ match pat   (normalB (build binders (newObs ++ rest))) []
    , match wildP (normalB nothingE) [] ]

matchPath :: [String] -> Exp -> PPath -> [Ob] -> Q Exp
matchPath binders e pp rest = case pp of
  PVarPath x -> letE [valD (varP (mkName x)) (normalB (pure e)) []] (build binders rest)
  PLitPath t -> [| if $(pure e) == T.pack $(strE t) then $(build binders rest) else Nothing |]

matchCreate :: [String] -> Exp -> Bool -> [Ob] -> Q Exp
matchCreate binders e flag rest =
  if flag
    then [| if $(pure e) then $(build binders rest) else Nothing |]
    else [| if $(pure e) then Nothing else $(build binders rest) |]

matchHunks :: [String] -> Exp -> PHunks -> [Ob] -> Q Exp
matchHunks binders e ph rest = case ph of
  PHunksVar x -> letE [valD (varP (mkName x)) (normalB (pure e)) []] (build binders rest)
  PHunksList phunks -> do
    names <- mapM (const (newName "h")) phunks
    let pat    = listP (map varP names)
        newObs = [ ObBody (AppE (VarE 'hBody) (VarE nm)) (phBody phk)
                 | (nm, phk) <- zip names phunks ]
    caseE (pure e)
      [ match pat   (normalB (build binders (newObs ++ rest))) []
      , match wildP (normalB nothingE) [] ]

matchBodyLines :: [String] -> Exp -> [PLine] -> [Ob] -> Q Exp
matchBodyLines binders e plines rest = do
  names <- mapM (const (newName "ln")) plines
  let pat    = listP (map varP names)
      newObs = [ ObLine (VarE nm) pl | (nm, pl) <- zip names plines ]
  caseE (pure e)
    [ match pat   (normalB (build binders (newObs ++ rest))) []
    , match wildP (normalB nothingE) [] ]

matchLine :: [String] -> Exp -> PLine -> [Ob] -> Q Exp
matchLine binders e pl rest = case pl of
  PLineLit (Ctx t)  -> litLine 'Ctx t
  PLineLit (Del t)  -> litLine 'Del t
  PLineLit (Ins t)  -> litLine 'Ins t
  PLineHole LCtx x  -> holeLine 'Ctx x
  PLineHole LDel x  -> holeLine 'Del x
  PLineHole LIns x  -> holeLine 'Ins x
  where
    litLine con t = do
      s <- newName "s"
      caseE (pure e)
        [ match (conP con [varP s])
            (normalB [| if $(varE s) == T.pack $(strE t) then $(build binders rest) else Nothing |]) []
        , match wildP (normalB nothingE) [] ]
    holeLine con x =
      caseE (pure e)
        [ match (conP con [varP (mkName x)]) (normalB (build binders rest)) []
        , match wildP (normalB nothingE) [] ]

finalJust :: [String] -> Q Exp
finalJust []  = [| Just () |]
finalJust [b] = [| Just $(varE (mkName b)) |]
finalJust bs  = [| Just $(tupE (map (varE . mkName) bs)) |]

resultPat :: [String] -> Q Pat
resultPat []  = conP 'Just [tupP []]
resultPat [b] = conP 'Just [varP (mkName b)]
resultPat bs  = conP 'Just [tupP (map (varP . mkName) bs)]
