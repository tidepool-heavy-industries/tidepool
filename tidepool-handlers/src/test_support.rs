use crate::*;
use tidepool_eval::value::Value;
use tidepool_mcp::CapturedOutput;
use tidepool_repr::{DataCon, DataConId, DataConTable};

/// Unwrap a handler Response: Complete passes through; Stream drains into
/// the equivalent cons-list Value.
pub(crate) fn response_value(r: tidepool_effect::Response, table: &DataConTable) -> Value {
    match r {
        tidepool_effect::Response::Complete(v) => v,
        tidepool_effect::Response::Stream(s) => {
            let (mut source, cons_id, nil_id) = s.into_parts();
            let mut items = Vec::new();
            while let Some(i) = source.next_value(table) {
                items.push(i.expect("stream element conversion"));
            }
            let mut acc = Value::Con(nil_id, vec![]);
            for i in items.into_iter().rev() {
                acc = Value::Con(cons_id, vec![i, acc]);
            }
            acc
        }
    }
}

pub(crate) fn repo_root() -> std::path::PathBuf {
    let mut dir = std::env::current_dir().unwrap();
    loop {
        if dir.join(".git").exists() {
            return dir;
        }
        if !dir.pop() {
            panic!("not inside a git repo");
        }
    }
}

pub(crate) fn prelude_include() -> std::path::PathBuf {
    let mut dir = repo_root();
    dir.push("haskell");
    dir.push("lib");
    dir
}

pub(crate) fn jit_test_source(code: &[&str]) -> String {
    let decls = tidepool_mcp::standard_decls();
    let preamble = tidepool_mcp::build_preamble(&decls, false);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let code_str = tidepool_mcp::wrap_do(&code.join("\n"));
    tidepool_mcp::template_haskell(&preamble, &stack, &code_str, "", "", None, None)
}

pub(crate) fn jit_eval(code: &[&str]) -> serde_json::Value {
    let source = jit_test_source(code);
    let include = prelude_include();
    let effects_dir = tidepool_mcp::ensure_effects_module(&tidepool_mcp::standard_decls()).unwrap();
    let include_paths: Vec<&std::path::Path> = vec![include.as_path(), effects_dir.as_path()];
    let kv_path = std::env::temp_dir().join("tidepool_jit_test_kv.json");
    let cwd = repo_root();
    let captured = CapturedOutput::new();
    let mut handlers = frunk::hlist![
        ConsoleHandler,
        KvHandler::new(kv_path),
        FsHandler::new(cwd.clone()),
        HttpHandler,
        ExecHandler::new(cwd.clone()),
        LlmHandler::new("ollama:llama3.2".to_string())
    ];
    let result = tidepool_runtime::compile_and_run(
        &source,
        "result",
        &include_paths,
        &mut handlers,
        &captured,
    );
    match result {
        Ok(eval_result) => eval_result.to_json(),
        Err(e) => panic!("JIT eval failed: {:?}", e),
    }
}

