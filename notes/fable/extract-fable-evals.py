#!/usr/bin/env python3
"""Extract all tidepool eval/resume/abort calls from fable-model sessions."""

import json
import os
import sys
from pathlib import Path
from collections import defaultdict

CLAUDE_DIR = Path.home() / ".claude" / "projects"
TIDEPOOL_TOOLS = {"mcp__tidepool__eval", "mcp__tidepool__resume", "mcp__tidepool__abort"}
OUTPUT = Path(__file__).parent / "fable-tidepool-evals.jsonl"


def find_fable_sessions():
    """Find all unique JSONL files (by inode) that mention claude-fable."""
    seen_inodes = set()
    files = []
    for jsonl in CLAUDE_DIR.rglob("*.jsonl"):
        try:
            inode = jsonl.stat().st_ino
            if inode in seen_inodes:
                continue
            seen_inodes.add(inode)
            # Quick grep-like check before parsing
            with open(jsonl, "rb") as f:
                raw = f.read()
            if b"claude-fable" not in raw and b"fable" not in raw:
                continue
            files.append(jsonl)
        except (OSError, PermissionError):
            continue
    return sorted(files)


def has_fable_model(lines):
    """Check if any assistant message in this session used a fable model."""
    for d in lines:
        if d.get("type") == "assistant":
            model = d.get("message", {}).get("model", "")
            if "fable" in model:
                return True
    return False


def extract_tidepool_calls(jsonl_path):
    """Extract eval/resume/abort tool_use + tool_result pairs from a session file."""
    lines = []
    with open(jsonl_path) as f:
        for raw_line in f:
            raw_line = raw_line.strip()
            if not raw_line:
                continue
            try:
                lines.append(json.loads(raw_line))
            except json.JSONDecodeError:
                continue

    if not has_fable_model(lines):
        return []

    # Collect tool_use calls (assistant messages) and tool_results (user messages)
    tool_uses = {}  # tool_use_id -> {call info}
    tool_results = {}  # tool_use_id -> {result info}
    # Track which model was active for each assistant message
    call_order = []

    for d in lines:
        msg = d.get("message", {})
        content = msg.get("content", [])
        if not isinstance(content, list):
            continue

        if d.get("type") == "assistant":
            model = msg.get("model", "")
            ts = d.get("timestamp", "")
            session_id = d.get("sessionId", "")
            for c in content:
                if (isinstance(c, dict)
                        and c.get("type") == "tool_use"
                        and c.get("name") in TIDEPOOL_TOOLS):
                    tool_id = c["id"]
                    tool_uses[tool_id] = {
                        "tool_use_id": tool_id,
                        "tool": c["name"],
                        "input": c.get("input", {}),
                        "model": model,
                        "timestamp": ts,
                        "session_id": session_id,
                        "source_file": str(jsonl_path),
                    }
                    call_order.append(tool_id)

        elif d.get("type") == "user":
            for c in content:
                if isinstance(c, dict) and c.get("type") == "tool_result":
                    tid = c.get("tool_use_id", "")
                    if tid in tool_uses:
                        result_content = c.get("content", "")
                        # Normalize: if it's a list of text blocks, join them
                        if isinstance(result_content, list):
                            texts = []
                            for rc in result_content:
                                if isinstance(rc, dict) and rc.get("type") == "text":
                                    texts.append(rc["text"])
                                elif isinstance(rc, str):
                                    texts.append(rc)
                            result_content = "\n".join(texts)
                        tool_results[tid] = {
                            "content": result_content,
                            "is_error": c.get("is_error", False),
                        }

    # Build paired records in call order, only fable-model calls
    records = []
    for tid in call_order:
        if "fable" not in tool_uses[tid].get("model", ""):
            continue
        rec = dict(tool_uses[tid])
        if tid in tool_results:
            rec["result"] = tool_results[tid]["content"]
            rec["is_error"] = tool_results[tid]["is_error"]
        else:
            rec["result"] = None
            rec["is_error"] = None
        records.append(rec)

    return records


def main():
    print("Scanning for fable sessions...", file=sys.stderr)
    sessions = find_fable_sessions()
    print(f"Found {len(sessions)} unique files mentioning fable", file=sys.stderr)

    all_records = []
    for f in sessions:
        records = extract_tidepool_calls(f)
        if records:
            print(f"  {f.name}: {len(records)} tidepool calls", file=sys.stderr)
            all_records.extend(records)

    # Deduplicate by tool_use_id (hardlinks could still cause dupes if we missed)
    seen = set()
    deduped = []
    for r in all_records:
        if r["tool_use_id"] not in seen:
            seen.add(r["tool_use_id"])
            deduped.append(r)

    deduped.sort(key=lambda r: r.get("timestamp", ""))

    with open(OUTPUT, "w") as out:
        for r in deduped:
            out.write(json.dumps(r) + "\n")

    print(f"\nWrote {len(deduped)} records to {OUTPUT}", file=sys.stderr)

    # Summary stats
    by_tool = defaultdict(int)
    by_model = defaultdict(int)
    by_session = defaultdict(int)
    for r in deduped:
        by_tool[r["tool"]] += 1
        by_model[r["model"]] += 1
        by_session[r["session_id"]] += 1

    print("\nBy tool:", file=sys.stderr)
    for k, v in sorted(by_tool.items()):
        print(f"  {k}: {v}", file=sys.stderr)
    print("By model:", file=sys.stderr)
    for k, v in sorted(by_model.items()):
        print(f"  {k}: {v}", file=sys.stderr)
    print(f"Across {len(by_session)} sessions", file=sys.stderr)
    if deduped:
        print(f"Time range: {deduped[0].get('timestamp','')} .. {deduped[-1].get('timestamp','')}", file=sys.stderr)


if __name__ == "__main__":
    main()
