//! MCP resources for the eval surface.
//!
//! The `eval` tool description carries only a self-sufficient floor (the code
//! model, the effect name list, the structured ask/llm essentials). The DEPTH —
//! full per-effect constructors/helpers, the Schema + Edit grammars, the library
//! vocabulary, worked patterns, and the vendored stdlib sources — lives here as
//! URI-addressed resources the client pulls on demand (`resources/list` +
//! `resources/read`). This keeps the per-eval tool-description token cost low
//! while making the whole surface discoverable.
//!
//! URI scheme:
//! - `tidepool://guide`            — the full eval guide (prose, examples, failure isolation)
//! - `tidepool://schema`           — Schema grammar + structured ask/llm + extraction optics
//! - `tidepool://edits`            — the declarative `Edit` verb JSON schema
//! - `tidepool://vocab`            — live project-library verb signatures
//! - `tidepool://capabilities`     — the Prelude shadow surface + qualifier-reached names
//! - `tidepool://patterns`         — worked examples (PATTERNS.md)
//! - `tidepool://effect/{name}`    — per-effect constructors + types + helpers (template)
//! - `tidepool://stdlib/{module}`  — vendored stdlib module source (template)

use crate::effect_decls::EffectDecl;
use std::path::{Path, PathBuf};

/// Everything `read_resource`/`list_resources` need, borrowed from the server.
pub struct ResourceCtx<'a> {
    pub effects: &'a [EffectDecl],
    pub lib_dirs: &'a [PathBuf],
    pub patterns_path: Option<&'a Path>,
    pub stdlib_dir: Option<&'a Path>,
}

/// A listed resource (for `resources/list`).
pub struct ResourceDescriptor {
    pub uri: String,
    pub name: String,
    pub description: String,
    pub mime: &'static str,
}

/// A parameterized resource (for `resources/list_templates`).
pub struct TemplateDescriptor {
    pub uri_template: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub mime: &'static str,
}

/// Rendered resource body (for `resources/read`).
pub struct ResourceBody {
    pub mime: &'static str,
    pub text: String,
}

const MD: &str = "text/markdown";
const HS: &str = "text/x-haskell";

/// Concrete resources advertised by `resources/list`: the fixed docs, one entry
/// per effect, and one per stdlib module found on disk.
pub fn list(ctx: &ResourceCtx) -> Vec<ResourceDescriptor> {
    let mut out = vec![
        descriptor("tidepool://guide", "Eval guide", "How to write eval `code`: the M-a model, composition, returning JSON, the input payload lane, examples, and failure isolation."),
        descriptor("tidepool://schema", "Schema + ask/llm", "The structured-output Schema grammar and the ask/llm/tryLlm primitives; how to extract results with optics."),
        descriptor("tidepool://edits", "Edit verb schema", "The declarative line/anchor `Edit` JSON schema (applyEdits/editsJ) and its conflict vocabulary."),
        descriptor("tidepool://vocab", "Vocabulary", "Live signatures of every verb in scope — core effect verbs + project library (.tidepool/lib), refreshed on read."),
        descriptor("tidepool://capabilities", "Capabilities", "The Prelude shadow surface (names in scope unqualified) plus the canonical Prelude/Data.List names that live under a qualifier."),
    ];
    if ctx.patterns_path.is_some() {
        out.push(descriptor(
            "tidepool://patterns",
            "Worked patterns",
            "Paste-able eval examples for the common shapes (aperture, classify, text surgery).",
        ));
    }
    for e in ctx.effects {
        out.push(ResourceDescriptor {
            uri: format!("tidepool://effect/{}", e.type_name),
            name: format!("Effect: {}", e.type_name),
            description: first_sentence(e.description),
            mime: MD,
        });
    }
    for m in stdlib_modules(ctx) {
        out.push(ResourceDescriptor {
            uri: format!("tidepool://stdlib/{}", m),
            name: format!("stdlib: {}", m),
            description: format!("Vendored source of the {} module.", m),
            mime: HS,
        });
    }
    out
}

