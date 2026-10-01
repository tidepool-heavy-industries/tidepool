"""Portable GHC compile-path checks for the shared worker evidence verifier."""

import hashlib
import io
from pathlib import Path
import sys
import tarfile
import tempfile
import unittest

TEST_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(TEST_DIR))
from ghc_source_options import audit_source_optimization_options, verify_worker_home_actions


class WorkerHomeActionTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name) / "checkout"
        self.build = self.root / "target" / "cabal-build"
        self.compiler = "/nix/store/pinned-ghc/bin/ghc"
        self.sources = {
            "bridge/haskell/src/Lib/One.hs": "1" * 64,
            "bridge/haskell/src/Lib/Two.hs": "2" * 64,
            "bridge/haskell/app/Main.hs": "3" * 64,
        }
        self.lib_out = self.build / "lib" / "opt" / "build"
        self.exe_out = self.build / "exe" / "opt" / "build"
        self.rows = [
            self.action(self.lib_out, ["Lib.One", "Lib.Two"]),
            self.action(self.exe_out, ["app/Main.hs"], no_link=True),
            self.action(self.exe_out, ["app/Main.hs"]),
        ]
        self.log = "\n".join((
            "[1 of 2] Compiling Lib.One ( src/Lib/One.hs, " + str(self.lib_out / "Lib/One.o") + ", " + str(self.lib_out / "Lib/One.dyn_o") + " )",
            "[2 of 2] Compiling Lib.Two ( src/Lib/Two.hs, " + str(self.lib_out / "Lib/Two.o") + ", " + str(self.lib_out / "Lib/Two.dyn_o") + " )",
            "[1 of 1] Compiling Main ( app/Main.hs, " + str(self.exe_out / "Main.o") + " )",
        ))
        self.source_options = {"schema": "ghc-source-options-v1", "sources": self.sources}

    def action(self, outputdir, inputs, no_link=False):
        header = self.build / "exe" / "autogen" / "cabal_macros.h"
        argv = [self.compiler, "--make"]
        if no_link:
            argv.append("-no-link")
        argv += ["-O2", "-outputdir", str(outputdir), "-odir", str(outputdir),
                 "-optP-include", "-optP" + str(header), *inputs]
        # compile_action is deliberately false: argv, not labels, is authoritative.
        return {"cwd": str(self.root / "bridge/haskell"), "argv": argv,
                "exit_code": 0, "compile_action": False}

    def verify(self, rows=None, log=None):
        return verify_worker_home_actions(self.rows if rows is None else rows,
                                          self.log if log is None else log,
                                          self.compiler, self.root, self.build,
                                          self.source_options)

    def test_actual_ghc_style_library_and_main_log_coverage(self):
        result = self.verify()
        self.assertEqual(result["make_action_count"], 3)
        self.assertEqual(result["compiled_source_count"], 3)
        self.assertEqual(set(result["compiled_sources"]), set(self.sources))

    def test_retained_source_and_macro_paths_need_not_exist_live(self):
        self.assertFalse((self.root / "bridge/haskell/src/Lib/One.hs").exists())
        self.assertFalse((self.build / "exe/autogen/cabal_macros.h").exists())
        self.assertEqual(self.verify()["compiled_source_count"], 3)

    def test_repeated_o2_is_valid_but_any_other_optimization_level_is_refused(self):
        rows = [dict(row, argv=list(row["argv"])) for row in self.rows]
        rows[0]["argv"].append("-O2")
        self.assertEqual(self.verify(rows=rows)["make_action_count"], 3)
        rows[0]["argv"].insert(rows[0]["argv"].index("-O2"), "-O0")
        with self.assertRaisesRegex(ValueError, "effective -O2"):
            self.verify(rows=rows)

    def test_library_o0_is_refused_even_when_main_actions_remain_o2(self):
        rows = [dict(row, argv=list(row["argv"])) for row in self.rows]
        rows[0]["argv"][rows[0]["argv"].index("-O2")] = "-O0"
        with self.assertRaisesRegex(ValueError, "effective -O2"):
            self.verify(rows=rows)

    def test_missing_home_compile_record_is_refused(self):
        log = "\n".join(line for line in self.log.splitlines() if "Compiling Lib.One " not in line)
        with self.assertRaisesRegex(ValueError, "were not compiled"):
            self.verify(log=log)

    def test_outside_source_compile_record_is_refused(self):
        log = self.log.replace("src/Lib/One.hs", "/tmp/Outside.hs", 1)
        with self.assertRaisesRegex(ValueError, "outside captured HOME tree"):
            self.verify(log=log)

    def test_compilation_under_conflicting_nested_outputdirs_is_ambiguous(self):
        rows = [dict(row, argv=list(row["argv"])) for row in self.rows]
        conflicting = dict(rows[0], argv=list(rows[0]["argv"]))
        output = Path(conflicting["argv"][conflicting["argv"].index("-outputdir") + 1]).parent
        conflicting["argv"][conflicting["argv"].index("-outputdir") + 1] = str(output)
        conflicting["argv"][conflicting["argv"].index("-odir") + 1] = str(output)
        rows.append(conflicting)
        with self.assertRaisesRegex(ValueError, "ambiguous --make attribution"):
            self.verify(rows=rows)

    def test_same_outputdir_main_link_action_must_share_cwd_and_compile_profile(self):
        rows = [dict(row, argv=list(row["argv"])) for row in self.rows]
        rows[2]["cwd"] = str(self.root / "other-cwd")
        with self.assertRaisesRegex(ValueError, "conflicting GHC actions share an output directory"):
            self.verify(rows=rows)
        rows[2]["cwd"] = rows[1]["cwd"]
        rows[2]["argv"].append("-XHaskell2010")
        with self.assertRaisesRegex(ValueError, "conflicting GHC actions share an output directory"):
            self.verify(rows=rows)

    def test_relative_source_is_resolved_only_under_its_object_owning_action(self):
        rows = [dict(row, argv=list(row["argv"])) for row in self.rows]
        wrong_output = self.build / "exe-with-wrong-cwd" / "opt" / "build"
        rows[1]["cwd"] = str(self.root / "wrong-main-cwd")
        rows[2]["cwd"] = str(self.root / "wrong-main-cwd")
        for row in rows[1:]:
            row["argv"][row["argv"].index("-outputdir") + 1] = str(wrong_output)
            row["argv"][row["argv"].index("-odir") + 1] = str(wrong_output)
        log = self.log.replace(str(self.exe_out), str(wrong_output))
        with self.assertRaisesRegex(ValueError, "outside captured HOME tree"):
            self.verify(rows=rows, log=log)

    def test_active_cpp_is_refused_even_with_cabal_macro_header_pair(self):
        rows = [dict(row, argv=list(row["argv"])) for row in self.rows]
        rows[0]["argv"].insert(rows[0]["argv"].index("-O2"), "-cpp")
        with self.assertRaisesRegex(ValueError, "preprocessing or phase override"):
            self.verify(rows=rows)

    def test_only_adjacent_owned_cabal_macro_pair_is_accepted(self):
        self.assertEqual(self.verify()["make_action_count"], 3)
        rows = [dict(row, argv=list(row["argv"])) for row in self.rows]
        index = rows[0]["argv"].index("-optP-include")
        rows[0]["argv"][index + 1] = "-optP/tmp/cabal_macros.h"
        with self.assertRaisesRegex(ValueError, "outside the fresh build directory"):
            self.verify(rows=rows)
        rows[0]["argv"][index:index + 2] = ["-optP-include", "-O2",
                                              "-optP" + str(self.build / "exe/autogen/cabal_macros.h")]
        with self.assertRaisesRegex(ValueError, "unexpected GHC preprocessor arguments"):
            self.verify(rows=rows)

    def test_custom_unlit_is_refused(self):
        rows = [dict(row, argv=list(row["argv"])) for row in self.rows]
        rows[0]["argv"].extend(["-pgmL", "/tmp/custom-unlit"])
        with self.assertRaisesRegex(ValueError, "preprocessing or phase override"):
            self.verify(rows=rows)

    def test_response_files_non_object_actions_and_joined_phase_aliases_are_refused(self):
        for flag in ("@flags.rsp", "-fno-code", "-E", "-M", "-S", "-xhs"):
            with self.subTest(flag=flag):
                rows = [dict(row, argv=list(row["argv"])) for row in self.rows]
                rows[0]["argv"].append(flag)
                with self.assertRaises(ValueError):
                    self.verify(rows=rows)

    def test_unlogged_standalone_haskell_compile_cannot_overwrite_accepted_object(self):
        for extra in (["-c", "-O0", "bridge/haskell/src/Lib/One.hs", "-o",
                       str(self.lib_out / "Lib/One.o")],
                      ["-c", "-O0", "-xhs", "/tmp/One"], ["@hidden.rsp"]):
            with self.subTest(extra=extra):
                rows = list(self.rows)
                rows.append({"cwd": str(self.root), "exit_code": 0, "compile_action": False,
                             "argv": [self.compiler, *extra]})
                with self.assertRaisesRegex(ValueError, "standalone Haskell compile"):
                    self.verify(rows=rows)

    def test_recorded_evidence_roots_must_already_be_absolute(self):
        with self.assertRaisesRegex(ValueError, "must be absolute"):
            verify_worker_home_actions(self.rows, self.log, self.compiler, Path("checkout"),
                                       self.build, self.source_options)

    def test_source_options_aliases_and_unicode_header_whitespace_are_refused(self):
        for source in (b"{-# OPTIONS -O0 #-}\n", b"{-# OPTIONS_GHC -O0 #-}\n",
                       "{-#\u00a0OPTIONS_GHC -O0 #-}\n".encode(),
                       "{-#\u00a0LANGUAGE\u00a0CPP #-}\n".encode()):
            with self.subTest(source=source), tempfile.TemporaryDirectory() as directory:
                archive_path = Path(directory) / "sources.tar"
                member_name = "bridge/haskell/src/One.hs"
                with tarfile.open(archive_path, "w") as archive:
                    member = tarfile.TarInfo(member_name)
                    member.size = len(source)
                    archive.addfile(member, io.BytesIO(source))
                entry = {"path": member_name, "kind": "file", "bytes": len(source),
                         "sha256": hashlib.sha256(source).hexdigest()}
                before = {"version": 1, "capture_changes": [],
                          "archive_sha256": hashlib.sha256(archive_path.read_bytes()).hexdigest(),
                          "source": {"entries": [entry]}}
                with self.assertRaises(ValueError):
                    audit_source_optimization_options(archive_path, before)


if __name__ == "__main__":
    unittest.main()
