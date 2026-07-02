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

/// Console effect: print text output.
pub fn console_decl() -> EffectDecl {
    EffectDecl {
        type_name: "Console",
        description: "Print text output.",
        constructors: &["Print :: Text -> Console ()"],
        type_defs: &[],
        helpers: &[
            "-- | Emit a line of console output. Thin wrapper over the Print effect\n-- so chains never need `send (Print …)`.\nsay :: Text -> M ()\nsay = send . Print",
            "-- | `say` on anything Showable (`say . show`).\nsayShow :: Show a => a -> M ()\nsayShow = say . show",
        ],
    }
}

/// Key-value store effect.
pub fn kv_decl() -> EffectDecl {
    EffectDecl {
        type_name: "KV",
        description:
            "Persistent key-value store. State survives across calls within one server session.",
        constructors: &[
            "KvGet :: Text -> KV (Maybe Value)",
            "KvSet :: Text -> Value -> KV ()",
            "KvDelete :: Text -> KV ()",
            "KvKeys :: KV [Text]",
        ],
        type_defs: &[],
        helpers: &[
            "kvGet :: Text -> M (Maybe Value)\nkvGet = send . KvGet",
            "kvSet :: Text -> Value -> M ()\nkvSet k v = send (KvSet k v)",
            "kvDel :: Text -> M ()\nkvDel = send . KvDelete",
            "kvKeys :: M [Text]\nkvKeys = send KvKeys",
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
            "-- | Regex-search files matching a path glob. ARG ORDER: regex FIRST, glob\n-- SECOND — a path glob like \"*.rs\" goes in arg 2, not arg 1. Returns [Hit]\n-- {path, line, text} (same shape as sgFind's matchLocs, so it composes with\n-- hitsByFile/refs). NB regex metachars are double-escaped here (JSON x Haskell),\n-- so a literal dot needs four backslashes; the handler error shows the exact\n-- form if you get it wrong.\ngrepGlob :: Text -> FilePath -> M [Hit]\ngrepGlob pat g = map (\\(f, l, t) -> Hit f l t) <$> send (FsGrep pat g)",
            // --- Editing: exact str-replace (the common case; mirrors the Edit tool) ---
            "-- | Exact str-replace, EXACTLY-ONCE: applies, or errors with a precise\n-- reason (not-found / ambiguous). The trained Edit-tool shape: no news is\n-- good news. Pass enough surrounding text that `old` is unique. Use planUpdate\n-- to review the diff first; the full editing surface is in tidepool://edits.\nupdate :: FilePath -> Text -> Text -> M ()\nupdate path old new\n  | T.null old = error \"update: 'old' must be non-empty\"\n  | otherwise = do\n      src <- readFile path\n      case len (T.splitOn old src) - 1 of\n        0 -> error (\"update: 'old' not found in \" <> path)\n        1 -> writeFile path (replace old new src)\n        n -> error (\"update: 'old' matches \" <> show n <> \" places in \" <> path <> \" (add surrounding context to disambiguate)\")",
            "-- | Replace EVERY occurrence of `old`; returns the count. Errors if zero.\nupdateAll :: FilePath -> Text -> Text -> M Int\nupdateAll path old new\n  | T.null old = error \"updateAll: 'old' must be non-empty\"\n  | otherwise = do\n      src <- readFile path\n      let n = len (T.splitOn old src) - 1\n      if n == 0 then error (\"updateAll: 'old' not found in \" <> path)\n                else writeFile path (replace old new src) >> pure n",
            "-- | Dry-run `update`: returns an `UpdateOutcome` (the review diff, or the\n-- reason it can't apply), writes NOTHING. Never errors — the conflict comes\n-- back as data so you can branch before committing.\nplanUpdate :: FilePath -> Text -> Text -> M UpdateOutcome\nplanUpdate path old new = do\n  er <- tryReadFile path\n  case er of\n    Left e -> pure (UpdateRejected (\"file not found: \" <> e) Nothing)\n    Right src ->\n      let n = if T.null old then 0 else len (T.splitOn old src) - 1\n      in if T.null old then pure (UpdateRejected \"'old' must be non-empty\" Nothing)\n         else if n == 0 then pure (UpdateRejected \"not found\" Nothing)\n         else if n > 1 then pure (UpdateRejected \"ambiguous\" (Just n))\n         else case Patch.genPatch path src (replace old new src) of\n                Left _ -> pure UpdateNoChange\n                Right fp -> pure (UpdateDiff (Patch.renderPatch [fp]))",
            "-- | `update` from the input lane: {file, old, new} (for big/quote-heavy fragments).\nupdateJ :: Value -> M ()\nupdateJ v = case (v ^? key \"file\" . _String, v ^? key \"old\" . _String, v ^? key \"new\" . _String) of\n  (Just f, Just o, Just n) -> update f o n\n  _ -> error \"updateJ: need {file, old, new} strings in input\"",
            "-- | Insert a block after the unique line containing `anchor`. Errors on 0 or 2+.\ninsertAfter :: FilePath -> Text -> Text -> M ()\ninsertAfter path anchor block = do\n  src <- readFile path\n  let ls = lines src\n  case len (filter (isInfixOf anchor) ls) of\n    1 -> writeFile path (unlines (concatMap (\\l -> if anchor `isInfixOf` l then [l, block] else [l]) ls))\n    n -> error (\"insertAfter: anchor matched \" <> show n <> \" lines in \" <> path)",
            "-- | Compute-check-commit: write only if every named check holds; failures\n-- come back as a `WriteOutcome` (nothing written on failure).\nwriteChecked :: FilePath -> [(Text, Bool)] -> Text -> M WriteOutcome\nwriteChecked path checks content = do\n  let failed = [name | (name, ok) <- checks, not ok]\n  if null failed\n    then writeFile path content >> pure (Written path (length checks))\n    else pure (WriteBlocked path failed)",
        ],
    }
}

