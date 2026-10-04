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

class NativeLocalFeatureAdmission(unittest.TestCase):
    def metadata(self, *, feature=None, kind="dev"):
        root = {"id": "actor", "name": "exomonad-actor", "source": None,
                "features": {}, "dependencies": [{"name": "tidepool-toolchain",
                "rename": "compiler_proof", "kind": kind, "target": None,
                "optional": False, "uses_default_features": False,
                "features": [feature] if feature else []}]}
        owner = {"id": "toolchain", "name": "tidepool-toolchain", "source": None,
                 "features": {}, "dependencies": []}
        return {"packages": [root, owner], "workspace_members": ["actor", "toolchain"]}

    def test_empty_local_requests_have_one_native_configuration(self):
        from buck2_cargo_features import reject_local_feature_requests
        reject_local_feature_requests(self.metadata(), {"exomonad-actor"})

    def test_development_feature_diagnostic_names_root_package_alias_and_kind(self):
        from buck2_cargo_features import reject_local_feature_requests
        with self.assertRaisesRegex(ValueError,
            "package tidepool-toolchain; native root exomonad-actor; "
            "edge exomonad-actor --dev:compiler_proof--> tidepool-toolchain"):
            reject_local_feature_requests(self.metadata(feature="test-support"), {"exomonad-actor"})

    def test_root_override_requires_explicit_native_variant(self):
        from buck2_cargo_features import reject_local_feature_requests
        with self.assertRaisesRegex(ValueError, r"unsupported local Cargo feature\(s\) custom"):
            reject_local_feature_requests(self.metadata(), {"exomonad-actor"},
                                          overrides={"exomonad-actor": {"custom"}})


if __name__ == "__main__":
    unittest.main()
