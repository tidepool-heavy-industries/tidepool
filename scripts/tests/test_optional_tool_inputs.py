"""Pinned resource refusal at the native toolchain's owning entry points."""

import ast
from pathlib import Path
from types import SimpleNamespace
import unittest


ROOT = Path(__file__).resolve().parents[2]


class OptionalToolInputs(unittest.TestCase):
    def setUp(self):
        def fail(message):
            raise ValueError(message)

        namespace = {
            "fail": fail,
            "DefaultInfo": lambda **kwargs: kwargs,
            "RunInfo": lambda **kwargs: kwargs,
        }
        source = ast.parse((ROOT / "toolchains/tidepool.bzl").read_text())
        functions = [node for node in source.body if isinstance(node, ast.FunctionDef)
                     and node.name in ("_nix_tool_impl", "_nix_directory_impl")]
        exec(compile(ast.Module(body=functions, type_ignores=[]),
                     "toolchains/tidepool.bzl", "exec"), namespace)
        self.implementations = namespace

    def test_missing_executable_refuses_at_analysis_with_selection_instruction(self):
        ctx = SimpleNamespace(label="toolchains//:browser_node",
                              attrs=SimpleNamespace(executable=""))
        with self.assertRaisesRegex(ValueError, r"browser_node.*--browser"):
            self.implementations["_nix_tool_impl"](ctx)

    def test_selected_executable_is_carried_without_ambient_fallback(self):
        path = "/nix/store/selected-node/bin/node"
        ctx = SimpleNamespace(label="toolchains//:browser_node",
                              attrs=SimpleNamespace(executable=path))
        result = self.implementations["_nix_tool_impl"](ctx)
        self.assertEqual(result, [{}, {"args": [path]}])

    def test_missing_optional_directory_refuses_before_action_construction(self):
        ctx = SimpleNamespace(label="toolchains//:playwright_browsers",
                              attrs=SimpleNamespace(store_path=""))
        with self.assertRaisesRegex(ValueError, "immutable Nix store directory"):
            self.implementations["_nix_directory_impl"](ctx)


if __name__ == "__main__":
    unittest.main()
