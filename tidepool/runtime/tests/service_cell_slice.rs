//! One-test vertical slice for the service-owned cell route.
//!
//! The slice publishes one real compiler world through the existing inspection
//! owner, then asks the runtime to admit one authored cell (a bind whose value
//! comes from an imported `Seed` module, then a later use of that bind),
//! compile it through the compiler service, execute it, and observe the result.
//!
//! The production admission/compiler/execution half of that route is not
//! connected at this revision. Each missing capability is spelled as a named
//! `Result<_, &'static str>` refusal so the test compiles, runs, and reports
//! the first absent production entry point instead of failing to build or
//! silently substituting a fixture route. Once a named capability lands,
//! replace the corresponding helper body with the production call shown in its
//! comment; later stages then become reachable in order.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use super::compiler_test_support::OwnedEnvironment;

use tidepool_codegen::scope::ScopeId;
use tidepool_repr::SessionId;
use tidepool_runtime::session::turn::TemplateSelector;
use tidepool_runtime::session::{
    finish_admitted_world_inspection, resident_cell_check_template, resident_workbench_templates,
    run_admitted_world_inspection, GenerationArtifactReference, ModuleEnv, NameNamespace,
    NameQuery, NameScope, ResidentSession, RuntimeCellAdmission, RuntimeCheckedPrefix,
    RuntimeLexicalScopeLease, SessionLib, WorldInspectionError,
};
use tidepool_testing::effect_surface::TestEffectSurface;
use tidepool_toolchain::cache::SourceRootManifest;
use tidepool_toolchain::checked_cell::{CellProgram, CellRecipe};
use tidepool_toolchain::compiler_world::{
    CapturedCompilerSourceRoot, CapturedCompilerSources, PublicModuleQuestionKind,
};

const SEED_ORIGIN: &[u8] = b"tidepool-service-cell-slice:Seed.hs";
const SEED_SOURCE: &str = "module Seed where\n\nseed :: Int\nseed = 7\n";
const SEED_IMPORTS: &str = "import Seed";
// One ordinary cell: bind the imported Seed value, then use that binding in a
// later bind. Both items remain within one admitted compiler operation.
const SEED_CELL: &str = "x <- pure (seed :: Int)\ny <- pure (x + 1 :: Int)\n";

// Source-verified gaps between the published world and one executed cell.
// Keep these spellings exact: they name the production owners that must grow a
// public consumer, not a replacement route.
const GAP_WORLD_ADMISSION: &str = concat!(
    "missing public world admission: ",
    "ResidentSession::begin_world_private_execution(public_scope, ",
    "&GenerationArtifactReference, &Arc<RuntimeLexicalScopeLease>) is pub(crate) at ",
    "tidepool/runtime/src/session/admission.rs:2892; the public ",
    "PersistentSession::begin_private_execution (admission.rs:2876) opens only the finite basis"
);
const GAP_WORLD_PROGRAM: &str = concat!(
    "missing service-issued executable world program: no runtime consumer calls ",
    "compiler_world::plan_cell (tidepool/toolchain/src/compiler_world.rs:1934) then ",
    "compiler_world::admit_world_cell_program (compiler_world.rs:2526) and converts ",
    "CheckedWorldCellProgram (compiler_world.rs:2231) into CellProgram; ",
    "CellProgram::from_checked (tidepool/toolchain/src/checked_cell.rs:1544) accepts only ",
    "ExactCompiledItem and CheckedValueImportAuthority::from_world_output ",
    "(checked_cell.rs:1901) is pub(crate)"
);
const GAP_CHECKED_PREFIX: &str = concat!(
    "missing world checked-prefix capability: begin_cell_program refuses a world ",
    "reservation with SessionError::WorldCellCheckedProgramCapabilityMissing at ",
    "tidepool/runtime/src/session/admission.rs:2649 (and 2700)"
);
const GAP_SUCCESSOR: &str = concat!(
    "missing world per-item successor capability: admit_checked_item refuses a world ",
    "reservation with SessionError::WorldCellSuccessorCapabilityMissing at ",
    "tidepool/runtime/src/session/admission.rs:2158; declaration adoption refuses at 2257"
);
const GAP_FINAL_PROMOTION: &str = concat!(
    "missing world final promotion product: publication refuses with ",
    "SessionError::WorldCellFinalPromotionCapabilityMissing at ",
    "tidepool/runtime/src/session/persistent.rs:3842"
);

