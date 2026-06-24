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
        descriptor("tidepool://vocab", "Library vocabulary", "Live signatures of every project-library verb (.tidepool/lib), refreshed from disk on read."),
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

// --- content renderers ------------------------------------------------------

fn guide_md(ctx: &ResourceCtx) -> String {
    let mut s = String::from(concat!(
        "# Tidepool eval guide\n\n",
        "`code` is a single Haskell EXPRESSION of type `M a`; its value is the eval's result. ",
        "The server wraps it in a module with the effect stack, pragmas, and imports. Compose with ",
        "`>>=`, `<&>`, `>=>`, point-free pipelines; attach a trailing `where` for local bindings. ",
        "For step-by-step sequencing write an explicit `do` block — bare statement lines do NOT parse. ",
        "Invoke effects with the helper verbs (prefer `putStrLn \"hi\"` over `send (Print \"hi\")`).\n\n",
        "## Returning results\n",
        "The final value is rendered to JSON for the caller — Int → number, [Char] → string, ",
        "Bool → true/false, lists → arrays, and a `Value` → that JSON directly. RETURN a `Value` ",
        "for structured output (`object`/`toJSON`/`parseJson`/`llm`/`tryHttpGet`); use `putStrLn`/`say` ",
        "only for human-readable debug traces, never to stringify a result. Extract from a `Value` with ",
        "optics: `v ^? key \"f\" . _String` (also `_Int`, `_Double`, `_Bool`, `_Array`); ",
        "`renderJson :: Value -> Text` renders one to compact JSON.\n\n",
        "## The input payload lane\n",
        "Pass large or quote-heavy content (file bodies, generated source) as a real JSON value in ",
        "`input` — no Haskell string escaping — and keep `code` a short verb consuming the `input` ",
        "binding. E.g. whole-file writes: ",
        "`writeFile \".tidepool/lib/Mod.hs\" src where src = case input of { String s -> s; _ -> \"\" }`.\n\n",
        "## Polymorphic Prelude ops\n",
        "`len` (length of Text or [a]), `isNull` (emptiness of either), `stake`/`sdrop` ",
        "(take/drop on either), `intercalate` joins Text (alias `joinText`), `tReverse` reverses Text. ",
        "List-only: `length`, `take`, `drop`, `null`.\n\n",
        "## Examples (expression-first)\n",
        "```haskell\n",
        "glob \"**/*.rs\" >>= mapM (\\p -> (,) p <$> getFileSize p)\n",
        "do { src <- readFile \"CLAUDE.md\"; pure (stake 5 (lines src)) }  -- explicit do when sequencing\n",
        "```\n\n",
        "Per-effect helper signatures live in `tidepool://effect/{name}`; library verbs in ",
        "`tidepool://vocab`; structured ask/llm in `tidepool://schema`.\n",
    ));
    if ctx
        .effects
        .iter()
        .any(|e| matches!(e.type_name, "Http" | "Exec" | "Llm" | "Fs"))
    {
        s.push_str(concat!(
            "\n## Failure isolation (long-running evals)\n",
            "The `try*` verbs return `M (Either Text a)` so one bad probe doesn't kill an eval. An ",
            "EXTERNAL failure — bad URL, 404/network error, LLM API error/refusal, exec spawn failure, ",
            "unreadable file — becomes `Left err` and the eval continues:\n",
            "```\n",
            "tryRun, tryRunIn        :: ... -> M (Either Text (Int, Text, Text))\n",
            "tryHttpGet, tryHttpPost :: ... -> M (Either Text Value)\n",
            "tryLlm                  :: Schema -> Text -> M (Either Text Value)\n",
            "tryReadFile             :: Text -> M (Either Text Text)\n",
            "```\n",
            "They do NOT catch: Haskell `error`/partial functions (including readProcess/callCommand on ",
            "a nonzero exit), other runtime faults, eval cancellation/timeout, or the LLM call-budget ",
            "limit — those still abort. A command that RUNS but exits nonzero is NOT a failure: ",
            "`tryRun` returns `Right (code, out, err)`; inspect `code` yourself.\n",
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
        "surrounding context to be UNIQUE), and the `new` text. No news is good news.\n",
        "```haskell\n",
        "update      :: FilePath -> Text -> Text -> M ()   -- applies, or ERRORS (not-found / ambiguous-with-count)\n",
        "updateAll   :: FilePath -> Text -> Text -> M Int  -- replace every occurrence; returns the count\n",
        "planUpdate  :: FilePath -> Text -> Text -> M Value -- dry-run: {changed,diff} | {ok:false,reason,count}; writes nothing, never errors\n",
        "updateJ     :: Value -> M ()                      -- input lane: {file, old, new} for big/quote-heavy fragments\n",
        "insertAfter :: FilePath -> Text -> Text -> M ()   -- insert a block after the unique line containing an anchor\n",
        "```\n",
        "`update` THROWS on failure (a bad edit is a bug — stop and fix); call `planUpdate` first if you ",
        "want to inspect/branch. Fragments are plain `Text`, so compute them: ",
        "`update p old (TF.camelToSnake x)` — no quoter, no escaping. Big fragments ride `input` via `updateJ`.\n\n",
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
        "## 4. Syntax-aware (AST) — rename across scopes, structural rewrite\n",
        "When you need to match CODE STRUCTURE rather than text (rename only the real identifier, not ",
        "strings/comments), use ast-grep: see `tidepool://effect/SG` (`hsDef`/`rsFn`/`sgRuleFind`/`planRw`).\n",
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
    let digest = crate::library_vocab(ctx.lib_dirs);
    if digest.trim().is_empty() {
        "# Library vocabulary\n\nNo project library found (.tidepool/lib).\n".to_string()
    } else {
        format!("# Library vocabulary (.tidepool/lib)\n{}", digest)
    }
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

fn first_sentence(s: &str) -> String {
    match s.find(". ") {
        Some(i) => s[..=i].trim().to_string(),
        None => s.trim().to_string(),
    }
}
