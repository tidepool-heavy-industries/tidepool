#!/usr/bin/env python3
"""Check that corpus probes survive preparation as the mechanism their cohort
claims, not as an optimizer-folded literal or pre-built CAF.

Parses a GHC `-ddump-simpl -dsuppress-all` Tidy Core dump into one body of
text per top-level binding, then for each manifest probe walks the closure of
top-level bindings reachable by identifier reference from the probe's own
binding and checks that every required marker substring appears somewhere in
that reachable text. A probe folded to a literal has an empty reachable
closure beyond its own binding, so it cannot show a marker that names real
library or primop machinery.

See bridge/haskell/test-prepared-stg/probe-opacity-manifest.json for the marker
naming rule this depends on.
"""
import json
import re
import sys

IDENT_RE = re.compile(r"[A-Za-z_$][A-Za-z0-9_$#']*")


def parse_tidy_core(text):
    lines = text.splitlines()
    stop = len(lines)
    for i, line in enumerate(lines):
        if line.startswith("------ Local rules"):
            stop = i
            break
    bindings = {}
    name = None
    body = []

    def flush():
        if name is not None:
            bindings[name] = "\n".join(body)

    started = False
    for line in lines[:stop]:
        if not started:
            if line.startswith("  = {terms:"):
                started = True
            continue
        if line in ("Rec {", "end Rec }"):
            continue
        if line.strip() == "":
            continue
        if line[0] not in (" ", "\t"):
            flush()
            if " = " in line:
                nm, rest = line.split(" = ", 1)
                name = nm.strip()
                body = [rest]
            else:
                name = line.strip()
                body = []
        else:
            body.append(line)
    flush()
    return bindings


def reachable_text(bindings, start):
    seen = set()
    stack = [start]
    parts = []
    while stack:
        n = stack.pop()
        if n in seen or n not in bindings:
            continue
        seen.add(n)
        body = bindings[n]
        parts.append(body)
        for tok in IDENT_RE.findall(body):
            if tok in bindings and tok not in seen:
                stack.append(tok)
    return "\n".join(parts)


def check_module(dump_path, probes):
    with open(dump_path) as f:
        bindings = parse_tidy_core(f.read())
    failures = []
    for probe, markers in probes.items():
        if probe not in bindings:
            failures.append((probe, None, "probe not found in Tidy Core dump"))
            continue
        closure = reachable_text(bindings, probe)
        for marker in markers:
            if marker not in closure:
                failures.append((probe, marker, "marker not found in reachable Core"))
    return failures


def main(argv):
    if len(argv) < 2:
        print("usage: probe-opacity-check.py <manifest.json> MODULE=dump.txt ...", file=sys.stderr)
        return 2
    manifest_path = argv[0]
    dump_paths = {}
    for arg in argv[1:]:
        module, _, path = arg.partition("=")
        if not path:
            print(f"error: expected MODULE=path, got {arg!r}", file=sys.stderr)
            return 2
        dump_paths[module] = path

    manifest = json.load(open(manifest_path))
    modules = manifest["modules"]

    missing_dumps = sorted(set(modules) - set(dump_paths))
    if missing_dumps:
        print(f"error: no dump supplied for manifest module(s): {missing_dumps}", file=sys.stderr)
        return 2

    total_probes = 0
    all_failures = []
    for module, probes in modules.items():
        total_probes += len(probes)
        for probe, marker, reason in check_module(dump_paths[module], probes):
            all_failures.append((module, probe, marker, reason))

    if all_failures:
        print("probe-opacity-check: FAILED", file=sys.stderr)
        for module, probe, marker, reason in all_failures:
            if marker is None:
                print(f"  {module}.{probe}: {reason}", file=sys.stderr)
            else:
                print(
                    f"  {module}.{probe}: missing marker {marker!r} ({reason}) — "
                    "this probe's reachable Tidy Core no longer shows the mechanism "
                    "its cohort claims; it may have been optimized to a literal or "
                    "the manifest marker is stale for the current source",
                    file=sys.stderr,
                )
        return 1

    print(f"probe-opacity-check: {total_probes} probes across {len(modules)} modules retain their claimed mechanism")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
