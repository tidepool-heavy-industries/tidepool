//! Schema validation and narrow rendering pins; generated consumers are build outputs.

#[test]
fn authored_projection_hides_capability_constructors_and_record_selectors() {
    use tidepool_protocol::gen::decl_rs;
    use tidepool_protocol::schema::{SumVariant, TypeShape, VariantFields};
    use tidepool_protocol::types::RecordField;
    use tidepool_protocol::HsType;

    let workspace = tidepool_protocol::effects::worktree::worktree();
    let mut scopes = tidepool_protocol::effects::resource_scope::resource_scopes();
    // Exercise a private record-shaped constructor too: its selector must not
    // remain an independent authored accessor after hiding the data type.
    scopes.type_defs[0].shape = TypeShape::Sum {
        variants: vec![SumVariant {
            ctor: "PrivateScopeToken",
            fields: VariantFields::Named(vec![RecordField {
                hs_name: "privateScopeIdentity",
                rust_name: "identity",
                ty: HsType::Int,
                doc: &[],
            }]),
            doc: &[],
        }],
    };
    let generated = decl_rs::module_index(&[workspace, scopes]).contents;
    assert!(generated.contains("\"WorkspaceHandle(..)\""));
    assert!(generated.contains("\"Scope(..)\""));
    assert!(generated.contains("\"PrivateScopeToken\""));
    assert!(generated.contains("\"privateScopeIdentity\""));
    assert!(generated.contains("AUTHORED_ABSTRACT_TYPES: &[&str] = &[\"WorkspaceHandle\"]"));
}

#[test]
fn lexical_scope_delimiter_keeps_body_and_cleanup_outcomes_separate() {
    use tidepool_protocol::schema::{HandlingClass, RustBinding};

    let scopes = tidepool_protocol::effects::resource_scope::resource_scopes();
    assert_eq!(
        scopes.constructor_signatures(),
        [
            "ScopeRunWith :: (Int -> Eff bodyEffs ()) -> ResourceScopes (Either ScopeFailure (), Either CleanupError ())",
            "ScopeDoneWith :: Int -> ResourceScopes ()",
        ]
    );
    assert_eq!(scopes.verbs[0].args[0].rust, RustBinding::HaskellValue);
    assert!(scopes
        .verbs
        .iter()
        .all(|verb| verb.handling == HandlingClass::Actor));
    assert!(!scopes.authored_surface.includes_type_def("Scope"));
    assert!(!scopes.authored_surface.includes_verb("ScopeRunWith"));
    assert!(!scopes.authored_surface.includes_verb("ScopeDoneWith"));

    let generated = tidepool_protocol::actor_generated_files();
    assert!(generated
        .iter()
        .any(|file| file.path == "exomonad/actor/src/generated/resource_scopes.rs"));
}

/// The suspension-decode roster must be internally consistent too — same
/// discipline as [`every_migrated_effect_validates`], over the disjoint
/// roster.
#[test]
fn every_suspension_roster_effect_validates() {
    for e in tidepool_protocol::effects::suspension_roster() {
        if let Err(problems) = e.validate() {
            panic!("{} is invalid:\n  {}", e.name, problems.join("\n  "));
        }
    }
}

