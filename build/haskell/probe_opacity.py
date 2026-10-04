"""Compile declared contract sources and run their existing Tidy Core checker."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("ghc", "checker", "manifest", "output"):
        parser.add_argument("--" + name, required=True)
    parser.add_argument("--source", action="append", nargs=2, default=[])
    args = parser.parse_args()
    manifest = json.loads(Path(args.manifest).read_text())
    sources = dict(args.source)
    if len(sources) != len(args.source) or set(sources) != set(manifest["modules"]):
        parser.error("declared source modules must exactly match the probe manifest")
    count = sum(len(probes) for probes in manifest["modules"].values())
    if not count:
        parser.error("optimizer-probe action cannot validate an empty inventory")
    output = Path(args.output).resolve()
    output.mkdir()
    environment = {"PATH": os.environ["PATH"], "LANG": "C.UTF-8", "LC_ALL": "C.UTF-8", "GHC_ENVIRONMENT": "-"}
    dumps = []
    identities = {}
    for module, source in sorted(sources.items()):
        if not module.isidentifier() or not module[0].isupper():
            parser.error("probe source names must be Haskell module identifiers")
        source = Path(source).resolve(strict=True)
        build = output / (module + ".build")
        build.mkdir()
        dump = output / (module + ".dump.txt")
        with dump.open("wb") as stdout:
            subprocess.run([args.ghc, "-clear-package-db", "-global-package-db", "-package-env", "-",
                            "-package", "containers", "-package", "text", "-XGHC2024", "-O2",
                            "-ddump-simpl", "-dsuppress-all", "-fforce-recomp", "-outputdir", str(build),
                            "-c", str(source)], env=environment, stdout=stdout, check=True)
        dumps.append(module + "=" + str(dump))
        identities[module] = hashlib.sha256(source.read_bytes()).hexdigest()
    subprocess.run([sys.executable, str(Path(args.checker).resolve(strict=True)), args.manifest, *dumps],
                   env=environment, check=True)
    (output / "proof.json").write_text(json.dumps({"schema": 1, "probes": count, "modules": identities,
        "manifest_sha256": hashlib.sha256(Path(args.manifest).read_bytes()).hexdigest()}, indent=2) + "\n")


if __name__ == "__main__":
    main()
