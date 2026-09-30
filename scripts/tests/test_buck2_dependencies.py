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
REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"
ROOT_NAMES = (
    "tidepool-atomic-write", "tidepool-repr", "tidepool-heap",
    "tidepool-bignum", "tidepool-bridge", "tidepool-effect", "tidepool-codegen",
    "tidepool-extract-cmd", "tidepool-extract-report", "tidepool-toolchain",
    "tidepool-bridge-derive", "tidepool-runtime",
    "exomonad-model", "exomonad-tool", "tidepool-bridge-effects",
    "exomonad-node", "exomonad-worktree", "exomonad-actor", "exomonad-agent",
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
        self.metadata_path = self.root / "metadata.json"
        self.cargo_lock = self.root / "Cargo.lock"
        metadata, lock_packages = self.fixture_metadata()
        self.metadata_path.write_text(json.dumps(metadata))
        lock = ["# generated fixture lock", "version = 4", ""]
        for name, version in sorted(lock_packages):
            lock.extend([
                "[[package]]", f'name = "{name}"', f'version = "{version}"',
                f'source = "{REGISTRY}"', 'checksum = "' + ("a" * 64) + '"', "",
            ])
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
            "cc": "1.0.0", "windows-only": "1.0.0",
        }
        lock_packages = set(registry_versions.items())
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
            "exomonad-agent": [],
        }
        edges_by_root = {}
        for root_name, deps in declarations.items():
            root_id = local_ids[root_name]
            package_deps = []
            edges = []
            for dep_name, kind, target, use_default, features in deps:
                local_name = dep_name if dep_name in local_ids else None
                package_id = local_ids[local_name] if local_name else f"registry+fixture#{dep_name}@{registry_versions[dep_name]}"
                package_deps.append({
                    "name": dep_name, "rename": None, "kind": kind, "target": target,
                    "optional": False, "uses_default_features": use_default,
                    "features": features,
                })
                edges.append({
                    "name": dep_name.replace("-", "_"), "pkg": package_id,
                    "dep_kinds": [{"kind": kind, "target": target}],
                })
            packages.append({
                "id": root_id, "name": root_name, "version": "0.1.0", "source": None,
                "manifest_path": f"/fixture/{root_name}/Cargo.toml", "features": {},
                "dependencies": package_deps, "targets": [],
            })
            edges_by_root[root_id] = edges

        for name in ("tidepool-test-data",):
            packages.append({
                "id": local_ids[name], "name": name, "version": "0.1.0", "source": None,
                "manifest_path": f"/fixture/{name}/Cargo.toml", "features": {},
                "dependencies": [], "targets": [],
            })

        # Cargo metadata nodes exist for all local package dependencies, including
        # local-only edges. The generator should skip local edges and use Buck labels.
        nodes = [{"id": package_id, "deps": edges_by_root.get(package_id, [])}
                 for package_id in local_ids.values()]
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
        self.assertEqual(deps["serde_json"]["features"], ["arbitrary_precision"])
        self.assertEqual(deps["serde_json"]["default-features"], True)
        self.assertIn("proptest", deps)  # existing repr/atomic test roots retain it
        self.assertNotIn("bridge-dev-only", deps)
        self.assertNotIn("effect-dev-only", deps)
        self.assertNotIn("cc", deps)
        self.assertNotIn("tidepool-bridge-derive", deps)
        self.assertNotIn("tidepool-test-data", deps)

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

    def test_no_default_feature_selection_excludes_optional_vendor_dependency(self):
        metadata, _ = self.fixture_metadata()
        package = next(p for p in metadata["packages"] if p["name"] == "tidepool-codegen")
        package["features"] = {
            "default": ["codex-compat"],
            "codex-compat": [
                "dep:codex_proto", "codex_proto/json", "codex_proto?/transport",
            ],
            "weak-only": ["codex_proto?/transport"],
            "embedded": [],
        }
        package["dependencies"].append({
            "name": "codex-shoal-protocol", "rename": "codex_proto", "kind": None,
            "target": None, "optional": True, "uses_default_features": True,
            "features": [],
        })
        vendor_id = "registry+fixture#codex-shoal-protocol@1.0.0"
        metadata["packages"].append({
            "id": vendor_id, "name": "codex-shoal-protocol", "version": "1.0.0",
            "source": REGISTRY, "features": {}, "dependencies": [], "targets": [],
        })
        node = next(n for n in metadata["resolve"]["nodes"] if n["id"] == package["id"])
        node["deps"].append({
            "name": "codex_proto", "pkg": vendor_id,
            "dep_kinds": [{"kind": None, "target": None}],
        })
        result = self.run_generator(
            metadata, "--no-default-features", "tidepool-codegen",
            "--features", "tidepool-codegen=embedded",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        manifest = tomllib.loads((self.output / "Cargo.toml").read_text())
        self.assertNotIn("codex-shoal-protocol", manifest["dependencies"])

        result = self.run_generator(metadata)
        self.assertEqual(result.returncode, 0, result.stderr)
        manifest = tomllib.loads((self.output / "Cargo.toml").read_text())
        self.assertIn("codex-shoal-protocol", manifest["dependencies"])
        self.assertEqual(
            manifest["dependencies"]["codex-shoal-protocol"]["features"],
            ["json", "transport"],
        )

        result = self.run_generator(
            metadata, "--no-default-features", "tidepool-codegen",
            "--features", "tidepool-codegen=weak-only",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        manifest = tomllib.loads((self.output / "Cargo.toml").read_text())
        self.assertNotIn("codex-shoal-protocol", manifest["dependencies"])


if __name__ == "__main__":
    unittest.main()