type SliceResult<T> = Result<T, &'static str>;

struct PublishedSeedWorld {
    scope: ScopeId,
    reference: GenerationArtifactReference,
    original_source: Arc<RuntimeLexicalScopeLease>,
}

/// Construct the immutable source capture through the production data owner.
///
/// The actor's source-capture layer is not reachable from a runtime integration
/// test, so this reproduces exactly what that owner hands the world builder:
/// one captured root whose manifest and bodies agree, sealed under the admitted
/// generation reference.
fn capture_seed_sources(
    reference: &GenerationArtifactReference,
) -> Result<Arc<CapturedCompilerSources>, String> {
    let root = tempfile::tempdir().map_err(|error| format!("seed source root: {error}"))?;
    let relative = PathBuf::from("Seed.hs");
    std::fs::write(root.path().join(&relative), SEED_SOURCE)
        .map_err(|error| format!("write Seed.hs: {error}"))?;
    let digest = blake3::hash(SEED_SOURCE.as_bytes()).to_hex().to_string();
    let manifest = SourceRootManifest::from_file_digests([(relative.clone(), digest)])
        .map_err(|error| format!("Seed.hs manifest: {error}"))?;
    let bodies = BTreeMap::from([(relative, Arc::<[u8]>::from(SEED_SOURCE.as_bytes()))]);
    let sources = CapturedCompilerSources::from_captured_roots(
        vec![CapturedCompilerSourceRoot { manifest, bodies }],
        Arc::from(reference.bytes()),
        Arc::new(()),
    )
    .map_err(|error| format!("capture Seed.hs: {error}"))?;
    Ok(Arc::new(sources))
}

/// Establish one published compiler world through the production inspection
/// owner. This is the real cold-world path: capture the admitted base, run the
/// source compiler transaction, finish the issued world, and install it only
/// while the base is unchanged. Publication is observable by borrowing the same
/// generation again and finding no cold source capture required.
fn publish_seed_world(
    session: &mut ResidentSession<frunk::HNil, tidepool_mcp::CapturedOutput>,
    scope: ScopeId,
    reference: &GenerationArtifactReference,
    original_source: &Arc<RuntimeLexicalScopeLease>,
) -> SliceResult<PublishedSeedWorld> {
    let sources =
        capture_seed_sources(reference).map_err(|_| "seed source capture failed")?;
    let admitted = session
        .capture_inspection_world_in(scope, reference, original_source)
        .map_err(|_| "the source owner refused a published-world capture")?;
    let query = NameQuery {
        scope: NameScope::PublicModule("Seed".into()),
        namespace: NameNamespace::Value,
        name: "seed".into(),
    };
    let settled = original_source
        .with_source_compiler_transaction(
            tidepool_toolchain::artifacts::CompileWorkload::Foreground,
            tidepool_runtime::CompilerTransactionCancellation::new(),
            None,
            |_| {},
            move |owner, _| {
                run_admitted_world_inspection(
                    owner,
                    admitted,
                    move || Ok(Some(sources)),
                    query,
                    PublicModuleQuestionKind::StructuredInfo,
                )
            },
        )
        .map_err(|_| "the source compiler transaction was refused")?;
    let completed =
        finish_admitted_world_inspection(settled).map_err(|error| named_world_error(&error))?;
    if !session.install_inspection_world_if_current(&completed, reference, original_source) {
        return Err("the inspection world base changed before installation");
    }
    let borrowed = session
        .capture_inspection_world_in(scope, reference, original_source)
        .map_err(|_| "the published world was not retained on the scope")?;
    if borrowed.needs_source_capture() {
        return Err("the installed world still requires cold source capture");
    }
    Ok(PublishedSeedWorld {
        scope,
        reference: reference.clone(),
        original_source: Arc::clone(original_source),
    })
}

