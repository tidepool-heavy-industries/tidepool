"""Owned-service orchestration fixture; no Nix registration is performed."""
import hashlib
import json
import os
from pathlib import Path
import struct
import sys


def option(name):
    return sys.argv[sys.argv.index(name) + 1]


def sha(data):
    return hashlib.sha256(data).digest()


def durable(path, data):
    with path.open("wb") as stream:
        stream.write(data)
        stream.flush()
        os.fsync(stream.fileno())


if sys.argv[1] == "release-compiler-service-source":
    receipt = json.loads(Path(option("--receipt")).read_bytes())
    if receipt["fixture_mode"] == "release-refusal":
        print("injected independent provider release refusal", file=sys.stderr)
        sys.exit(9)
    # Native finalization owns the final Python guard after this return.
    sys.exit(0)

assert sys.argv[1] == "acquire-compiler-service-source"
assert sys.stdin.read() == "", "fd0 must be detached from the owner carrier"
owner = Path(option("--owner-directory"))
attempt_path = Path(option("--attempt"))
attempt_bytes = attempt_path.read_bytes()
assert sha(attempt_bytes).hex() == option("--attempt-sha256")
attempt = json.loads(attempt_bytes)
record = json.loads(Path(option("--record")).read_bytes())
mode = record["fixture_mode"]
root_directory = owner / "package-source" / "gc-roots"
root_directory.mkdir(parents=True)
for root in attempt["roots"]:
    os.symlink(root["store_path"], root["path"])
durable(owner / "fixture-registrations.json", json.dumps(attempt["roots"]).encode())
reference = {"path": str(attempt_path), "sha256": list(sha(attempt_bytes))}
if mode == "preboot-refusal":
    for root in attempt["roots"]:
        Path(root["path"]).unlink()
    outcome = {"status": "refused", "attempt": reference,
               "primary": "injected refusal after fixture root registrations",
               "settlement": "preboot_released", "cleanup": [], "publication": []}
else:
    receipt = {"schema": 2, "kind": "compiler-service-package-acquisition",
               "status": "acquired", "service_epoch": attempt["service_epoch"],
               "source_id": attempt["source_id"], "profile_id": attempt["profile_id"],
               "attempt_sha256": sha(attempt_bytes).hex(), "roots": attempt["roots"],
               "fixture_mode": mode}
    receipt_path = owner / "package-source" / "receipt.json"
    receipt_bytes = json.dumps(receipt).encode()
    durable(receipt_path, receipt_bytes)
    outcome = {"status": "acquired", "attempt": reference,
               "receipt": {"path": str(receipt_path), "sha256": list(sha(receipt_bytes))},
               "profile": {"path": record["profile_store_path"],
                           "sha256": list(bytes.fromhex(record["profile_sha256"]))},
               "publication": []}
payload = json.dumps(outcome).encode()
sys.stdout.buffer.write(b"TPSAO001" + struct.pack("<I", len(payload)) + payload)
sys.stdout.buffer.flush()
