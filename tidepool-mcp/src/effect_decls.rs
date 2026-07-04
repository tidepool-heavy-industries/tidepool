//! Effect-declaration layer for the Tidepool MCP server.
//!
//! Defines [`EffectDecl`] (static Haskell-side metadata for an effect type),
//! the [`DescribeEffect`] / [`CollectEffectDecls`] traits used to gather
//! declarations from an HList of handlers, and the nine standard `*_decl()`
//! builders. These mostly assemble Haskell-source strings consumed by the
//! preamble/tool-description assembly.

// ---------------------------------------------------------------------------
// Effect metadata — lives next to the handler, discovered via trait
// ---------------------------------------------------------------------------

/// Static metadata describing a Haskell effect type.
///
/// Each effect handler that wants to participate in the MCP templating system
/// implements `DescribeEffect` to provide its Haskell-side type declaration.
#[derive(Debug, Clone, Copy)]
pub struct EffectDecl {
    /// Haskell GADT type name, e.g. `"Console"`.
    pub type_name: &'static str,
    /// Human-readable description of what this effect does.
    pub description: &'static str,
    /// Haskell GADT constructor declarations (one per line inside `data T a where`).
    pub constructors: &'static [&'static str],
    /// Extra Haskell type/function definitions emitted before the GADT.
    /// Use for supporting types (e.g. `data Lang = ...`) and helper functions.
    pub type_defs: &'static [&'static str],
    /// Thin curried helper definitions emitted after the `type M` alias.
    /// Each string is one or more lines of Haskell (signature + definition).
    pub helpers: &'static [&'static str],
}

/// Parsed constructor info extracted from an EffectDecl constructor string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedConstructor {
    pub name: String,
    pub arity: u32,
}

/// Parse `"GitLog :: Text -> Int -> Git [Value]"` → `ParsedConstructor { name: "GitLog", arity: 2 }`
///
/// Arity = number of `->` in the type signature (each `->` separates one argument from the rest).
pub fn parse_constructor(decl: &str) -> Result<ParsedConstructor, String> {
    let (name_part, type_part) = decl
        .split_once("::")
        .ok_or_else(|| format!("constructor decl must contain '::': {:?}", decl))?;
    let name = name_part.trim().to_string();
    let arity = type_part.matches("->").count() as u32;
    Ok(ParsedConstructor { name, arity })
}

/// Trait for effect handlers that can describe their Haskell-side type.
pub trait DescribeEffect {
    fn effect_decl() -> EffectDecl;
}

/// Trait for collecting effect declarations from an HList of handlers.
pub trait CollectEffectDecls {
    fn collect_decls() -> Vec<EffectDecl>;
}

impl CollectEffectDecls for frunk::HNil {
    fn collect_decls() -> Vec<EffectDecl> {
        Vec::new()
    }
}

impl<H, T> CollectEffectDecls for frunk::HCons<H, T>
where
    H: DescribeEffect,
    T: CollectEffectDecls,
{
    fn collect_decls() -> Vec<EffectDecl> {
        let mut decls = vec![H::effect_decl()];
        decls.extend(T::collect_decls());
        decls
    }
}

// ---------------------------------------------------------------------------
// Standard effect declarations
// ---------------------------------------------------------------------------

// Console effect: `console_decl()` is generated from the single-source
// definition in `effect_defs.rs` (the T6 spike prototype) — constructors,
// description, and helper docstrings all live THERE, alongside the facts the
// Rust half (`ConsoleReq`, dispatch) projects from the same table.
crate::console_effect_def!(crate::effect_defs::effect_decl_projection);