/// Parameterized resources advertised by `resources/list_templates`.
pub fn templates() -> Vec<TemplateDescriptor> {
    vec![
        TemplateDescriptor {
            uri_template: "tidepool://effect/{name}",
            name: "Effect detail",
            description: "Constructors, supporting types, and helper signatures for one effect.",
            mime: MD,
        },
        TemplateDescriptor {
            uri_template: "tidepool://stdlib/{module}",
            name: "Stdlib module source",
            description: "Vendored Haskell source for a stdlib module (e.g. Tidepool.Prelude).",
            mime: HS,
        },
    ]
}

/// Render the body for a concrete URI, or `None` if unknown.
pub fn read(ctx: &ResourceCtx, uri: &str) -> Option<ResourceBody> {
    match uri {
        "tidepool://guide" => Some(body(MD, guide_md(ctx))),
        "tidepool://schema" => Some(body(MD, schema_md())),
        "tidepool://edits" => Some(body(MD, edits_md())),
        "tidepool://vocab" => Some(body(MD, vocab_md(ctx))),
        "tidepool://capabilities" => Some(body(MD, capabilities_md(ctx))),
        "tidepool://patterns" => ctx
            .patterns_path
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|t| body(MD, t)),
        _ => {
            if let Some(name) = uri.strip_prefix("tidepool://effect/") {
                ctx.effects
                    .iter()
                    .find(|e| e.type_name == name)
                    .map(|e| body(MD, effect_md(e)))
            } else if let Some(module) = uri.strip_prefix("tidepool://stdlib/") {
                stdlib_source(ctx, module).map(|t| body(HS, t))
            } else {
                None
            }
        }
    }
}

/// Resolve a `help` topic to rendered content (the same text behind the
/// resources), or a topic index for an empty/unknown topic. Backs the `help`
/// TOOL, which any MCP client can call even without `resources/read` support.
pub fn help(ctx: &ResourceCtx, topic: &str) -> String {
    let t = topic.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("topics") || t.eq_ignore_ascii_case("index") {
        return help_index(ctx);
    }
    for uri in topic_uris(t) {
        if let Some(b) = read(ctx, &uri) {
            return b.text;
        }
    }
    format!("Unknown help topic: {t:?}\n\n{}", help_index(ctx))
}

fn topic_uris(t: &str) -> Vec<String> {
    match t {
        "guide" | "schema" | "edits" | "vocab" | "patterns" | "capabilities" => {
            vec![format!("tidepool://{t}")]
        }
        _ => {
            if let Some(rest) = t.strip_prefix("effect") {
                vec![format!(
                    "tidepool://effect/{}",
                    rest.trim_start_matches([':', ' ', '/'])
                )]
            } else if let Some(rest) = t.strip_prefix("stdlib") {
                vec![format!(
                    "tidepool://stdlib/{}",
                    rest.trim_start_matches([':', ' ', '/'])
                )]
            } else {
                // Bare token: try it as an effect name, then a stdlib module.
                vec![
                    format!("tidepool://effect/{t}"),
                    format!("tidepool://stdlib/{t}"),
                ]
            }
        }
    }
}

fn help_index(ctx: &ResourceCtx) -> String {
    let effects: Vec<&str> = ctx.effects.iter().map(|e| e.type_name).collect();
    let modules = stdlib_modules(ctx);
    let mut s = String::from(
        "# help topics\n\nCall `help` with one of:\n\
         - `guide`     — how to write eval code (the M-a model, returning JSON, the input lane)\n\
         - `schema`    — the Schema grammar + ask/llm/tryLlm\n\
         - `edits`     — editing verbs (update, planUpdate, the Edit DSL, diffs)\n\
         - `vocab`     — every verb signature in scope (effects + project library)\n\
         - `capabilities` — the Prelude shadow surface + names that live under a qualifier\n\
         - `patterns`  — worked examples\n\
         - `effect <Name>`   — one effect's constructors + helpers\n\
         - `stdlib <Module>` — a vendored stdlib module's source\n\n",
    );
    s.push_str(&format!("Effects: {}\n", effects.join(", ")));
    if !modules.is_empty() {
        s.push_str(&format!("Stdlib modules: {}\n", modules.join(", ")));
    }
    s
}

// --- content renderers ------------------------------------------------------

