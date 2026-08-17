//! The inner-loop guard: every committed generated file equals what the schema
//! generates right now.
//!
//! This test lives HERE, in the leaf schema crate, rather than beside the files
//! it guards — deliberately. `tidepool-protocol` is outside
//! `.config/nextest.toml`'s `default-filter` exclusion set and needs no GHC, so
//! a bare `cargo nextest run` reaches it. The workspace's older instance of this
//! idiom (`tidepool-handlers/tests/bridged_records.rs`) sits inside an excluded
//! package and therefore never runs on the inner loop despite being pure string
//! comparison. A check nothing runs is not a check.
//!
//! Set `TIDEPOOL_REGEN_PROTOCOL=1` to rewrite the committed files instead of
//! asserting on them.

use std::path::PathBuf;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tidepool-protocol must live one level under the workspace root")
        .to_path_buf()
}

#[test]
fn generated_files_are_current() {
    let regen = std::env::var_os("TIDEPOOL_REGEN_PROTOCOL").is_some();
    let root = workspace_root();
    let mut stale = Vec::new();

    for f in tidepool_protocol::generated_files() {
        let path = root.join(&f.path);
        let current = std::fs::read_to_string(&path).ok();
        if current.as_deref() == Some(f.contents.as_str()) {
            continue;
        }
        if regen {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&path, &f.contents).unwrap();
            continue;
        }
        stale.push(f.path.clone());
    }

    assert!(
        stale.is_empty(),
        "these committed files are stale vs the schema:\n  {}\n\
         regenerate with `cargo run -p tidepool-protocol --bin tidepool-protocol-gen` \
         (or `TIDEPOOL_REGEN_PROTOCOL=1 cargo test -p tidepool-protocol`)",
        stale.join("\n  ")
    );
}

/// The schema must be internally consistent before it generates anything —
/// a verb tagged with an error ADT the effect does not declare, a helper
/// wrapping a constructor that does not exist, an arity mismatch between a
/// helper's parameters and its verb's arguments. All of these are GENERATION
/// failures by design: PRD 22's acceptance line is that a malformed verb fails
/// generation, not runtime.
#[test]
fn every_migrated_effect_validates() {
    for e in tidepool_protocol::effects::all() {
        if let Err(problems) = e.validate() {
            panic!("{} is invalid:\n  {}", e.name, problems.join("\n  "));
        }
    }
}

/// The independent pin — the third layer of the `bridged_records` idiom, and
/// the one that matters most.
///
/// These are LITERAL strings in the test source, not read from any golden file
/// and not derived from the schema. Their whole job is to stay true when
/// someone regenerates the goldens blindly: a change to the renderer that also
/// rewrites every golden would still have to get past these. They are the bytes
/// that cross to Haskell, and they are a compile-cache key — a single changed
/// byte invalidates every cached compile for every user.
#[test]
fn exec_contract_text_is_pinned() {
    let exec = tidepool_protocol::effects::exec::exec();

    assert_eq!(
        exec.constructor_signatures(),
        vec![
            "Run :: Text -> Exec (Either ExecError Proc)",
            "RunIn :: Text -> Text -> Exec (Either ExecError Proc)",
            "RunArgv :: [Text] -> Exec (Either ExecError Proc)",
        ]
    );

    assert_eq!(
        exec.type_def_texts(),
        vec![concat!(
            "data ExecError = ExecSpawn Text | ExecBadDir Text deriving (Show, Eq)\n",
            "instance ToJSON ExecError where\n",
            "  toJSON e = case e of\n",
            "    ExecSpawn detail -> object [\"tag\" .= (\"ExecSpawn\" :: Text), \"detail\" .= detail]\n",
            "    ExecBadDir detail -> object [\"tag\" .= (\"ExecBadDir\" :: Text), \"detail\" .= detail]\n",
        )]
    );

    assert_eq!(
        exec.helper_texts(),
        vec![
            concat!(
                "-- | Run a shell command; returns a `Proc` record {exitCode, stdout, stderr}\n",
                "-- (use `ok p` for the zero-exit check). Failure is TYPED (#335): `Left\n",
                "-- (ExecSpawn _)` when the process can't be spawned, `Left (ExecBadDir _)`\n",
                "-- for `runIn` with a bad/escaping directory. A nonzero EXIT is NOT a\n",
                "-- failure — inspect `p.exitCode`. Natural spelling: `Right p <- run cmd`.\n",
                "run :: Text -> M (Either ExecError Proc)\n",
                "run = send . Run",
            ),
            concat!(
                "runIn :: Text -> Text -> M (Either ExecError Proc)\n",
                "runIn dir cmd = send (RunIn dir cmd)",
            ),
            concat!(
                "runArgv :: [Text] -> M (Either ExecError Proc)\n",
                "runArgv = send . RunArgv",
            ),
        ]
    );

    assert_eq!(
        exec.description_text(),
        "Run shell commands and capture output."
    );
    assert_eq!(
        exec.extra_imports,
        &[
            "import qualified Tidepool.Shell as Shell",
            "import Tidepool.Shell (sh)",
            "import qualified Tidepool.Cargo as Cargo",
        ]
    );
    assert!(exec.prompt_card.is_none());
    assert!(exec.type_params.is_empty());
    assert!(exec.default_row_args.is_empty());
    assert!(!exec.helpers_row_polymorphic);
}
