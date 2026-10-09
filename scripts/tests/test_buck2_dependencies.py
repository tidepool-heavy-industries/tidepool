#!/usr/bin/env python3
"""Keep the Reindeer manifest aligned with selected Linux Cargo declarations."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
import tomllib


SCRIPT = Path(__file__).resolve().parents[1] / "buck2-dependencies.py"
FEATURES = SCRIPT.with_name("buck2_cargo_features.py")
PROFILE = SCRIPT.with_name("native-profile.toml")
REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"
ROOT_NAMES = (
    "tidepool-atomic-write", "tidepool-repr", "tidepool-heap",
    "tidepool-bignum", "tidepool-bridge", "tidepool-effect", "tidepool-codegen",
    "tidepool-extract-cmd", "tidepool-extract-report", "tidepool-toolchain",
    "tidepool-bridge-derive", "tidepool-runtime",
    "exomonad-model", "exomonad-tool", "tidepool-bridge-effects",
    "exomonad-node", "exomonad-worktree", "exomonad-actor", "exomonad-model-output", "jev-integration",
    "tidepool-mcp", "tidepool-handlers", "tidepool", "tidepool-testing",
    "tidepool-test-data", "tidepool-prepared-corpus", "tidepool-protocol",
)
HARNESS_GIT_SOURCE = (
    "git+https://github.com/tidepool-heavy-industries/exomonad-harness.git"
    "?rev=763d38d57691da31d2bf3c3c7493ca9910067540"
    "#763d38d57691da31d2bf3c3c7493ca9910067540"
)


class DependencyManifest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="buck-dependencies-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "scripts").mkdir()
        (self.root / "bin").mkdir()
        self.script = self.root / "scripts/buck2-dependencies.py"
        self.script.write_bytes(SCRIPT.read_bytes())
        self.script.chmod(0o755)
        (self.root / "scripts/buck2_cargo_features.py").write_bytes(FEATURES.read_bytes())
        (self.root / "scripts/native-profile.toml").write_bytes(PROFILE.read_bytes())
        self.metadata_path = self.root / "metadata.json"
        self.cargo_lock = self.root / "Cargo.lock"
        metadata, lock_packages = self.fixture_metadata()
        self.metadata_path.write_text(json.dumps(metadata))
        lock = ["# generated fixture lock", "version = 4", ""]
        for name, version, source in sorted(lock_packages):
            lock.extend(["[[package]]", f'name = "{name}"', f'version = "{version}"'])
            if source.startswith("git+"):
                lock.append(f'source = "{source}"')
            else:
                lock.extend([f'source = "{source}"', 'checksum = "' + ("a" * 64) + '"'])
            lock.append("")
        self.cargo_lock.write_text("\n".join(lock))
        fake_cargo = self.root / "bin/cargo"
        fake_cargo.write_text(
            """#!/usr/bin/env python3
import json, os, sys
if '--offline' in sys.argv:
    raise SystemExit(0)
