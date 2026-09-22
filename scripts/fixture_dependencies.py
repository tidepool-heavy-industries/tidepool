#!/usr/bin/env python3
"""Build and consume the corpus index from compiler dependency evidence."""
import hashlib
import json
import re
from pathlib import Path
import sys

VERSION = 1


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def build(run_root, repository, output, expected):
    cohorts = {}
    for manifest in sorted(run_root.glob("*/manifest.json")):
        try:
            evidence = json.loads(manifest.with_name("dependencies.json").read_text())
        except (OSError, ValueError):
            evidence = {"version": VERSION, "selection_complete": False}
        # Canonical source identities preserve authored symlink targets when the
        # temporary corpus import tree is removed. Negative witnesses keep their
        # spelling: canonicalizing an absent candidate can hide later shadows.
        for source in evidence.get("sources", []):
            source["path"] = str(Path(source["path"]).resolve())
        cohorts[manifest.parent.name] = evidence
    expected_names = sorted(expected)
    document = {
        "version": VERSION,
        "repository": str(repository.resolve()),
        "cohorts": cohorts,
        "cohort_count": len(cohorts),
        "complete": sorted(cohorts) == expected_names,
        "expected_cohorts": expected_names,
    }
    output.parent.mkdir(parents=True, exist_ok=True)
    temporary = output.with_suffix(".tmp")
    temporary.write_text(json.dumps(document, sort_keys=True) + "\n")
    temporary.replace(output)


def affected(index, changed, repository):
    """Return cohort names, or None when complete selection cannot be proven."""
    try:
        document = json.loads(index.read_text())
        if (
            document["version"] != VERSION
            or document["repository"] != str(repository.resolve())
            or document["complete"] is not True
        ):
            return None
        cohorts = document["cohorts"]
        if (
            not isinstance(cohorts, dict)
            or not cohorts
            or len(cohorts) != document["cohort_count"]
            or sorted(cohorts) != sorted(document["expected_cohorts"])
        ):
            return None
        changed_paths = {str((repository / path).absolute()) for path in changed}
        changed_paths |= {str((repository / path).resolve()) for path in changed}
        selected = []
        for name, evidence in cohorts.items():
            if evidence["version"] != VERSION or evidence["selection_complete"] is not True:
                return None
            sources = evidence["sources"]
            resolutions = evidence["resolutions"]
            if not sources or not isinstance(resolutions, list):
                return None
            dependencies = set()
            for source in sources:
                path = Path(source["path"])
                if not path.is_absolute() or re.fullmatch(r"[0-9a-f]{64}", source["sha256"]) is None:
                    return None
                dependencies.add(str(path))
                # A stale index cannot silently omit dependencies from a newer
                # source graph. Known edits select their previous consumers;
                # unknown edits require rebuilding the complete index.
                if str(path) not in changed_paths and digest(path) != source["sha256"]:
                    return None
            for resolution in resolutions:
                chosen = resolution["selected"]
                for candidate in resolution["candidates"]:
                    path = Path(candidate)
                    if not path.is_absolute():
                        return None
                    dependencies.add(str(path))
                    if candidate != chosen and candidate not in changed_paths:
                        try:
                            path.stat()
                        except FileNotFoundError:
                            pass
                        else:
                            return None
            if dependencies & changed_paths:
                selected.append(name)
        return sorted(selected)
    except (OSError, ValueError, KeyError, TypeError):
        return None


if __name__ == "__main__":
    if len(sys.argv) < 5:
        sys.exit("usage: fixture_dependencies.py RUN_ROOT REPOSITORY OUTPUT COHORT...")
    build(*(Path(argument) for argument in sys.argv[1:4]), sys.argv[4:])