/// Structural grep (ast-grep) effect.
pub fn sg_decl() -> EffectDecl {
    EffectDecl {
        type_name: "SG",
        description: concat!(
            "Structural code search via ast-grep. ",
            "Use patterns with $VAR for single-node captures and $$$VAR for multi-node. ",
            "Pass the language as a Lang DATA CONSTRUCTOR (e.g. `Rust`, `Python`), not a string. ",
            "Supported: Rust | Python | TypeScript | JavaScript | Go | Java | C | Cpp | Haskell | Nix | Html | Css | Json | Yaml | Toml. ",
            "Example: hsDef \"filter\" [\"haskell/lib\"] returns the definition of filter with file/line. ",
            "Use grepGlob for structured text-level search with regex and filename globbing.",
        ),
        type_defs: &[
            "data Lang = Rust | Python | TypeScript | JavaScript | Go | Java | C | Cpp | Haskell | Nix | Html | Css | Json | Yaml | Toml",
            // matchVarsList is the wire field the bridge fills (a list of pairs);
            // matchVars is the Map-typed accessor models actually reach for
            // (`Map.lookup \"NAME\" (matchVars m)`). Map.fromList reuses Data.Map's
            // own balancing in the JIT — no Rust-side tree build needed.
            "data Match = Match { matchText :: Text, matchFile :: Text, matchLine :: Int, matchVarsList :: [(Text, Text)], matchReplacement :: Text }",
            "matchVars :: Match -> Map Text Text",
            "matchVars = Map.fromList . matchVarsList",
            "instance ToJSON Match where\n  toJSON m@(Match t f l _ r) = object ([\"text\" .= t, \"file\" .= f, \"line\" .= l] ++ (let vs = matchVars m in if Map.null vs then [] else [\"vars\" .= toJSON vs]) ++ (if T.null r then [] else [\"replacement\" .= r]))",
            "var :: Match -> Text -> Text",
            "var m k = Map.findWithDefault \"\" k (matchVars m)",
            // Compact projectors: a survey shape that mirrors grepGlob's [Hit]
            // so it composes with hitsByFile/refs and does NOT flood context with
            // full match bodies. The [Match] stays live in the same eval, so
            // drilling into a chosen match is free.
            "-- | Compact location of one match as a `Hit` {path, line, text}\n-- (text = first line of the match).\nmatchLoc :: Match -> Hit\nmatchLoc m = Hit (matchFile m) (matchLine m) (T.takeWhile (/= '\\n') (matchText m))",
            "-- | Compact survey of matches: [Hit] — same shape as grepGlob. Browse with\n-- this, then index back into the [Match] for full detail.\nmatchLocs :: [Match] -> [Hit]\nmatchLocs = map matchLoc",
        ],
        constructors: &[
            "SgFind    :: Lang -> Text -> [Text] -> SG [Match]",
            "SgRuleFind    :: Lang -> Value -> [Text] -> SG [Match]",
            "SgPlan    :: Lang -> Text -> Text -> [Text] -> SG [Match]",
            "SgApply    :: Lang -> Text -> Text -> [Text] -> SG Int",
        ],
        helpers: &[
            "sgFind :: Lang -> Text -> [Text] -> M [Match]\nsgFind l p fs = send (SgFind l p fs)",
            "-- | Dry-run structural rewrite: matches with replacements, NO writes.\nplanRw :: Lang -> Text -> Text -> [Text] -> M [Match]\nplanRw l p r fs = send (SgPlan l p r fs)",
            "-- | Apply a structural rewrite in place. Run planRw first to preview the matches.\napplyRw :: Lang -> Text -> Text -> [Text] -> M Int\napplyRw l p r fs = send (SgApply l p r fs)",
            "sgRuleFind :: Lang -> Value -> [Text] -> M [Match]\nsgRuleFind l r fs = send (SgRuleFind l r fs)",
            "rPat :: Text -> Value\nrPat p = object [\"pattern\" .= p]",
            "rKind :: Text -> Value\nrKind k = object [\"kind\" .= k]",
            "rRegex :: Text -> Value\nrRegex r = object [\"regex\" .= r]",
            "rHas :: Value -> Value\nrHas r = object [\"has\" .= (r .+. object [\"stopBy\" .= (\"end\" :: Text)])]",
            "rHasChild :: Value -> Value\nrHasChild r = object [\"has\" .= r]",
            "rInside :: Value -> Value\nrInside r = object [\"inside\" .= (r .+. object [\"stopBy\" .= (\"end\" :: Text)])]",
            "rInsideParent :: Value -> Value\nrInsideParent r = object [\"inside\" .= r]",
            "rFollows :: Value -> Value\nrFollows r = object [\"follows\" .= r]",
            "rPrecedes :: Value -> Value\nrPrecedes r = object [\"precedes\" .= r]",
            "rAll :: [Value] -> Value\nrAll rs = object [\"all\" .= rs]",
            "rAny :: [Value] -> Value\nrAny rs = object [\"any\" .= rs]",
            "rNot :: Value -> Value\nrNot r = object [\"not\" .= r]",
            // Object merge (primary combinator) — left-biased key union
            "infixr 6 .+.\n(.+.) :: Value -> Value -> Value\n(.+.) (Object a) (Object b) = Object (KM.unionWith const a b)\n(.+.) a _ = a",
            // Conjunction / Disjunction
            "infixr 5 .&.\n(.&.) :: Value -> Value -> Value\na .&. b = object [\"all\" .= [a, b]]",
            "infixr 4 .|.\n(.|.) :: Value -> Value -> Value\na .|. b = object [\"any\" .= [a, b]]",
            // Relational operators
            "infixl 7 ?>\n(?>) :: Value -> Value -> Value\nparent ?> child = parent .+. rHas child",
            "infixl 7 <?\n(<?) :: Value -> Value -> Value\nchild <? ancestor = child .+. rInside ancestor",
            // Extra field helpers
            "rField :: Text -> Value\nrField name = object [\"field\" .= name]",
            // Recipes
            "-- | Find a Haskell function definition by name.\nhsDef :: Text -> [Text] -> M [Match]\nhsDef name paths = sgRuleFind Haskell (rAll [rKind \"function\", rHas (rField \"name\" .+. rRegex (\"^\" <> name <> \"$\"))]) paths",
            "-- | Find a Haskell function signature by name.\nhsSig :: Text -> [Text] -> M [Match]\nhsSig name paths = sgRuleFind Haskell (rAll [rKind \"signature\", rHas (rField \"name\" .+. rRegex (\"^\" <> name <> \"$\"))]) paths",
            "-- | Find a Rust function definition by name.\nrsFn :: Text -> [Text] -> M [Match]\nrsFn name paths = sgRuleFind Rust (rAll [rKind \"function_item\", rHas (rField \"name\" .+. rRegex (\"^\" <> name <> \"$\"))]) paths",
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

/// Http effect: JSON I/O — fetch from HTTP endpoints, or parse a JSON string.
pub fn http_decl() -> EffectDecl {
    EffectDecl {
        type_name: "Http",
        description: "JSON I/O. Fetch JSON from HTTP endpoints (returns Value), or \
                      parse a JSON Text into a Value with `parseJson`/`tryParseJson` \
                      (spec-compliant, parsed Rust-side via serde_json).",
        constructors: &[
            "HttpGet :: Text -> Http Value",
            "HttpPost :: Text -> Value -> Http Value",
            // Failure-isolating variants: a network error or non-2xx status
            // becomes `Left err` instead of killing the eval.
            "TryHttpGet :: Text -> Http (Either Text Value)",
            "TryHttpPost :: Text -> Value -> Http (Either Text Value)",
            // Parse a JSON string Rust-side (serde_json) into a Value. ParseJson
            // raises on invalid JSON; TryParseJson returns Left.
            "ParseJson :: Text -> Http Value",
            "TryParseJson :: Text -> Http (Either Text Value)",
        ],
        type_defs: &[],
        helpers: &[
            "httpGet :: Text -> M Value\nhttpGet = send . HttpGet",
            "httpPost :: Text -> Value -> M Value\nhttpPost url body = send (HttpPost url body)",
            // Isolating variants: a 404/network failure becomes `Left err`
            // (carrying the URL + cause) instead of aborting the eval.
            "tryHttpGet :: Text -> M (Either Text Value)\ntryHttpGet = send . TryHttpGet",
            "tryHttpPost :: Text -> Value -> M (Either Text Value)\ntryHttpPost url body = send (TryHttpPost url body)",
            // Parse JSON Text into ANY FromJSON type: the result type drives the
            // decode (`FromJSON Value` is identity, so `parseJson t :: M Value`
            // gives the raw value; `:: M Cfg` decodes a record). Raises on a parse
            // OR decode failure.
            "parseJson :: FromJSON a => Text -> M a\nparseJson t = send (ParseJson t) >>= \\v -> case fromJSON v of { Success a -> pure a; Error e -> error (T.pack e) }",
            // Failure-isolating: a parse OR decode error becomes `Left err`.
            "tryParseJson :: FromJSON a => Text -> M (Either Text a)\ntryParseJson t = send (TryParseJson t) >>= \\r -> pure (r >>= resultToEither . fromJSON)",
        ],
    }
}

/// Exec effect: run shell commands.
pub fn exec_decl() -> EffectDecl {
    EffectDecl {
        type_name: "Exec",
        description: "Run shell commands and capture output.",
        constructors: &[
            "Run :: Text -> Exec (Int, Text, Text)",
            "RunIn :: Text -> Text -> Exec (Int, Text, Text)",
            // Failure-isolating spawn: Left only when the process cannot be
            // SPAWNED (sandbox/exec error). A command that runs and exits
            // nonzero is Right (code, out, err) — the eval inspects the code.
            "TryRun :: Text -> Exec (Either Text (Int, Text, Text))",
            "TryRunIn :: Text -> Text -> Exec (Either Text (Int, Text, Text))",
            // Shell-free exec: argv list, no sh -c. Safe with metachars ($1, globs).
            "RunArgv :: [Text] -> Exec (Int, Text, Text)",
        ],
        type_defs: &[],
        helpers: &[
            "callCommand :: Text -> M ()\ncallCommand cmd = do { p <- run cmd; when (not (ok p)) (error (\"command failed (\" <> show p.exitCode <> \"): \" <> p.stderr)) }",
            "readProcess :: Text -> M Text\nreadProcess cmd = do { p <- run cmd; if ok p then pure p.stdout else error (\"command failed (\" <> show p.exitCode <> \"): \" <> p.stderr) }",
            "-- | Run a shell command; returns a `Proc` record {exitCode, stdout, stderr}\n-- (use `ok p` for the zero-exit check).\nrun :: Text -> M Proc\nrun cmd = (\\(ec, o, e) -> Proc ec o e) <$> send (Run cmd)",
            "runIn :: Text -> Text -> M Proc\nrunIn dir cmd = (\\(ec, o, e) -> Proc ec o e) <$> send (RunIn dir cmd)",
            // Isolating variants: spawn failure becomes `Left err` instead of
            // aborting the eval. A nonzero exit is NOT a failure here — it
            // arrives as `Right (code, out, err)`, so the common eval-killer
            // (readProcess on nonzero exit) is avoided by inspecting the code.
            "tryRun :: Text -> M (Either Text Proc)\ntryRun cmd = send (TryRun cmd) <&> fmap (\\(ec, o, e) -> Proc ec o e)",
            "tryRunIn :: Text -> Text -> M (Either Text Proc)\ntryRunIn dir cmd = send (TryRunIn dir cmd) <&> fmap (\\(ec, o, e) -> Proc ec o e)",
            // Shell-free: argv list, no sh -c. $1/$VAR/globs are literal — safe.
            "runArgv :: [Text] -> M Proc\nrunArgv argv = (\\(ec, o, e) -> Proc ec o e) <$> send (RunArgv argv)",
        ],
    }
}

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

/// Meta effect: self-mirror for querying runtime metadata.
pub fn meta_decl() -> EffectDecl {
    EffectDecl {
        type_name: "Meta",
        description:
            "Self-mirror for the runtime. Query constructors, primops, effects, diagnostics.",
        constructors: &[
            "MetaConstructors :: Meta [(Text, Int)]",
            "MetaLookupCon    :: Text -> Meta (Maybe (Int, Int))",
            "MetaPrimOps      :: Meta [Text]",
            "MetaEffects      :: Meta [Text]",
            "MetaDiagnostics  :: Meta [Text]",
            "MetaVersion      :: Meta Text",
            "MetaHelp         :: Meta [Text]",
        ],
        type_defs: &[],
        helpers: &[
            "metaConstructors :: M [(Text, Int)]\nmetaConstructors = send MetaConstructors",
            "metaLookupCon :: Text -> M (Maybe (Int, Int))\nmetaLookupCon = send . MetaLookupCon",
            "metaPrimOps :: M [Text]\nmetaPrimOps = send MetaPrimOps",
            "metaEffects :: M [Text]\nmetaEffects = send MetaEffects",
            "metaDiagnostics :: M [Text]\nmetaDiagnostics = send MetaDiagnostics",
            "metaVersion :: M Text\nmetaVersion = send MetaVersion",
            "metaHelp :: M [Text]\nmetaHelp = send MetaHelp",
        ],
    }
}

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
