"""Native GHC expectations using the existing SuiteOracleTH/Render issuer."""
import argparse
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile

FLAGS = ["-package", "ghc", "-O2", "-fno-full-laziness", "-fno-cpr-anal", "-fexpose-all-unfoldings", "-fexpose-overloaded-unfoldings"]


def evaluate(executable, occurrence, environment, scratch, timeout):
    process = subprocess.Popen([str(executable), "eval", occurrence], cwd=scratch, env=environment, start_new_session=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    try:
        stdout, stderr = process.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        process.communicate()
        return None, "timeout", "native evaluation exceeded its declared timeout"
    if process.returncode == 0:
        return json.loads(stdout), None, None
    if process.returncode in (3, 4):
        return None, "unrepresentable", stderr.decode().strip()
    raise RuntimeError(f"native evaluation of {occurrence} exited {process.returncode}: {stderr.decode()}")


def main():
    parser = argparse.ArgumentParser()
    for name in ("manifest", "ghc", "oracle-source", "output", "classifications", "nonterminating"):
        parser.add_argument("--" + name, required=True)
    parser.add_argument("--include", action="append", default=[])
    parser.add_argument("--timeout", type=float, default=10)
    parser.add_argument("--module", default="Suite")
    parser.add_argument("--contract", action="store_true")
    args = parser.parse_args()
    if any(not component.isidentifier() or not component[0].isupper() for component in args.module.split(".")):
        parser.error("native oracle requires a Haskell module name")
    if args.timeout <= 0:
        parser.error("timeout must be positive")
    ghc = Path(args.ghc).resolve(strict=True)
    oracle_source = Path(args.oracle_source).resolve(strict=True)
    includes = [str(Path(path).resolve(strict=True)) for path in args.include]
    manifest = json.loads(Path(args.manifest).read_text())
    keys = sorted({row["expectation_key"] for row in manifest["programs"] if row.get("expectation_key")})
    if not keys:
        parser.error("native oracle requires a nonempty declared projection domain")
    classes = json.loads(Path(args.classifications).read_text())
    nonterminating = {}
    for line in Path(args.nonterminating).read_text().splitlines():
        if line and not line.startswith("#"):
            name, reason = line.split("\t", 1)
            if not reason or name in nonterminating:
                parser.error("nontermination declarations require unique names and reasons")
            nonterminating[name] = reason
    output = Path(args.output).absolute()
    if output.exists():
        parser.error("oracle output must be absent")
    with tempfile.TemporaryDirectory(prefix="suite-native-oracle-", dir=Path.cwd()) as scratch:
        names = Path(scratch) / "names"
        names.write_text("\n".join(keys) + "\n")
        environment = {"PATH": os.environ["PATH"], "LANG": "C.UTF-8", "LC_ALL": "C.UTF-8", "GHC_ENVIRONMENT": "-", "TMPDIR": scratch, "SUITE_ORACLE_NAMES": str(names)}
        executable = Path(scratch) / "suite-oracle"
        generated_source = Path(scratch) / "OracleMain.hs"
        original = oracle_source.read_text()
        if original.count("import qualified Suite\n") != 1:
            raise RuntimeError("native oracle template lost its exact module import")
        generated_source.write_text(original.replace("import qualified Suite\n", f"import qualified {args.module} as Suite\n"))
        command = [str(ghc), "--make", *FLAGS, "-i", *("-i" + root for root in includes), "-outputdir", str(Path(scratch) / "ghc"), "-o", str(executable), str(generated_source)]
        subprocess.run(command, cwd=scratch, env=environment, check=True)
        result = subprocess.run([str(executable), "domain"], cwd=scratch, env=environment, check=True, stdout=subprocess.PIPE)
        rows = [json.loads(line) for line in result.stdout.splitlines()]
        domain = {row["occurrence"]: row for row in rows}
        if len(domain) != len(rows) or sorted(domain) != keys:
            raise RuntimeError("native oracle domain differs from the complete projection domain")
        for name, declaration in classes.items():
            if name not in domain or domain[name].get("class") == "value" or not declaration.get("reason") or declaration.get("expectation", {}).get("kind") != "cyclic_observation":
                raise RuntimeError(f"invalid native classification for {name}")
        expectations, refusals = {}, {}
        for name, declaration in domain.items():
            if declaration["scope"] != "source_top":
                continue
            if declaration["class"] != "value":
                refusals[name] = {"class": declaration["class"], "reason": declaration["reason"]}
                continue
            value, refusal, reason = evaluate(executable, name, environment, scratch, args.timeout)
            if refusal == "timeout":
                if name not in nonterminating:
                    raise RuntimeError(f"undeclared native nontermination for {name}")
                expectations[name] = {"kind": "no_finite_observation"}
            elif refusal:
                refusals[name] = {"class": refusal, "reason": reason}
            else:
                expectations[name] = value
        for name in nonterminating:
            if expectations.get(name, {}).get("kind") != "no_finite_observation":
                raise RuntimeError(f"declared nontermination {name} terminated or is not a source value")
        expectations.update({name: declaration["expectation"] for name, declaration in classes.items()})
        payload = {"source_revision": "native GHC SuiteOracleTH/Render from declared compiler and source inputs", "source_tops": sorted(name for name, row in domain.items() if row["scope"] == "source_top"), "refusals": refusals, "expectations": expectations}
        if args.contract:
            if refusals or sorted(expectations) != keys:
                raise RuntimeError("a contract oracle must evaluate every requested target")
            payload.pop("source_tops")
        output.write_text(json.dumps(payload, sort_keys=True, indent=2) + "\n")


if __name__ == "__main__":
    main()