/// Admit one authored cell through the world admission owner.
///
/// Intended production spelling once `begin_world_private_execution` is
/// callable from the resident session:
///
/// ```ignore
/// session.with_source_compiler_transaction(
///     published.original_source.clone(),
///     CompileWorkload::Foreground,
///     CompilerTransactionCancellation::new(),
///     None,
///     |_| {},
///     |session, _| {
///         let execution = Arc::new(session.begin_world_private_execution(
///             published.scope,
///             &published.reference,
///             &published.original_source,
///         )?);
///         let world = execution.world_basis()?;
///         let plan = tidepool_toolchain::compiler_world::plan_cell(
///             &endpoint,
///             world,
///             Arc::new(WorldCellRecipe::from(recipe)),
///         )?;
///         session.admit_world_planned_cell_for_execution(execution, plan)
///     },
/// )
/// ```
fn admit_authored_seed_cell(
    _session: &mut ResidentSession<frunk::HNil, tidepool_mcp::CapturedOutput>,
    _published: &PublishedSeedWorld,
    _recipe: &CellRecipe,
) -> SliceResult<Arc<RuntimeCellAdmission>> {
    Err(GAP_WORLD_ADMISSION)
}

/// Compile the admitted authored cell through the compiler service.
///
/// Intended production spelling once the service issuer is connected:
///
/// ```ignore
/// let reservation = admission.plan_reservation()?.ok_or(...)?;
/// let checked = tidepool_toolchain::compiler_world::admit_world_cell_program(
///     &endpoint,
///     reservation,
///     original,
///     target,
///     admission.origin(),
/// )??;
/// CellProgram::from_world_output(checked) // names the missing issuer, not a fixture
/// ```
fn compile_admitted_seed_cell(
    _admission: &Arc<RuntimeCellAdmission>,
) -> SliceResult<Arc<CellProgram>> {
    Err(GAP_WORLD_PROGRAM)
}

/// Open the ordered execution prefix from the service-issued checked program.
///
/// Intended production spelling once the world checked-prefix capability lands:
///
/// ```ignore
/// session.begin_cell_program(program)
/// ```
fn begin_checked_seed_cell(
    _session: &ResidentSession<frunk::HNil, tidepool_mcp::CapturedOutput>,
    _admission: Arc<RuntimeCellAdmission>,
    _program: Arc<CellProgram>,
) -> SliceResult<Option<Arc<RuntimeCheckedPrefix>>> {
    Err(GAP_CHECKED_PREFIX)
}

/// Execute the admitted cell item by item and return the final bound value.
///
/// Intended production spelling once the world prefix/successor capabilities
/// land: admit each `checked_item` through `admit_checked_item`, decode it with
/// `consume_cell_program_item`, run its `CompiledTurn` through
/// `run_bind_with_sites` on the private scope, and settle the final promotion
/// through the missing World final-product issuer.
fn execute_seed_cell(
    session: &mut ResidentSession<frunk::HNil, tidepool_mcp::CapturedOutput>,
    admission: Arc<RuntimeCellAdmission>,
    program: Arc<CellProgram>,
) -> SliceResult<i64> {
    let _prefix = begin_checked_seed_cell(session, admission, program)?;
    Err(GAP_SUCCESSOR)
}