fn guide_md(ctx: &ResourceCtx) -> String {
    let mut s = String::from(concat!(
        "# Tidepool eval guide\n\n",
        "`code` is a single Haskell EXPRESSION of type `M a`; its value is the eval's result. ",
        "The server wraps it in a module with the effect stack, pragmas, and imports. Compose with ",
        "`>>=`, `<&>`, `>=>`, and point-free pipelines; attach a trailing `where` for local bindings. ",
        "For step-by-step sequencing write an explicit `do` block. Invoke effects with the helper ",
        "verbs (`putStrLn \"hi\"` sends to the Console; `run \"...\"` runs a shell command).\n\n",
        "## Returning results\n",
        "The final value renders to JSON for the caller — Int → number, [Char] → string, ",
        "Bool → true/false, lists → arrays, and a `Value` → that JSON directly. Return a `Value` ",
        "for structured output (`object`/`toJSON`/`parseJson`/`llm`/`tryHttpGet`); `putStrLn`/`say` ",
        "carry human-readable debug traces, and `pure x` returns a value directly. Extract from a `Value` with ",
        "optics: `v ^? key \"f\" . _String` (also `_Int`, `_Double`, `_Bool`, `_Array`); ",
        "`renderJson :: Value -> Text` renders one to compact JSON.\n\n",
        "## The input payload lane\n",
        "Pass large or quote-heavy content (file bodies, generated source, config) as a real JSON value in ",
        "`input` — the eval reads it via the `input` binding, so `code` stays a short verb. Decode it ",
        "into a typed record and the whole payload is available by field:\n",
        "```haskell\n",
        "data Cfg = Cfg { target :: Text, limit :: Int } deriving (Generic, FromJSON)\n",
        "do { Cfg{..} <- liftEither (resultToEither (fromJSON input)); grepGlob target \"**/*.rs\" <&> stake limit }\n",
        "```\n",
        "For a single field, optics read straight off the `Value`: `input ^? key \"target\" . _String`. ",
        "For a whole-file write, put the body on `input`: `writeFile \".tidepool/lib/Mod.hs\" (input ^. _String)`.\n\n",
        "## Polymorphic Prelude ops\n",
        "`len` (length of Text or [a]), `isNull` (emptiness of either), `stake`/`sdrop` ",
        "(take/drop on either), `intercalate` joins Text (alias `joinText`), `tReverse` reverses Text. ",
        "List-only: `length`, `take`, `drop`, `null`. tidepool://capabilities indexes the full shadow surface.\n\n",
        "## Examples (expression-first)\n",
        "```haskell\n",
        "glob \"**/*.rs\" >>= mapM (\\p -> (,) p <$> getFileSize p)\n",
        "do { Right src <- readFile \"CLAUDE.md\"; pure (stake 5 (lines src)) }  -- explicit do when sequencing\n",
        "```\n\n",
        "Per-effect helper signatures live in `tidepool://effect/{name}`; library verbs in ",
        "`tidepool://vocab`; structured ask/llm in `tidepool://schema`; the Prelude shadow surface ",
        "in `tidepool://capabilities`.\n\n",
        "## Effect result records\n",
        "Verbs return named records — read fields with **record-dot** (`p.stdout`, `h.path`):\n",
        "- `run cmd :: M (Either <EffectError> Proc)` — bind the `Right`; `Proc` fields `exitCode`, `stdout`, `stderr`; `ok p` = zero exit\n",
        "- `grepGlob`/`searchFiles` → `[Hit]` — fields `path`, `line`, `text`\n",
        "- `readGlob` → `[Doc]` — fields `path`, `body`\n",
        "- `fsMeta` → `Maybe FileMeta` — fields `size`, `isFile`, `isDir`\n",
    ));
    if ctx
        .effects
        .iter()
        .any(|e| matches!(e.type_name, "Http" | "Exec" | "Llm" | "Fs"))
    {
        s.push_str(concat!(
            "\n## Effect failures are values\n",
            "Every effect verb returns `Either <EffectError> a`: an external condition — a 404 or network ",
            "error, an LLM API error or refusal, an exec spawn failure, a missing file — arrives as `Left err`, ",
            "so one probe's failure is data the eval reads and routes on:\n",
            "```haskell\n",
            "readFile \"notes.md\" >>= \\case\n",
            "  Right body          -> pure (T.length body)\n",
            "  Left (FsNotFound _) -> pure 0      -- match a specific cause to recover\n",
            "Right p <- run \"git status --short\"  -- bind the Right; liftEither aborts the eval on a Left\n",
            "```\n",
            "`liftEither :: Either e a -> M a` unwraps a `Right` or aborts rendering the `Left`. A command that ",
            "RUNS and exits nonzero is a `Right proc` — read `proc.exitCode` (record-dot) to branch on the code.\n",
        ));
    }
    s
}

