use super::*;
use crate::resident_workbench::{ActorGeneration, ActorRequestSnapshot, InstalledToolSelection};

struct IssuedSourceService(crate::SourceLayerIssuer);

impl crate::ActorSourceLayers for IssuedSourceService {
    fn validate_source_authority(
        &self,
        source: &crate::CheckpointSourceLayer,
    ) -> Result<(), String> {
        self.0
            .owns(source)
            .then_some(())
            .ok_or_else(|| "foreign source issuer".into())
    }

    fn stage_reload(
        self: Arc<Self>,
        _: tidepool_repr::PrincipalId,
        _: &crate::CheckpointSourceLayer,
        _: &[String],
    ) -> Result<Box<dyn crate::StagedActorSourceReload>, crate::SourceLayerReload> {
        Err(crate::SourceLayerReload::Unavailable(
            "preflight source owner does not publish reloads".into(),
        ))
    }
}

/// Reuse genuinely published generations from the native replacement history.
/// The selected endpoint must beat both a conflicting invocation snapshot and
/// the actor's newer current installation at the actual admission boundary.
pub(crate) fn assert_pinned_preflight(
    runner: ResidentActorRunner<frunk::HNil, tidepool_mcp::CapturedOutput>,
    descriptor: ActorDescriptor,
    initial: Arc<ActorGeneration>,
    replacement: Arc<ActorGeneration>,
    pinned: InstalledToolSelection,
    issuer: crate::SourceLayerIssuer,
) {
    assert_ne!(
        initial.context.placement.lexical_scope,
        replacement.context.placement.lexical_scope
    );
    let (deployments, _receiver) = mpsc::channel(1);
    let mut environment = ResidentEnvironment::new(runner, deployments, None);
    environment.source_layers = Some(Arc::new(IssuedSourceService(issuer)));
    let mut behavior = ResidentKernelBehavior::with_boot(
        descriptor,
        environment,
        ResidentBoot::Workbench,
        Vec::new(),
    );
    behavior.policy_installed = true;
    behavior.standing = ResidentStanding::Workbench;
    behavior
        .installed_tools
        .publish_generation(replacement.clone());

    for (selected, expected) in [
        (pinned, initial),
        (
            ActorRequestSnapshot::Generation(replacement.clone())
                .select("probe")
                .expect("replacement probe"),
            replacement.clone(),
        ),
    ] {
        let endpoint_effects = selected.endpoint().execution_effects().clone();
        let expected_dispatch = selected.lease().tools().unwrap().dispatch.clone();
        let admitted = selected
            .admit(exomonad_tool::ToolArguments::Structured(
                serde_json::json!({"topic": "pinned"}),
            ))
            .expect("generation probe arguments");
        let invocation = crate::ActorWorkbenchInvocation::issued(
            admitted.request(),
            Some(ActorRequestSnapshot::Generation(replacement.clone())),
            None,
        )
        .with_context(None, Some(admitted));
        let preflight = behavior
            .preflight_workbench(expected.context.actor, invocation, None)
            .unwrap_or_else(|failure| panic!("pinned endpoint admission failed: {failure:?}"));
        let WorkbenchPreflight::Admitted(admission) = preflight else {
            panic!("fresh endpoint invocation must be admitted");
        };
        assert!(Arc::ptr_eq(
            admission
                .issued_snapshot
                .as_ref()
                .unwrap()
                .generation()
                .unwrap(),
            &expected,
        ));
        let mut expected_context = expected
            .context
            .clone()
            .with_issued_source(expected.installation.source())
            .unwrap();
        expected_context.haskell_effects_alias = endpoint_effects;
        assert_eq!(admission.context, expected_context);
        assert_eq!(&admission.admitted_source, expected.installation.source());
        assert!(Arc::ptr_eq(&admission.public_owner, &expected.public_owner));
        assert!(Arc::ptr_eq(
            &admission
                .selected_tool
                .as_ref()
                .unwrap()
                .selected()
                .lease()
                .tools()
                .unwrap()
                .dispatch,
            &expected_dispatch,
        ));
        assert!(Arc::ptr_eq(
            &admission
                .installed_tools
                .as_ref()
                .unwrap()
                .tools()
                .unwrap()
                .dispatch,
            &expected_dispatch,
        ));
    }
}
