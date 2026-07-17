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
//! reads through the full 10-effect stack + Library.hs preamble.

use std::path::{Path, PathBuf};
use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_testing::eval_harness::mock::{self, FsReq};
use tidepool_testing::eval_harness::EvalHarness;

// ---------------------------------------------------------------------------
// Effect handlers — real Fs, mocks for everything else
// ---------------------------------------------------------------------------

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
                    Ok(m) => cx.respond(Some(tidepool_bridge_effects::FileMeta {
                        size: m.len() as i64,
                        is_file: m.is_file(),
                        is_dir: m.is_dir(),
                    })),
                    Err(_) => cx.respond(None::<tidepool_bridge_effects::FileMeta>),
                }
            }
        }
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
        mock::MockConsole,
        mock::MockKv::new(),
        RealFs::new(),
        mock::MockHttp,
        mock::MockExec,
        mock::MockLsp,
        mock::MockLlm,
        mock::MockGit,
        mock::MockTime,
        mock::MockAsk
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
