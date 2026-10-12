"""Exercise browser input selection and refusal through its production projector."""
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
PROJECTOR = ROOT / "web/prepare-harness-browser-inputs.py"


class HarnessBrowserProjection(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.source = Path(self.temp.name) / "source"
        self.output = Path(self.temp.name) / "output"
        self.write("scripts/browser-source-identity.py", "import pathlib, sys\npathlib.Path(sys.argv[2]).write_text('{}\\n')\n")
        self.write("web/BUCK", '''WEB_TEST_EXCLUDES = ["src/**/*.test.ts"]
WEB_PRODUCTION_SRCS = glob(["src/**/*"], exclude = WEB_TEST_EXCLUDES + ["src/generated/**"])
BROWSER_IDENTITY_SRCS = WEB_PRODUCTION_SRCS + ["BUCK"]
''')
        self.write("web/src/app.ts", "production")
        self.write("web/src/app.test.ts", "excluded test")
        self.write("web/src/generated/contract.ts", "excluded generated product")
        self.write("build/rust/profiles.bzl", "authoritative profile")
        for package, prefix in (("harness-schema", "SCHEMA"), ("harness-presentation", "PRESENTATION")):
            self.write(f"crates/{package}/BUCK", f'''{prefix}_SRCS = glob(["src/**/*.rs"], exclude = ["src/tests.rs"])
{prefix}_SOURCE_INPUTS = {{source: source for source in {prefix}_SRCS}}
{prefix}_SOURCE_INPUTS.update({{"Cargo.toml": "Cargo.toml", "profile-policy.bzl": "//build/rust:profile_policy_source"}})
''')
            self.write(f"crates/{package}/Cargo.toml", package)
            self.write(f"crates/{package}/src/lib.rs", package + " production")
            self.write(f"crates/{package}/src/tests.rs", "excluded test")
        self.write("crates/harness/BUCK", '''BROWSER_RUNTIME_SRCS = ["src/lib.rs"]
BROWSER_SOURCE_INPUTS = {source: source for source in BROWSER_RUNTIME_SRCS}
BROWSER_SOURCE_INPUTS.update({
    "web.json": "//web:browser_source_identity",
    "presentation.json": "//crates/harness-presentation:browser_source_identity",
    "schema.json": "//crates/harness-schema:browser_source_identity",
    "profile-policy.bzl": "//build/rust:profile_policy_source",
})
''')
        self.write("crates/harness/src/lib.rs", "runtime")

    def write(self, relative, content):
        path = self.source / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content)

    def project(self):
        return subprocess.run([sys.executable, str(PROJECTOR), str(self.source), str(self.output)], capture_output=True, text=True)

    def test_actual_leaf_profile_and_named_exclusions_are_projected(self):
        result = self.project()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual({p.relative_to(self.output / "web-inputs").as_posix() for p in (self.output / "web-inputs").rglob("*") if p.is_file()}, {"BUCK", "src/app.ts"})
        for prefix, package in (("schema", "harness-schema"), ("presentation", "harness-presentation")):
            inputs = self.output / (prefix + "_source_inputs-inputs")
            self.assertEqual((inputs / "src/lib.rs").read_text(), package + " production")
            self.assertFalse((inputs / "src/tests.rs").exists())
            self.assertEqual((inputs / "profile-policy.bzl").read_text(), "authoritative profile")
        runtime = self.output / "browser_source_inputs-inputs"
        for identity in ("web.json", "presentation.json", "schema.json"):
            self.assertEqual((runtime / identity).read_text(), "{}\n")

    def test_unknown_leaf_label_refuses(self):
        path = self.source / "crates/harness/BUCK"
        path.write_text(path.read_text().replace("//crates/harness-schema:browser_source_identity", "//crates/unknown:browser_source_identity"))
        self.assertNotEqual(self.project().returncode, 0)
        self.assertFalse((self.output / "browser-source-identity.json").exists())

    def test_unsupported_list_operation_refuses(self):
        self.write("web/BUCK", 'BROWSER_IDENTITY_SRCS = sorted(["BUCK"])\n')
        self.assertNotEqual(self.project().returncode, 0)

    def test_cyclic_named_exclusions_refuse(self):
        path = self.source / "web/BUCK"
        path.write_text(path.read_text().replace('WEB_TEST_EXCLUDES = ["src/**/*.test.ts"]', 'WEB_TEST_EXCLUDES = WEB_TEST_EXCLUDES'))
        result = self.project()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("cyclic Harness browser list declaration", result.stderr)
