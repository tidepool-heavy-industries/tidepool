{-# LANGUAGE OverloadedStrings #-}
-- | @System.FilePath@ vendored over 'Text'.
--
-- The model is fluent in @System.FilePath@ already (it is deep in the weights),
-- so we expose the SAME names and shapes — @(\</>)@, @takeExtension@,
-- @takeBaseName@, … — but over our 'Text'-everywhere world. As in the real
-- package, @FilePath@ is a TYPE ALIAS (there @= String@; here @= Text@), so
-- paths interoperate freely with every 'Text' operation, string literals, and
-- the lens\/JSON surface. Semantics are POSIX ('/' separator).
--
-- Predicate-taking helpers route through 'Tidepool.Data.Text' (the vendored,
-- JIT-safe bodies), never an external-package unfolding.
--
-- 'normalise', 'splitExtension', and 'takeExtensions' are ports of
-- @System.FilePath.Posix.normalise@\/@splitExtension@\/@splitExtensions@
-- (the @filepath@ package, filepath-1.5.2.0, BSD-3-Clause, (c) 2005-2020
-- Neil Mitchell), translated from @String@ to 'Text' and specialized to the
-- POSIX branch of the (String\/CHAR-generic in the real package) source —
-- see
-- <https://hackage.haskell.org/package/filepath-1.5.2.0/docs/System-FilePath-Posix.html>
-- and its linked @docs\/src@ source.
module Tidepool.FilePath
  ( FilePath
    -- * Separator
  , pathSeparator
    -- * Combine / split
  , (</>), joinPath, splitFileName, splitDirectories, normalise
    -- * Filename / directory
  , takeFileName, takeBaseName, takeDirectory
    -- * Extensions
  , (<.>), (-<.>)
  , takeExtension, takeExtensions, dropExtension, dropExtensions
  , addExtension, replaceExtension, splitExtension
  , hasExtension, isExtensionOf
    -- * Predicates
  , isAbsolute, isRelative
  ) where

import Prelude hiding (FilePath)
import Data.Text (Text)
import qualified Tidepool.Data.Text as T

-- | A file path. An alias for 'Text' (mirroring @System.FilePath@'s
-- @type FilePath = String@), so paths ARE text — every 'Text' op applies.
type FilePath = Text

-- | The POSIX path separator, @\'/\'@.
pathSeparator :: Char
pathSeparator = '/'

-- | Is the path absolute (does it start at the root @\"/\"@)?
isAbsolute :: FilePath -> Bool
isAbsolute = T.isPrefixOf "/"

-- | Is the path relative (not 'isAbsolute')?
isRelative :: FilePath -> Bool
isRelative = not . isAbsolute

-- | Join two paths with a single separator. An absolute right-hand side
-- replaces the left, matching @System.FilePath@.
--
-- >>> "usr" </> "bin"   == "usr/bin"
-- >>> "usr/" </> "bin"  == "usr/bin"
-- >>> "usr" </> "/bin"  == "/bin"
(</>) :: FilePath -> FilePath -> FilePath
a </> b
  | T.null b           = a
  | isAbsolute b       = b
  | T.null a           = b
  | T.isSuffixOf "/" a = a <> b
  | otherwise          = a <> "/" <> b
infixr 5 </>

-- | Reassemble a list of components into a path.
joinPath :: [FilePath] -> FilePath
joinPath = foldr (</>) ""

-- | Split into @(directory-with-trailing-slash, filename)@ (dir is @\"./\"@
-- when there is none), matching @System.FilePath@.
--
-- >>> splitFileName "a/b/c"  == ("a/b/", "c")
-- >>> splitFileName "file"   == ("./", "file")
splitFileName :: FilePath -> (FilePath, FilePath)
splitFileName p = case T.breakOnEnd "/" p of
  ("", f) -> ("./", f)
  r       -> r

-- | Split a path into its directory components, keeping a leading @\"/\"@ for
-- absolute paths.
--
-- >>> splitDirectories "/a/b/c" == ["/", "a", "b", "c"]
-- >>> splitDirectories "a/b"    == ["a", "b"]
splitDirectories :: FilePath -> [FilePath]
splitDirectories p =
  let parts = filter (not . T.null) (T.splitOn "/" p)
  in if isAbsolute p then "/" : parts else parts

-- | Normalise a path: collapse a run of leading separators to one, collapse
-- interior separator runs, and drop @.@ components — and, matching
-- upstream, PRESERVE a meaningful trailing separator, including a bare
-- @\"./\"@.
--
-- Any number of leading @\/@ collapse to exactly one — POSIX.1 leaves
-- exactly-two-leading-slashes implementation-defined, but @filepath@ itself
-- always collapses to one (@normalise \"\/\/home\" == \"\/home\"@ is one of
-- its own doctests), and this port follows @filepath@, not the standard.
--
-- Like @System.FilePath.normalise@, @..@ segments are PRESERVED, not
-- resolved — resolving @..@ lexically is unsound when the path crosses a
-- symlink (the lexical parent is not the real parent), so the canonical
-- function deliberately leaves @..@ for the OS. Do NOT use this for sandbox
-- containment checks; it is a lexical tidy-up, not a safe-path oracle.
--
-- >>> normalise "a/./b/../c" == "a/b/../c"
-- >>> normalise "/test/./file" == "/test/file"
-- >>> normalise "a/" == "a/"
-- >>> normalise "/test////" == "/test/"
-- >>> normalise "./" == "./"
-- >>> normalise "" == "."
normalise :: FilePath -> FilePath
normalise p = body <> trailingSep
  where
    lead = T.takeWhile (== pathSeparator) p
    rest = T.dropWhile (== pathSeparator) p
    drv  = if T.null lead then "" else T.singleton pathSeparator

    comps  = filter (/= ".") (filter (not . T.null) (T.splitOn "/" rest))
    joined = T.intercalate "/" comps

    body | T.null drv && T.null joined = "."
         | otherwise                   = drv <> joined

    hasTrailingSep xs = not (T.null xs) && T.last xs == pathSeparator
    isDirPath xs = hasTrailingSep xs
      || (not (T.null xs) && T.last xs == '.' && hasTrailingSep (T.init xs))

    trailingSep
      | isDirPath rest && not (hasTrailingSep body) = T.singleton pathSeparator
      | otherwise                                   = ""

-- | The component after the final separator.
--
-- >>> takeFileName "a/b/c.ext" == "c.ext"
takeFileName :: FilePath -> FilePath
takeFileName = snd . T.breakOnEnd "/"

-- | Everything up to (not including) the final separator; @\".\"@ if there is
-- none, @\"/\"@ at the root.
takeDirectory :: FilePath -> FilePath
takeDirectory p = case T.breakOnEnd "/" p of
  ("", _) -> "."
  (d, _)  -> let d' = T.dropWhileEnd (== '/') d
             in if T.null d' then "/" else d'

-- | The filename without directory or extension. A name that begins with a
-- @.@ (a hidden file, e.g. @\".bashrc\"@) has an EMPTY base name — see
-- 'splitExtension'.
--
-- >>> takeBaseName "a/b/c.tar.gz" == "c.tar"
-- >>> takeBaseName ".bashrc" == ""
takeBaseName :: FilePath -> FilePath
takeBaseName = dropExtension . takeFileName

-- | The final extension, including the leading @\'.\'@, or @\"\"@ if none.
--
-- >>> takeExtension "file.txt"  == ".txt"
-- >>> takeExtension "file"      == ""
-- >>> takeExtension ".bashrc"   == ".bashrc"
takeExtension :: FilePath -> Text
takeExtension = snd . splitExtension

-- | All extensions, e.g. @\".tar.gz\"@ — everything from the first @.@ in
-- the final path component onward.
--
-- >>> takeExtensions "file.tar.gz" == ".tar.gz"
takeExtensions :: FilePath -> Text
takeExtensions p = snd (T.breakOn "." (takeFileName p))

-- | Drop the final extension.
--
-- >>> dropExtension "file.txt" == "file"
dropExtension :: FilePath -> FilePath
dropExtension = fst . splitExtension

-- | Drop every extension.
dropExtensions :: FilePath -> FilePath
dropExtensions p =
  let exts = takeExtensions p
  in if T.null exts then p else T.dropEnd (T.length exts) p

-- | Add an extension. A leading @\'.\'@ on the extension is not duplicated.
addExtension :: FilePath -> Text -> FilePath
addExtension p ext
  | T.null ext           = p
  | T.isPrefixOf "." ext = p <> ext
  | otherwise            = p <> "." <> ext

-- | Operator alias for 'addExtension'.
(<.>) :: FilePath -> Text -> FilePath
(<.>) = addExtension
infixr 7 <.>

-- | Replace the final extension.
--
-- >>> replaceExtension "file.txt" "md" == "file.md"
replaceExtension :: FilePath -> Text -> FilePath
replaceExtension p ext = addExtension (dropExtension p) ext

-- | Operator alias for 'replaceExtension'.
(-<.>) :: FilePath -> Text -> FilePath
(-<.>) = replaceExtension
infixr 7 -<.>

-- | Split into @(path-without-final-ext, final-ext-with-dot)@. The extension
-- is taken from the LAST @.@ in the whole path, not just the final
-- component — if what follows that @.@ crosses a @\/@, it doesn't count as
-- an extension. A name that begins with a @.@ (a hidden file, e.g.
-- @\".bashrc\"@) has no non-dot text before its first separator, so it
-- splits as an empty base name and an all-extension — surprising, but
-- upstream's own canonical behavior; a fluent-Haskell caller expects it.
--
-- >>> splitExtension "file.txt" == ("file", ".txt")
-- >>> splitExtension "file.txt/boris" == ("file.txt/boris", "")
-- >>> splitExtension ".bashrc" == ("", ".bashrc")
splitExtension :: FilePath -> (FilePath, Text)
splitExtension p
  | T.null nameDot     = (p, "")
  | T.any (== '/') ext = (p, "")
  | otherwise          = (T.init nameDot, T.cons '.' ext)
  where (nameDot, ext) = T.breakOnEnd "." p

-- | Does the path have an extension?
--
-- >>> hasExtension ".bashrc" == True
hasExtension :: FilePath -> Bool
hasExtension = not . T.null . takeExtension

-- | @ext \`isExtensionOf\` path@ — does @path@ end in extension @ext@
-- (with or without a leading dot on @ext@)?
--
-- >>> "txt" `isExtensionOf` "notes.txt"  == True
-- >>> ".md" `isExtensionOf` "notes.txt"  == False
isExtensionOf :: Text -> FilePath -> Bool
isExtensionOf ext = T.isSuffixOf dotted . takeExtensions
  where dotted = if T.isPrefixOf "." ext then ext else T.cons '.' ext
