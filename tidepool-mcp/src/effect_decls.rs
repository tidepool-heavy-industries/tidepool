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

/// LSP effect: a node-addressed semantic code graph via the `tidepool-lsp-daemon`.
///
/// `LspNode` is the composition currency — both the output of one op and the input
/// of the next, so navigation chains without destructuring. `lspWhere name`
/// seeds from a name; every other op takes a `LspNode` and the daemon re-resolves
/// it by position (so there is no name ambiguity). Graph edges
/// (`lspCallers`/`lspCallees`/`lspDef`) return `LspNode`s, so you fold them with
/// `concatMapM`/`loopM`. All LSP detail (positions, UTF-16, `WorkspaceEdit`,
/// call hierarchy) lives in the daemon. The `Lsp` lib module adds the `steer`
/// cascade + ready-made explorers (`explore`/`the`/`saferRename`/`chart`).
pub fn lsp_decl() -> EffectDecl {
    EffectDecl {
        type_name: "Lsp",
        description: concat!(
            "Semantic code-graph navigation via a language server (rust-analyzer, .rs). ",
            "Everything is a LspNode {name, container, kind, file, line, text} — the currency you thread. ",
            "`lspWhere name` → all definitions of NAME (the seed). Then walk the graph: ",
            "`lspCallers n` / `lspCallees n` (incoming/outgoing calls), `lspRefs n` (use sites), ",
            "`lspDef n` (any node → its definition), `lspHover n` (type/sig/docs), ",
            "`lspRename n new` (→ unified diff; review then `applyDiff`). Each returns LspNodes you feed ",
            "back in (e.g. `lspWhere \"x\" >>= concatMapM lspCallers`). `lspDiags file` for a file's errors. ",
            "Needs the `tidepool-lsp-daemon` running in the workspace; queries error cleanly if not.",
        ),
        type_defs: &[
            "data Position = Position { posLine :: Int, posChar :: Int }",
            "data LspNode = LspNode { nodeName :: Text, nodeContainer :: Text, nodeKind :: Text, nodeFile :: Text, nodePos :: Position, nodeText :: Text }",
            "data Diag = Diag { diagFile :: Text, diagLine :: Int, diagSeverity :: Text, diagMessage :: Text }",
            // nodeLine: the human-facing 1-based line, derived from the exact pos.
            "nodeLine :: LspNode -> Int\nnodeLine = posLine . nodePos",
            "instance ToJSON Position where\n  toJSON (Position l c) = object [\"line\" .= l, \"char\" .= c]",
            "instance ToJSON LspNode where\n  toJSON nd@(LspNode n c k f _ t) = object [\"name\" .= n, \"container\" .= c, \"kind\" .= k, \"file\" .= f, \"line\" .= nodeLine nd, \"text\" .= t]",
            "instance ToJSON Diag where\n  toJSON (Diag f l s m) = object [\"file\" .= f, \"line\" .= l, \"severity\" .= s, \"message\" .= m]",
        ],
        constructors: &[
            "LspWhere       :: Text -> Lsp [LspNode]",
            "LspCallers     :: LspNode -> Lsp (Maybe [LspNode])",
            "LspCallees     :: LspNode -> Lsp (Maybe [LspNode])",
            "LspRefs        :: LspNode -> Lsp (Maybe [LspNode])",
            "LspDef         :: LspNode -> Lsp (Maybe LspNode)",
            "LspHover       :: LspNode -> Lsp (Maybe Text)",
            "LspRename      :: LspNode -> Text -> Lsp (Maybe Text)",
            "LspDiagnostics :: Text -> Lsp [Diag]",
        ],
        helpers: &[
            "-- | Seed: every workspace definition named X (each a LspNode with container/file/line/source line).\nlspWhere :: Text -> M [LspNode]\nlspWhere = send . LspWhere",
            "-- | Incoming calls. Nothing = node not callable; Just [] = callable, none. Unwrap with callersOf for plain chaining.\nlspCallers :: LspNode -> M (Maybe [LspNode])\nlspCallers = send . LspCallers",
            "-- | Outgoing calls. Nothing = node not callable; Just [] = callable, none.\nlspCallees :: LspNode -> M (Maybe [LspNode])\nlspCallees = send . LspCallees",
            "-- | Use sites of this node's symbol (kind = \"reference\"). Nothing = not a symbol.\nlspRefs :: LspNode -> M (Maybe [LspNode])\nlspRefs = send . LspRefs",
            "-- | Resolve any node (e.g. a use site) to its definition node.\nlspDef :: LspNode -> M (Maybe LspNode)\nlspDef = send . LspDef",
            "-- | Type / signature / docs for a node.\nlspHover :: LspNode -> M (Maybe Text)\nlspHover = send . LspHover",
            "-- | Rename a node's symbol to NEW; returns a unified diff (apply with applyDiff). Nothing = can't rename.\nlspRename :: LspNode -> Text -> M (Maybe Text)\nlspRename n new = send (LspRename n new)",
            "-- | Diagnostics (errors / warnings) for FILE.\nlspDiags :: FilePath -> M [Diag]\nlspDiags = send . LspDiagnostics",
        ],
    }
}

