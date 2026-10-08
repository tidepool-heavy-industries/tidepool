"""Resolve profile policy through the real rule functions without starting Buck."""

import configparser
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]


def load_definitions(path, namespace):
    # Stub Buck configuration and final rule admission; execute the owning
    # policy in its Python-compatible Starlark subset.
    exec("\n".join(line for line in path.read_text().splitlines()
                   if not line.startswith("load(")), namespace)


def resolve(profile):
    calls = []

    def read_config(section, key, default=None):
        if (section, key) == ("tidepool", "profile"):
            return profile
        return "/pinned/" + key

    def fail(message):
        raise ValueError(message)

    namespace = {"read_root_config": read_config, "fail": fail}
    load_definitions(ROOT / "build/native_profile.bzl", namespace)
    for kind in ("library", "binary", "test"):
        namespace["rust_" + kind] = lambda kind=kind, **kwargs: calls.append((kind, kwargs))
    namespace["sh_test"] = lambda **kwargs: calls.append(("runner", kwargs))
    load_definitions(ROOT / "build/rust/defs.bzl", namespace)
    return namespace, calls


class BuckProfileConfiguration(unittest.TestCase):
    def test_checked_in_default_resolves_fast_dev_and_unknown_profile_refuses(self):
        config = configparser.ConfigParser(interpolation=None)
        config.read(ROOT / ".buckconfig")
        namespace, _ = resolve(config["tidepool"]["profile"])
        self.assertEqual(namespace["selected_native_profile"](), "fast-dev")
        refused, _ = resolve("unknown")
        with self.assertRaisesRegex(ValueError, "unknown Tidepool native profile"):
            refused["rust_optimization_level"]("tidepool", True)
        with self.assertRaises(ValueError):
            refused["haskell_optimization_flags"]()

    def test_resolved_rule_argv_keeps_hot_libraries_and_harness_policy_distinct(self):
        for profile in ("fast-dev", "debug", "production"):
            for package in ("tidepool", "tidepool-codegen", "tidepool-repr", "tidepool-heap"):
                for wrapper in ("tidepool_rust_library", "tidepool_rust_binary",
                                "tidepool_rust_test", "tidepool_rust_isolated_test"):
                    with self.subTest(profile=profile, package=package, wrapper=wrapper):
                        namespace, calls = resolve(profile)
                        namespace[wrapper](
                            name="renamed_case", package_name=package,
                            package_dir="owned/package", version="0.1.0",
                            crate_root="owned/package/src/lib.rs",
                        )
                        compiled = calls[0][1]
                        expected = "0"
                        if profile == "production" or (
                            profile == "fast-dev" and package != "tidepool"
                            and wrapper == "tidepool_rust_library"
                        ):
                            expected = "3"
                        # The rule's final optimization flag overrides the
                        # current toolchain's O2 prefix.
                        argv = ["-Copt-level=2", *compiled["rustc_flags"]]
                        levels = [arg for arg in argv if arg.startswith("-Copt-level=")]
                        self.assertEqual(levels, ["-Copt-level=2", "-Copt-level=" + expected])
                        if wrapper == "tidepool_rust_isolated_test":
                            self.assertIn("--test", argv)
                            self.assertNotIn("TIDEPOOL_PROPTEST_REGRESSIONS", calls[-1][1]["env"])

    def test_shared_libtest_binary_remains_unoptimized_in_fast_dev(self):
        namespace, calls = resolve("fast-dev")
        namespace["tidepool_rust_binary"](
            name="tidepool_heap", package_name="tidepool-heap",
            package_dir="tidepool/heap", version="0.1.0", rustc_flags=["--test"],
        )
        self.assertEqual(calls[0][1]["rustc_flags"], ["-Copt-level=0", "--test"])

    def test_haskell_policy_retains_production_optimization(self):
        for profile, expected in (("fast-dev", []), ("debug", []), ("production", ["-O2"])):
            with self.subTest(profile=profile):
                namespace, _ = resolve(profile)
                self.assertEqual(namespace["haskell_optimization_flags"](), expected)

    def test_resolved_link_driver_selects_only_the_declared_lld(self):
        calls = []
        paths = {
            "cc": "/pinned/cc/bin/cc", "cxx": "/pinned/cc/bin/c++",
            "ar": "/pinned/binutils/bin/ar", "lld_bin": "/pinned/lld/bin",
        }
        namespace = {
            "read_root_config": lambda section, key, default=None: paths.get(key, "/pinned/" + key),
            "glob": lambda _: [], "JEV_SOURCE_PATHS": [],
        }
        for name in ("NATIVE_LINT_ALLOW", "NATIVE_LINT_WARN", "NATIVE_LINT_DENY", "NATIVE_LINT_FORBID"):
            namespace[name] = []
        for rule in ("nix_rust_toolchain", "nix_haskell_toolchain", "system_genrule_toolchain",
                     "noop_test_toolchain", "remote_test_execution_toolchain",
                     "system_python_bootstrap_toolchain", "nix_tool", "nix_directory",
                     "native_catalog_retention", "filegroup"):
            namespace[rule] = lambda **kwargs: None
        namespace["system_cxx_toolchain"] = lambda **kwargs: calls.append(kwargs)
        load_definitions(ROOT / "toolchains/BUCK", namespace)
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0]["linker"], paths["cxx"])
        self.assertEqual(calls[0]["link_flags"], [
            "-fuse-ld=lld", "-B/pinned/lld/bin/", "-B/pinned/binutils/bin/",
        ])


if __name__ == "__main__":
    unittest.main()
