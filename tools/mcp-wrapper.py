#!/usr/bin/env python3
"""Thin MCP stdio proxy that adds a __mcp_restart tool.

Spawns a child MCP server, proxies all JSON-RPC messages, and injects
a synthetic restart tool. Calling __mcp_restart kills the child, spawns
a fresh one, re-initializes, and notifies the client that tools changed.

Usage: python3 tools/mcp-wrapper.py <command> [args...]
"""

import json
import os
import signal
import subprocess
import sys
import threading
import time

def main():
    if len(sys.argv) < 2:
        print("Usage: mcp-wrapper.py <command> [args...]", file=sys.stderr)
        sys.exit(1)

    cmd = sys.argv[1:]
    state = State(cmd)
    state.spawn_child()

    # Read child stdout in a background thread
    child_thread = threading.Thread(target=state.read_child_stdout, daemon=True)
    child_thread.start()

    # Read client stdin on main thread
    state.read_client_stdin()


class State:
    def __init__(self, cmd):
        self.cmd = cmd
        self.child = None
        self.init_request = None  # cached initialize request
        self.restarting = False
        self.reinit_id = None  # synthetic ID for re-init, swallow its response
        self.write_lock = threading.Lock()  # protects stdout writes

    def spawn_child(self):
        self.child = subprocess.Popen(
            self.cmd,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=None,  # inherit stderr
            bufsize=0,
        )

    def write_to_client(self, line):
        with self.write_lock:
            sys.stdout.write(line + "\n")
            sys.stdout.flush()

    def write_to_child(self, line):
        try:
            self.child.stdin.write((line + "\n").encode())
            self.child.stdin.flush()
        except (BrokenPipeError, OSError):
            pass

    def read_client_stdin(self):
        """Main thread: read lines from client, dispatch."""
        for line in sys.stdin:
            line = line.rstrip("\n")
            if not line:
                continue
            self.handle_client_message(line)
        # stdin closed — kill child and exit
        if self.child and self.child.poll() is None:
            self.child.kill()
        sys.exit(0)

    def read_child_stdout(self):
        """Background thread: read lines from child stdout, dispatch."""
        while True:
            if self.child is None or self.child.stdout is None:
                time.sleep(0.01)
                continue
            stdout = self.child.stdout
            for raw in stdout:
                line = raw.decode().rstrip("\n")
                if not line:
                    continue
                self.handle_child_message(line)
            # Child stdout closed
            if not self.restarting:
                break
            # If restarting, loop back and read from new child
            time.sleep(0.01)

    def handle_client_message(self, line):
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            self.write_to_child(line)
            return

        # Cache initialize request for re-init on restart
        if msg.get("method") == "initialize":
            self.init_request = msg

        # Intercept restart tool call
        if (msg.get("method") == "tools/call"
                and msg.get("params", {}).get("name") == "__mcp_restart"):
            # Respond immediately
            response = {
                "jsonrpc": "2.0",
                "id": msg["id"],
                "result": {
                    "content": [{"type": "text", "text": "MCP server restarted."}],
                },
            }
            self.write_to_client(json.dumps(response))
            self.do_restart()
            return

        # Forward to child
        self.write_to_child(line)

    def handle_child_message(self, line):
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            self.write_to_client(line)
            return

        # Swallow the re-init response
        if self.reinit_id is not None and msg.get("id") == self.reinit_id:
            self.reinit_id = None
            return

        # Inject restart tool into tools/list responses
        result = msg.get("result")
        if isinstance(result, dict) and isinstance(result.get("tools"), list):
            result["tools"].append({
                "name": "__mcp_restart",
                "description": (
                    "Restart the MCP server subprocess. "
                    "Use after `cargo install` to pick up a newly built binary."
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {},
                },
            })
            self.write_to_client(json.dumps(msg))
            return

        # Forward to client
        self.write_to_client(line)

    def do_restart(self):
        self.restarting = True

        # Kill old child
        if self.child and self.child.poll() is None:
            self.child.terminate()
            try:
                self.child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.child.kill()
                self.child.wait()

        # Spawn new child
        self.spawn_child()

        # Re-initialize the new child
        if self.init_request:
            self.reinit_id = f"__reinit_{time.monotonic_ns()}"
            reinit_msg = {**self.init_request, "id": self.reinit_id}
            self.write_to_child(json.dumps(reinit_msg))
            # Wait for reinit response to be swallowed (read_child_stdout handles it)
            deadline = time.monotonic() + 10
            while self.reinit_id is not None and time.monotonic() < deadline:
                time.sleep(0.01)
            # Send initialized notification
            self.write_to_child(json.dumps({
                "jsonrpc": "2.0",
                "method": "notifications/initialized",
            }))

        # Notify client that tools may have changed
        self.write_to_client(json.dumps({
            "jsonrpc": "2.0",
            "method": "notifications/tools/list_changed",
        }))

        # Now safe for reader thread to proceed with new child
        self.restarting = False


if __name__ == "__main__":
    main()
