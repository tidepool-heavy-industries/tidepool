#!/usr/bin/env python3
"""Validate the registered prepared artifacts embedded in Rust tests.

The corpus checker covers generated corpus outputs, while these artifacts are
checked in because Rust tests include them directly. Keep this inventory tied
to their real producers so a schema change cannot leave an old embedded byte
blob behind unnoticed.
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "haskell/test-prepared-stg/embedded-fixtures.json"
FIXTURE_DIRS = (
    ROOT / "haskell/test-prepared-stg/fixtures",
    ROOT / "haskell/test-execution-schema-encode/fixtures",
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

    source = ROOT / manifest["schema"]["version_source"]
    version_match = re.search(r"SCHEMA_VERSION\s*:\s*u64\s*=\s*(\d+)", source.read_text())
    if version_match is None:
        raise SystemExit(f"could not find SCHEMA_VERSION in {source}")
    expected_version = int(version_match.group(1))
    expected_magic = manifest["schema"]["magic"].encode("ascii")
    if expected_magic != b"TPSTG":
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
        data = path.read_bytes()
        if len(data) < 8 or data[0] != 0x90 or data[1:6] != b"eTPST" or data[6:7] != b"G":
            raise SystemExit(f"{path_name}: invalid TPSTG prepared envelope")
        if data[7] != expected_version:
            raise SystemExit(
                f"{path_name}: schema {data[7]} does not match current schema {expected_version}"
            )

    print(f"embedded fixtures: {len(paths)} registered artifacts, schema {expected_version}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (KeyError, TypeError, ValueError, json.JSONDecodeError) as error:
        print(f"embedded fixture inventory is malformed: {error}", file=sys.stderr)
        raise SystemExit(1) from error
