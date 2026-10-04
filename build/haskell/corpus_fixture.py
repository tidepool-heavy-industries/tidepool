"""Run one compiler-owned corpus transaction with declared inputs and scratch."""
import argparse
import os
from pathlib import Path
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser()
    for name in ("producer", "validator", "source", "module", "targets", "output", "ghc-libdir", "runtime-libraries"):
        parser.add_argument("--" + name, required=True)
    parser.add_argument("--include", action="append", default=[])
    parser.add_argument("--metadata-target", action="append", default=[])
    parser.add_argument("--all-tops", action="store_true")
    args = parser.parse_args()
    source, targets = Path(args.source).resolve(strict=True), Path(args.targets).resolve(strict=True)
    includes = [str(Path(root).resolve(strict=True)) for root in args.include]
    output = Path(args.output).absolute()
    producer, validator = Path(args.producer).resolve(strict=True), Path(args.validator).resolve(strict=True)
    libraries = Path(args.runtime_libraries).resolve(strict=True)
    if output.exists():
        parser.error("output directory must be absent")
    if not args.ghc_libdir.startswith("/nix/store/"):
        parser.error("compiler package must be pinned in the Nix store")
    with tempfile.TemporaryDirectory(prefix="prepared-corpus-action-") as scratch:
        environment = {
            "PATH": os.environ["PATH"], "LANG": "C.UTF-8", "LC_ALL": "C.UTF-8",
            "GHC_ENVIRONMENT": "-", "TIDEPOOL_GHC_LIBDIR": args.ghc_libdir,
            "LD_LIBRARY_PATH": str(libraries), "TMPDIR": scratch,
            "XDG_CACHE_HOME": str(Path(scratch) / "cache"),
        }
        command = [str(producer)]
        if args.metadata_target:
            command += ["--metadata-targets", " ".join(args.metadata_target)]
        if args.all_tops:
            command += ["--all-tops"]
        command += [str(source), args.module, str(targets), str(output), *includes]
        subprocess.run(command, cwd=scratch, env=environment, check=True)
        subprocess.run([str(validator), "validate-fixture", str(output), str(source), str(targets), *includes], cwd=scratch, env=environment, check=True)


if __name__ == "__main__":
    main()