/// Build a DataConTable with standard types + all effect constructors.
pub(crate) fn full_effect_test_table() -> DataConTable {
    let mut t = tidepool_testing::gen::datacon_table::standard_datacon_table();
    let mut decls = tidepool_mcp::standard_decls();
    decls.push(tidepool_mcp::meta_decl());
    let mut next_id = 100u64;

    for decl in &decls {
        for con_str in decl.constructors {
            let parsed = tidepool_mcp::parse_constructor(con_str)
                .unwrap_or_else(|e| panic!("bad constructor decl: {e}"));
            if t.get_by_name(&parsed.name).is_some() {
                continue;
            }
            t.insert(DataCon {
                id: DataConId(next_id),
                name: parsed.name,
                tag: 1,
                rep_arity: parsed.arity,
                field_bangs: vec![],
                qualified_name: None,
            });
            next_id += 1;
        }
    }

    let response_extras: &[(&str, u32)] = &[
        ("Object", 1),
        ("Array", 1),
        ("String", 1),
        ("Number", 1),
        // Exact-integer JSON numbers ride NumberI (BUG-8 fix in bridge/json.rs).
        // Must be in the table for kvInfo and any handler that returns serde_json
        // objects containing integer fields.
        ("NumberI", 1),
        ("Bool", 1),
        ("Null", 0),
        ("Bin", 5),
        ("Tip", 0),
        ("()", 0),
        ("(,,)", 3),
        // Either — the per-file readGlob surface + the WriteCas result + #335 typed failures.
        ("Right", 1),
        ("Left", 1),
        // #335 Fs typed-failure ADT + the readGlob record (these live in the Fs
        // effect's type_defs, not its GADT constructors, so they aren't picked up
        // by the decl-constructor loop above).
        ("FsNotFound", 1),
        ("FsNotUtf8", 1),
        ("FsSandbox", 1),
        ("FsBadRegex", 1),
        ("FsIo", 1),
        ("FileRead", 2),
        // #335 rest-wave typed-failure ADTs (Exec/Http/Git/Llm/Lsp) — same
        // reason as the Fs constructors above: they live in each effect's
        // type_defs, not its GADT constructors.
        ("ExecSpawn", 1),
        ("ExecBadDir", 1),
        ("HttpInvalidUrl", 1),
        ("HttpRestricted", 1),
        ("HttpNetwork", 1),
        ("HttpStatus", 2),
        ("HttpBadJson", 1),
        ("GitBadRevspec", 1),
        ("GitFailed", 2),
        ("LlmApi", 1),
        ("LlmRefusal", 1),
        ("LlmBudget", 0),
        ("LspDaemonDown", 1),
        ("Match", 5),
        ("Rust", 0),
        ("Python", 0),
        ("TypeScript", 0),
        ("JavaScript", 0),
        ("Go", 0),
        ("Java", 0),
        ("C", 0),
        ("Cpp", 0),
        ("Haskell", 0),
        ("Nix", 0),
        ("Html", 0),
        ("Css", 0),
        ("Json", 0),
        ("Yaml", 0),
        ("Toml", 0),
        // Git effect response types
        ("Commit", 5),
        ("StatusEntry", 2),
        ("FileDelta", 4),
        // Exec/Fs bridged records (Proc/Hit/FileMeta) — same reason as the
        // Git records above: they aren't GADT constructors, so the
        // decl-constructor loop above doesn't pick them up.
        ("Proc", 3),
        ("Hit", 3),
        ("FileMeta", 3),
        // Time effect response types
        ("UTCTime", 1),
    ];
    for &(name, arity) in response_extras {
        if t.get_by_name(name).is_some() {
            continue;
        }
        t.insert(DataCon {
            id: DataConId(next_id),
            name: name.into(),
            tag: 1,
            rep_arity: arity,
            field_bangs: vec![],
            qualified_name: None,
        });
        next_id += 1;
    }
    t
}

pub(crate) fn assert_is_haskell_list(val: &Value, table: &DataConTable) {
    match val {
        Value::Con(id, fields) => {
            let name = table.name_of(*id).unwrap();
            match name {
                "[]" => assert!(fields.is_empty()),
                ":" => {
                    assert_eq!(fields.len(), 2, "cons cell should have 2 fields");
                    assert_is_json_value(&fields[0], table);
                    assert_is_haskell_list(&fields[1], table);
                }
                other => panic!("Expected list constructor, got {}", other),
            }
        }
        other => panic!("Expected Con (list), got {:?}", other),
    }
}

pub(crate) fn assert_is_json_value(val: &Value, table: &DataConTable) {
    match val {
        Value::Con(id, _) => {
            let name = table.name_of(*id).unwrap();
            assert!(
                ["Object", "Array", "String", "Number", "Bool", "Null"].contains(&name),
                "Expected JSON Value constructor, got {}",
                name
            );
        }
        _ => panic!("Expected Con (JSON Value), got {:?}", val),
    }
}

pub(crate) fn assert_is_cons_list(val: &Value, table: &DataConTable) {
    match val {
        Value::Con(id, fields) => {
            let name = table.name_of(*id).unwrap();
            match name {
                "[]" => assert!(fields.is_empty()),
                ":" => {
                    assert_eq!(fields.len(), 2, "cons cell should have 2 fields");
                    assert_is_cons_list(&fields[1], table);
                }
                other => panic!("Expected list constructor, got {}", other),
            }
        }
        other => panic!("Expected Con (list), got {:?}", other),
    }
}