fn schema_md() -> String {
    String::from(concat!(
        "# Structured LLM / Ask — one Schema vocabulary\n\n",
        "Both primitives take a `Schema`, return a validated `Value`, and you extract with optics.\n\n",
        "```haskell\n",
        "Schema = SObj [(Text,Schema)] | SArr Schema | SStr | SNum | SBool | SEnum [Text] | SOpt Schema\n\n",
        "ask    :: Schema -> Text -> M Value   -- SUSPEND to the calling agent; reply validated vs schema, no token burn\n",
        "llm    :: Schema -> Text -> M Value   -- AUTONOMOUS server-side model call (costs tokens); structured, no fences\n",
        "tryLlm :: Schema -> Text -> M (Either Text Value)  -- as llm, API error/refusal -> Left err\n",
        "```\n\n",
        "A non-object top-level schema (`SEnum`/`SStr`/…) is auto-wrapped for the provider and ",
        "unwrapped on return, so `llm (SEnum [\"a\",\"b\"]) prompt` yields the bare value.\n\n",
        "## Extracting\n",
        "```haskell\n",
        "cat <- llm (SObj [(\"category\", SEnum [\"bug\",\"feat\"])]) prompt <&> (^? key \"category\" . _String)\n",
        "ok  <- ask (SObj [(\"ok\", SBool)]) \"proceed?\" <&> (^? key \"ok\" . _Bool)\n",
        "```\n\n",
        "## Orchestration rule\n",
        "Let the LLM DECIDE (`SEnum`/`SBool`) and let deterministic code EMIT syntax (regex/AST ",
        "patterns) — models are unreliable at generating domain-specific syntax directly. So: classify ",
        "with a small enum, then map the chosen strategy to vetted code in Haskell.\n",
    ))
}

fn edits_md() -> String {
    String::from(concat!(
        "# Editing files — common case first\n\n",
        "## 1. `update` — exact str-replace (the 90% case, core, always available)\n",
        "Mirrors the Edit tool you already know: name the file, the exact `old` text (with enough ",
        "surrounding context to name it uniquely), and the `new` text.\n",
        "```haskell\n",
        "update      :: FilePath -> Text -> Text -> M ()   -- applies the one unique `old` → `new` match\n",
        "updateAll   :: FilePath -> Text -> Text -> M Int  -- replace every occurrence; returns the count\n",
        "planUpdate  :: FilePath -> Text -> Text -> M Value -- dry-run: {changed,diff} | {ok:false,reason,count}; writes nothing\n",
        "updateJ     :: Value -> M ()                      -- input lane: {file, old, new} for big/quote-heavy fragments\n",
        "insertAfter :: FilePath -> Text -> Text -> M ()   -- insert a block after the unique line containing an anchor\n",
        "```\n",
        "`update` applies the single unique occurrence of `old`; `planUpdate` returns the diff as data ",
        "when you want to inspect or branch before committing. Fragments are plain `Text`, so compute ",
        "them: `update p old (TF.camelToSnake x)` — no quoter, no escaping. Big fragments ride `input` via `updateJ`.\n\n",
        "## 2. `Edit` DSL — line/anchor batch (project library)\n",
        "When the change is naturally line- or anchor-shaped (replace lines 10–15, insert before an anchor) ",
        "and you want several edits applied atomically. Lowers to a context-anchored patch; conflicts as DATA.\n",
        "```haskell\n",
        "applyEdits :: Text -> [Edit] -> M Value   -- atomic; planEdits for a dry-run diff\n",
        "editsJ     :: Value -> M Value            -- input lane: { file, edits:[{op,...}] }\n",
        "-- Edit = ReplaceLines lo hi [Text] | InsertAt n [Text] | ReplaceAnchor a [Text]\n",
        "--      | InsertAfterAnchor a [Text] | InsertBeforeAnchor a [Text]   (line numbers 1-based)\n",
        "```\n",
        "JSON ops for `editsJ`: `replaceLines{lo,hi,lines}` / `insertAt{line,lines}` / ",
        "`replaceAnchor|insertAfterAnchor|insertBeforeAnchor{anchor,lines}`. Conflicts come back as data: ",
        "`anchor-missing`, `anchor-ambiguous`, `range-out-of-bounds`, `edits-overlap`.\n\n",
        "## 3. Unified diffs — when you ALREADY have a patch (project library)\n",
        "`applyDiff :: Text -> M Value` / `planDiff` apply a real unified diff (context-is-truth, atomic, ",
        "conflicts as data). The `[patch|...|]` quasiquoter builds one inline, but quoted bodies must be ",
        "LEFT-ALIGNED and can't contain `|]` — so ride the `input` lane for any non-trivial diff: ",
        "`applyDiff (case input of { String s -> s; _ -> \"\" })`. `genPatchTo path newContent` generates the diff for you.\n\n",
        "## 4. Semantic (LSP) — rename across scopes\n",
        "When you need to rename the real symbol across scopes (not a text match that also hits ",
        "strings/comments), use the Lsp effect: `lspWhere`/`lspRename` — see `tidepool://effect/Lsp`.\n",
    ))
}

