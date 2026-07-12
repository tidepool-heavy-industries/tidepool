//! Shared server machinery between the eval server (`tidepool-mcp`'s own
//! [`crate::server`]) and the session server (`tidepool-repl`).
//!
//! The two servers keep DISTINCT concurrency models — spawned-per-eval
//! continuations here vs. a resident-worker session state machine there —
//! and this module does not touch that boundary. What lives here is the
//! machinery that turned out byte-for-byte identical on both sides once the
//! concurrency-specific driving loops were subtracted out: response-envelope
//! formatting, continuation-id minting, MCP tool/schema boilerplate, and
//! process startup diagnostics.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use rmcp::model::Tool;

use crate::validate::Violation;

/// Prepend captured output (if any) to a result body:
/// `"## Output\n<lines>\n\n## Result\n<body>"`. `body` is returned unchanged
/// when there is no captured output.
pub fn format_with_output(output: &[String], body: &str) -> String {
    if output.is_empty() {
        return body.to_string();
    }
    let mut s = String::from("## Output\n");
    for line in output {
        s.push_str(line);
        s.push('\n');
    }
    s.push_str("\n## Result\n");
    s.push_str(body);
    s
}

/// Build the `{"suspended": true, "continuation_id", "prompt", ...}` envelope
/// for an Ask suspension. A "schema" key in `meta` is hoisted to the top
/// level (it arms resume validation); everything else rides under "meta"
/// verbatim. Returns the envelope alongside the extracted schema, which the
/// caller stores as the continuation's `expected_schema`.
pub fn build_suspension_envelope(
    continuation_id: &str,
    prompt: &str,
    meta: Option<serde_json::Value>,
) -> (serde_json::Value, Option<serde_json::Value>) {
    let mut json_obj = serde_json::json!({
        "suspended": true,
        "continuation_id": continuation_id,
        "prompt": prompt,
    });
    let mut expected_schema = None;
    match meta {
        Some(serde_json::Value::Object(mut meta_map)) => {
            if let Some(obj) = json_obj.as_object_mut() {
                if let Some(schema) = meta_map.remove("schema") {
                    obj.insert("schema".into(), schema.clone());
                    expected_schema = Some(schema);
                }
                if !meta_map.is_empty() {
                    obj.insert("meta".into(), serde_json::Value::Object(meta_map));
                }
            }
        }
        Some(other) => {
            if let Some(obj) = json_obj.as_object_mut() {
                obj.insert("meta".into(), other);
            }
        }
        None => {}
    }
    (json_obj, expected_schema)
}

/// The error-result text for a resume reply that failed schema validation:
/// violations + schema + continuation_id as a JSON body, prefixed with a
/// message naming the caller's own resume/abort tool names (`"resume"`/
/// `"abort"` for the eval server, `"session_resume"`/`"session_abort"` for
/// the session server) so the retry hint points at the right tool. The
/// continuation is NOT consumed — callers must leave it in place.
pub fn validation_failed_body(
    resume_tool: &str,
    abort_tool: &str,
    violations: &[Violation],
    schema: Option<&serde_json::Value>,
    continuation_id: &str,
) -> String {
    let body = serde_json::json!({
        "validation_failed": true,
        "violations": violations.iter().map(Violation::to_json).collect::<Vec<_>>(),
        "schema": schema,
        "continuation_id": continuation_id,
        "continuation_not_consumed": true,
    });
    format!(
        "Response does not match the suspension's schema. Call {resume_tool} again with the \
         same continuation_id and a corrected response (or {abort_tool}).\n{body}"
    )
}

/// Mint the next id from a counter with the given prefix (`cont_1`,
/// `scont_1`, ...). The prefix is what lets a caller tell an eval-server
/// continuation id apart from a session-server one at a glance.
pub fn mint_id(counter: &AtomicU64, prefix: &str) -> String {
    let id = counter.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}_{id}")
}

/// Convert a `schemars::Schema` into the `Arc<Map<...>>` shape `rmcp::Tool`
/// wants for `input_schema`.
pub fn schema_to_map(
    schema: schemars::Schema,
) -> Result<Arc<serde_json::Map<String, serde_json::Value>>, String> {
    let json =
        serde_json::to_value(&schema).map_err(|e| format!("failed to serialize schema: {e}"))?;
    match json {
        serde_json::Value::Object(o) => Ok(Arc::new(o)),
        _ => Ok(Arc::new(serde_json::Map::new())),
    }
}

