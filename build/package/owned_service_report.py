"""Schema-2 observations from the native compiler service owner.

Reading reports never issues process-retirement or package-release authority.
"""
import json
import os
from pathlib import Path
import re
import stat

REPORT_LIMIT = 16 * 1024 * 1024


def validate_owned_service_report(report: dict, owner: Path) -> dict:
    """Validate schema-2 observations. This reader issues no release authority."""
    fields = {"schema", "kind", "owner_directory", "operation", "daemon_epoch", "daemon_pid",
              "producer", "endpoint", "socket_path", "action", "acquisition", "daemon_reap",
              "service_terminal", "source_settlement", "artifact_retention", "settlement_failures",
              "publication_failures", "complete"}
    if (not isinstance(report, dict) or set(report) != fields or report["schema"] != 2
            or report["kind"] != "owned-compiler-service" or report["owner_directory"] != str(owner)):
        raise ValueError("owned service report requires schema 2 and its exact owner")
    def strings(value):
        if not isinstance(value, list) or any(not isinstance(item, str) for item in value):
            raise ValueError("owned service report requires string failures")
    def digest_array(value):
        if not isinstance(value, list) or len(value) != 32 or any(type(item) is not int or not 0 <= item <= 255 for item in value):
            raise ValueError("owned service report requires SHA-256 byte arrays")
    def reference(value):
        if not isinstance(value, dict) or set(value) != {"path", "sha256"}:
            raise ValueError("owned service report requires exact references")
        path = value["path"]
        if not isinstance(path, str) or not Path(path).is_absolute() or str(Path(path)) != path or ".." in Path(path).parts:
            raise ValueError("owned service report reference path is not normalized")
        digest_array(value["sha256"])
    if not isinstance(report["operation"], str) or re.fullmatch(r"[0-9a-f]{64}", report["operation"]) is None:
        raise ValueError("owned service report has invalid operation identity")
    for key, length in (("daemon_epoch", 64), ("producer", 64), ("endpoint", 192)):
        value = report[key]
        if value is not None and (not isinstance(value, str) or re.fullmatch("[0-9a-f]{" + str(length) + "}", value) is None):
            raise ValueError("owned service report has invalid identity: " + key)
    if report["daemon_pid"] is not None and (type(report["daemon_pid"]) is not int or report["daemon_pid"] <= 0):
        raise ValueError("owned service report has invalid daemon PID")
    if report["socket_path"] is not None and (not isinstance(report["socket_path"], str) or not Path(report["socket_path"]).is_absolute()):
        raise ValueError("owned service report has invalid socket path")
    states = {"action": {"not_started", "running", "exited", "failed"},
              "daemon_reap": {"not_started", "reaped", "unconfirmed"},
              "source_settlement": {"pending", "settled", "not_configured", "retained"},
              "artifact_retention": {"pending", "removed", "failed", "retained"}}
    for key in states:
        value = report[key]
        if not isinstance(value, dict) or set(value) != {"status", "exit_code", "success", "signal", "failure"} or not isinstance(value["status"], str):
            raise ValueError("owned service report has invalid observation: " + key)
        if value["status"] not in states[key]:
            raise ValueError("owned service report has invalid observation state: " + key)
        if value["status"] not in ("exited", "reaped") and any(value[field] is not None for field in ("exit_code", "success", "signal")):
            raise ValueError("owned service report has process facts outside process exit")
        if value["exit_code"] is not None and (type(value["exit_code"]) is not int or not 0 <= value["exit_code"] <= 255):
            raise ValueError("owned service report has invalid exit code")
        if value["success"] is not None and type(value["success"]) is not bool:
            raise ValueError("owned service report has invalid process success")
        if value["signal"] is not None and (type(value["signal"]) is not int or value["signal"] <= 0):
            raise ValueError("owned service report has invalid signal")
        if value["failure"] is not None and not isinstance(value["failure"], str):
            raise ValueError("owned service report has invalid failure")
        if value["success"] is True and (value["exit_code"] != 0 or value["signal"] is not None):
            raise ValueError("owned service report has inconsistent process success")
    acquisition = report["acquisition"]
    if not isinstance(acquisition, dict):
        raise ValueError("owned service report lacks acquisition")
    status = acquisition.get("status")
    shapes = {"not_configured": {"status"}, "acquired": {"status", "attempt", "receipt", "profile", "publication"},
              "refused": {"status", "attempt", "primary", "settlement", "cleanup", "publication"},
              "unknown": {"status", "attempt", "failure"}}
    if not isinstance(status, str) or status not in shapes or set(acquisition) != shapes[status]:
        raise ValueError("owned service report has invalid acquisition outcome")
    if status != "not_configured":
        if acquisition["attempt"] is not None:
            reference(acquisition["attempt"])
            if acquisition["attempt"]["path"] != str(owner / "compiler-service-source.attempt.json"):
                raise ValueError("owned service report has a foreign attempt")
        elif status != "unknown":
            raise ValueError("owned service report lacks its attempt")
    if status == "acquired":
        reference(acquisition["receipt"])
        reference(acquisition["profile"])
        strings(acquisition["publication"])
        if acquisition["receipt"]["path"] != str(owner / "package-source/receipt.json"):
            raise ValueError("owned service report has a foreign receipt")
    elif status == "refused":
        if not isinstance(acquisition["primary"], str) or acquisition["settlement"] not in ("no_roots_issued", "preboot_released", "retained_unknown"):
            raise ValueError("owned service report has invalid acquisition refusal")
        strings(acquisition["cleanup"])
        strings(acquisition["publication"])
    elif status == "unknown" and not isinstance(acquisition["failure"], str):
        raise ValueError("owned service report has invalid acquisition uncertainty")
    terminal = report["service_terminal"]
    if terminal is not None:
        if not isinstance(terminal, dict) or set(terminal) != {"epoch", "workers_retired", "socket_retired", "failures"}:
            raise ValueError("owned service report has invalid terminal")
        digest_array(terminal["epoch"])
        if report["daemon_epoch"] != bytes(terminal["epoch"]).hex() or any(type(terminal[key]) is not bool for key in ("workers_retired", "socket_retired")):
            raise ValueError("owned service report has crossed terminal ownership")
        strings(terminal["failures"])
    strings(report["settlement_failures"])
    strings(report["publication_failures"])
    if type(report["complete"]) is not bool:
        raise ValueError("owned service report has invalid completeness")
    if report["complete"] and (terminal is None or not terminal["workers_retired"] or not terminal["socket_retired"]
            or report["daemon_reap"]["status"] != "reaped" or report["daemon_reap"]["exit_code"] is None
            or report["daemon_pid"] is None or report["settlement_failures"] or report["publication_failures"]
            or report["source_settlement"]["status"] not in ("settled", "not_configured")
            or status == "unknown"
            or acquisition.get("publication")
            or ((status == "not_configured") != (report["source_settlement"]["status"] == "not_configured"))):
        raise ValueError("owned service report completeness lacks matching settlement observations")
    return report


def read_owned_service_report(owner: Path) -> dict:
    if not owner.is_absolute() or owner.resolve(strict=True) != owner:
        raise ValueError("owned service report owner must be canonical")
    info = owner.stat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
        raise ValueError("owned service report owner must be private")
    path = owner / "owned-service.json"
    # Open the exact regular record without following a swapped final symlink.
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
    with os.fdopen(fd, "rb") as source:
        info = os.fstat(source.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
            raise ValueError("owned service report must be a private regular owner file")
        if info.st_size > REPORT_LIMIT:
            raise ValueError("owned service report exceeds 16 MiB")
        body = source.read(REPORT_LIMIT + 1)
        if len(body) > REPORT_LIMIT:
            raise ValueError("owned service report grew beyond 16 MiB")
    return validate_owned_service_report(json.loads(body), owner)