fn effect_md(e: &EffectDecl) -> String {
    let mut s = format!("# Effect: {}\n\n{}\n", e.type_name, e.description);
    if !e.constructors.is_empty() {
        s.push_str("\n## Constructors (invoke via `send`)\n```haskell\n");
        for c in e.constructors {
            s.push_str(c);
            s.push('\n');
        }
        s.push_str("```\n");
    }
    if !e.type_defs.is_empty() {
        s.push_str("\n## Types & supporting definitions\n```haskell\n");
        for t in e.type_defs {
            s.push_str(t);
            s.push('\n');
        }
        s.push_str("```\n");
    }
    if !e.helpers.is_empty() {
        s.push_str("\n## Helpers (prefer over raw `send`)\n```haskell\n");
        for h in e.helpers {
            s.push_str(h);
            s.push('\n');
        }
        s.push_str("```\n");
    }
    s
}

fn vocab_md(ctx: &ResourceCtx) -> String {
    // The complete verb surface: core effect verbs (always available) + the
    // project library. The per-effect detail (constructors/types) is in
    // tidepool://effect/{name}; this is the flat signature index.
    let mut s = String::from(
        "# Vocabulary — every verb in scope\n\nEffect verbs are core (always available); \
         library verbs come from `.tidepool/lib`.\n\n## Effect verbs\n",
    );
    for eff in ctx.effects {
        let sigs = crate::extract_sigs(&eff.helpers.join("\n"));
        if sigs.is_empty() {
            continue;
        }
        s.push_str(&format!("\n### {}\n", eff.type_name));
        for sig in sigs {
            s.push_str("  ");
            s.push_str(&sig);
            s.push('\n');
        }
    }
    let digest = crate::library_vocab(ctx.lib_dirs, None);
    if digest.trim().is_empty() {
        s.push_str("\n## Project library\n\n(none — no .tidepool/lib in scope)\n");
    } else {
        s.push_str("\n## Project library (.tidepool/lib)\n");
        s.push_str(&digest);
    }
    s
}

// --- capabilities: the Prelude shadow surface -------------------------------

