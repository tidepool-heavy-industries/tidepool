"""Exercise Cargo feature resolution against Cargo's actual metadata graph."""

import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from buck2_cargo_features import (
    effective_feature_selection,
    metadata_feature_args,
    reject_forbidden_closure,
    resolve,
)

PROFILE = Path(__file__).resolve().parents[1] / "native-profile.toml"


class CargoFeatureGraph(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="buck-cargo-features-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "vendor-protocol/src").mkdir(parents=True)
        (self.root / "probe/src").mkdir(parents=True)
        (self.root / "vendor-protocol/Cargo.toml").write_text(
            "[package]\nname='vendor-protocol'\nversion='0.1.0'\nedition='2021'\n"
            "[features]\ndefault=[]\njson=[]\ntransport=[]\n"
        )
        (self.root / "vendor-protocol/src/lib.rs").write_text("")
        (self.root / "probe/Cargo.toml").write_text(
            "[package]\nname='feature-probe'\nversion='0.1.0'\nedition='2021'\n"
            "[features]\ndefault=['codex-compat']\n"
            "codex-compat=['dep:codex_proto','codex_proto/json','codex_proto?/transport']\n"
            "embedded=[]\nnormal=['codex_proto/json']\nplain=['plain_alias/json']\n"
            "weak=['codex_proto?/transport']\nother=['other_alias']\n"
            "[dependencies]\n"
            "codex_proto={package='vendor-protocol',path='../vendor-protocol',optional=true,default-features=false}\n"
            "other_alias={package='vendor-protocol',path='../vendor-protocol',optional=true,default-features=false}\n"
            "plain_alias={package='vendor-protocol',path='../vendor-protocol',optional=true,default-features=false}\n"
        )
        (self.root / "probe/src/lib.rs").write_text("")

    def metadata(self, no_defaults=False, features=()):
        base = ["cargo", "metadata", "--manifest-path", str(self.root / "probe/Cargo.toml"),
                "--format-version", "1"]
        baseline = json.loads(subprocess.check_output(base, cwd=self.root, text=True))
        command = [*base, "--no-default-features"]
        command.extend(metadata_feature_args(
            baseline,
            ["feature-probe"] if no_defaults else (),
            {"feature-probe": set(features)} if features else {},
        ))
        return json.loads(subprocess.check_output(command, cwd=self.root, text=True))

    def selected(self, *, no_defaults=False, features=()):
        metadata = self.metadata(no_defaults, features)
        package = next(p for p in metadata["packages"] if p["name"] == "feature-probe")
        node = next(n for n in metadata["resolve"]["nodes"] if n["id"] == package["id"])
        return package, node, resolve(package, features, not no_defaults, node)

    def test_default_and_embedded_feature_graphs_are_exact(self):
        _, _, (features, dependencies, forwarded) = self.selected()
        self.assertEqual(features, ["codex-compat", "default"])
        self.assertEqual(dependencies, {"codex_proto"})
        self.assertEqual(forwarded, {"codex_proto": {"json", "transport"}})

        _, _, (features, dependencies, forwarded) = self.selected(
            no_defaults=True, features=("embedded",))
        self.assertEqual(features, ["embedded"])
        self.assertEqual(dependencies, set())
        self.assertEqual(forwarded, {})

    def test_normal_weak_and_renamed_dependency_forwarding(self):
        _, _, (features, dependencies, forwarded) = self.selected(
            no_defaults=True, features=("normal",))
        self.assertEqual(features, ["normal"])
        self.assertEqual(dependencies, {"codex_proto"})
        self.assertEqual(forwarded, {"codex_proto": {"json"}})

        _, _, (features, dependencies, forwarded) = self.selected(
            no_defaults=True, features=("plain",))
        self.assertEqual(features, ["plain", "plain_alias"])
        self.assertEqual(dependencies, {"plain_alias"})
        self.assertEqual(forwarded, {"plain_alias": {"json"}})

        _, _, (features, dependencies, forwarded) = self.selected(
            no_defaults=True, features=("weak",))
        self.assertEqual(features, ["weak"])
        self.assertEqual(dependencies, set())
        self.assertEqual(forwarded, {})

        _, _, (features, dependencies, forwarded) = self.selected(
            no_defaults=True, features=("other",))
        self.assertEqual(features, ["other", "other_alias"])
        self.assertEqual(dependencies, {"other_alias"})
        self.assertEqual(forwarded, {})

    def test_workspace_facade_default_cannot_reactivate_embedded_agent_backend(self):
        root = self.root / "workspace"
        (root / "agent/src").mkdir(parents=True)
        (root / "facade/src").mkdir(parents=True)
        (root / "protocol/src").mkdir(parents=True)
        (root / "scripts").mkdir()
        (root / "scripts/native-profile.toml").write_bytes(PROFILE.read_bytes())
        (root / "Cargo.toml").write_text(
            "[workspace]\nmembers=['agent','facade','protocol']\nresolver='2'\n"
        )
        (root / "agent/Cargo.toml").write_text(
            "[package]\nname='exomonad-agent'\nversion='0.1.0'\nedition='2021'\n"
            "[features]\ndefault=['codex-compat']\n"
            "codex-compat=['dep:codex-shoal-protocol']\n"
            "[dependencies]\n"
            "codex-shoal-protocol={path='../protocol',optional=true,default-features=false}\n"
        )
        (root / "facade/Cargo.toml").write_text(
            "[package]\nname='tidepool'\nversion='0.1.0'\nedition='2021'\n"
            "[features]\ndefault=['codex-compat']\n"
            "codex-compat=['exomonad-agent/codex-compat','dep:codex-shoal-protocol']\n"
            "[dependencies]\n"
            "exomonad-agent={path='../agent',default-features=false}\n"
            "codex-shoal-protocol={path='../protocol',optional=true,default-features=false}\n"
        )
        (root / "protocol/Cargo.toml").write_text(
            "[package]\nname='codex-shoal-protocol'\nversion='0.1.0'\nedition='2021'\n"
        )
        for package in ("agent", "facade", "protocol"):
            (root / package / "src/lib.rs").write_text("")

        base = ["cargo", "metadata", "--manifest-path", str(root / "Cargo.toml"), "--format-version", "1"]
        baseline = json.loads(subprocess.check_output(base, cwd=root, text=True))
        default_graph = json.loads(subprocess.check_output(base, cwd=root, text=True))
        default_nodes = {node["id"]: node for node in default_graph["resolve"]["nodes"]}
        agent_id = next(p["id"] for p in default_graph["packages"] if p["name"] == "exomonad-agent")
        self.assertIn("codex-compat", default_nodes[agent_id]["features"])

        disabled, overrides = effective_feature_selection(
            root, {"tidepool", "exomonad-agent", "codex-shoal-protocol"}, set(), {}
        )
        self.assertEqual(disabled, {"tidepool", "exomonad-agent"})
        command = [*base, "--no-default-features"]
        command.extend(metadata_feature_args(baseline, disabled, overrides))
        embedded_graph = json.loads(subprocess.check_output(command, cwd=root, text=True))
        packages = {package["id"]: package for package in embedded_graph["packages"]}
        nodes = {node["id"]: node for node in embedded_graph["resolve"]["nodes"]}
        agent = next(package for package in packages.values() if package["name"] == "exomonad-agent")
        facade = next(package for package in packages.values() if package["name"] == "tidepool")
        protocol_id = next(package["id"] for package in packages.values() if package["name"] == "codex-shoal-protocol")
        self.assertNotIn("codex-compat", nodes[agent["id"]]["features"])
        self.assertFalse(any(edge["pkg"] == protocol_id for edge in nodes[agent["id"]]["deps"]))
        self.assertFalse(any(edge["pkg"] == protocol_id for edge in nodes[facade["id"]]["deps"]))
        reject_forbidden_closure(embedded_graph, {"tidepool", "exomonad-agent"})

        _, compatibility = effective_feature_selection(
            root,
            {"tidepool", "exomonad-agent", "codex-shoal-protocol"},
            set(),
            {"tidepool": {"default"}},
        )
        command = [*base, "--no-default-features"]
        command.extend(metadata_feature_args(baseline, disabled, compatibility))
        compatibility_graph = json.loads(subprocess.check_output(command, cwd=root, text=True))
        with self.assertRaisesRegex(ValueError, "forbidden Codex package codex-shoal-protocol"):
            reject_forbidden_closure(compatibility_graph, {"tidepool", "exomonad-agent"})


if __name__ == "__main__":
    unittest.main()