/// Key-value store effect.
///
/// ## Namespacing convention
///
/// Keys are plain `Text`; there is no automatic per-session scoping (deferred
/// design decision, tracked in issue #327). To avoid cross-agent collision use a
/// slash-delimited prefix: `"agent-42/foo"`, `"session-abc/bar"`. The `kvClear`
/// and `kvKeysP` verbs operate on prefix boundaries, so a namespace is a usable
/// first-class scope without any server-side change.
pub fn kv_decl() -> EffectDecl {
    EffectDecl {
        type_name: "KV",
        description:
            "Persistent key-value store. State survives across calls within one server session. \
             Key convention: use slash-delimited namespaces (e.g. \"agent-42/foo\") to avoid \
             cross-agent collision. kvClear/kvKeysP operate on prefix boundaries.",
        constructors: &[
            "KvGet :: Text -> KV (Maybe Value)",
            "KvSet :: Text -> Value -> KV ()",
            "KvDelete :: Text -> KV ()",
            "KvKeys :: KV [Text]",
            // Delete all keys with the given prefix; return count deleted.
            // Pass \"\" to clear the ENTIRE store (dangerous — see kvClear docstring).
            "KvClear :: Text -> KV Int",
            // List keys matching a prefix, sorted.
            "KvKeysP :: Text -> KV [Text]",
            // Summary: {count, sample, file_size_bytes} — inspect the junk-drawer.
            "KvInfo :: KV Value",
        ],
        type_defs: &[],
        helpers: &[
            "kvGet :: Text -> M (Maybe Value)\nkvGet = send . KvGet",
            "kvSet :: Text -> Value -> M ()\nkvSet k v = send (KvSet k v)",
            "kvDel :: Text -> M ()\nkvDel = send . KvDelete",
            "kvKeys :: M [Text]\nkvKeys = send KvKeys",
            "-- | Delete all keys whose name starts with @prefix@; return the count deleted.\n\
             -- Pass \\\"\\\" (empty string) to clear the ENTIRE store — this erases ALL\n\
             -- persisted KV data for this server session, so use with caution.\n\
             -- Recommended pattern: namespace keys as \\\"ns/key\\\" and clear with \\\"ns/\\\".\n\
             -- NOTE: per-session automatic scoping is a deferred design decision (#327);\n\
             -- callers manage namespaces manually via this prefix argument.\n\
             kvClear :: Text -> M Int\n\
             kvClear = send . KvClear",
            "-- | All keys whose name starts with @prefix@, returned sorted.\n\
             -- E.g. @kvKeysP \\\"agent/\\\"@ returns @[\\\"agent/bar\\\", \\\"agent/foo\\\", ...]@.\n\
             -- Pass \\\"\\\" to list ALL keys sorted (like kvKeys but deterministically ordered).\n\
             kvKeysP :: Text -> M [Text]\n\
             kvKeysP = send . KvKeysP",
            "-- | Summary of KV store state as a JSON Value:\n\
             -- @{count :: Int, sample :: [Text], file_size_bytes :: Int}@.\n\
             -- Use to inspect junk-drawer accumulation without listing all keys.\n\
             -- Extract fields with optics: @i <- kvInfo; i ^? key \\\"count\\\" . _Int@\n\
             kvInfo :: M Value\n\
             kvInfo = send KvInfo",
        ],
    }
}

