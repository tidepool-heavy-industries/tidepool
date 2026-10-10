#!/usr/bin/env python3
"""Check one negative type contract after its paired positive control compiles."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tempfile


def expected_error(path):
    text = path.read_text()
    error = re.search(r"^error\[(E\d+)\]: (.+)$", text, re.MULTILINE)
    span = re.search(r"^\s*--> .*[/\\]([^/\\:]+\.rs):(\d+):(\d+)$", text, re.MULTILINE)
    if error is None or span is None:
        raise ValueError(f"missing exact coded diagnostic/primary span in {path}")
    return {"code": error[1], "message": error[2], "file": span[1],
            "line": int(span[2]), "column": int(span[3])}


def coded_errors(stderr):
    errors = []
    for line in stderr.splitlines():
        diagnostic = json.loads(line)
        if diagnostic.get("level") != "error":
            continue
        # rustc's final 'aborting due to previous error' summary has no spans.
        if diagnostic.get("code") is None and not diagnostic.get("spans"):
            continue
        errors.append(diagnostic)
    return errors


def check_refusal(result, expected):
    errors = coded_errors(result.stderr)
    if result.returncode != 1 or len(errors) != 1:
        raise ValueError(f"expected one type refusal, got status {result.returncode}, {len(errors)} errors")
    actual = errors[0]
    spans = [span for span in actual["spans"] if span["is_primary"]]
    if (actual.get("code") or {}).get("code") != expected["code"] or actual["message"] != expected["message"]:
        raise ValueError("type refusal differs from the pinned error code/message")
    if not any(Path(span["file_name"]).name == expected["file"]
               and span["line_start"] == expected["line"]
               and span["column_start"] == expected["column"] for span in spans):
        raise ValueError("type refusal has no matching primary source span")


def compiler_command(inputs):
    command = inputs.get("rustc_command")
    if (not isinstance(command, list) or not command
            or any(not isinstance(argument, str) or not argument for argument in command)):
        raise ValueError("declared rustc command must be a non-empty argument vector")
    return command


def run_action(args):
    output = args.output
    output.mkdir(parents=True, exist_ok=False)
    inputs = json.loads(args.inputs.read_text())
    rustc = compiler_command(inputs)
    with tempfile.TemporaryDirectory(prefix="metadata-", dir=output) as scratch:
        metadata = Path(scratch)
        for index, item in enumerate(inputs["transitive"]):
            crate = (Path(item["dynamic_crate"]).read_text().strip()
                     if item["dynamic_crate"] else item["crate"])
            if re.fullmatch(r"[A-Za-z_][A-Za-z_0-9]*", crate) is None:
                raise ValueError(f"invalid declared crate identity {crate!r}")
            artifact = Path(item["artifact"]).resolve(strict=True)
            (metadata / f"lib{crate}-{index}{artifact.suffix}").symlink_to(artifact)
        common = [*rustc, "--edition", args.edition, "--emit=metadata",
                  "--error-format=json", "-L", f"dependency={metadata}"]
        for alias, artifact in inputs["direct"].items():
            common.extend(["--extern", f"{alias}={artifact}"])
        control = subprocess.run([*common, "--crate-name", "contract_control",
                                  str(args.control), "-o", str(metadata / "control.rmeta")],
                                 text=True, capture_output=True, check=False)
        (output / "control.jsonl").write_text(control.stderr)
        if control.returncode != 0 or coded_errors(control.stderr):
            raise ValueError("paired valid control did not compile; refusal is unqualified")
        refusal = subprocess.run([*common, "--crate-name", "contract_refusal",
                                  str(args.source), "-o", str(metadata / "refusal.rmeta")],
                                 text=True, capture_output=True, check=False)
        (output / "refusal.jsonl").write_text(refusal.stderr)
        expectation = expected_error(args.expected)
        check_refusal(refusal, expectation)
        proof = {"control_compile_count": 1, "refusal_compile_count": 1,
                 "control_status": control.returncode, "refusal_status": refusal.returncode,
                 "source_sha256": hashlib.sha256(args.source.read_bytes()).hexdigest(),
                 "control_sha256": hashlib.sha256(args.control.read_bytes()).hexdigest(),
                 "expected": expectation}
        (output / "proof.json").write_text(json.dumps(proof, indent=2) + "\n")


def verify(path):
    proof = json.loads(path.read_text())
    if (proof["control_compile_count"], proof["refusal_compile_count"],
        proof["control_status"]) != (1, 1, 0) or proof["refusal_status"] != 1:
        raise ValueError("compile action did not retain one valid control and one refusal")
    check_refusal(subprocess.CompletedProcess([], proof["refusal_status"],
        stderr=(path.parent / "refusal.jsonl").read_text()), proof["expected"])
    print("1 paired compile contract passed (1 control, 1 refusal)")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--verify", type=Path)
    for name in ("inputs", "control", "source", "expected", "output"):
        parser.add_argument("--" + name, type=Path)
    parser.add_argument("--edition", default="2021")
    args = parser.parse_args()
    if args.verify:
        verify(args.verify)
    else:
        if any(getattr(args, name) is None for name in
            ("inputs", "control", "source", "expected", "output")):
            parser.error("compile action requires declared compiler inputs, control, source, expected and output")
        run_action(args)


if __name__ == "__main__":
    main()