/// The schema must be internally consistent before it generates anything —
/// a verb tagged with an error ADT the effect does not declare, a helper
/// wrapping a constructor that does not exist, an arity mismatch between a
/// helper's parameters and its verb's arguments. All of these are GENERATION
/// failures by design: a malformed verb fails generation, not runtime.
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
            "data ExecError = ExecSpawn Text | ExecBadDir Text | ExecTimeout Text | ExecOutput Text | ExecWait Text deriving (Show, Eq)\n",
            "instance ToJSON ExecError where\n",
            "  toJSON e = case e of\n",
            "    ExecSpawn detail -> object [\"tag\" .= (\"ExecSpawn\" :: Text), \"detail\" .= detail]\n",
            "    ExecBadDir detail -> object [\"tag\" .= (\"ExecBadDir\" :: Text), \"detail\" .= detail]\n",
            "    ExecTimeout detail -> object [\"tag\" .= (\"ExecTimeout\" :: Text), \"detail\" .= detail]\n",
            "    ExecOutput detail -> object [\"tag\" .= (\"ExecOutput\" :: Text), \"detail\" .= detail]\n",
            "    ExecWait detail -> object [\"tag\" .= (\"ExecWait\" :: Text), \"detail\" .= detail]\n",
        )]
    );

    assert_eq!(
        exec.helper_texts(),
        vec![
            concat!(
                "-- | Run a shell command; returns a `Proc` record {exitCode, stdout, stderr}\n",
                "-- (use `ok p` for the zero-exit check). Failure is TYPED (#335): `Left\n",
                "-- (ExecSpawn _)` when the process can't be spawned, `Left (ExecBadDir _)`\n",
                "-- for `runIn` with a bad/escaping directory, `Left (ExecTimeout _)` when\n",
                "-- execution or output draining outran its timeout, `Left (ExecOutput _)` for a read failure,\n",
                "-- and `Left (ExecWait _)` if its exit status cannot be collected. A nonzero\n",
                "-- EXIT is NOT a failure — inspect `p.exitCode`. Natural spelling:\n",
                "-- `Right p <- run cmd`.\n",
                "run :: forall effs. Member Exec effs => Text -> Eff effs (Either ExecError Proc)\n",
                "run = send . Run",
            ),
            concat!(
                "runIn :: forall effs. Member Exec effs => Text -> Text -> Eff effs (Either ExecError Proc)\n",
                "runIn dir cmd = send (RunIn dir cmd)",
            ),
            concat!(
                "runArgv :: forall effs. Member Exec effs => [Text] -> Eff effs (Either ExecError Proc)\n",
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
}

/// The same independent pin as [`exec_contract_text_is_pinned`], for Journal.
#[test]
fn journal_contract_text_is_pinned() {
    let journal = tidepool_protocol::effects::journal::journal();

    assert_eq!(
        journal.constructor_signatures(),
        vec![
            "RecordStep :: Text -> Text -> Value -> Journal ()",
            "TraceStep :: Text -> Text -> Value -> Journal ()",
        ]
    );

    assert!(journal.type_def_texts().is_empty());

    assert_eq!(
        journal.helper_texts(),
        vec![
            concat!(
            "-- | Append one durable journal entry. `kind` and `key` are\n",
            "-- caller-chosen labels; `payload` is an opaque JSON value. Flushed\n",
            "-- immediately; append-only — never rewritten or compacted.\n",
            "record :: forall effs. Member Journal effs => Text -> Text -> Value -> Eff effs ()\n",
            "record kind key payload = send (RecordStep kind key payload)",
        ),
            concat!(
            "-- | Append one observability entry to the run's sibling TRACE stream: ",
            "decision narration and telemetry. `stage` ",
            "names what kind of moment this is (e.g. \"resume-verdict\", \"park\"); ",
            "`key` is the branch or unit it concerns; `payload` is an opaque JSON ",
            "value whose shape may evolve freely. The handler stamps a timestamp ",
            "on every line, so trace timestamps can be correlated with journal entries.\n",
            "trace :: forall effs. Member Journal effs => Text -> Text -> Value -> Eff effs ()\n",
            "trace stage key payload = send (TraceStep stage key payload)",
        )
        ]
    );

    assert_eq!(
        journal.description_text(),
        "Durable append-only run journal: a resident harness records completed \
         steps as it happens, mid-loop, so progress survives a crash. `record kind key \
         payload` appends ONE entry — `kind` and `key` are caller-chosen labels \
         (e.g. a step kind and the branch or task it concerns), `payload` is an \
         opaque JSON value. Every append is flushed immediately; the journal is \
         append-only forever — there is no rewrite or compaction verb. \
         `trace stage key payload` appends ONE observability entry to the run's \
         sibling TRACE stream instead, timestamped at the handler for decision \
         narration and telemetry; timestamps can be correlated with journal entries."
    );
    assert!(journal.extra_imports.is_empty());
    assert!(journal.prompt_card.is_none());
    assert!(journal.type_params.is_empty());
    assert!(journal.default_row_args.is_empty());
}
