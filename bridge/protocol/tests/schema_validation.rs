//! Schema validation and narrow rendering pins; generated consumers are build outputs.

#[test]
fn record_actor_workspace_binding_uses_its_declared_external_wire_type() {
    use tidepool_protocol::schema::RustBinding;

    let mut actor = tidepool_protocol::effects::actor::actor();
    let start = actor
        .verbs
        .iter()
        .find(|verb| verb.ctor == "ActorStartWith")
        .unwrap();
    let workspace = start
        .args
        .iter()
        .find(|arg| arg.name == "workspace")
        .unwrap();
    assert_eq!(
        workspace
            .rust
            .rust_type(&workspace.ty, "ActorStartWith::workspace", &actor),
        "Option<tidepool_bridge_effects::WtWorkspaceHandle>"
    );

    actor
        .verbs
        .iter_mut()
        .find(|verb| verb.ctor == "ActorStartWith")
        .unwrap()
        .args
        .iter_mut()
        .find(|arg| arg.name == "workspace")
        .unwrap()
        .rust = RustBinding::Derived;
    let errors = actor.validate().unwrap_err();
    assert!(errors.iter().any(|error| {
        error == "Actor::ActorStartWith::workspace: Maybe WorkspaceHandle needs an explicit RustBinding"
    }));
}

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
            "ScopeRunWith :: RequestSite '[] (Either ScopeFailure (), Either CleanupError ()) -> (Int -> Eff bodyEffs ()) -> ResourceScopes (Either ScopeFailure (), Either CleanupError ())",
            "ScopeDoneWith :: Int -> ResourceScopes ()",
        ]
    );
    assert_eq!(scopes.verbs[0].args[0].rust, RustBinding::External);
    assert_eq!(scopes.verbs[0].args[1].rust, RustBinding::HaskellValue);
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

#[test]
fn green_completion_requires_integer_thread_identity_after_valid_control() {
    use tidepool_protocol::HsType;
    let green = tidepool_protocol::effects::green::green();
    green.validate().expect("valid emitted Green control");
    for wrong_type in [HsType::Text, HsType::Unit, HsType::list(HsType::Int)] {
        let mut malformed = green.clone();
        malformed
            .verbs
            .iter_mut()
            .find(|verb| verb.ctor == "AsyncDoneWith")
            .unwrap()
            .args[0]
            .ty = wrong_type;
        assert!(
            malformed.validate().is_err(),
            "non-integer completion identity was accepted after valid control"
        );
    }
}

#[test]
fn source_inspection_stays_with_its_actor_owner() {
    use tidepool_protocol::schema::HandlingClass;
    let source = tidepool_protocol::effects::source::source();
    source.validate().unwrap();
    assert!(!source.generated_handler);
    assert!(source
        .verbs
        .iter()
        .all(|verb| verb.handling == HandlingClass::Actor));
    assert_eq!(
        source.constructor_signatures(),
        [
            "SourceReadModule :: Text -> Source (Either SourceError (Text, Text))",
            "SourceStatusWith :: Source (Either SourceError SourceStatus)",
        ]
    );
    assert!(!source.type_defs.iter().any(|ty| ty.name == "ReloadOutcome"));
    let generated = tidepool_protocol::actor_generated_files();
    let decoder = generated
        .iter()
        .find(|file| file.path == "exomonad/actor/src/generated/source.rs")
        .unwrap();
    assert!(decoder.contents.contains("pub enum SourceError"));
    assert!(!decoder.contents.contains("SourceReloadWith"));
    assert!(!tidepool_protocol::generated_files()
        .iter()
        .any(|file| file.path == "bridge/handlers/src/generated/source.rs"));
}