fn named_world_error(error: &WorldInspectionError) -> &'static str {
    match error {
        WorldInspectionError::UnsupportedSource => "world inspection refused the source owner",
        WorldInspectionError::SourceUnavailable(_) => "world inspection source unavailable",
        WorldInspectionError::SourceUnreadable(_) => "world inspection source unreadable",
        WorldInspectionError::Runtime(_) => "world inspection runtime failure",
        WorldInspectionError::CompilerClose { .. } => "world inspection close unconfirmed",
        WorldInspectionError::UnknownBindingNamespace(_) => "world binding namespace unknown",
    }
}

#[test]
#[serial_test::serial]
#[ignore = "service cell world route capabilities pending"]
fn service_cell_slice_imports_seed_bind_and_uses_it() {
    tidepool_testing::eval_harness::require_extract();
    let cache = tempfile::tempdir().expect("compile cache");
    let _cache = OwnedEnvironment::set("TIDEPOOL_COMPILE_CACHE_DIR", Some(cache.path()));
    let root = tempfile::tempdir().expect("session root");
    let effects = TestEffectSurface::minimal(&[]).expect("effect surface");
    let lib = SessionLib::open(
        SessionId(20261019),
        root.path().join("decl-lib"),
        ModuleEnv::standalone_default(),
    )
    .expect("open declaration plane")
    .with_validation_include(effects.include_paths().to_vec());
    let mut session = ResidentSession::unbootstrapped(
        frunk::HNil,
        tidepool_mcp::CapturedOutput::new(),
        tidepool_runtime::DEFAULT_NURSERY_SIZE,
        Some(lib),
    );

    let scope = session.mint_isolated_scope();
    let original_source = session
        .adopt_initial_notebook_scope(scope)
        .expect("adopt initial notebook scope");
    let reference = GenerationArtifactReference::capture(SEED_ORIGIN.to_vec());

    let published = publish_seed_world(&mut session, scope, &reference, &original_source).unwrap_or_else(
        |gap| {
            panic!(
                "service cell slice failed while publishing the Seed world: {gap}\n\
                 named route gaps:\n- {GAP_WORLD_ADMISSION}\n- {GAP_WORLD_PROGRAM}\n\
                 - {GAP_CHECKED_PREFIX}\n- {GAP_SUCCESSOR}\n- {GAP_FINAL_PROMOTION}"
            )
        },
    );

    // The authored cell is ordinary workbench source: one bind imported
    // through the real module path, then a later use of that binding.
    let template = resident_cell_check_template(
        effects.preamble(),
        effects.row(),
        SEED_IMPORTS,
    );
    let templates =
        resident_workbench_templates(effects.preamble(), effects.row(), SEED_IMPORTS);
    let recipe = CellRecipe::new(
        SEED_CELL.to_string(),
        template,
        templates
            .iter()
            .map(|template| {
                let selector = match template.kind {
                    TemplateSelector::Decl => "decl",
                    TemplateSelector::Bind => "bind",
                    TemplateSelector::BindDiscard => "binddiscard",
                    TemplateSelector::Expr => "expr",
                };
                (selector.into(), template.source.clone())
            })
            .collect(),
        Vec::new(),
    )
    .expect("authored Seed cell recipe");

    let admitted = admit_authored_seed_cell(&mut session, &published, &recipe).unwrap_or_else(
        |gap| {
            panic!(
                "service cell slice blocked at admission: {gap}\n\
                 named route gaps:\n- {GAP_WORLD_ADMISSION}\n- {GAP_WORLD_PROGRAM}\n\
                 - {GAP_CHECKED_PREFIX}\n- {GAP_SUCCESSOR}\n- {GAP_FINAL_PROMOTION}"
            )
        },
    );
    let program = compile_admitted_seed_cell(&admitted)
        .unwrap_or_else(|gap| panic!("service cell slice blocked at compilation: {gap}"));
    let value = execute_seed_cell(&mut session, admitted, program)
        .unwrap_or_else(|gap| panic!("service cell slice blocked at execution: {gap}"));

    // `seed = 7` is imported through the real module path and `y = x + 1`
    // uses that binding inside the same admitted cell.
    assert_eq!(value, 8);
}