print(open(os.environ['FAKE_METADATA']).read())
"""
        )
        fake_cargo.chmod(0o755)
        self.env = os.environ | {
            "PATH": f"{self.root / 'bin'}:{os.environ['PATH']}",
            "FAKE_METADATA": str(self.metadata_path),
        }
        self.output = self.root / "generated"

    def fixture_metadata(self):
        packages = []
        nodes = []
        local_ids = {name: f"path+file:///fixture/{name}#{name}@0.1.0" for name in ROOT_NAMES}
        local_ids.update({
            name: f"path+file:///fixture/{name}#{name}@0.1.0"
            for name in (
                "tidepool-bridge-derive", "tidepool-test-data", "tidepool-extract-cmd",
                "tidepool-extract-report", "tidepool-toolchain",
            )
        })
        registry_versions = {
            "tempfile": "3.27.0", "ciborium": "0.2.2", "thiserror": "2.0.20",
            "blake3": "1.8.2", "serde": "1.0.228",
            "proptest": "1.11.0", "num-bigint": "0.4.8", "serde_json": "1.0.151",
            "chrono": "0.4.45", "libc": "0.2.189", "frunk": "0.5.0",
            "serial_test": "3.2.0", "syn": "2.0.0", "quote": "1.0.0",
            "proc-macro2": "1.0.0",
            "parking_lot": "0.12.5", "cranelift-codegen": "0.129.1",
            "bridge-dev-only": "1.0.0", "effect-dev-only": "1.0.0",
            "cc": "1.0.0", "windows-only": "1.0.0", "harness": "0.1.0",
        }
        lock_packages = {(name, version, REGISTRY) for name, version in registry_versions.items()}
        for name, version in registry_versions.items():
            package_id = f"registry+fixture#{name}@{version}"
            packages.append({
                "id": package_id, "name": name, "version": version,
                "source": REGISTRY, "features": {}, "dependencies": [],
                "targets": [],
            })

        declarations = {
            "tidepool-atomic-write": [
                ("tempfile", None, None, True, []),
                ("proptest", "dev", None, True, []),
            ],
            "tidepool-repr": [
                ("ciborium", None, None, True, []),
                ("proptest", "dev", None, True, []),
                ("tidepool-atomic-write", None, None, True, []),
            ],
            "tidepool-heap": [
                ("thiserror", None, None, True, []),
                ("tidepool-repr", None, None, True, []),
            ],
            "tidepool-bignum": [
                ("num-bigint", None, None, True, []),
            ],
            "tidepool-bridge": [
                ("serde_json", None, None, True, ["arbitrary_precision"]),
                ("tidepool-repr", None, None, True, []),
                ("proptest", "dev", None, True, []),
                ("bridge-dev-only", "dev", None, True, []),
                ("tidepool-bridge-derive", "dev", None, True, []),
                ("tidepool-test-data", "dev", None, True, []),
            ],
            "tidepool-effect": [
                ("frunk", None, None, True, []),
                ("parking_lot", None, None, True, []),
                ("proptest", "dev", None, True, []),
                ("effect-dev-only", "dev", None, True, []),
                ("tidepool-bridge", None, None, True, []),
                ("tidepool-repr", None, None, True, []),
            ],
            "tidepool-codegen": [
                ("chrono", None, None, False, ["alloc"]),
                ("serde_json", None, None, True, ["arbitrary_precision"]),
                ("cranelift-codegen", None, None, True, []),
                ("libc", None, "cfg(unix)", True, []),
                ("cc", "build", None, True, []),
                ("tidepool-bignum", None, None, True, []),
                ("tidepool-bridge", None, None, True, []),
                ("tidepool-effect", None, None, True, []),
                ("tidepool-heap", None, None, True, []),
                ("tidepool-repr", None, None, True, []),
            ],
            "tidepool-extract-cmd": [
                ("blake3", None, None, True, []),
                ("serde_json", "dev", None, True, []),
                ("tempfile", "dev", None, True, []),
            ],
            "tidepool-extract-report": [
                ("blake3", None, None, True, []),
                ("serde", None, None, True, ["derive"]),
                ("serde_json", None, None, True, ["arbitrary_precision"]),
                ("thiserror", None, None, True, []),
            ],
            "tidepool-toolchain": [
                ("tidepool-atomic-write", None, None, True, []),
                ("tidepool-extract-cmd", None, None, True, []),
                ("tidepool-extract-report", None, None, True, []),
                ("tidepool-repr", None, None, True, []),
                ("serial_test", "dev", None, True, []),
            ],
            "tidepool-bridge-derive": [
                ("syn", None, None, True, ["extra-traits"]),
                ("quote", None, None, True, []),
                ("proc-macro2", None, None, True, []),
            ],
            "tidepool-runtime": [
                ("tidepool-toolchain", None, None, True, []),
                ("thiserror", None, None, True, []),
            ],
            "exomonad-model": [],
            "exomonad-tool": [],
            "tidepool-bridge-effects": [],
            "exomonad-node": [],
            "exomonad-worktree": [],
            "exomonad-actor": [],
            "tidepool-mcp": [],
            "tidepool-handlers": [],
            "tidepool": [
                ("tidepool-toolchain", "build", None, True, []),
                ("serde", None, None, True, ["derive"]),
                ("serde", "build", None, True, ["derive"]),
                ("serde_json", None, None, True, []),
                ("serde_json", "build", None, True, []),
                ("harness", None, None, True, [], "harness_git", "git"),
                ("harness", None, None, True, [], "harness_registry", "registry"),
                ("tidepool-testing", "dev", None, True, []),
            ],
            "tidepool-testing": [
                ("tidepool-repr", None, None, True, []),
                ("serde_json", None, None, True, ["float_roundtrip"]),
                ("tidepool-runtime", None, None, True, []),
                ("tidepool-toolchain", None, None, True, []),
                ("tidepool-mcp", None, None, True, []),
                ("tidepool-effect", None, None, True, []),
                ("tidepool-bridge", None, None, True, []),
                ("tidepool-bridge-derive", None, None, True, []),
                ("tidepool-bridge-effects", None, None, True, []),
                ("frunk", None, None, True, []),
            ],
        }
        edges_by_root = {}
        for root_name in ROOT_NAMES:
            deps = declarations.get(root_name, [])
            root_id = local_ids[root_name]
            package_deps = []
            edges = []
            for declaration in deps:
                dep_name, kind, target, use_default, features, *extra = declaration
                rename = extra[0] if extra else None
                source_mode = extra[1] if len(extra) > 1 else None
                local_name = dep_name if dep_name in local_ids else None
                if local_name:
                    package_id = local_ids[local_name]
                elif dep_name == "harness" and source_mode == "git":
                    package_id = f"{HARNESS_GIT_SOURCE}#harness@0.1.0"
                else:
                    package_id = f"registry+fixture#{dep_name}@{registry_versions[dep_name]}"
                dep_entry = {
                    "name": dep_name, "rename": rename, "kind": kind, "target": target,
                    "optional": False, "uses_default_features": use_default,
                    "features": features,
                }
                if source_mode == "git":
                    dep_entry["source"] = HARNESS_GIT_SOURCE.rsplit("#", 1)[0]
                package_deps.append(dep_entry)
                edges.append({
                    "name": (rename or dep_name).replace("-", "_"), "pkg": package_id,
                    "dep_kinds": [{"kind": kind, "target": target}],
                })
                if dep_name == "harness" and not any(p["id"] == package_id for p in packages):
                    source = HARNESS_GIT_SOURCE if source_mode == "git" else REGISTRY
                    packages.append({
                        "id": package_id, "name": "harness", "version": "0.1.0",
                        "source": source, "features": {}, "dependencies": [], "targets": [],
                    })
            packages.append({
                "id": root_id, "name": root_name, "version": "0.1.0", "source": None,
                "manifest_path": f"/fixture/{root_name}/Cargo.toml", "features": {},
                "dependencies": package_deps, "targets": [],
            })
            edges_by_root[root_id] = edges

        # Cargo metadata nodes exist for all local package dependencies, including
        # local-only edges. The generator should skip local edges and use Buck labels.
        nodes = [{"id": package_id, "deps": edges_by_root.get(package_id, [])}
                 for package_id in local_ids.values()]
        node_ids = {node["id"] for node in nodes}
        nodes.extend({"id": package["id"], "deps": []}
                     for package in packages if package["id"] not in node_ids)
        return {
            "packages": packages,
            "workspace_members": list(local_ids.values()),
            "resolve": {"nodes": nodes},
        }, lock_packages

    def run_generator(self, metadata=None, *args):
        if metadata is not None:
            self.metadata_path.write_text(json.dumps(metadata))
        return subprocess.run(
            ["python3", str(self.script), "--output-dir", str(self.output), *args],
            cwd=self.root, env=self.env, capture_output=True, text=True, timeout=10,
        )

    def test_library_roots_use_declared_features_and_preserve_existing_test_deps(self):
        result = self.run_generator()
        self.assertEqual(result.returncode, 0, result.stderr)
        manifest = tomllib.loads((self.output / "Cargo.toml").read_text())
        deps = manifest["dependencies"]
        self.assertEqual(deps["chrono"]["default-features"], False)
        self.assertEqual(deps["chrono"]["features"], ["alloc"])
        self.assertEqual(deps["serde_json"]["features"], ["arbitrary_precision", "float_roundtrip"])
        self.assertEqual(deps["serde_json"]["default-features"], True)
        self.assertIn("proptest", deps)  # existing repr/atomic test roots retain it
        self.assertIn("bridge-dev-only", deps)
        self.assertIn("effect-dev-only", deps)
        self.assertNotIn("cc", deps)
        self.assertNotIn("tidepool-bridge-derive", deps)
        self.assertNotIn("tidepool-test-data", deps)

    def test_facade_build_dependencies_preserve_normal_edges_and_fail_closed(self):
        control = self.run_generator()
        self.assertEqual(control.returncode, 0, control.stderr)
        manifest_path = self.output / "Cargo.toml"
        previous = manifest_path.read_bytes()
        dependencies = tomllib.loads(previous.decode())["dependencies"]
        self.assertIn("derive", dependencies["serde"]["features"])
        self.assertTrue(dependencies["serde"]["default-features"])
        self.assertTrue(dependencies["serde_json"]["default-features"])
        for name in ("serde", "serde_json"):
            with self.subTest(missing_build_edge=name):
                metadata, _ = self.fixture_metadata()
                facade = next(p for p in metadata["packages"] if p["name"] == "tidepool")
                node = next(n for n in metadata["resolve"]["nodes"] if n["id"] == facade["id"])
                node["deps"] = [
                    edge for edge in node["deps"]
                    if not (edge["name"] == name and edge["dep_kinds"][0]["kind"] == "build")
                ]
                self.assertTrue(any(edge["name"] == name for edge in node["deps"]))
                result = self.run_generator(metadata)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(f"Missing or ambiguous Linux-resolved dependency {name}", result.stderr)
                self.assertEqual(manifest_path.read_bytes(), previous)
        for field, value in (("name", "new-build-helper"), ("rename", "serde_alias"),
                             ("target", "cfg(unix)")):
            with self.subTest(unmodeled_build_declaration=field):
                metadata, _ = self.fixture_metadata()
                facade = next(p for p in metadata["packages"] if p["name"] == "tidepool")
                dependency = next(d for d in facade["dependencies"]
                                  if d["name"] == "serde" and d["kind"] == "build")
                dependency[field] = value
                if field == "target":
                    # Keep the alias unambiguous so build admission owns this refusal.
                    facade["dependencies"] = [d for d in facade["dependencies"]
                                              if not (d["name"] == "serde" and d["kind"] is None)]
                result = self.run_generator(metadata)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("Model build dependency for tidepool", result.stderr)
                self.assertEqual(manifest_path.read_bytes(), previous)

    def test_unmodeled_codegen_build_dependency_is_rejected(self):
        metadata, _ = self.fixture_metadata()
        package = next(p for p in metadata["packages"] if p["name"] == "tidepool-codegen")
        dependency = next(d for d in package["dependencies"] if d["kind"] == "build")
        dependency["name"] = "new-build-helper"
        result = self.run_generator(metadata)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Unmodeled codegen build dependency", result.stderr)

    def test_unsupported_linux_selector_fails_before_writing_manifest(self):
        metadata, _ = self.fixture_metadata()
        codegen_id = next(p["id"] for p in metadata["packages"] if p["name"] == "tidepool-codegen")
        package = next(p for p in metadata["packages"] if p["name"] == "tidepool-codegen")
        package["dependencies"].append({
            "name": "windows-only", "rename": None, "kind": None,
            "target": "cfg(windows)", "optional": False,
            "uses_default_features": True, "features": [],
        })
        node = next(n for n in metadata["resolve"]["nodes"] if n["id"] == codegen_id)
        node["deps"].append({
            "name": "windows_only", "pkg": f"registry+fixture#windows-only@1.0.0",
            "dep_kinds": [{"kind": None, "target": "cfg(windows)"}],
        })
        result = self.run_generator(metadata)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Unsupported Linux dependency selector", result.stderr)
        self.assertFalse((self.output / "Cargo.toml").exists())

    def test_missing_active_linux_edge_fails_closed(self):
        metadata, _ = self.fixture_metadata()
        codegen_id = next(p["id"] for p in metadata["packages"] if p["name"] == "tidepool-codegen")
        node = next(n for n in metadata["resolve"]["nodes"] if n["id"] == codegen_id)
        node["deps"] = [edge for edge in node["deps"] if edge["name"] != "libc"]
        # Remove the inactive windows declaration so the active libc edge is the failure.
        package = next(p for p in metadata["packages"] if p["name"] == "tidepool-codegen")
        package["dependencies"] = [d for d in package["dependencies"] if d["name"] != "windows-only"]
        result = self.run_generator(metadata)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Missing or ambiguous Linux-resolved dependency libc", result.stderr)
        self.assertFalse((self.output / "Cargo.toml").exists())

    def test_local_development_features_refuse_before_manifest_publication(self):
        control = self.run_generator()
        self.assertEqual(control.returncode, 0, control.stderr)
        previous = (self.output / "Cargo.toml").read_bytes()
        metadata, _ = self.fixture_metadata()
        runtime = next(p for p in metadata["packages"] if p["name"] == "tidepool-runtime")
        runtime["dependencies"].append({
            "name": "tidepool-toolchain", "rename": "proof_fixture", "kind": "dev",
            "target": None, "optional": False, "uses_default_features": False,
            "features": ["test-support"],
        })
        result = self.run_generator(metadata)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unsupported local Cargo feature(s) test-support", result.stderr)
        self.assertIn("package tidepool-toolchain; native root tidepool-runtime", result.stderr)
        self.assertIn("tidepool-runtime --dev:proof_fixture--> tidepool-toolchain", result.stderr)
        self.assertEqual((self.output / "Cargo.toml").read_bytes(), previous)

    def test_plain_shared_profile_rejects_transitive_codex_closure_without_publishing(self):
        metadata, _ = self.fixture_metadata()
        tidepool = next(p for p in metadata["packages"] if p["name"] == "tidepool")
        codex_id = "registry+fixture#codex-shoal-protocol@1.0.0"
        metadata["packages"].append({
            "id": codex_id, "name": "codex-shoal-protocol", "version": "1.0.0",
            "source": REGISTRY, "features": {}, "dependencies": [], "targets": [],
        })
        transitive_id = "registry+fixture#profile-transitive@1.0.0"
        metadata["packages"].append({
            "id": transitive_id, "name": "profile-transitive", "version": "1.0.0",
            "source": REGISTRY, "features": {}, "dependencies": [], "targets": [],
        })
        tidepool_node = next(n for n in metadata["resolve"]["nodes"] if n["id"] == tidepool["id"])
        tidepool_node["deps"].append({
            "name": "profile_transitive", "pkg": transitive_id,
            "dep_kinds": [{"kind": None, "target": None}],
        })
        metadata["resolve"]["nodes"].append({
            "id": transitive_id, "deps": [{
                "name": "codex_shoal_protocol", "pkg": codex_id,
                "dep_kinds": [{"kind": None, "target": None}],
            }],
        })
        metadata["resolve"]["nodes"].append({"id": codex_id, "deps": []})
        self.output.mkdir()
        (self.output / "Cargo.toml").write_text("previous manifest\n")
        (self.output / "Cargo.lock").write_text("previous lock\n")

        result = self.run_generator(metadata)

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("forbidden Codex package codex-shoal-protocol", result.stderr)
        self.assertEqual((self.output / "Cargo.toml").read_text(), "previous manifest\n")
        self.assertEqual((self.output / "Cargo.lock").read_text(), "previous lock\n")

    def test_locked_https_git_source_and_same_version_registry_identity_are_preserved(self):
        result = self.run_generator()
        self.assertEqual(result.returncode, 0, result.stderr)
        manifest = tomllib.loads((self.output / "Cargo.toml").read_text())
        aliases = [
            (alias, spec) for alias, spec in manifest["dependencies"].items()
            if spec.get("package") == "harness"
        ]
        self.assertEqual(len(aliases), 2)
        self.assertEqual(len({alias for alias, _ in aliases}), 2)
        git_specs = [(alias, spec) for alias, spec in aliases if "git" in spec]
        registry_specs = [(alias, spec) for alias, spec in aliases if "git" not in spec]
        self.assertEqual(len(git_specs), 1)
        self.assertEqual(len(registry_specs), 1)
        self.assertEqual(git_specs[0][1]["git"], "https://github.com/tidepool-heavy-industries/exomonad-harness.git")
        self.assertEqual(git_specs[0][1]["rev"], "763d38d57691da31d2bf3c3c7493ca9910067540")
        self.assertEqual(git_specs[0][1]["version"], registry_specs[0][1]["version"])

    def test_unlocked_git_sources_fail_closed(self):
        metadata, _ = self.fixture_metadata()
        package = next(p for p in metadata["packages"] if p["name"] == "tidepool")
        dependency = next(d for d in package["dependencies"] if d["rename"] == "harness_git")
        dependency["source"] = "git+ssh://github.com/tidepool-heavy-industries/exomonad-harness.git#deadbeef"
        harness = next(p for p in metadata["packages"] if p["name"] == "harness" and p["source"].startswith("git+"))
        harness["source"] = dependency["source"]
        result = self.run_generator(metadata)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unsupported Cargo dependency source", result.stderr)


if __name__ == "__main__":
    unittest.main()
