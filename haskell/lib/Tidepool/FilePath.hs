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

-- | Normalise a path: collapse @.@ and empty segments, resolve @..@ against
-- preceding components (leading @..@s are preserved), keep absolute-ness.
-- Mirrors the System.FilePath name (the kata link-checker reached for it and
-- found it missing — friction #26).
--
-- >>> normalise "a/./b/../c" == "a/c"
normalise :: FilePath -> FilePath
normalise p =
  let go acc seg
        | seg == "."  = acc
        | seg == ".." = case acc of
            (top : rest) | top /= ".." -> rest
            _                          -> ".." : acc
        | otherwise   = seg : acc
      parts = reverse (foldl go [] (filter (not . T.null) (T.splitOn "/" p)))
      body  = T.intercalate "/" parts
  in if isAbsolute p then "/" <> body else if T.null body then "." else body

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

-- | The filename without directory or extension.
--
-- >>> takeBaseName "a/b/c.tar.gz" == "c.tar"
takeBaseName :: FilePath -> FilePath
takeBaseName = dropExtension . takeFileName

-- | The final extension, including the leading @\'.\'@, or @\"\"@ if none.
-- A leading dot (hidden file) is not an extension.
--
-- >>> takeExtension "file.txt"  == ".txt"
-- >>> takeExtension "file"      == ""
-- >>> takeExtension ".bashrc"   == ""
takeExtension :: FilePath -> Text
takeExtension p = case T.breakOnEnd "." (takeFileName p) of
  (pre, post) | T.null pre || pre == "." -> ""
              | otherwise                -> T.cons '.' post

-- | All extensions, e.g. @\".tar.gz\"@.
takeExtensions :: FilePath -> Text
takeExtensions p =
  let fn = takeFileName p
      body = case T.uncons fn of { Just ('.', r) -> r; _ -> fn }   -- skip a hidden-file dot
  in snd (T.breakOn "." body)

-- | Drop the final extension.
--
-- >>> dropExtension "file.txt" == "file"
dropExtension :: FilePath -> FilePath
dropExtension p =
  let ext = takeExtension p
  in if T.null ext then p else T.dropEnd (T.length ext) p

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

-- | Split into @(path-without-final-ext, final-ext-with-dot)@.
splitExtension :: FilePath -> (FilePath, Text)
splitExtension p = (dropExtension p, takeExtension p)

-- | Does the path have an extension?
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
