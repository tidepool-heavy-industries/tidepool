//! Stress test for nested mapM + readFile through the full MCP effect stack.
//!
//! Originally written to reproduce a tag=255 crash during MCP eval. The crash
//! turned out to be caused by running a new preamble (post-rename: readFile,
//! putStrLn) against a stale CBOR cache compiled with the old preamble (fsRead,
//! say). The unresolved bindings → tag corruption → "application of non-closure
//! (tag=255)". Fixed by the cache auto-invalidation (PR #259) which fingerprints
//! the tidepool-extract binary in the cache key.
//!
//! This test remains as a stress test: nested effectful mapM with real filesystem
//! reads through the full 8-effect stack + Library.hs preamble.

// Effect request enums mirror Haskell GADT constructors by name.
#![allow(dead_code, clippy::enum_variant_names)]

use std::path::{Path, PathBuf};
use tidepool_bridge_derive::FromCore;
use tidepool_bridge_effects::{FileMeta, Proc};
use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::value::Value;
use tidepool_testing::eval_harness::EvalHarness;

// ---------------------------------------------------------------------------
// Effect handlers — real Fs, stubs for everything else
// ---------------------------------------------------------------------------

#[derive(FromCore)]
enum ConsoleReq {
    #[core(name = "Print")]
    Print(String),
}
struct StubConsole;
impl EffectHandler for StubConsole {
    type Request = ConsoleReq;
    fn handle(
        &mut self,
        _req: ConsoleReq,
        cx: &EffectContext,
    ) -> Result<tidepool_effect::Response, EffectError> {
        cx.respond(())
    }
}

#[derive(FromCore)]
enum KvReq {
    #[core(name = "KvGet")]
    KvGet(String),
    #[core(name = "KvSet")]
    KvSet(String, Value),
    #[core(name = "KvDelete")]
    KvDelete(String),
    #[core(name = "KvKeys")]
    KvKeys,
}
struct StubKv;
impl EffectHandler for StubKv {
    type Request = KvReq;
    fn handle(
        &mut self,
        req: KvReq,
        cx: &EffectContext,
    ) -> Result<tidepool_effect::Response, EffectError> {
        match req {
            KvReq::KvGet(_) => {
                let nothing: Option<serde_json::Value> = None;
                cx.respond(nothing)
            }
            KvReq::KvSet(_, _) | KvReq::KvDelete(_) => cx.respond(()),
            KvReq::KvKeys => {
                let empty: Vec<String> = vec![];
                cx.respond(empty)
            }
        }
    }
}

#[derive(FromCore)]
enum FsReq {
    #[core(name = "FsRead")]
    FsRead(String),
    #[core(name = "FsWrite")]
    FsWrite(String, String),
    #[core(name = "FsListDir")]
    FsListDir(String),
    #[core(name = "FsGlob")]
    FsGlob(String),
    #[core(name = "FsExists")]
    FsExists(String),
    #[core(name = "FsMetadata")]
    FsMetadata(String),
}