// Http effect: `http_decl()` is generated from the single-source definition
// (`effect_defs.rs`).
crate::http_effect_def!(crate::effect_defs::effect_decl_projection);

// Exec effect: `exec_decl()` is generated from the single-source definition
// (`effect_defs.rs`).
crate::exec_effect_def!(crate::effect_defs::effect_decl_projection);

/// Git effect: typed read-only repository queries.
///
/// Returns typed records parsed Rust-side from machine-format git output.
/// `Commit`, `StatusEntry`, and `FileDelta` come from `Tidepool.Records` and
/// are available via `Tidepool.Prelude` without an explicit import.
pub fn git_decl() -> EffectDecl {
    EffectDecl {
        type_name: "Git",
        description: concat!(
            "Read-only git repository queries. Returns typed records parsed Rust-side ",
            "from machine-format git output — no text-splitting needed. ",
            "`gitLog n` → last N commits newest-first; `gitStatus` → working-tree status; ",
            "`gitDiffStat rev` → per-file diff stats vs a revspec; `gitShow rev` → one commit. ",
            "All three list verbs return typed records: `Commit {sha,subject,author,date,files}`, ",
            "`StatusEntry {path,state}` (state = 2-char XY porcelain code), ",
            "`FileDelta {path,adds,dels,binary}`.",
        ),
        // Commit/StatusEntry/FileDelta are defined in Tidepool.Records and
        // re-exported by Tidepool.Prelude, so the generated Effects module
        // (which imports Tidepool.Prelude) sees them without type_defs here.
        type_defs: &[],
        constructors: &[
            "GitLog      :: Int  -> Git [Commit]",
            "GitStatus   ::         Git [StatusEntry]",
            "GitDiffStat :: Text -> Git [FileDelta]",
            "GitShow     :: Text -> Git Commit",
        ],
        helpers: &[
            "-- | Last N commits, newest-first. Each 'Commit' carries sha/subject/author/date/files.\ngitLog :: Int -> M [Commit]\ngitLog = send . GitLog",
            "-- | Working-tree status. Each 'StatusEntry' has path and 2-char XY state code\n-- (e.g. \"M \", \"??\", \"A \").\ngitStatus :: M [StatusEntry]\ngitStatus = send GitStatus",
            "-- | Per-file diff stats vs a revspec (\"HEAD~1\", \"main\", \"HEAD~3..HEAD\", etc.).\n-- 'FileDelta' carries path/adds/dels/binary.\ngitDiffStat :: Text -> M [FileDelta]\ngitDiffStat = send . GitDiffStat",
            "-- | Single commit by revspec. Fails the eval on an unknown or ambiguous revspec.\ngitShow :: Text -> M Commit\ngitShow = send . GitShow",
        ],
    }
}

// Time effect: `time_decl()` is generated from the single-source definition
// (`effect_defs.rs`).
crate::time_effect_def!(crate::effect_defs::effect_decl_projection);

// Meta effect: `meta_decl()` is generated from the single-source definition
// (`effect_defs.rs`).
crate::meta_effect_def!(crate::effect_defs::effect_decl_projection);