/// File I/O effect (sandboxed).
pub fn fs_decl() -> EffectDecl {
    EffectDecl {
        type_name: "Fs",
        description: "Read and write files (sandboxed to server working directory).",
        constructors: &[
            "FsRead :: Text -> Fs Text",
            "FsWrite :: Text -> Text -> Fs ()",
            "FsListDir :: Text -> Fs [Text]",
            "FsGlob :: Text -> Fs [Text]",
            "FsGrep :: Text -> Text -> Fs [(Text, Int, Text)]",
            "FsExists :: Text -> Fs Bool",
            // Value-native: `{size, is_file, is_dir}` on success, `Null` for a
            // missing/unreadable path — lens in with `^? key \"size\" . _Int`.
            "FsMetadata :: Text -> Fs Value",
            // Failure-isolating read: Left on a read error (missing file,
            // permission, non-UTF-8) instead of killing the eval.
            "TryFsRead :: Text -> Fs (Either Text Text)",
            // Per-file failure-isolating glob read (#328): each match is
            // (path, Right content) or (path, Left err) — a mixed glob (text +
            // binary) survives, the binary just comes back as a Left.
            "FsReadGlob :: Text -> Fs [(Text, Either Text Text)]",
            // Content-hash compare-and-swap surface (#330). FsHash = current
            // blake3 digest (Nothing = absent); FsWriteCas writes only if the
            // current hash equals the expected one (Nothing = require absent),
            // else returns `Left actual` with the ACTUAL hash (Nothing = absent).
            "FsHash :: Text -> Fs (Maybe Text)",
            "FsWriteCas :: Text -> Maybe Text -> Text -> Fs (Either (Maybe Text) ())",
        ],
        type_defs: &[],
        helpers: &[
            "readFile :: FilePath -> M Text\nreadFile = send . FsRead",
            "-- | Read a file, isolating failure: `Left err` on a read error\n-- (missing file, permission, non-UTF-8) instead of aborting the eval.\ntryReadFile :: FilePath -> M (Either Text Text)\ntryReadFile = send . TryFsRead",
            "writeFile :: FilePath -> Text -> M ()\nwriteFile f c = send (FsWrite f c)",
            "appendFile :: FilePath -> Text -> M ()\nappendFile p t = readFile p >>= \\old -> writeFile p (old <> t)",
            "listDirectory :: FilePath -> M [FilePath]\nlistDirectory = send . FsListDir",
            "doesFileExist :: FilePath -> M Bool\ndoesFileExist = send . FsExists",
            "doesDirectoryExist :: FilePath -> M Bool\ndoesDirectoryExist p = send (FsMetadata p) <&> (== Just True) . (^? key \"is_dir\" . _Bool)",
            "-- | File size in bytes, or `Nothing` if the path is missing.\ngetFileSize :: FilePath -> M (Maybe Int)\ngetFileSize p = send (FsMetadata p) <&> (^? key \"size\" . _Int)",
            "-- | Parse the raw metadata Value into a `FileMeta`, or `Nothing` for a\n-- missing/unreadable path.\nparseFileMeta :: Value -> Maybe FileMeta\nparseFileMeta v = case (v ^? key \"size\" . _Int, v ^? key \"is_file\" . _Bool, v ^? key \"is_dir\" . _Bool) of\n  (Just s, Just f, Just d) -> Just (FileMeta s f d)\n  _ -> Nothing",
            "-- | File metadata as a `FileMeta` record {size, isFile, isDir}, or `Nothing`\n-- if the path is missing/unreadable (use record-dot: `m.size`, `m.isDir`).\nfsMeta :: FilePath -> M (Maybe FileMeta)\nfsMeta p = send (FsMetadata p) <&> parseFileMeta",
            "-- | Alias of `fsMeta` — metadata as a `Maybe FileMeta`.\nfsMetadata :: FilePath -> M (Maybe FileMeta)\nfsMetadata = fsMeta",
            "getCurrentDirectory :: M FilePath\ngetCurrentDirectory = do { p <- run \"pwd\"; pure (T.strip p.stdout) }",
            "glob :: FilePath -> M [FilePath]\nglob = send . FsGlob",
            "-- | Alias of `glob` — expand a glob to matching paths.\nfsGlob :: FilePath -> M [FilePath]\nfsGlob = send . FsGlob",
            "-- | Regex-search files matching a path glob. ARG ORDER: regex FIRST, glob\n-- SECOND — a path glob like \"*.rs\" goes in arg 2, not arg 1. Returns [Hit]\n-- {path, line, text} (the shared Hit shape, so it composes with\n-- hitsByFile/refs). NB regex metachars are double-escaped here (JSON x Haskell),\n-- so a literal dot needs four backslashes; the handler error shows the exact\n-- form if you get it wrong.\ngrepGlob :: Text -> FilePath -> M [Hit]\ngrepGlob pat g = map (\\(f, l, t) -> Hit f l t) <$> send (FsGrep pat g)",
            "-- | Read every file matching a glob with PER-FILE failure isolation: each\n-- result is (path, Right content) on a clean UTF-8 read, or (path, Left err) on\n-- a per-file failure (binary / non-UTF-8, permission). Unlike readGlob, one bad\n-- file (e.g. a binary swept up by a wide glob) does NOT fail the whole batch —\n-- the Left rides alongside the Rights (the #328 mixed-glob case). An empty glob\n-- is rejected loudly (\"\" matches everything). Split with partitionEithers on the\n-- snd, or `[(p,t) | (p, Right t) <- rs]` for just the readable ones.\ntryReadGlob :: Text -> M [(Text, Either Text Text)]\ntryReadGlob = send . FsReadGlob",
            // --- Editing: exact str-replace (the common case; mirrors the Edit tool) ---
            "-- | Exact str-replace, EXACTLY-ONCE: applies, or errors with a precise\n-- reason (not-found / ambiguous). The trained Edit-tool shape: no news is\n-- good news. Pass enough surrounding text that `old` is unique. Use planUpdate\n-- to review the diff first; the full editing surface is in tidepool://edits.\nupdate :: FilePath -> Text -> Text -> M ()\nupdate path old new\n  | T.null old = error \"update: 'old' must be non-empty\"\n  | otherwise = do\n      src <- readFile path\n      case len (T.splitOn old src) - 1 of\n        0 -> error (\"update: 'old' not found in \" <> path)\n        1 -> writeFile path (replace old new src)\n        n -> error (\"update: 'old' matches \" <> show n <> \" places in \" <> path <> \" (add surrounding context to disambiguate)\")",
            "-- | Replace EVERY occurrence of `old`; returns the count. Errors if zero.\nupdateAll :: FilePath -> Text -> Text -> M Int\nupdateAll path old new\n  | T.null old = error \"updateAll: 'old' must be non-empty\"\n  | otherwise = do\n      src <- readFile path\n      let n = len (T.splitOn old src) - 1\n      if n == 0 then error (\"updateAll: 'old' not found in \" <> path)\n                else writeFile path (replace old new src) >> pure n",
            "-- | Dry-run `update`: returns an `UpdateOutcome` (the review diff, or the\n-- reason it can't apply), writes NOTHING. Never errors — the conflict comes\n-- back as data so you can branch before committing.\nplanUpdate :: FilePath -> Text -> Text -> M UpdateOutcome\nplanUpdate path old new = do\n  er <- tryReadFile path\n  case er of\n    Left e -> pure (UpdateRejected (\"file not found: \" <> e) Nothing)\n    Right src ->\n      let n = if T.null old then 0 else len (T.splitOn old src) - 1\n      in if T.null old then pure (UpdateRejected \"'old' must be non-empty\" Nothing)\n         else if n == 0 then pure (UpdateRejected \"not found\" Nothing)\n         else if n > 1 then pure (UpdateRejected \"ambiguous\" (Just n))\n         else case Patch.genPatch path src (replace old new src) of\n                Left _ -> pure UpdateNoChange\n                Right fp -> pure (UpdateDiff (Patch.renderPatch [fp]))",
            "-- | `update` from the input lane: {file, old, new} (for big/quote-heavy fragments).\nupdateJ :: Value -> M ()\nupdateJ v = case (v ^? key \"file\" . _String, v ^? key \"old\" . _String, v ^? key \"new\" . _String) of\n  (Just f, Just o, Just n) -> update f o n\n  _ -> error \"updateJ: need {file, old, new} strings in input\"",
            "-- | Insert a block after the unique line containing `anchor`. Errors on 0 or 2+.\ninsertAfter :: FilePath -> Text -> Text -> M ()\ninsertAfter path anchor block = do\n  src <- readFile path\n  let ls = lines src\n  case len (filter (isInfixOf anchor) ls) of\n    1 -> writeFile path (unlines (concatMap (\\l -> if anchor `isInfixOf` l then [l, block] else [l]) ls))\n    n -> error (\"insertAfter: anchor matched \" <> show n <> \" lines in \" <> path)",
            "-- | Compute-check-commit: write only if every named check holds; failures\n-- come back as a `WriteOutcome` (nothing written on failure).\nwriteChecked :: FilePath -> [(Text, Bool)] -> Text -> M WriteOutcome\nwriteChecked path checks content = do\n  let failed = [name | (name, ok) <- checks, not ok]\n  if null failed\n    then writeFile path content >> pure (Written path (length checks))\n    else pure (WriteBlocked path failed)",
            "-- | Blake3 content hash (hex) of a file, or Nothing if it does not exist.\n-- The compare-and-swap token for writeCheckedIf: read it, compute your new\n-- content, then write back only if the file still hashes the same.\nfileHash :: FilePath -> M (Maybe Text)\nfileHash = send . FsHash",
            "-- | Content-hash compare-and-swap write (#330). Writes CONTENT only if the\n-- file's current blake3 hash equals EXPECTED (Nothing = expect the file ABSENT,\n-- i.e. create-only). The compare-and-write is atomic within the handler, closing\n-- the lost-update race between parallel agents. Returns a WriteOutcome: 'Written'\n-- on success, or 'WriteConflict' (carrying expected vs actual hash) if the\n-- precondition failed — conflicts come back as DATA, nothing is written. Get\n-- EXPECTED from fileHash; on a conflict re-read, recompute, and retry.\nwriteCheckedIf :: Maybe Text -> FilePath -> Text -> M WriteOutcome\nwriteCheckedIf expected path content = do\n  r <- send (FsWriteCas path expected content)\n  pure $ case r of\n    Right () -> Written path 1\n    Left actual -> WriteConflict path expected actual",
        ],
    }
}