struct RealFs {
    root: PathBuf,
}
impl RealFs {
    fn new() -> Self {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        Self {
            root: manifest.parent().unwrap().to_path_buf(),
        }
    }
    fn resolve(&self, p: &str) -> PathBuf {
        let path = PathBuf::from(p);
        if path.is_absolute() {
            path
        } else {
            self.root.join(path)
        }
    }
}
impl EffectHandler for RealFs {
    type Request = FsReq;
    fn handle(
        &mut self,
        req: FsReq,
        cx: &EffectContext,
    ) -> Result<tidepool_effect::Response, EffectError> {
        match req {
            FsReq::FsRead(path) => {
                let content = std::fs::read_to_string(self.resolve(&path)).unwrap_or_default();
                cx.respond(Ok::<String, String>(content))
            }
            FsReq::FsWrite(_, _) => cx.respond(Ok::<(), String>(())),
            FsReq::FsListDir(path) => {
                let entries: Vec<String> = std::fs::read_dir(self.resolve(&path))
                    .map(|rd| {
                        rd.filter_map(|e| e.ok())
                            .map(|e| e.file_name().to_string_lossy().to_string())
                            .collect()
                    })
                    .unwrap_or_default();
                cx.respond(Ok::<Vec<String>, String>(entries))
            }
            FsReq::FsGlob(pattern) => {
                let full = self.root.join(pattern.as_str());
                let root = self.root.clone();
                let entries: Vec<String> = glob::glob(full.to_str().unwrap_or(""))
                    .map(|paths| {
                        paths
                            .filter_map(|p: Result<PathBuf, _>| p.ok())
                            .filter_map(move |p: PathBuf| {
                                p.strip_prefix(&root)
                                    .ok()
                                    .map(|rel: &Path| rel.to_string_lossy().to_string())
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                cx.respond(Ok::<Vec<String>, String>(entries))
            }
            FsReq::FsExists(path) => cx.respond(Ok::<bool, String>(self.resolve(&path).exists())),
            FsReq::FsMetadata(path) => {
                let p = self.resolve(&path);
                match std::fs::metadata(&p) {
                    Ok(m) => cx.respond(Some(FileMeta {
                        size: m.len() as i64,
                        is_file: m.is_file(),
                        is_dir: m.is_dir(),
                    })),
                    Err(_) => cx.respond(None::<FileMeta>),
                }
            }
        }
    }
}

#[derive(FromCore)]
enum SgReq {
    #[core(name = "SgFind")]
    SgFind(String, String, String, Vec<String>),
    #[core(name = "SgRuleFind")]
    SgRuleFind(String, Value, Vec<String>),
}
struct StubSg;
impl EffectHandler for StubSg {
    type Request = SgReq;
    fn handle(
        &mut self,
        _req: SgReq,
        cx: &EffectContext,
    ) -> Result<tidepool_effect::Response, EffectError> {
        let empty: Vec<Value> = vec![];
        cx.respond(empty)
    }
}

#[derive(FromCore)]
enum HttpReq {
    #[core(name = "HttpGet")]
    HttpGet(String),
    #[core(name = "HttpPost")]
    HttpPost(String, Value),
}
struct StubHttp;
impl EffectHandler for StubHttp {
    type Request = HttpReq;
    fn handle(
        &mut self,
        _req: HttpReq,
        cx: &EffectContext,
    ) -> Result<tidepool_effect::Response, EffectError> {
        cx.respond(())
    }
}

#[derive(FromCore)]
enum ExecReq {
    #[core(name = "Run")]
    Run(String),
    #[core(name = "RunIn")]
    RunIn(String, String),
}
struct StubExec;
impl EffectHandler for StubExec {
    type Request = ExecReq;
    fn handle(
        &mut self,
        _req: ExecReq,
        cx: &EffectContext,
    ) -> Result<tidepool_effect::Response, EffectError> {
        cx.respond(Ok::<Proc, String>(Proc {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
        }))
    }
}

#[derive(FromCore)]
enum LlmReq {
    #[core(name = "LlmChat")]
    LlmChat(String),
    #[core(name = "LlmStructured")]
    LlmStructured(String, Value),
}
struct StubLlm;
impl EffectHandler for StubLlm {
    type Request = LlmReq;
    fn handle(
        &mut self,
        _req: LlmReq,
        cx: &EffectContext,
    ) -> Result<tidepool_effect::Response, EffectError> {
        cx.respond(String::from("stub"))
    }
}

#[derive(FromCore)]
enum AskReq {
    #[core(name = "Ask")]
    Ask(String),
}
struct StubAsk;
impl EffectHandler for StubAsk {
    type Request = AskReq;
    fn handle(
        &mut self,
        _req: AskReq,
        cx: &EffectContext,
    ) -> Result<tidepool_effect::Response, EffectError> {
        cx.respond(String::from("stub"))
    }
}

// ---------------------------------------------------------------------------
// The test — uses the REAL MCP preamble to reproduce the exact crash
// ---------------------------------------------------------------------------

/// Exact reproduction of the MCP eval crash using build_preamble + template_haskell.
/// This generates the identical Haskell module the MCP server produces.
#[test]
fn nested_mapm_readfile_full_mcp_preamble() {
    let decls = tidepool_mcp::standard_decls();
    let preamble = tidepool_mcp::build_preamble(&decls, true);
    let names: Vec<&str> = decls.iter().map(|e| e.type_name).collect();
    let stack = format!("'[{}]", names.join(", "));

    let user_code = r#"
crates <- listDirectory "." >>= liftEither
let rustCrates = filter (\d -> T.isPrefixOf "tidepool-" d) crates
stats <- mapM (\crate -> do
  files <- glob (crate <> "/src/**/*.rs") >>= liftEither
  total <- foldM (\acc f -> do
    content <- readFile f >>= liftEither
    pure (acc + length (T.lines content))) (0 :: Int) files
  pure (object ["crate" .= crate, "files" .= length files, "lines" .= total])) rustCrates
pure stats
"#;

    let full_module = tidepool_mcp::template_haskell(
        &preamble,
        &stack,
        &tidepool_mcp::wrap_do(user_code),
        "",
        "",
        None,
        Some(4096),
    );

    // Dump the generated module for debugging
    eprintln!(
        "=== Generated module ({} lines) ===",
        full_module.lines().count()
    );

    // workspace root = the crate's grandparent
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let lib_dir = workspace.join(".tidepool/lib");
    let mut harness = EvalHarness::new().with_stdlib();
    if lib_dir.exists() {
        harness = harness.with_include(lib_dir);
    }
    let harness = harness.with_effects_module();
    let handlers = frunk::hlist![
        StubConsole,
        StubKv,
        RealFs::new(),
        StubSg,
        StubHttp,
        StubExec,
        StubLlm,
        StubAsk
    ];
    let result = harness.run(&full_module, "result", handlers).into_result();

    match &result {
        Ok(val) => {
            let json = val.to_json();
            eprintln!("Success: {}", json);
            assert!(json.is_array(), "Expected array result, got: {}", json);
        }
        Err(e) => {
            panic!(
                "REPRODUCED: nested mapM + readFile with full MCP preamble crashed: {}\n\
                 This is the tag=255 GC forwarding pointer bug.",
                e
            );
        }
    }
}