/// Build an `rmcp::Tool` with the common boilerplate fields defaulted
/// (`title`/`output_schema`/`annotations`/`icons`/`meta`/`execution` are all
/// `None` on every tool either server advertises).
pub fn make_tool(
    name: &str,
    description: &str,
    input_schema: Arc<serde_json::Map<String, serde_json::Value>>,
) -> Tool {
    Tool {
        name: name.to_string().into(),
        title: None,
        description: Some(description.to_string().into()),
        input_schema,
        output_schema: None,
        annotations: None,
        icons: None,
        meta: None,
        execution: None,
    }
}

/// Install the process-wide diagnostics surface both server binaries need at
/// startup: the `tracing` subscriber (RUST_LOG-driven, stderr-writing) and
/// the JIT subsystems' own `log`-crate diagnostics (`tidepool::calls` /
/// `scope` / `heap` / `effects` / `fp`, routed to stderr via `env_logger`,
/// independent of the `tracing` subscriber). Does NOT install the JIT signal
/// handler (`tidepool_codegen::signal_safety::install()`) — callers install
/// that themselves, since the eval server installs it a second time per-eval
/// thread and the two call sites should stay visually distinct.
pub fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();
    tidepool_codegen::debug::init_logging();
}

/// Load `.tidepool/secrets/*_API_KEY` (project, then global) into the process
/// environment, logging what was loaded and what was skipped. Must run
/// before any effect handler reads the env.
pub fn load_secrets_logged() {
    let report = tidepool_runtime::paths::load_secrets();
    for name in &report.loaded {
        tracing::info!("loaded {name} from secrets dir");
    }
    for skipped in &report.ignored {
        tracing::info!("secrets: {skipped} ignored (bad name, empty, or already set)");
    }
}

/// Resolve the layered verb-library dirs for the GHC include path: the
/// nearest project `.tidepool/lib` (if any), followed by the user-global
/// dirs — GHC first-match-wins order, so a project `Library`/module shadows
/// the global one.
pub fn resolve_lib_dirs(project_root: Option<&Path>) -> Vec<PathBuf> {
    let mut lib_dirs = Vec::new();
    if let Some(root) = project_root {
        let project_lib = root.join(".tidepool").join("lib");
        if project_lib.is_dir() {
            lib_dirs.push(project_lib);
        }
    }
    lib_dirs.extend(tidepool_runtime::paths::global_lib_dirs());
    lib_dirs
}

/// Whether any of `dirs` defines a `Library.hs` facade — the signal that
/// `import Library` should be auto-added to the preamble.
pub fn has_library_facade(dirs: &[PathBuf]) -> bool {
    dirs.iter().any(|d| d.join("Library.hs").exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_with_output_empty_is_passthrough() {
        assert_eq!(format_with_output(&[], "result"), "result");
    }

    #[test]
    fn format_with_output_prepends_section() {
        let out = format_with_output(&["a".into(), "b".into()], "result");
        assert_eq!(out, "## Output\na\nb\n\n## Result\nresult");
    }

    #[test]
    fn suspension_envelope_hoists_schema() {
        let (env, schema) = build_suspension_envelope(
            "cont_1",
            "pick one",
            Some(serde_json::json!({"schema": {"type": "string"}, "moves": ["a"]})),
        );
        assert_eq!(env["suspended"], true);
        assert_eq!(env["continuation_id"], "cont_1");
        assert_eq!(env["prompt"], "pick one");
        assert_eq!(env["schema"], serde_json::json!({"type": "string"}));
        assert_eq!(env["meta"]["moves"], serde_json::json!(["a"]));
        assert!(env.get("moves").is_none());
        assert_eq!(schema, Some(serde_json::json!({"type": "string"})));
    }

    #[test]
    fn suspension_envelope_no_meta() {
        let (env, schema) = build_suspension_envelope("cont_2", "hi", None);
        assert_eq!(env["prompt"], "hi");
        assert!(env.get("schema").is_none());
        assert!(env.get("meta").is_none());
        assert_eq!(schema, None);
    }

    #[test]
    fn mint_id_uses_prefix_and_increments() {
        let counter = AtomicU64::new(1);
        assert_eq!(mint_id(&counter, "cont"), "cont_1");
        assert_eq!(mint_id(&counter, "cont"), "cont_2");
    }

    #[test]
    fn has_library_facade_checks_any_dir() {
        let tmp = std::env::temp_dir().join(format!(
            "tidepool-server-common-test-{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&tmp);
        assert!(!has_library_facade(std::slice::from_ref(&tmp)));
        std::fs::write(tmp.join("Library.hs"), "module Library where").unwrap();
        assert!(has_library_facade(std::slice::from_ref(&tmp)));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
