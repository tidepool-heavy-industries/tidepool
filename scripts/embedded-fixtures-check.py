#!/usr/bin/env python3
"""Validate the registered prepared artifacts embedded in Rust tests.

The corpus checker covers generated corpus outputs, while these artifacts are
checked in because Rust tests include them directly. Keep this inventory tied
to their real producers and admit every artifact through the production
prepared-artifact decoder.
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "bridge/haskell/test-prepared-stg/embedded-fixtures.json"
FIXTURE_DIRS = (
    ROOT / "bridge/haskell/test-prepared-stg/fixtures",
    ROOT / "bridge/haskell/test-execution-schema-encode/fixtures",
)


def main() -> int:
    manifest = json.loads(MANIFEST.read_text())
    artifacts = manifest.get("artifacts")
    if not isinstance(artifacts, list) or not artifacts:
        raise SystemExit("embedded fixture inventory has no artifacts")

    paths: list[str] = []
    for artifact in artifacts:
        if not isinstance(artifact, dict):
            raise SystemExit("embedded fixture inventory contains a non-object entry")
        path = artifact.get("path")
        producer = artifact.get("producer")
        consumers = artifact.get("consumers")
        if not isinstance(path, str) or not isinstance(producer, str) or not producer:
            raise SystemExit(f"embedded fixture entry has no producer: {artifact!r}")
        if not isinstance(consumers, list) or not consumers or not all(
            isinstance(consumer, str) and consumer for consumer in consumers
        ):
            raise SystemExit(f"embedded fixture entry has no consumers: {path}")
        if path in paths:
            raise SystemExit(f"embedded fixture is registered twice: {path}")
        paths.append(path)

    schema = manifest.get("schema")
    if not isinstance(schema, dict) or not isinstance(schema.get("version_source"), str):
        raise SystemExit("embedded fixture inventory has no schema owner")
    if not (ROOT / schema["version_source"]).is_file():
        raise SystemExit(f"embedded fixture schema owner is missing: {schema['version_source']}")
    if schema.get("magic") != "TPSTG":
        raise SystemExit("embedded fixture checker only supports the TPSTG envelope")

    registered = set(paths)
    discovered = {
        path.relative_to(ROOT).as_posix()
        for directory in FIXTURE_DIRS
        for path in directory.glob("*.cbor")
    }
    if discovered != registered:
        missing = sorted(registered - discovered)
        unregistered = sorted(discovered - registered)
        details = []
        if missing:
            details.append(f"missing registered files: {', '.join(missing)}")
        if unregistered:
            details.append(f"unregistered CBOR files: {', '.join(unregistered)}")
        raise SystemExit("; ".join(details))

    for path_name in paths:
        path = ROOT / path_name
        if not path.is_file():
            raise SystemExit(f"registered embedded fixture is missing: {path_name}")

    # The repr decoder owns the complete wire contract, including the
    # execution ABI. Keep Python responsible for inventory and producer
    # metadata; do not mirror CBOR field offsets here.
    result = subprocess.run(
        [
            "cargo",
            "run",
            "--quiet",
            "-p",
            "tidepool-toolchain",
            "--bin",
            "embedded-artifact-check",
            "--",
            *paths,
        ],
        cwd=ROOT,
        check=False,
    )
    if result.returncode != 0:
        raise SystemExit(result.returncode)

    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (KeyError, TypeError, ValueError, json.JSONDecodeError) as error:
        print(f"embedded fixture inventory is malformed: {error}", file=sys.stderr)
        raise SystemExit(1) from error
