"""Focused evidence checks for the pinned Cabal/GHC build recorder."""

import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import tarfile
import hashlib
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT))
SPEC = importlib.util.spec_from_file_location("record_ghc_build", ROOT / "record-ghc-build.py")
BUILD = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BUILD)
from ghc_source_options import audit_source_optimization_options, reject_preprocessing_arguments


class SourceOptimizationAuditTests(unittest.TestCase):
    def snapshot(self, root, files):
        snapshot = root / "snapshot"
        snapshot.mkdir()
        entries = []
        with tarfile.open(snapshot / "sources.tar", "w") as archive:
            for name, data in files.items():
                info = tarfile.TarInfo(name)
                info.size = len(data)
                archive.addfile(info, __import__("io").BytesIO(data))
                entries.append({"path": name, "kind": "file", "bytes": len(data),
                                "sha256": hashlib.sha256(data).hexdigest()})
        manifest = {"version": 1, "capture_changes": [],
                    "archive_sha256": BUILD.digest(snapshot / "sources.tar"),
                    "source": {"entries": entries}}
        (snapshot / "manifest.json").write_text(json.dumps(manifest))
        return snapshot

    def audit_snapshot(self, snapshot):
        manifest = json.loads((snapshot / "manifest.json").read_text())
        return audit_source_optimization_options(snapshot / "sources.tar", manifest)

    def test_exact_archived_pragma_o0_fails_even_with_driver_o2(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            snapshot = self.snapshot(root, {
                "bridge/haskell/app/Main.hs": b"{-# OPTIONS_GHC -O0 #-}\nmain = pure ()\n"
            })
            with self.assertRaisesRegex(ValueError, "OPTIONS_GHC"):
                self.audit_snapshot(snapshot)

    def test_actual_pragma_free_worker_source_is_accepted_from_archive(self):
        root = ROOT.parents[1]
        sources = {}
        for source_root in (root / "bridge/haskell/src", root / "bridge/haskell/app"):
            for source in source_root.rglob("*"):
                if source.is_file() and source.suffix in (".hs", ".lhs"):
                    sources[source.relative_to(root).as_posix()] = source.read_bytes()
        with tempfile.TemporaryDirectory() as temporary:
            snapshot_dir = self.snapshot(Path(temporary), sources)
            result = self.audit_snapshot(snapshot_dir)
            self.assertGreater(result["scanned_files"], 40)
            self.assertEqual(result["scanned_files"], len(result["sources"]))

    def test_any_module_options_pragma_is_refused_to_avoid_partial_optimizer_allowlist(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            snapshot = self.snapshot(root, {
                "bridge/haskell/src/Module.hs": b"{-# OPTIONS_GHC -Wno-unused-imports #-}\n"
            })
            with self.assertRaisesRegex(ValueError, "OPTIONS_GHC"):
                self.audit_snapshot(snapshot)

    def test_legacy_and_case_insensitive_options_pragmas_are_refused(self):
        for pragma in (b"OPTIONS", b"options", b"options_ghc", b"Options_Ghc"):
            with self.subTest(pragma=pragma), tempfile.TemporaryDirectory() as temporary:
                snapshot = self.snapshot(Path(temporary), {
                    "bridge/haskell/app/Main.hs": b"{-# " + pragma + b" -O0 #-}\nmain = pure ()\n"
                })
                with self.assertRaisesRegex(ValueError, "OPTIONS_GHC"):
                    self.audit_snapshot(snapshot)

    def test_unicode_header_whitespace_cannot_hide_module_options(self):
        for pragma in ("{-#\u00a0OPTIONS_GHC -O0 #-}", "{-#\u00a0LANGUAGE\u00a0CPP #-}"):
            with self.subTest(pragma=pragma), tempfile.TemporaryDirectory() as temporary:
                snapshot = self.snapshot(Path(temporary), {
                    "bridge/haskell/app/Main.hs": (pragma + "\nmain = pure ()\n").encode("utf-8")
                })
                with self.assertRaises(ValueError):
                    self.audit_snapshot(snapshot)

    def test_cpp_generated_pragma_path_is_refused(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for source in (b"{-# LANGUAGE CPP #-}\n", b"#if 1\n{-# OPTIONS_GHC -O0 #-}\n#endif\n",
                           b"{-# OPTIONS_GHC -cpp #-}\n", b"{-# language cpp #-}\n"):
                snapshot = self.snapshot(root, {"bridge/haskell/src/Module.hs": source})
                with self.subTest(source=source), self.assertRaises(ValueError):
                    self.audit_snapshot(snapshot)
                import shutil
                shutil.rmtree(snapshot)

    def test_archive_manifest_mismatch_fails_closed(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            snapshot = self.snapshot(root, {"bridge/haskell/app/Main.hs": b"main = pure ()\n"})
            archive = snapshot / "sources.tar"
            archive.write_bytes(archive.read_bytes() + b"changed")
            with self.assertRaisesRegex(ValueError, "archive digest"):
                self.audit_snapshot(snapshot)

    def test_preprocessor_unlit_and_source_phase_overrides_are_refused(self):
        for flag in ("-pgmP", "-pgmF", "-pgmL", "-optP", "-optF", "-optL", "-x"):
            for argument in (flag, flag + "override"):
                with self.subTest(argument=argument), self.assertRaisesRegex(ValueError, "preprocessed"):
                    reject_preprocessing_arguments(["ghc", "-c", "-O2", argument, "Main.lhs"])


class ResponseFileTests(unittest.TestCase):
    def test_matches_ghc_quotes_backslash_escaping_and_whitespace(self):
        self.assertEqual(
            BUILD.ghc_response_words("-c\n-O2\n'path with space.hs'\n\"double quote\"\nplain\\ value\n"),
            ["-c", "-O2", "path with space.hs", "double quote", "plain value"],
        )

    def test_matches_ghc_empty_argument_filter(self):
        self.assertEqual(BUILD.ghc_response_words("'' \"\" -O2"), ["-O2"])

    def test_expands_one_response_layer_and_retains_exact_file_digest(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            response = root / "flags.rsp"
            response.write_text("-c -O2 'Main File.hs' @nested.rsp")
            expanded, files = BUILD.expand_response_args(["@flags.rsp", "-package-db=db"], root)
            self.assertEqual(expanded, ["-c", "-O2", "Main File.hs", "@nested.rsp", "-package-db=db"])
            self.assertEqual(files, [{"path": str(response), "bytes": response.stat().st_size,
                                      "sha256": BUILD.digest(response)}])

    def test_missing_or_oversized_response_file_is_refused(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with self.assertRaises(FileNotFoundError):
                BUILD.expand_response_args(["@missing.rsp"], root)
            response = root / "large.rsp"
            response.write_bytes(b" " * (BUILD.RESPONSE_FILE_LIMIT + 1))
            with self.assertRaisesRegex(ValueError, "exceeds"):
                BUILD.expand_response_args(["@large.rsp"], root)


class CompilerActionTests(unittest.TestCase):
    def test_output_lookup_uses_the_same_optimized_cabal_configuration(self):
        arguments = ("cabal", "wrapper", "ghc-pkg", "/evidence/build")
        build = BUILD.build_command(*arguments)
        lookup = BUILD.build_command(*arguments, "list-bin")
        self.assertEqual(lookup[:2], ["cabal", "list-bin"])
        self.assertEqual(lookup[2:], build[4:])
        self.assertIn("--enable-optimization=2", lookup)

    def test_cabal_build_pins_target_jobs_compiler_package_tool_and_o2(self):
        command = BUILD.build_command("/nix/store/cabal", "/evidence/ghc-wrapper",
                                     "/nix/store/ghc-pkg", "/evidence/cabal-build")
        self.assertEqual(command, ["/nix/store/cabal", "build", "-j2", "--offline",
                                   "--builddir=/evidence/cabal-build",
                                   "--with-compiler=/evidence/ghc-wrapper",
                                   "--with-hc-pkg=/nix/store/ghc-pkg",
                                   "--enable-optimization=2", "tidepool-extract-bin"])

    def test_wrapper_preserves_package_db_flags_and_exact_exit_code(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            response = root / "flags.rsp"
            response.write_text("-c -O2 Main.hs '-package-db=/nix/store/pkg db'")
            action_dir = root / "actions"
            action_dir.mkdir()
            observed = root / "forwarded.json"
            fake = root / "fake-ghc"
            fake.write_text(
                f"#!{sys.executable}\n"
                "import json, os, sys\n"
                "json.dump(sys.argv[1:], open(os.environ['GHC_ARG_CAPTURE'], 'w'))\n"
                "raise SystemExit(23)\n")
            fake.chmod(0o755)
            environment = {BUILD.COMPILER_ENV: str(fake), BUILD.ACTION_DIR_ENV: str(action_dir),
                           "GHC_ARG_CAPTURE": str(observed)}
            with patch.dict(os.environ, environment), patch("os.getcwd", return_value=str(root)):
                result = BUILD.ghc_wrapper_main(["@flags.rsp"])
            self.assertEqual(result, 23)
            self.assertEqual(json.loads(observed.read_text()), ["@flags.rsp"])
            row_path, = action_dir.glob("*.json")
            row = json.loads(row_path.read_text())
            self.assertEqual(row["cwd"], str(root))
            self.assertEqual(row["raw_argv"], ["@flags.rsp"])
            self.assertEqual(row["argv"], [str(fake.resolve()), "-c", "-O2", "Main.hs",
                                            "-package-db=/nix/store/pkg db"])
            self.assertEqual(row["exit_code"], 23)

    def test_environment_keeps_cabal_package_db_context(self):
        environment = {"PATH": "/nix/store/bin", "GHC_PACKAGE_PATH": "/nix/store/db",
                       "GHC_ENVIRONMENT": "-"}
        result = BUILD.build_environment(environment, Path("/tmp/actions"), Path("/nix/store/ghc"))
        self.assertEqual(result["GHC_PACKAGE_PATH"], "/nix/store/db")
        self.assertEqual(result["GHC_ENVIRONMENT"], "-")
        self.assertEqual(result[BUILD.COMPILER_ENV], "/nix/store/ghc")
        self.assertNotIn(BUILD.COMPILER_ENV, environment)


if __name__ == "__main__":
    unittest.main()
