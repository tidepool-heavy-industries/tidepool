//! Reexported quotation compiles across checked slots, and the fixture-owned
//! runtime original preserves its durable source recipe before native execution.

use std::sync::Arc;

use super::compiler_test_support::OwnedEnvironment;

use tidepool_codegen::scope::ScopeId;
use tidepool_repr::SessionId;
use tidepool_runtime::session::turn::{
    compile_cell_program_admitted, consume_cell_program_item, TemplateSelector, TurnResult,
};
use tidepool_runtime::session::{
    resident_cell_check_template, resident_workbench_templates, ModuleEnv, PersistentSession,
    SessionLib, SourceImports,
};
use tidepool_testing::effect_surface::TestEffectSurface;
use tidepool_toolchain::checked_cell::CheckedCellSpecification;

#[test]
#[serial_test::serial]
fn fresh_checked_program_retains_reexported_quoter_across_slots_cold_and_warm() {
    tidepool_testing::eval_harness::require_extract();
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
        let plan = tidepool_testing::with_settlement(|settlement| {
            tidepool_toolchain::artifacts::parse_cell_plan(
                specification.clone(),
                &include,
                settlement,
            )
        })
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
        let started = std::time::Instant::now();
        let (checked, program) = tidepool_testing::with_settlement(|settlement| {
            compile_cell_program_admitted(admission.clone(), settlement)
        })
        .unwrap_or_else(|error| panic!("{phase} checked quoter program failed: {error:?}"));
        assert_eq!(program.items().len(), 3);
        assert_eq!(
            (0..3)
                .map(|index| checked.checked_item(index).unwrap().kind())
                .collect::<Vec<_>>(),
            vec![
                tidepool_toolchain::checked_cell::CheckedItemKind::Expression,
                tidepool_toolchain::checked_cell::CheckedItemKind::Bind,
                tidepool_toolchain::checked_cell::CheckedItemKind::Expression
            ]
        );
        assert_eq!(
            checked.checked_item(0).unwrap().source().trim(),
            "42 :: Int"
        );
        for item in program.items() {
            assert!(item.native().is_some());
            assert!(item.native_products().is_some());
        }
        let first = &program.items()[0];
        let sealed = first.native_products().unwrap();
        let observation = first
            .native()
            .unwrap()
            .observation_name()
            .expect("expression owns its issued observation capture")
            .to_owned();
        let prefix = session
            .begin_cell_program(admission, program.clone())
            .unwrap()
            .expect("checked program starts with its first expression");
        let reservation = session
            .admit_checked_item(prefix, checked.checked_item(0).unwrap())
            .unwrap();
        assert_eq!(reservation.item().index(), 0);
        assert_eq!(reservation.observation_name(), Some(observation.as_str()));
        // Authored expressions execute through their compiler-issued singleton
        // capture; the sealed execution receipt is a Bind observation.
        let TurnResult::Bind {
            compiled,
            binders,
            bound,
            ..
        } = consume_cell_program_item(reservation).unwrap()
        else {
            panic!("the authored expression must retain its issued Bind observation");
        };
        assert_eq!(binders, vec![observation.clone()]);
        assert_eq!(
            bound
                .iter()
                .map(|binder| binder.name.as_str())
                .collect::<Vec<_>>(),
            vec![observation.as_str()]
        );
        let original_interfaces = first
            .native()
            .unwrap()
            .original_interface_context(compiled.prepared(), compiled.table(), compiled.sites())
            .unwrap();
        let products = &sealed.recovery_products;
        let originals = products
            .iter()
            .filter(|product| {
                product.owner().unit == "main" && product.owner().module == "CheckedQuoterFacade"
            })
            .collect::<Vec<_>>();
        assert_eq!(originals.len(), 1);
        let original = originals[0];
        // Quotation expands during compilation. The fixture's opaque runtime
        // wrapper independently supplies a fresh original for durable recovery.
        assert!(sealed.certified_groups.iter().any(|group| {
            group.owner() == original.owner()
                && group.group().binders().iter().any(|binder| {
                    binder.unit == "main"
                        && binder.module == "CheckedQuoterFacade"
                        && binder.occurrence == "describe"
                })
        }));
        assert!(program.items()[1]
            .native()
            .unwrap()
            .target_owned()
            .globals()
            .iter()
            .any(|global| {
                global.identity.unit == "main"
                    && global.identity.module == "CheckedQuoterFacade"
                    && global.identity.occurrence == "describe"
            }));
        let durable = tempfile::tempdir().unwrap();
        let references = tidepool_toolchain::recovery_artifacts::materialize_certified_products(
            durable.path(),
            program.parsed_plan().producer_sha256(),
            products,
        )
        .unwrap();
        let module_interfaces = original_interfaces
            .materialize_module_interfaces(durable.path())
            .unwrap();
        let reference = references
            .iter()
            .find(|reference| reference.unit == "main" && reference.module == "CheckedQuoterFacade")
            .unwrap();
        assert!(reference.execution_source.is_some());
        let recovered =
            tidepool_toolchain::declaration_join::ExactDeclarationContext::capture_recovery(
                durable.path(),
                &references,
                &module_interfaces,
                &[],
                Vec::new(),
            )
            .unwrap();
        let recovered_products = recovered.recovery_products();
        let recovered_original = recovered_products
            .iter()
            .find(|product| {
                product.owner().unit == "main" && product.owner().module == "CheckedQuoterFacade"
            })
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
            .find(|reference| reference.unit == "main" && reference.module == "CheckedQuoterFacade")
            .unwrap();
        assert_eq!(reference, recovered_reference);
        let graph = &reference.execution_source.as_ref().unwrap().path;
        assert!(
            std::fs::read(durable.path().join(graph)).unwrap()
                == std::fs::read(recovered_durable.path().join(graph)).unwrap()
        );
        eprintln!(
            "checked quoter {phase}: {} ms",
            started.elapsed().as_millis()
        );
        // Preparation must retain authority without creating authored bindings
        // or executing the quoted command's contents.
        assert_eq!(session.scope_binding_count(public), 0);
    }
}
