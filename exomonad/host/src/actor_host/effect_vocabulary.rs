//! Exomonad runtime effect vocabulary shared by production and validation.

pub(crate) fn exomonad_effect_declarations() -> Vec<tidepool_mcp::EffectDecl> {
    let mut declarations = vec![
        tidepool_mcp::agent_session_decl(),
        tidepool_mcp::agent_tools_decl(),
        tidepool_mcp::actor_decl(),
        tidepool_mcp::actor_context_decl(),
        tidepool_mcp::agent_control_decl(),
        tidepool_mcp::notifications_decl(),
        tidepool_mcp::jev_decl(),
    ];
    declarations.extend(
        exomonad_tool::ActorEffectKey::ALL
            .iter()
            .filter_map(|key| key.model_permission())
            .map(|permission| {
                let declaration = match permission.operation {
                    exomonad_tool::ModelOperation::ModelCall => tidepool_mcp::model_call_decl(),
                    exomonad_tool::ModelOperation::ForkAgent => tidepool_mcp::fork_agent_decl(),
                    exomonad_tool::ModelOperation::SpawnAgent => tidepool_mcp::spawn_agent_decl(),
                };
                declaration.for_model_permission(permission)
            }),
    );
    declarations.extend([
        tidepool_mcp::commands_decl(),
        tidepool_mcp::agent_inspection_decl(),
        tidepool_mcp::agent_launch_decl(),
        tidepool_mcp::resource_scopes_decl(),
        tidepool_mcp::actor_kernel_decl(),
        tidepool_mcp::actor_local_decl(),
        tidepool_mcp::reflect_decl(),
        tidepool_mcp::source_decl(),
        tidepool_mcp::sleep_decl(),
        tidepool_mcp::fs_read_decl(),
        tidepool_mcp::fs_write_decl(),
        tidepool_mcp::web_decl(),
        tidepool_mcp::worktree_decl(),
        tidepool_mcp::bound_worktree_decl(),
        tidepool_mcp::worktree_registry_decl(),
        tidepool_mcp::worktree_allocation_decl(),
        tidepool_mcp::worktree_integration_decl(),
        tidepool_mcp::event_decl(),
        tidepool_mcp::journal_decl(),
    ]);
    declarations
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_vocabulary_retains_all_selected_model_families() {
        let declarations = exomonad_effect_declarations();
        let model_declarations: Vec<_> = declarations
            .iter()
            .filter(|decl| decl.model_permission.is_some())
            .collect();
        let expected: Vec<_> = exomonad_tool::ActorEffectKey::ALL
            .iter()
            .filter_map(|key| key.model_permission().map(|permission| (*key, permission)))
            .collect();
        assert_eq!(model_declarations.len(), 9);
        for ((key, permission), declaration) in expected.into_iter().zip(model_declarations) {
            assert_eq!(declaration.model_permission, Some(permission));
            assert_eq!(
                tidepool_mcp::row_entry(declaration, &tidepool_mcp::RowArgs::default()),
                key.haskell_row_entry()
            );
        }
        assert!(declarations
            .iter()
            .filter(|decl| matches!(decl.type_name, "ModelCall" | "ForkAgent" | "SpawnAgent"))
            .all(|decl| decl.model_permission.is_some()));
    }
}
