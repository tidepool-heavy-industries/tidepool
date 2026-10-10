use crate::*;
use tidepool_bridge::HaskellValue;
use tidepool_mcp::CapturedOutput;
use tidepool_repr::execution_schema::JsonLayout;
use tidepool_repr::{DataCon, DataConId, DataConTable};

pub(crate) fn response_value(r: tidepool_effect::Response, table: &DataConTable) -> HaskellValue {
    r.to_value(table)
        .expect("handler response should materialize")
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
    let fallbacks = tidepool_toolchain::toolchain::StdlibFallbacks {
        bundle: None,
        build_tree: Some(repo_root().join("bridge").join("haskell").join("lib")),
    };
    tidepool_toolchain::toolchain::locate_stdlib(&fallbacks)
        .expect("resolve the Haskell stdlib root")
        .dir
}

pub(crate) fn jit_test_source(code: &[&str]) -> String {
    let decls = tidepool_mcp::standard_decls();
    let preamble = tidepool_mcp::build_preamble(&decls, false);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let code_str = tidepool_mcp::wrap_do(&code.join("\n"));
    tidepool_mcp::template_haskell(&preamble, &stack, &code_str, "", "", None)
}

pub(crate) fn jit_eval(code: &[&str]) -> serde_json::Value {
    let source = jit_test_source(code);
    let include = prelude_include();
    let effects_dirs =
        tidepool_mcp::ensure_effects_module(&tidepool_mcp::standard_decls()).unwrap();
    let mut include_paths: Vec<&std::path::Path> = vec![include.as_path()];
    include_paths.push(effects_dirs.core.as_path());
    include_paths.push(effects_dirs.orchestration.as_path());
    let kv_path = std::env::temp_dir().join("tidepool_jit_test_kv.json");
    let cwd = repo_root();
    let captured = CapturedOutput::new();
    let mut handlers = frunk::hlist![
        ConsoleHandler,
        KvHandler::new(
            &tidepool_atomic_write::DirectoryAnchor::open_existing(std::env::temp_dir()).unwrap(),
            kv_path.file_name().unwrap()
        )
        .unwrap(),
        FsReadHandler::new(cwd.clone()),
        FsWriteHandler::new(cwd.clone()),
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
    let mut t = tidepool_test_data::standard_datacon_table();
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
            t.insert_checked(DataCon {
            identity: tidepool_repr::execution_schema::SymbolIdentity { unit: "fixture".into(), module: "Fixture".into(), namespace: "constructor".into(), occurrence: (parsed.name).clone(), record_parent: None },
                id: DataConId(next_id),
                name: parsed.name,
                tag: 1,
                rep_arity: parsed.arity,
                field_bangs: vec![],
                qualified_name: None,
                type_name: String::new(),
            }).expect("valid fixture metadata");
            next_id += 1;
        }
    }

    let exec_schema = tidepool_protocol::effects::exec::exec();
    let exec_constructors: Vec<_> = exec_schema
        .errors
        .as_ref()
        .unwrap()
        .variants
        .iter()
        .map(|variant| (variant.ctor, variant.fields.len() as u32))
        .collect();

    let response_extras: &[(&str, u32)] = &[
        ("WorktreeAuthorityDenied", 1),
        ("WorktreeUnauthorized", 1),
        ("WorktreeId", 1),
        ("Object", 1),
        ("Array", 1),
        ("String", 1),
        ("Number", 1),
        // JSON numbers ride Number(Scientific coeff exp) — the coefficient an
        // exact Integer (IS/IP/IN), the exponent an Int (BUG-8, tidepool/bridge/src/json_builder.rs).
        // Must be in the table for kvInfo and any handler that returns serde_json
        // objects containing numeric fields.
        ("Scientific", 2),
        ("IS", 1),
        ("IP", 1),
        ("IN", 1),
        ("Bool", 1),
        ("Null", 0),
        ("Bin", 5),
        ("Tip", 0),
        ("()", 0),
        ("(,,)", 3),
        // Either — the per-file readGlob surface + the WriteCas result + #335 typed failures.
        ("Right", 1),
        ("Left", 1),
        ("FileRead", 2),
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
        ("CommitDeltas", 2),
        // Exec/Fs bridged records (Proc/Hit/FileMeta) — same reason as the
        // Git records above: they aren't GADT constructors, so the
        // decl-constructor loop above doesn't pick them up.
        ("Proc", 3),
        ("Hit", 3),
        ("FileMeta", 3),
        // Time effect response types
        ("UTCTime", 1),
    ];
    for &(name, arity) in response_extras
        .iter()
        .chain(FsError::TEST_CONSTRUCTORS)
        .chain(KvError::TEST_CONSTRUCTORS)
        .chain(&exec_constructors)
        .chain(HttpError::TEST_CONSTRUCTORS)
        .chain(GitError::TEST_CONSTRUCTORS)
        .chain(LlmError::TEST_CONSTRUCTORS)
    {
        if t.get_by_name(name).is_some() {
            continue;
        }
        t.insert_checked(DataCon {
            identity: tidepool_repr::execution_schema::SymbolIdentity { unit: "fixture".into(), module: "GHC.Tuple".into(), namespace: "constructor".into(), occurrence: (name.into()).clone(), record_parent: None },
            id: DataConId(next_id),
            name: name.into(),
            tag: 1,
            rep_arity: arity,
            field_bangs: vec![],
            qualified_name: match name {
                "()" => Some("GHC.Tuple.()".into()),
                "(,,)" => Some("GHC.Tuple.(,,)".into()),
                _ => None,
            },
            type_name: String::new(),
        }).expect("valid fixture metadata");
        next_id += 1;
    }
    let role = |name: &str| {
        t.get_by_name(name)
            .unwrap_or_else(|| panic!("missing JSON test constructor {name}"))
    };
    let layout = JsonLayout {
        object: role("Object"),
        array: role("Array"),
        string: role("String"),
        number: role("Number"),
        bool_: role("Bool"),
        null: role("Null"),
        map_bin: role("Bin"),
        map_tip: role("Tip"),
        true_: role("True"),
        false_: role("False"),
        cons: role(":"),
        nil: role("[]"),
        scientific: role("Scientific"),
        integer_small: role("IS"),
        integer_positive: role("IP"),
        integer_negative: role("IN"),
        text: role("Text"),
        int: role("I#"),
    };
    t.with_json_layout(Some(layout))
}

pub(crate) fn assert_is_cons_list(val: &HaskellValue, table: &DataConTable) {
    match val {
        HaskellValue::Con(id, fields) => {
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