/// Canonical `Prelude` / `Data.List` names that resolve through a qualifier
/// rather than the unqualified `Tidepool.Prelude` shadow, each paired with the
/// mechanical fact of where it lives. Consumed by the `tidepool://capabilities`
/// resource and by the compile error-hint path (`exclusion_reason`), so a
/// "not in scope" on one of these names carries the reach-path with it.
///
/// Every entry is reachable: the list version lives in `Data.List` (`L.`), the
/// Text version in `Data.Text` (`T.`), or the base definition under `P.`.
pub(crate) const QUALIFIED_NAMES: &[(&str, &str)] = &[
    // List combinators — the `Data.List` (`L.`) surface.
    ("subsequences", "list combinatorics live in Data.List — `L.subsequences`"),
    ("permutations", "`L.permutations`"),
    ("delete", "list delete is `L.delete`; maps/sets have `Map.delete`/`Set.delete`"),
    ("insert", "list insert is `L.insert`; `Map.insert`/`Set.insert` for maps/sets"),
    ("union", "list union is `L.union`; `Set.union`/`Map.union` for sets/maps"),
    ("intersect", "list intersect is `L.intersect`; `Set.intersection` for sets"),
    ("stripPrefix", "`L.stripPrefix` for lists, `T.stripPrefix` for Text"),
    ("mapAccumR", "`L.mapAccumR`; `mapAccumL` is in the shadow"),
    ("foldl1'", "`L.foldl1'`; `foldl1` and `foldl'` are in the shadow"),
    ("isSubsequenceOf", "`L.isSubsequenceOf`"),
    ("genericTake", "the length-polymorphic variants are `L.genericTake`/`L.genericDrop`; `genericLength` is in the shadow"),
    // Text — the `Data.Text` (`T.`) surface.
    ("pack", "`pack` is in the shadow (polymorphic `Pack`); `T.pack` is the same function"),
    // Rendering / parsing — the Text-first shadow spellings.
    ("showsPrec", "`show :: a -> Text` is the shadow's renderer"),
    ("shows", "`show :: a -> Text` is the shadow's renderer"),
    ("showString", "`show :: a -> Text` is the shadow's renderer"),
    ("reads", "`read` is in the shadow; `parseInt`/`parseIntM`/`parseDouble`/`parseDoubleM` are the Text-first parsers"),
    ("readsPrec", "`read` is in the shadow; `parseIntM`/`parseDoubleM` are the Text-first parsers"),
    // IO console/stdin — modelled as effects.
    ("print", "console output is an effect — `say`/`putStrLn :: Text -> M ()` (Console)"),
    ("getLine", "stdin arrives on the `input` payload lane; `ask` suspends for a caller reply"),
    ("interact", "read stdin from the `input` payload lane; write via the Console verbs"),
    // Numeric — reach base under `P.`.
    ("gcd", "`P.gcd` (base, polymorphic over Integral)"),
    ("lcm", "`P.lcm` (base, polymorphic over Integral)"),
    ("properFraction", "`P.properFraction`; `truncate`/`round`/`ceiling`/`floor` are in the shadow"),
    // Ranges / abort — shaped by eager evaluation.
    ("enumFrom", "`enumFromTo lo hi` builds a finite `[Int]`; `[lo..hi]` desugars to it"),
    ("enumFromThen", "`enumFromTo lo hi` builds a finite `[Int]`; `[lo..hi]` desugars to it"),
    ("errorWithoutStackTrace", "`error :: Text -> a` is the shadow's abort"),
];

/// The reach-path fact for a canonical name that lives under a qualifier, or
/// `None` for a name that is either in the unqualified shadow or unknown here.
pub(crate) fn exclusion_reason(name: &str) -> Option<&'static str> {
    QUALIFIED_NAMES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, reason)| *reason)
}

