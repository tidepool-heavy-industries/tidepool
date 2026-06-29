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
            "-- | Raw metadata as a Value: `{size, is_file, is_dir}`, or `Null` if missing.\nfsMeta :: FilePath -> M Value\nfsMeta = send . FsMetadata",
            "-- | Alias of `fsMeta` — metadata as a Value, lens in with `^? key \"size\" . _Int`.\nfsMetadata :: FilePath -> M Value\nfsMetadata = send . FsMetadata",
            "getCurrentDirectory :: M FilePath\ngetCurrentDirectory = do { (_, d, _) <- run \"pwd\"; pure (T.strip d) }",
            "glob :: FilePath -> M [FilePath]\nglob = send . FsGlob",
            "-- | Alias of `glob` — expand a glob to matching paths.\nfsGlob :: FilePath -> M [FilePath]\nfsGlob = send . FsGlob",
            "-- | Search for a regex pattern in files matching a glob.\ngrepGlob :: Text -> FilePath -> M [(FilePath, Int, Text)]\ngrepGlob pat g = send (FsGrep pat g)",
            // --- Editing: exact str-replace (the common case; mirrors the Edit tool) ---
            "-- | Exact str-replace, EXACTLY-ONCE: applies, or errors with a precise\n-- reason (not-found / ambiguous). The trained Edit-tool shape: no news is\n-- good news. Pass enough surrounding text that `old` is unique. Use planUpdate\n-- to review the diff first; the full editing surface is in tidepool://edits.\nupdate :: FilePath -> Text -> Text -> M ()\nupdate path old new\n  | T.null old = error \"update: 'old' must be non-empty\"\n  | otherwise = do\n      src <- readFile path\n      case len (T.splitOn old src) - 1 of\n        0 -> error (\"update: 'old' not found in \" <> path)\n        1 -> writeFile path (replace old new src)\n        n -> error (\"update: 'old' matches \" <> show n <> \" places in \" <> path <> \" (add surrounding context to disambiguate)\")",
            "-- | Replace EVERY occurrence of `old`; returns the count. Errors if zero.\nupdateAll :: FilePath -> Text -> Text -> M Int\nupdateAll path old new\n  | T.null old = error \"updateAll: 'old' must be non-empty\"\n  | otherwise = do\n      src <- readFile path\n      let n = len (T.splitOn old src) - 1\n      if n == 0 then error (\"updateAll: 'old' not found in \" <> path)\n                else writeFile path (replace old new src) >> pure n",
            "-- | Dry-run `update`: returns the review diff, writes NOTHING. Never errors —\n-- the conflict comes back as data so you can branch before committing.\nplanUpdate :: FilePath -> Text -> Text -> M Value\nplanUpdate path old new = do\n  er <- tryReadFile path\n  case er of\n    Left e -> pure (object [\"ok\" .= False, \"reason\" .= (\"file not found: \" <> e)])\n    Right src ->\n      let n = if T.null old then 0 else len (T.splitOn old src) - 1\n      in if T.null old then pure (object [\"ok\" .= False, \"reason\" .= (\"'old' must be non-empty\" :: Text)])\n         else if n == 0 then pure (object [\"ok\" .= False, \"reason\" .= (\"not found\" :: Text)])\n         else if n > 1 then pure (object [\"ok\" .= False, \"reason\" .= (\"ambiguous\" :: Text), \"count\" .= n])\n         else case Patch.genPatch path src (replace old new src) of\n                Left _ -> pure (object [\"ok\" .= True, \"changed\" .= False])\n                Right fp -> pure (object [\"ok\" .= True, \"changed\" .= True, \"diff\" .= Patch.renderPatch [fp]])",
            "-- | `update` from the input lane: {file, old, new} (for big/quote-heavy fragments).\nupdateJ :: Value -> M ()\nupdateJ v = case (v ^? key \"file\" . _String, v ^? key \"old\" . _String, v ^? key \"new\" . _String) of\n  (Just f, Just o, Just n) -> update f o n\n  _ -> error \"updateJ: need {file, old, new} strings in input\"",
            "-- | Insert a block after the unique line containing `anchor`. Errors on 0 or 2+.\ninsertAfter :: FilePath -> Text -> Text -> M ()\ninsertAfter path anchor block = do\n  src <- readFile path\n  let ls = lines src\n  case len (filter (isInfixOf anchor) ls) of\n    1 -> writeFile path (unlines (concatMap (\\l -> if anchor `isInfixOf` l then [l, block] else [l]) ls))\n    n -> error (\"insertAfter: anchor matched \" <> show n <> \" lines in \" <> path)",
            "-- | Compute-check-commit: write only if every named check holds; failures are data.\nwriteChecked :: FilePath -> [(Text, Bool)] -> Text -> M Value\nwriteChecked path checks content = do\n  let failed = [name | (name, ok) <- checks, not ok]\n  if null failed\n    then writeFile path content >> pure (object [\"file\" .= path, \"written\" .= True, \"checks\" .= length checks])\n    else pure (object [\"file\" .= path, \"written\" .= False, \"failed\" .= failed])",
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
/// `Node` is the composition currency — both the output of one op and the input
/// of the next, so navigation chains without destructuring. `lspWhere name`
/// seeds from a name; every other op takes a `Node` and the daemon re-resolves
/// it by position (so there is no name ambiguity). Graph edges
/// (`lspCallers`/`lspCallees`/`lspDef`) return `Node`s, so you fold them with
/// `concatMapM`/`loopM`. All LSP detail (positions, UTF-16, `WorkspaceEdit`,
/// call hierarchy) lives in the daemon. The `Lsp` lib module adds the `steer`
/// cascade + ready-made explorers (`explore`/`the`/`saferRename`/`chart`).
pub fn lsp_decl() -> EffectDecl {
    EffectDecl {
        type_name: "Lsp",
        description: concat!(
            "Semantic code-graph navigation via a language server (rust-analyzer, .rs). ",
            "Everything is a Node {name, container, kind, file, line, text} — the currency you thread. ",
            "`lspWhere name` → all definitions of NAME (the seed). Then walk the graph: ",
            "`lspCallers n` / `lspCallees n` (incoming/outgoing calls), `lspRefs n` (use sites), ",
            "`lspDef n` (any node → its definition), `lspHover n` (type/sig/docs), ",
            "`lspRename n new` (→ unified diff; review then `applyDiff`). Each returns Nodes you feed ",
            "back in (e.g. `lspWhere \"x\" >>= concatMapM lspCallers`). `lspDiags file` for a file's errors. ",
            "Needs the `tidepool-lsp-daemon` running in the workspace; queries error cleanly if not.",
        ),
        type_defs: &[
            "data Position = Position { posLine :: Int, posChar :: Int }",
            "data Node = Node { nodeName :: Text, nodeContainer :: Text, nodeKind :: Text, nodeFile :: Text, nodePos :: Position, nodeText :: Text }",
            "data Diag = Diag { diagFile :: Text, diagLine :: Int, diagSeverity :: Text, diagMessage :: Text }",
            // nodeLine: the human-facing 1-based line, derived from the exact pos.
            "nodeLine :: Node -> Int\nnodeLine = posLine . nodePos",
            "instance ToJSON Position where\n  toJSON (Position l c) = object [\"line\" .= l, \"char\" .= c]",
            "instance ToJSON Node where\n  toJSON nd@(Node n c k f _ t) = object [\"name\" .= n, \"container\" .= c, \"kind\" .= k, \"file\" .= f, \"line\" .= nodeLine nd, \"text\" .= t]",
            "instance ToJSON Diag where\n  toJSON (Diag f l s m) = object [\"file\" .= f, \"line\" .= l, \"severity\" .= s, \"message\" .= m]",
        ],
        constructors: &[
            "LspWhere       :: Text -> Lsp [Node]",
            "LspCallers     :: Node -> Lsp (Maybe [Node])",
            "LspCallees     :: Node -> Lsp (Maybe [Node])",
            "LspRefs        :: Node -> Lsp (Maybe [Node])",
            "LspDef         :: Node -> Lsp (Maybe Node)",
            "LspHover       :: Node -> Lsp (Maybe Text)",
            "LspRename      :: Node -> Text -> Lsp (Maybe Text)",
            "LspDiagnostics :: Text -> Lsp [Diag]",
        ],
        helpers: &[
            "-- | Seed: every workspace definition named X (each a Node with container/file/line/source line).\nlspWhere :: Text -> M [Node]\nlspWhere = send . LspWhere",
            "-- | Incoming calls. Nothing = node not callable; Just [] = callable, none. Unwrap with callersOf for plain chaining.\nlspCallers :: Node -> M (Maybe [Node])\nlspCallers = send . LspCallers",
            "-- | Outgoing calls. Nothing = node not callable; Just [] = callable, none.\nlspCallees :: Node -> M (Maybe [Node])\nlspCallees = send . LspCallees",
            "-- | Use sites of this node's symbol (kind = \"reference\"). Nothing = not a symbol.\nlspRefs :: Node -> M (Maybe [Node])\nlspRefs = send . LspRefs",
            "-- | Resolve any node (e.g. a use site) to its definition node.\nlspDef :: Node -> M (Maybe Node)\nlspDef = send . LspDef",
            "-- | Type / signature / docs for a node.\nlspHover :: Node -> M (Maybe Text)\nlspHover = send . LspHover",
            "-- | Rename a node's symbol to NEW; returns a unified diff (apply with applyDiff). Nothing = can't rename.\nlspRename :: Node -> Text -> M (Maybe Text)\nlspRename n new = send (LspRename n new)",
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
            "callCommand :: Text -> M ()\ncallCommand cmd = do { (ec, _, err) <- send (Run cmd); when (ec /= 0) (error (\"command failed (\" <> show ec <> \"): \" <> err)) }",
            "readProcess :: Text -> M Text\nreadProcess cmd = do { (ec, out, err) <- send (Run cmd); if ec == 0 then pure out else error (\"command failed (\" <> show ec <> \"): \" <> err) }",
            "run :: Text -> M (Int, Text, Text)\nrun = send . Run",
            "runIn :: Text -> Text -> M (Int, Text, Text)\nrunIn dir cmd = send (RunIn dir cmd)",
            // Isolating variants: spawn failure becomes `Left err` instead of
            // aborting the eval. A nonzero exit is NOT a failure here — it
            // arrives as `Right (code, out, err)`, so the common eval-killer
            // (readProcess on nonzero exit) is avoided by inspecting the code.
            "tryRun :: Text -> M (Either Text (Int, Text, Text))\ntryRun = send . TryRun",
            "tryRunIn :: Text -> Text -> M (Either Text (Int, Text, Text))\ntryRunIn dir cmd = send (TryRunIn dir cmd)",
            // Shell-free: argv list, no sh -c. $1/$VAR/globs are literal — safe.
            "runArgv :: [Text] -> M (Int, Text, Text)\nrunArgv = send . RunArgv",
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