/// Ask effect: suspend execution to ask the calling LLM a question.
pub fn ask_decl() -> EffectDecl {
    EffectDecl {
        type_name: "Ask",
        description: "Suspend execution and ask the calling agent a STRUCTURED question. `ask schema prompt` carries the schema as JSON Schema in the suspension; the resume reply is validated against it server-side before re-entering the computation (invalid replies do NOT consume the continuation). Extract fields from the returned Value with optics, e.g. `v ^? key \"path\" . _String`.",
        constructors: &[
            "AskWith :: Text -> Value -> Ask Value",
        ],
        type_defs: &[
            // Schema vocabulary lives on the Ask effect (always present in
            // every stack) so .tidepool/lib modules and Llm-less stacks can
            // build schemas. llm (llm_decl) references schemaToValue from
            // here — same generated module.
            "data Schema = SObj [(Text, Schema)] | SArr Schema | SStr | SNum | SBool | SEnum [Text] | SOpt Schema",
        ],
        helpers: &[
            "ask :: Schema -> Text -> M Value\nask schema prompt = send (AskWith prompt (object [\"schema\" .= schemaToValue schema]))",
            "isOpt :: Schema -> Bool\nisOpt (SOpt _) = True\nisOpt _ = False",
            "innerSchema :: Schema -> Schema\ninnerSchema (SOpt s) = s\ninnerSchema s = s",
            "schemaToValue :: Schema -> Value\nschemaToValue SStr = object [\"type\" .= (\"string\" :: Text)]\nschemaToValue SNum = object [\"type\" .= (\"number\" :: Text)]\nschemaToValue SBool = object [\"type\" .= (\"boolean\" :: Text)]\nschemaToValue (SEnum vs) = object [\"type\" .= (\"string\" :: Text), \"enum\" .= vs]\nschemaToValue (SArr item) = object [\"type\" .= (\"array\" :: Text), \"items\" .= schemaToValue item]\nschemaToValue (SOpt s) = schemaToValue s\nschemaToValue (SObj fields) = object [\"type\" .= (\"object\" :: Text), \"properties\" .= object (map (\\(k,s) -> k .= schemaToValue (innerSchema s)) fields), \"required\" .= map fst (filter (not . isOpt . snd) fields)]",
        ],
    }
}

/// LLM effect: call an LLM for classification, extraction, or judgment.
pub fn llm_decl() -> EffectDecl {
    EffectDecl {
        type_name: "Llm",
        description: "Call an LLM for classification, extraction, or judgment. `llm schema prompt` returns a Value validated against the schema (structured output, no markdown fences). Extract with optics, e.g. `v ^? key \"category\" . _String`.",
        constructors: &[
            "LlmStructured :: Text -> Value -> Llm Value",
            // Failure-isolating variant: an API/network error or refusal
            // becomes `Left err` instead of killing the eval. (Budget
            // exhaustion still aborts — that's a hard control limit.)
            "TryLlmStructured :: Text -> Value -> Llm (Either Text Value)",
        ],
        type_defs: &[],
        helpers: &[
            // schemaToValue lives in ask_decl (Ask is always present).
            "llm :: Schema -> Text -> M Value\nllm schema prompt = send (LlmStructured prompt (schemaToValue schema))",
            // Isolating variant: an API failure/refusal becomes `Left err`
            // instead of aborting the eval (the LLM call-budget limit still
            // aborts — it is a hard control limit, not a probe failure).
            "tryLlm :: Schema -> Text -> M (Either Text Value)\ntryLlm schema prompt = send (TryLlmStructured prompt (schemaToValue schema))",
            // Pure tally utilities (no LLM/Ask): build a frequency list while
            // preserving first-seen order. Kept for .tidepool/lib verbs.
            "findTally :: Eq a => a -> [(a, Int)] -> Maybe [(a, Int)]\nfindTally _ [] = Nothing\nfindTally x ((k, n):rest) = if x == k then Just ((k, n + 1) : rest) else case findTally x rest of { Just rest' -> Just ((k, n) : rest'); Nothing -> Nothing }",
            "tallyList :: Eq a => [a] -> [(a, Int)]\ntallyList = foldl' (\\acc x -> case findTally x acc of { Just acc' -> acc'; Nothing -> acc ++ [(x, 1)] }) []",
        ],
    }
}