/// Parse the `Tidepool.Prelude` export list into its unqualified shadow names
/// plus the qualified (`Map.`/`Set.`) and wholesale (`module …`) re-exports.
/// The header runs `module Tidepool.Prelude ( … ) where`; entries are
/// comma-separated and may carry `-- …` comments, `Type(..)` method bundles,
/// and `(op)` operator sections.
fn prelude_exports(src: &str) -> (Vec<String>, Vec<String>, Vec<String>) {
    let after = src
        .split_once("module Tidepool.Prelude")
        .map_or("", |(_, r)| r);
    let list = after.split_once(") where").map_or(after, |(l, _)| l);
    // Strip per-line `-- …` comments, then join.
    let mut cleaned = String::new();
    for line in list.lines() {
        cleaned.push_str(line.split_once("--").map_or(line, |(c, _)| c));
        cleaned.push(' ');
    }
    let cleaned = cleaned.trim_start().strip_prefix('(').unwrap_or(&cleaned);
    // Split on depth-0 commas so `Type(a, b)` stays one entry.
    let mut entries: Vec<String> = Vec::new();
    let (mut depth, mut cur) = (0i32, String::new());
    for ch in cleaned.chars() {
        match ch {
            '(' => {
                depth += 1;
                cur.push(ch);
            }
            ')' => {
                depth -= 1;
                cur.push(ch);
            }
            ',' if depth == 0 => entries.push(std::mem::take(&mut cur)),
            _ => cur.push(ch),
        }
    }
    entries.push(cur);

    let (mut unqualified, mut qualified, mut wholesale) = (Vec::new(), Vec::new(), Vec::new());
    for e in entries {
        let e = e.split_whitespace().collect::<Vec<_>>().join(" ");
        if e.is_empty() {
            continue;
        }
        if let Some(m) = e.strip_prefix("module ") {
            wholesale.push(m.to_string());
        } else if is_module_qualified(&e) {
            qualified.push(e);
        } else {
            unqualified.push(e);
        }
    }
    unqualified.sort();
    unqualified.dedup();
    qualified.sort();
    (unqualified, qualified, wholesale)
}

/// True for a `Module.name` export entry (an identifier run followed directly
/// by `.`), distinguishing `Map.fromList` from a `Type(..)` method bundle.
fn is_module_qualified(e: &str) -> bool {
    let head: String = e
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    !head.is_empty() && e[head.len()..].starts_with('.')
}

fn capabilities_md(ctx: &ResourceCtx) -> String {
    let mut s = String::from(
        "# Tidepool capabilities — the Prelude shadow surface\n\n\
         `import Tidepool.Prelude hiding (error)` is the unqualified surface every eval and \
         session sees. The names below are in scope without a qualifier.\n\n",
    );
    let prelude_src = stdlib_source(ctx, "Tidepool.Prelude");
    match prelude_src.as_deref().map(prelude_exports) {
        Some((unqualified, qualified, wholesale)) if !unqualified.is_empty() => {
            s.push_str("## In scope unqualified\n```haskell\n");
            s.push_str(&unqualified.join(", "));
            s.push_str("\n```\n\n");
            if !wholesale.is_empty() {
                s.push_str(&format!(
                    "Re-exported wholesale: {} (every name from these modules is unqualified).\n\n",
                    wholesale.join(", ")
                ));
            }
            if !qualified.is_empty() {
                s.push_str("Re-exported under a qualifier (call as written):\n```haskell\n");
                s.push_str(&qualified.join(", "));
                s.push_str("\n```\n\n");
            }
        }
        _ => {
            s.push_str(
                "(The live export list renders from the vendored `Tidepool.Prelude` source.)\n\n",
            );
        }
    }
    s.push_str(
        "The T. (Data.Text), L. (Data.List), Map. (Data.Map.Strict), MM. (Data.Map.Merge.Strict), \
         Set. (Data.Set), KM. (Tidepool.Aeson.KeyMap), TF. (Tidepool.TextFormat), Tab. \
         (Tidepool.Table), and P. (base Prelude) qualifiers are always in scope.\n\n",
    );
    s.push_str(
        "## Names that live under a qualifier\n\
         Canonical `Prelude` / `Data.List` names reached through a qualifier rather than the \
         unqualified shadow:\n\n",
    );
    for (name, reason) in QUALIFIED_NAMES {
        s.push_str(&format!("- `{name}` — {reason}\n"));
    }
    s
}

// --- stdlib source ----------------------------------------------------------

/// Dotted module names available under the stdlib dir (e.g. `Tidepool.Prelude`).
fn stdlib_modules(ctx: &ResourceCtx) -> Vec<String> {
    let Some(root) = ctx.stdlib_dir else {
        return Vec::new();
    };
    let mut out = Vec::new();
    collect_hs(root, root, &mut out);
    out.sort();
    out
}

fn collect_hs(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_hs(root, &path, out);
        } else if path.extension().is_some_and(|e| e == "hs") {
            if let Ok(rel) = path.strip_prefix(root) {
                let dotted = rel
                    .with_extension("")
                    .to_string_lossy()
                    .replace(['/', '\\'], ".");
                out.push(dotted);
            }
        }
    }
}

