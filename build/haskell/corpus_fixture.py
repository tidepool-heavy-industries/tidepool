"""Run one compiler-owned corpus transaction with declared inputs and scratch."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile


def generate_corpus(args, scratch, admission):
    source, targets = Path(args.source).resolve(strict=True), Path(args.targets).resolve(strict=True)
    includes = [str(Path(root).resolve(strict=True)) for root in args.include]
    output = Path(args.output).absolute()
    producer, validator = Path(args.producer).resolve(strict=True), Path(args.validator).resolve(strict=True)
    libraries = Path(args.runtime_libraries).resolve(strict=True)
    if output.exists():
        raise ValueError("output directory must be absent")
    if not args.ghc_libdir.startswith("/nix/store/"):
        raise ValueError("compiler package must be pinned in the Nix store")
    environment = {
        "PATH": os.environ["PATH"], "LANG": "C.UTF-8", "LC_ALL": "C.UTF-8",
        "GHC_ENVIRONMENT": "-", "TIDEPOOL_GHC_LIBDIR": args.ghc_libdir,
        "LD_LIBRARY_PATH": str(libraries), "TMPDIR": str(scratch),
        "XDG_CACHE_HOME": str(scratch / "cache"),
    }
    command = [str(producer)]
    if args.metadata_target:
        command += ["--metadata-targets", " ".join(args.metadata_target)]
    if args.all_tops:
        command += ["--all-tops"]
    command += [str(source), args.module, str(targets), str(output), *includes]
    subprocess.run(command, cwd=scratch, env=environment, check=True)
    subprocess.run([str(validator), admission, str(output), str(source), str(targets), *includes], cwd=scratch, env=environment, check=True)
    return validator, environment


def source_roots(roots):
    result = []
    for tree, subtree in roots:
        if Path(subtree).is_absolute() or any(part in (".", "..") for part in subtree.split("/")):
            raise ValueError("source subtree must be a relative directory")
        result.append(str((Path(tree) / subtree).resolve(strict=True)))
    return result


def fresh_test(inputs_path):
    inputs = json.loads(Path(inputs_path).read_text())
    if inputs["version"] != 1:
        raise ValueError("unsupported fresh corpus invocation version")
    # Resolve every declared artifact before entering the run-owned directory.
    args = argparse.Namespace(
        source=str(Path(inputs["source"]).resolve(strict=True)),
        targets=str(Path(inputs["targets"]).resolve(strict=True)),
        include=source_roots(inputs["source_roots"]),
        producer=str(Path(inputs["producer"]).resolve(strict=True)),
        validator=str(Path(inputs["validator"]).resolve(strict=True)),
        runtime_libraries=str(Path(inputs["runtime_libraries"]).resolve(strict=True)),
        ghc_libdir=inputs["ghc_libdir"], module=inputs["module"],
        metadata_target=inputs["metadata_targets"], all_tops=inputs["all_tops"],
    )
    ghc_command = inputs["ghc_command"]
    if not isinstance(ghc_command, list) or len(ghc_command) != 1:
        raise ValueError("native oracle requires the configured single GHC executable")
    oracle = [sys.executable, str(Path(inputs["oracle_driver"]).resolve(strict=True)),
              "--ghc", str(Path(ghc_command[0]).resolve(strict=True)),
              "--oracle-source", str(Path(inputs["oracle_source"]).resolve(strict=True)),
              "--classifications", str(Path(inputs["classifications"]).resolve(strict=True)),
              "--nonterminating", str(Path(inputs["nonterminating"]).resolve(strict=True)),
              "--module", args.module, "--timeout", str(inputs["timeout"])]
    for root in source_roots(inputs["oracle_source_roots"]):
        oracle += ["--include", root]
    work = Path(tempfile.mkdtemp(prefix="fresh-corpus-test-"))
    args.output = str(work / "corpus")
    scratch = work / "compiler"
    scratch.mkdir()
    try:
        validator, environment = generate_corpus(args, scratch, "validate-original")
        expectations = work / "expectations.json"
        oracle += ["--manifest", str(Path(args.output) / "manifest.json"), "--output", str(expectations),
                   "--scratch-directory", str(work / "oracle")]
        subprocess.run(oracle, cwd=work, env=environment, check=True)
        subprocess.run([str(validator), "verify-cohort", args.output, str(expectations), str(work / "results.json")], cwd=work, env=environment, check=True)
    except BaseException:
        print(f"corpus failure evidence: {work}", file=sys.stderr)
        raise
    else:
        shutil.rmtree(work)


def main():
    if len(sys.argv) == 3 and sys.argv[1] == "--fresh-test-inputs":
        fresh_test(sys.argv[2])
        return
    parser = argparse.ArgumentParser()
    for name in ("producer", "validator", "source", "module", "targets", "output", "ghc-libdir", "runtime-libraries"):
        parser.add_argument("--" + name, required=True)
    parser.add_argument("--include", action="append", default=[])
    parser.add_argument("--metadata-target", action="append", default=[])
    parser.add_argument("--all-tops", action="store_true")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="prepared-corpus-action-", dir=Path.cwd()) as scratch:
        generate_corpus(args, Path(scratch), "validate-fixture")


if __name__ == "__main__":
    main()
