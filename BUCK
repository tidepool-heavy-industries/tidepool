load("@prelude//:rules.bzl", "export_file", "filegroup")

export_file(name = "workspace_cargo_manifest", src = "Cargo.toml", visibility = ["PUBLIC"])
export_file(name = "workspace_cargo_lock", src = "Cargo.lock", visibility = ["PUBLIC"])
export_file(name = "workspace_flake_lock", src = "flake.lock", visibility = ["PUBLIC"])
export_file(name = "workspace_flake_nix", src = "flake.nix", visibility = ["PUBLIC"])
export_file(name = "embedded_web_provenance_script", src = "//scripts:embedded_web_provenance_script", visibility = ["PUBLIC"])
export_file(name = "facade_test_jev_operators", src = ".exomonad/workspace/Jev/Operators.hs", visibility = ["PUBLIC"])

export_file(name = "facade_doc__exomonad_workspace_checks_progress_route_producer_hs", src = ".exomonad/workspace/checks/progress-route-producer.hs", visibility = ["PUBLIC"])
export_file(name = "facade_doc__exomonad_workspace_checks_progress_route_questions_hs", src = ".exomonad/workspace/checks/progress-route-questions.hs", visibility = ["PUBLIC"])
export_file(name = "facade_doc__exomonad_workspace_checks_progress_route_hs", src = ".exomonad/workspace/checks/progress-route.hs", visibility = ["PUBLIC"])
export_file(name = "facade_doc__exomonad_workspace_checks_project_decision_consumer_hs", src = ".exomonad/workspace/checks/project_decision_consumer.hs", visibility = ["PUBLIC"])
export_file(name = "facade_doc__exomonad_workspace_checks_project_decision_return_hs", src = ".exomonad/workspace/checks/project_decision_return.hs", visibility = ["PUBLIC"])
export_file(name = "facade_doc__exomonad_workspace_checks_project_delivery_setup_hs", src = ".exomonad/workspace/checks/project_delivery_setup.hs", visibility = ["PUBLIC"])
export_file(name = "facade_doc__exomonad_workspace_checks_project_design_question_hs", src = ".exomonad/workspace/checks/project_design_question.hs", visibility = ["PUBLIC"])
export_file(name = "facade_doc__exomonad_workspace_checks_project_plan_incorporation_hs", src = ".exomonad/workspace/checks/project_plan_incorporation.hs", visibility = ["PUBLIC"])
export_file(name = "facade_doc__exomonad_workspace_checks_project_review_questions_hs", src = ".exomonad/workspace/checks/project_review_questions.hs", visibility = ["PUBLIC"])
export_file(name = "facade_doc__exomonad_workspace_checks_project_review_repair_hs", src = ".exomonad/workspace/checks/project_review_repair.hs", visibility = ["PUBLIC"])
export_file(name = "facade_doc__exomonad_workspace_checks_project_review_start_hs", src = ".exomonad/workspace/checks/project_review_start.hs", visibility = ["PUBLIC"])
export_file(name = "facade_doc__exomonad_workspace_checks_route_reply_setup_hs", src = ".exomonad/workspace/checks/route-reply-setup.hs", visibility = ["PUBLIC"])
export_file(name = "facade_doc__exomonad_workspace_checks_route_reply_worker_hs", src = ".exomonad/workspace/checks/route-reply-worker.hs", visibility = ["PUBLIC"])
export_file(name = "facade_doc__exomonad_workspace_skills_exomonad_jev_SKILL_md", src = ".exomonad/workspace/skills/exomonad-jev/SKILL.md", visibility = ["PUBLIC"])
export_file(name = "facade_doc__exomonad_workspace_skills_exomonad_jev_references_recent_changes_md", src = ".exomonad/workspace/skills/exomonad-jev/references/recent-changes.md", visibility = ["PUBLIC"])
export_file(name = "facade_doc__exomonad_workspace_skills_exomonad_workbench_SKILL_md", src = ".exomonad/workspace/skills/exomonad-workbench/SKILL.md", visibility = ["PUBLIC"])

export_file(name = "test_fixture_manifest", src = "build/test-fixtures.json", visibility = ["PUBLIC"])

export_file(name = "workspace_clippy", src = ".clippy.toml", visibility = ["PUBLIC"])
filegroup(
    name = "native_profile_test_sources",
    srcs = {source: source for source in [".buckconfig", "build/native_profile.bzl"]},
    visibility = ["PUBLIC"],
)

filegroup(
    name = "qualification_inputs",
    srcs = {source: source for source in glob([
        "BUCK", "Cargo.toml", "Cargo.lock", "flake.nix", "flake.lock", ".buckconfig", ".gitmodules", ".clippy.toml",
        "build/test-fixtures.json", "build/native_profile.bzl", "build/native-targets.json", "build/native-workspace-gitlink.json",
        ".exomonad/workspace/**/*.hs", ".exomonad/workspace/**/*.md", ".exomonad/workspace/**/*.toml", ".exomonad/workspace/**/*.nix",
    ])},
    visibility = ["PUBLIC"],
)

export_file(name = "workspace_gitlink", src = "build/native-workspace-gitlink.json", visibility = ["PUBLIC"])

filegroup(
    name = "operator_test_sources",
    srcs = {source: source for source in glob(["exomonad/scripts/*.sh", "exomonad/scripts/tests/*.py"])},
    visibility = ["PUBLIC"],
)