#[test]
fn reload_publication_keeps_typed_outcomes_and_actor_authority() {
    use tidepool_protocol::schema::{HandlingClass, TypeShape};
    let reload = tidepool_protocol::effects::reload::reload();
    reload.validate().unwrap();
    assert!(!reload.generated_handler);
    assert!(reload
        .verbs
        .iter()
        .all(|verb| verb.handling == HandlingClass::Actor));
    assert_eq!(
        reload.constructor_signatures(),
        ["ReloadSourceWith :: ReloadOptions -> Reload (Either ReloadError ReloadOutcome)",]
    );
    let outcome = reload
        .type_defs
        .iter()
        .find(|ty| ty.name == "ReloadOutcome")
        .unwrap();
    let TypeShape::Sum { variants } = &outcome.shape else {
        panic!("reload outcomes must be distinct constructors")
    };
    assert_eq!(
        variants
            .iter()
            .map(|variant| variant.ctor)
            .collect::<Vec<_>>(),
        [
            "ReloadRetirement",
            "ReloadRefused",
            "ReloadUnchanged",
            "ReloadPublished",
            "ReloadRejected",
            "ReloadCancelled",
            "ReloadPublishedWithWarnings",
            "ReloadPublicationUnconfirmed",
        ]
    );
    let resource = reload
        .type_defs
        .iter()
        .find(|ty| ty.name == "ReloadResource")
        .unwrap();
    let TypeShape::Sum { variants } = &resource.shape else {
        panic!("reload resource owners must be distinct constructors")
    };
    use tidepool_protocol::hs::HsType;
    for (constructor, fields) in [
        (
            "ReloadActorCompilerResource",
            vec![HsType::Tuple(vec![HsType::Int, HsType::Int]), HsType::Text],
        ),
        (
            "ReloadPreparationCompilerResource",
            vec![HsType::Text, HsType::Text],
        ),
        (
            "ReloadSourceBindingResource",
            vec![HsType::Tuple(vec![HsType::Int, HsType::Int]), HsType::Text],
        ),
        (
            "ReloadCompilerResource",
            vec![
                HsType::Tuple(vec![HsType::Int, HsType::Int]),
                HsType::Named("ReloadReservation"),
                HsType::Int,
            ],
        ),
    ] {
        let variant = variants
            .iter()
            .find(|variant| variant.ctor == constructor)
            .unwrap();
        assert_eq!(variant.fields.types(), fields.iter().collect::<Vec<_>>());
    }
    let source_state = reload
        .type_defs
        .iter()
        .find(|ty| ty.name == "ReloadRetirementState")
        .unwrap();
    let TypeShape::Sum { variants } = &source_state.shape else {
        panic!("source outcome sum")
    };
    assert_eq!(
        variants.iter().map(|v| v.ctor).collect::<Vec<_>>(),
        [
            "ReloadAReopened",
            "ReloadRetirementFenced",
            "ReloadBPublished"
        ]
    );
    let settlement = reload
        .type_defs
        .iter()
        .find(|ty| ty.name == "ReloadOperationalSettlement")
        .unwrap();
    let TypeShape::Sum { variants } = &settlement.shape else {
        panic!("operational settlement must be a nonempty sum")
    };
    assert_eq!(
        variants.iter().map(|v| v.ctor).collect::<Vec<_>>(),
        ["ReloadOperationalSettled", "ReloadOperationalUnsettled"]
    );
    assert!(variants[0].fields.types().is_empty());
    let fields = [
        HsType::Named("ReloadRetirementObligation"),
        HsType::list(HsType::Named("ReloadRetirementObligation")),
    ];
    assert_eq!(
        variants[1].fields.types(),
        fields.iter().collect::<Vec<_>>()
    );
    let observation = reload
        .type_defs
        .iter()
        .find(|ty| ty.name == "ReloadObligationObservation")
        .unwrap();
    let TypeShape::Sum { variants } = &observation.shape else {
        panic!("observation sum")
    };
    assert_eq!(
        variants.iter().map(|v| v.ctor).collect::<Vec<_>>(),
        [
            "ReloadObligationPending",
            "ReloadObligationUnconfirmed",
            "ReloadObligationRefused"
        ]
    );
    let obligation = reload
        .type_defs
        .iter()
        .find(|ty| ty.name == "ReloadRetirementObligation")
        .unwrap();
    let TypeShape::Sum { variants } = &obligation.shape else {
        panic!("obligation sum")
    };
    assert_eq!(
        variants.iter().map(|v| v.ctor).collect::<Vec<_>>(),
        [
            "ReloadCandidateSettlementObligation",
            "ReloadOriginalRetirementObligation",
            "ReloadPhysicalCleanupObligation",
            "ReloadPublicationNotificationObligation",
            "ReloadSourceDeliveryObligation",
            "ReloadDiagnosticDeliveryObligation"
        ]
    );
    for (index, variant) in variants.iter().enumerate() {
        let expected = if index == 4 {
            vec![
                HsType::Named("ReloadResource"),
                HsType::Named("ReloadObligationObservation"),
            ]
        } else {
            vec![HsType::Named("ReloadObligationObservation")]
        };
        assert_eq!(variant.fields.types(), expected.iter().collect::<Vec<_>>());
    }
    let receipt = reload
        .type_defs
        .iter()
        .find(|ty| ty.name == "ReloadRetirementReceipt")
        .unwrap();
    let TypeShape::Record { fields } = &receipt.shape else {
        panic!("receipt record")
    };
    assert_eq!(fields.len(), 10);
    assert_eq!(
        fields.last().unwrap().ty,
        HsType::Named("ReloadOperationalSettlement")
    );
    let generated = tidepool_protocol::actor_generated_files();
    let decoder = generated
        .iter()
        .find(|file| file.path == "exomonad/actor/src/generated/reload.rs")
        .unwrap();
    assert!(decoder.contents.contains("pub enum ReloadError"));
    assert!(decoder.contents.contains("ReloadSourceWith"));
    assert!(!tidepool_protocol::generated_files()
        .iter()
        .any(|file| file.path == "bridge/handlers/src/generated/reload.rs"));
}

#[test]
fn runtime_info_is_read_only_actor_routed_and_reuses_startup_observation() {
    use tidepool_protocol::{
        hs::HsType,
        schema::{HandlingClass, TypeShape},
    };
    let effect = tidepool_protocol::effects::runtime_info::runtime_info();
    effect.validate().unwrap();
    assert!(!effect.generated_handler);
    assert_eq!(effect.verbs.len(), 2);
    assert!(effect
        .verbs
        .iter()
        .all(|verb| verb.handling == HandlingClass::Actor));
    assert_eq!(
        effect.constructor_signatures(),
        [
            "GetRuntimeInfoWith :: RuntimeInfo (Either RuntimeInfoError RuntimeInfoSnapshot)",
            "CurrentEffectDocsWith :: RuntimeInfo (Either RuntimeInfoError EffectDocsSnapshot)"
        ]
    );
    let startup = effect
        .type_defs
        .iter()
        .find(|ty| ty.name == "RuntimeStartup")
        .unwrap();
    let TypeShape::Sum { variants } = &startup.shape else {
        panic!("startup must distinguish unadmitted from observed")
    };
    assert_eq!(variants.len(), 2);
    assert_eq!(variants[0].ctor, "RuntimeStartupNotAdmitted");
    assert_eq!(variants[1].ctor, "RuntimeStartupObserved");
    let tidepool_protocol::types::VariantFields::Positional(fields) = &variants[1].fields else {
        panic!("startup observation must retain the existing typed payload")
    };
    assert!(matches!(
        fields.as_slice(),
        [HsType::Named("InitialContextObservation")]
    ));
    assert!(effect
        .external_types
        .iter()
        .any(|ty| ty.haskell_name == "InitialContextObservation" && ty.core_module.is_none()));
}
