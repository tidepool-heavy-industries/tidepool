//! Exomonad runtime effect vocabulary shared by production and validation.

pub(crate) fn exomonad_effect_declarations() -> Vec<tidepool_mcp::EffectDecl> {
    vec![
        tidepool_mcp::agent_session_decl(),
        tidepool_mcp::agent_tools_decl(),
        tidepool_mcp::actor_decl(),
        tidepool_mcp::actor_context_decl(),
        tidepool_mcp::agent_control_decl(),
        tidepool_mcp::notifications_decl(),
        tidepool_mcp::jev_decl(),
        tidepool_mcp::model_call_decl(),
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
        tidepool_mcp::worktree_decl(),
        tidepool_mcp::bound_worktree_decl(),
        tidepool_mcp::worktree_registry_decl(),
        tidepool_mcp::worktree_allocation_decl(),
        tidepool_mcp::worktree_integration_decl(),
        tidepool_mcp::event_decl(),
        tidepool_mcp::journal_decl(),
    ]
}
