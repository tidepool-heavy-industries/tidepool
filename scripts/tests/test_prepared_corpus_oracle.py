#!/usr/bin/env python3
"""Check that Suite oracle fingerprints use source files and safe file names."""

import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "prepared-corpus-oracle.sh"
STATIC_INPUTS = (
    "flake.nix",
    "flake.lock",
    "bridge/haskell/cabal.project",
    "bridge/haskell/test/Suite.hs",
    "scripts/prepared-corpus-oracle.sh",
)
FLAGS = (
    "-package ghc -O2 -fno-full-laziness -fno-cpr-anal "
    "-fexpose-all-unfoldings -fexpose-overloaded-unfoldings"
)


class SuiteOracleFingerprint(unittest.TestCase):
    def test_generated_cbor_is_ignored_and_quoted_source_name_is_hashed(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            sources = root / "bridge/haskell/lib"
            oracle = root / "bridge/haskell/test-prepared-stg/suite-oracle"
            sources.mkdir(parents=True)
            oracle.mkdir(parents=True)
            for name in (*STATIC_INPUTS, "bridge/haskell/lib/Fixture.hs",
                         "bridge/haskell/test-prepared-stg/suite-oracle/SuiteOracle.hs"):
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(name)
            shutil.copyfile(SCRIPT, root / "scripts/prepared-corpus-oracle.sh")
            manifest = root / "manifest.json"
            manifest.write_text(json.dumps({"programs": [{"expectation_key": "fixture"}]}))
            fixture = root / "tidepool/prepared-corpus/fixtures/prepared-corpus-expectations.json"
            fixture.parent.mkdir(parents=True)

            def fingerprint():
                paths = [root / name for name in STATIC_INPUTS]
                paths.extend(path for directory in (sources, oracle)
                             for path in directory.rglob("*") if path.is_file()
                             and path.suffix != ".cbor"
                             and not any(part.endswith("_cbor") for part in path.parts))
                lines = b"".join(
                    hashlib.sha256(path.read_bytes()).hexdigest().encode()
                    + b"  " + str(path.relative_to(root)).encode() + b"\n"
                    for path in sorted(paths, key=lambda path: str(path.relative_to(root)))
                )
                jq = subprocess.check_output(["jq", "--version"]).strip()
                names = hashlib.sha256(b"fixture\n").hexdigest().encode()
                lines += b"flags " + FLAGS.encode() + b"\n"
                lines += b"ghc 9.12.2\ntimeout 10s\njq " + jq + b"\nnames " + names + b"\n"
                return hashlib.sha256(lines).hexdigest()

            def check():
                fixture.write_text(json.dumps({
                    "oracle_fingerprint": fingerprint(),
                    "oracle_payload_digest": hashlib.sha256(b"{}\n").hexdigest(),
                }))
                return subprocess.run(
                    ["bash", "scripts/prepared-corpus-oracle.sh", "check", "manifest.json"],
                    cwd=root, capture_output=True, text=True, timeout=10,
                )

            baseline = check()
            self.assertEqual(baseline.returncode, 0, baseline.stderr)
            original_fingerprint = fingerprint()
            generated = sources / "Prelude_cbor"
            generated.mkdir()
            (generated / "junk.cbor").write_text("generated")
            self.assertEqual(fingerprint(), original_fingerprint)
            self.assertEqual(check().returncode, 0)
            (sources / "Odd'Name.hs").write_text("module OddName where")
            self.assertNotEqual(fingerprint(), original_fingerprint)
            quoted = check()
            self.assertEqual(quoted.returncode, 0, quoted.stderr)


if __name__ == "__main__":
    unittest.main()