// Lsp effect: `lsp_decl()` is generated from the single-source definition
// (`effect_defs.rs`).
crate::lsp_effect_def!(crate::effect_defs::effect_decl_projection);

// Http effect: `http_decl()` is generated from the single-source definition
// (`effect_defs.rs`).
crate::http_effect_def!(crate::effect_defs::effect_decl_projection);

// Exec effect: `exec_decl()` is generated from the single-source definition
// (`effect_defs.rs`).
crate::exec_effect_def!(crate::effect_defs::effect_decl_projection);

// Git effect: `git_decl()` is generated from the single-source definition
// (`effect_defs.rs`).
crate::git_effect_def!(crate::effect_defs::effect_decl_projection);

// Time effect: `time_decl()` is generated from the single-source definition
// (`effect_defs.rs`).
crate::time_effect_def!(crate::effect_defs::effect_decl_projection);

// Meta effect: `meta_decl()` is generated from the single-source definition
// (`effect_defs.rs`).
crate::meta_effect_def!(crate::effect_defs::effect_decl_projection);

// Ask effect: `ask_decl()` is generated from the single-source definition
// (`effect_defs.rs`).
crate::ask_effect_def!(crate::effect_defs::effect_decl_projection);

// Llm effect: `llm_decl()` is generated from the single-source definition
// (`effect_defs.rs`).
crate::llm_effect_def!(crate::effect_defs::effect_decl_projection);
