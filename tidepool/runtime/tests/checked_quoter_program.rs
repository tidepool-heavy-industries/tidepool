//! The real parser, whole-program worker, and sequential Rust certifier share
//! fresh originals across slots before any native effect can execute.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tidepool_codegen::scope::ScopeId;
use tidepool_repr::SessionId;
use tidepool_runtime::session::turn::{
    compile_cell_program_admitted, CellCheckRequest, TemplateSelector,
};
use tidepool_runtime::session::{
    resident_cell_check_template, resident_workbench_templates, ModuleEnv, PersistentSession,
    SessionLib, SourceImports,
};
use tidepool_testing::effect_surface::TestEffectSurface;
use tidepool_toolchain::checked_cell::CheckedCellSpecification;

struct OwnedEnvironment {
    name: &'static str,
    previous: Option<OsString>,
}

impl OwnedEnvironment {
    fn set(name: &'static str, value: Option<&Path>) -> Self {
        let previous = std::env::var_os(name);
        // This test is serial and owns these compiler environment inputs.
        unsafe {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
        Self { name, previous }
    }
}

impl Drop for OwnedEnvironment {
    fn drop(&mut self) {
        // Restore the same serial test's original compiler environment.
        unsafe {
            match &self.previous {
                Some(value) => std::env::set_var(self.name, value),
                None => std::env::remove_var(self.name),
            }
        }
    }
}

#[test]
#[serial_test::serial]
fn fresh_checked_program_retains_reexported_quoter_across_slots_cold_and_warm() {
    tidepool_testing::eval_harness::require_extract();
    let _daemon = OwnedEnvironment::set("TIDEPOOL_EXTRACT_DAEMON_SOCKET", None);
    let cache = tempfile::tempdir().unwrap();
    let _cache = OwnedEnvironment::set("TIDEPOOL_COMPILE_CACHE_DIR", Some(cache.path()));
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("CheckedQuoterFacade.hs"),
        include_str!("fixtures/CheckedQuoterFacade.hs"),
    )
    .unwrap();
    let effects = TestEffectSurface::minimal(&[]).unwrap();
    let lib = SessionLib::open(
        SessionId(1088),
        root.path(),
        ModuleEnv::standalone_default(),
    )
    .unwrap()
    .with_validation_include(effects.include_paths().to_vec());
    let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
    let public = session.mint_scope(ScopeId::ROOT).unwrap();
    let source = include_str!("fixtures/checked-quoter-program.hs");
    for phase in ["cold", "warm artifact cache"] {
        let execution = Arc::new(session.begin_private_execution(public).unwrap());
        let view = execution.view();
        let imports = view.turn_imports(&SourceImports::from_specs([
            "qualified CheckedQuoterFacade as Q",
        ]));
        let template = resident_cell_check_template(effects.preamble(), effects.row(), &imports);
        let templates = resident_workbench_templates(effects.preamble(), effects.row(), &imports);
        let specification = Arc::new(CheckedCellSpecification {
            admission_digest: [0; 32],
            cell_source: source.into(),
            template_source: template.clone(),
            turn_templates: templates
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
            injected_modules: view.injected_module_names(),
            reserved_declaration_modules: Vec::new(),
        });
        let mut roots = effects.include_paths().to_vec();
        roots.insert(0, root.path().to_path_buf());
        let include = view.include_paths(&roots);
        let plan = tidepool_toolchain::artifacts::parse_cell_plan(specification.clone(), &include)
            .unwrap();
        assert_eq!(plan.items().len(), 3);
        let admission = session
            .admit_planned_cell_for_execution(
                execution,
                plan,
                specification.clone(),
                specification.specification_digest(),
                [7; 32],
                include,
                None,
            )
            .unwrap();
        let view = admission.view();
        let include = admission
            .include_paths()
            .iter()
            .map(PathBuf::as_path)
            .collect::<Vec<_>>();
        let started = std::time::Instant::now();
        let (_, program) = compile_cell_program_admitted(
            CellCheckRequest {
                exact_context: view.exact_compile_context(),
                session_id: Some(view.session()),
                cell_text: source,
                template: &template,
                include: &include,
                session_root: view.session_root(),
                inject_modules: &specification.injected_modules,
                compile_generation: view.next_value_generation().0,
                compile_view_evidence: "fresh original quoter across checked program slots",
            },
            admission.clone(),
            &templates,
        )
        .unwrap_or_else(|error| panic!("{phase} checked quoter program failed: {error:?}"));
        assert_eq!(program.items().len(), 3);
        for item in program.items() {
            assert!(item.native().is_some());
            assert!(item.native_products().is_some());
        }
        let products = &program.items()[0]
            .native_products()
            .unwrap()
            .recovery_products;
        let original = products
            .iter()
            .find(|product| product.owner().module == "Tidepool.QQ.Bash")
            .expect("first ordinary item must retain the defining quoter owner");
        let durable = tempfile::tempdir().unwrap();
        let references = tidepool_toolchain::recovery_artifacts::materialize_certified_products(
            durable.path(),
            program.parsed_plan().producer_sha256(),
            products,
        )
        .unwrap();
        let reference = references
            .iter()
            .find(|reference| reference.module == "Tidepool.QQ.Bash")
            .unwrap();
        assert!(reference.execution_source.is_some());
        let recovered =
            tidepool_toolchain::declaration_join::ExactDeclarationContext::capture_recovery(
                durable.path(),
                &references,
                &[],
                &[],
                Vec::new(),
            )
            .unwrap();
        let recovered_products = recovered.recovery_products();
        let recovered_original = recovered_products
            .iter()
            .find(|product| product.owner().module == "Tidepool.QQ.Bash")
            .unwrap();
        // Recovery preserves durable authority, but does not reissue the fresh
        // source witness used to admit declarations in the original transaction.
        assert!(original.source_sha256().is_some());
        assert!(recovered_original.source_sha256().is_none());
        assert_eq!(original.owner(), recovered_original.owner());
        assert!(original.interface_bytes() == recovered_original.interface_bytes());
        assert!(original.product_bytes() == recovered_original.product_bytes());
        let recovered_durable = tempfile::tempdir().unwrap();
        let recovered_references =
            tidepool_toolchain::recovery_artifacts::materialize_certified_products(
                recovered_durable.path(),
                program.parsed_plan().producer_sha256(),
                &recovered_products,
            )
            .unwrap();
        let recovered_reference = recovered_references
            .iter()
            .find(|reference| reference.module == "Tidepool.QQ.Bash")
            .unwrap();
        assert_eq!(reference, recovered_reference);
        let graph = &reference.execution_source.as_ref().unwrap().path;
        assert!(
            std::fs::read(durable.path().join(graph)).unwrap()
                == std::fs::read(recovered_durable.path().join(graph)).unwrap()
        );
        assert!(session
            .begin_cell_program(admission, program.clone())
            .unwrap()
            .is_some());
        eprintln!(
            "checked quoter {phase}: {} ms",
            started.elapsed().as_millis()
        );
        // Preparation must retain authority without creating authored bindings
        // or executing the quoted command's contents.
        assert_eq!(session.scope_binding_count(public), 0);
    }
}
