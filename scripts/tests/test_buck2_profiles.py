"""Resolve profile policy through the real rule functions without starting Buck."""

import configparser
from pathlib import Path
import unittest
from types import SimpleNamespace

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
    namespace["test_source_requirements"] = lambda **kwargs: None
    load_definitions(ROOT / "build/rust/defs.bzl", namespace)
    namespace["cargo"] = SimpleNamespace(
        rust_library=lambda **kwargs: calls.append(("dependency_library", kwargs)),
        rust_binary=lambda **kwargs: calls.append(("dependency_binary", kwargs)),
    )
    load_definitions(ROOT / "build/rust/dependency_profiles.bzl", namespace)
    namespace["buildscript_run"] = lambda **kwargs: calls.append(("buildscript_run", kwargs))
    load_definitions(ROOT / "build/rust/buildscript.bzl", namespace)
    return namespace, calls


def toolchain_flags():
    class Attrs:
        def __getattr__(self, name):
            return lambda *args, **kwargs: None
    namespace = {
        "attrs": Attrs(), "rule": lambda **kwargs: None,
        "DefaultInfo": lambda: None, "RunInfo": lambda **kwargs: kwargs,
        "PanicRuntime": lambda value: value, "RustToolchainInfo": lambda **kwargs: kwargs,
    }
    load_definitions(ROOT / "toolchains/tidepool.bzl", namespace)
    context = SimpleNamespace(attrs=SimpleNamespace(
        python="/pinned/python", test_source_wrapper="wrapper.py", rustc="/pinned/rustc",
        rustdoc="/pinned/rustdoc", clippy="/pinned/clippy", clippy_flags=[],
        linker="/pinned/linker", clippy_toml="clippy.toml",
    ))
    return namespace["_nix_rust_toolchain_impl"](context)[1]["rustc_flags"]


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
            for package in ("tidepool", "tidepool-codegen", "tidepool-repr", "tidepool-heap",
                            "tidepool-toolchain"):
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
                        argv = [*toolchain_flags(), *compiled["rustc_flags"]]
                        levels = [arg for arg in argv if arg.startswith("-Copt-level=")]
                        self.assertEqual(levels, ["-Copt-level=" + expected])
                        if wrapper == "tidepool_rust_isolated_test":
                            self.assertIn("--test", argv)
                            self.assertNotIn("TIDEPOOL_PROPTEST_REGRESSIONS", calls[-1][1]["env"])

    def test_generated_libraries_proc_macros_and_buildscripts_have_one_profile_flag(self):
        for profile in ("fast-dev", "debug", "production"):
            for wrapper, extra in (
                ("tidepool_cargo_rust_library", {}),
                ("tidepool_cargo_rust_library", {"proc_macro": True}),
                ("tidepool_cargo_rust_binary", {"crate": "build_script_build"}),
            ):
                for policy in ("dependency", "harness"):
                    with self.subTest(profile=profile, wrapper=wrapper, policy=policy, extra=extra):
                        namespace, calls = resolve(profile)
                        env = {"CARGO_PKG_NAME": "arbitrary-renamed-package"}
                        if policy == "harness":
                            env["TIDEPOOL_NATIVE_RUST_POLICY"] = policy
                        namespace[wrapper](name="arbitrary-renamed-target", env=env,
                                           rustc_flags=["--cfg=retained"], **extra)
                        compiled = calls[0][1]
                        expected = "2" if profile == "production" or (profile == "fast-dev" and policy == "dependency") else "0"
                        argv = [*toolchain_flags(), *compiled["rustc_flags"]]
                        self.assertEqual([flag for flag in argv if flag.startswith("-Copt-level=")], ["-Copt-level=" + expected])
                        self.assertIn("--cfg=retained", argv)
                        self.assertEqual(compiled["env"], {"CARGO_PKG_NAME": "arbitrary-renamed-package"})
                        for key, value in extra.items():
                            self.assertEqual(compiled[key], value)

    def test_policy_marker_and_manual_optimization_refuse_unknown_or_duplicate_selection(self):
        namespace, calls = resolve("fast-dev")
        with self.assertRaisesRegex(ValueError, "unknown generated Rust policy"):
            namespace["tidepool_cargo_rust_library"](env={"TIDEPOOL_NATIVE_RUST_POLICY": "unknown"})
        for flags in (["-O"], ["-Copt-level=2"], ["-C", "opt-level=2"],
                      ["--codegen=opt-level=2"], ["--codegen", "opt-level=2"]):
            for wrapper in ("tidepool_rust_library", "tidepool_cargo_rust_library", "tidepool_cargo_rust_binary"):
                with self.subTest(flags=flags, wrapper=wrapper), self.assertRaisesRegex(ValueError, "optimization belongs"):
                    args = {"rustc_flags": flags}
                    if wrapper == "tidepool_rust_library":
                        args.update(name="target", package_name="tidepool", package_dir="owned", version="0.1.0")
                    namespace[wrapper](**args)
        for attributes in ({"env": {"TIDEPOOL_NATIVE_RUST_POLICY": "harness"}},
                           {"rustc_flags": ["-Copt-level=2"]}):
            with self.subTest(platform=attributes), self.assertRaises(ValueError):
                namespace["tidepool_cargo_rust_binary"](platform={"linux-x86_64": attributes})
        self.assertEqual(calls, [])

    def test_buildscript_environment_describes_owning_code_and_preserves_no_debuginfo(self):
        for profile in ("fast-dev", "debug", "production"):
            for policy, package in (("dependency", "arbitrary"), ("harness", "arbitrary"),
                                    ("first-party", "tidepool"), ("first-party", "tidepool-codegen")):
                with self.subTest(profile=profile, policy=policy, package=package):
                    namespace, calls = resolve(profile)
                    env = {"CARGO_PKG_NAME": package}
                    if policy == "harness":
                        env["TIDEPOOL_NATIVE_RUST_POLICY"] = policy
                    namespace["tidepool_buildscript_run"](package_name=package, env=env,
                                                          first_party=policy == "first-party")
                    emitted = calls[0][1]["env"]
                    if policy == "first-party":
                        expected = namespace["rust_optimization_level"](package, True)
                    else:
                        expected = namespace["generated_rust_optimization_level"](policy)
                    self.assertEqual(emitted["OPT_LEVEL"], expected)
                    self.assertEqual(emitted["PROFILE"], "release" if profile == "production" else "dev")
                    self.assertEqual(emitted["DEBUG"], "false")
                    self.assertNotIn("TIDEPOOL_NATIVE_RUST_POLICY", emitted)
                    self.assertFalse(any("debuginfo" in flag for flag in toolchain_flags()))
                    self.assertEqual(emitted["CARGO_PKG_NAME"], package)

    def test_plain_maintained_harness_exporter_preserves_rule_and_profile(self):
        for profile, expected in (("fast-dev", "0"), ("debug", "0"), ("production", "2")):
            namespace, calls = resolve(profile)
            namespace["tidepool_rust_binary"](
                name="exporter", package_name="harness", package_dir="web", version="0.1.0",
                compiler_policy="harness", rustc_flags=["--cfg=retained"],
            )
            self.assertEqual(calls[0][0], "binary")
            self.assertEqual(calls[0][1]["rustc_flags"], ["-Copt-level=" + expected, "--cfg=retained"])
            self.assertNotIn("--cap-lints=allow", calls[0][1]["rustc_flags"])
        namespace, calls = resolve("fast-dev")
        with self.assertRaisesRegex(ValueError, "unknown generated Rust policy"):
            namespace["tidepool_rust_binary"](
                name="exporter", package_name="harness", package_dir="web", version="0.1.0",
                compiler_policy="unknown",
            )
        self.assertEqual(calls, [])

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
        toolchains = {call["name"]: call for call in calls}
        self.assertEqual(set(toolchains), {"c", "cxx"})
        self.assertEqual(len(toolchains), len(calls), "duplicate toolchain declaration")
        for name, linker in (("c", paths["cc"]), ("cxx", paths["cxx"])):
            with self.subTest(toolchain=name):
                self.assertEqual(toolchains[name]["linker"], linker)
                self.assertEqual(toolchains[name]["compiler"], paths["cc"])
                self.assertEqual(toolchains[name]["cxx_compiler"], paths["cxx"])
                self.assertEqual(toolchains[name]["archiver"], paths["ar"])
                self.assertEqual(toolchains[name]["link_flags"], [
                    "-fuse-ld=lld", "-B/pinned/lld/bin/", "-B/pinned/binutils/bin/",
                ])


if __name__ == "__main__":
    unittest.main()