/// Read a stdlib module source by dotted name, guarding against path escape.
fn stdlib_source(ctx: &ResourceCtx, module: &str) -> Option<String> {
    let root = ctx.stdlib_dir?;
    if module.is_empty() || module.contains("..") || module.contains('/') || module.contains('\\') {
        return None;
    }
    let rel = module.replace('.', "/");
    let path = root.join(format!("{rel}.hs"));
    // Containment check: the resolved path must stay under root.
    let (cpath, croot) = (path.canonicalize().ok()?, root.canonicalize().ok()?);
    if !cpath.starts_with(&croot) {
        return None;
    }
    std::fs::read_to_string(&cpath).ok()
}

// --- small helpers ----------------------------------------------------------

fn descriptor(uri: &str, name: &str, description: &str) -> ResourceDescriptor {
    ResourceDescriptor {
        uri: uri.to_string(),
        name: name.to_string(),
        description: description.to_string(),
        mime: MD,
    }
}

fn body(mime: &'static str, text: String) -> ResourceBody {
    ResourceBody { mime, text }
}

/// One-line effect summary — delegates to the shared parser
/// ([`crate::describe::first_sentence`]) so the resource-descriptor blurb, the
/// tool-description index, and `:browse` share ONE sentence-splitter.
fn first_sentence(s: &str) -> String {
    crate::describe::first_sentence(s).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_resolves_topics() {
        let decls = crate::standard_decls();
        let ctx = ResourceCtx {
            effects: &decls,
            lib_dirs: &[],
            patterns_path: None,
            stdlib_dir: None,
        };
        // Fixed docs.
        assert!(help(&ctx, "guide").contains("eval guide"));
        assert!(help(&ctx, "schema").to_lowercase().contains("schema"));
        assert!(help(&ctx, "edits").contains("update"));
        // Effect topics: bare name and `effect <Name>` both resolve.
        assert!(help(&ctx, "Fs").contains("readFile"));
        assert!(help(&ctx, "effect Llm").contains("llm :: Schema"));
        // Empty / unknown fall back to the index.
        assert!(help(&ctx, "").contains("help topics"));
        assert!(help(&ctx, "nope").contains("Unknown help topic"));
    }

    #[test]
    fn capabilities_serves_shadow_surface_and_reasoned_exclusions() {
        let decls = crate::standard_decls();
        let ctx = ResourceCtx {
            effects: &decls,
            lib_dirs: &[],
            patterns_path: None,
            stdlib_dir: None,
        };
        let body = read(&ctx, "tidepool://capabilities").expect("capabilities resource resolves");
        // The reasoned-exclusion table renders with each name and its reach-path.
        assert!(
            body.text.contains("`subsequences`"),
            "lists an excluded name: {}",
            body.text
        );
        assert!(
            body.text.contains("Data.List"),
            "reach-path names the qualifier: {}",
            body.text
        );
        // help topic resolves too.
        assert!(help(&ctx, "capabilities").contains("shadow surface"));
    }

    #[test]
    fn exclusion_reason_hits_known_names_and_misses_shadowed_ones() {
        assert!(exclusion_reason("subsequences").is_some());
        assert!(exclusion_reason("gcd").is_some());
        // A name that IS in the unqualified shadow has no reach-path.
        assert!(exclusion_reason("sortBy").is_none());
        assert!(exclusion_reason("definitely_not_a_prelude_name").is_none());
    }

    #[test]
    fn prelude_exports_parses_a_header_export_list() {
        let src = "\
module Tidepool.Prelude
  ( -- * Types
    Int, Bool(..), Maybe(..)
  , map, filter  -- a trailing comment
  , (<$>)
  , Map.fromList, Map.toList
  , module Control.Lens
  ) where

import Prelude
";
        let (unqualified, qualified, wholesale) = super::prelude_exports(src);
        assert!(unqualified.contains(&"map".to_string()));
        assert!(unqualified.contains(&"filter".to_string()));
        assert!(unqualified.contains(&"Bool(..)".to_string()));
        assert!(unqualified.contains(&"(<$>)".to_string()));
        assert!(qualified.iter().any(|q| q == "Map.fromList"));
        assert!(wholesale.iter().any(|w| w == "Control.Lens"));
    }
}
