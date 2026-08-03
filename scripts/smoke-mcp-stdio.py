#!/usr/bin/env python3
"""Black-box MCP stdio lifecycle test for the built container image.

The test intentionally supplies no credentials and calls only the embedded,
read-only repo_map tool. It validates protocol framing and dispatch without
contacting node, brain, Cloudflare, DNS, Kubernetes, or any production system.
"""

from __future__ import annotations

import json
import subprocess
import sys
from typing import Any


ALLOWED_EXIT_CODES = {0, 1, 124}
REQUIRED_TOOLS = {
    "repo_map",
    "node_status",
    "cloudflare_dns_upsert",
    "cloudflare_dns_delete",
}


def fail(message: str, *, stdout: str = "", stderr: str = "") -> "NoReturn":
    print(f"MCP stdio smoke failed: {message}", file=sys.stderr)
    if stdout:
        print("--- stdout ---", file=sys.stderr)
        print(stdout, file=sys.stderr)
    if stderr:
        print("--- stderr ---", file=sys.stderr)
        print(stderr, file=sys.stderr)
    raise SystemExit(1)


def response_by_id(messages: list[dict[str, Any]], request_id: int) -> dict[str, Any]:
    matches = [message for message in messages if message.get("id") == request_id]
    if len(matches) != 1:
        fail(f"expected exactly one response for id={request_id}, found {len(matches)}")
    return matches[0]


def run(image: str) -> None:
    frames = [
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "fiducia-mcp-ci", "version": "1.0"},
            },
        },
        {
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {},
        },
        {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}},
        {
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {"name": "repo_map", "arguments": {}},
        },
        {"jsonrpc": "2.0", "id": 4, "method": "ping", "params": {}},
        {
            "jsonrpc": "2.0",
            "id": 5,
            "method": "fiducia/definitely-unknown",
            "params": {},
        },
    ]
    payload = "".join(
        f"{json.dumps(frame, separators=(',', ':'))}\n" for frame in frames
    )

    command = ["docker", "run", "--rm", "-i", image, "--log-filter=debug"]
    timed_out = False
    try:
        completed = subprocess.run(
            command,
            input=payload,
            text=True,
            capture_output=True,
            timeout=20,
            check=False,
        )
        stdout = completed.stdout
        stderr = completed.stderr
        returncode = completed.returncode
    except subprocess.TimeoutExpired as error:
        timed_out = True
        stdout = _as_text(error.stdout)
        stderr = _as_text(error.stderr)
        returncode = 124

    if returncode not in ALLOWED_EXIT_CODES:
        fail(
            f"container exited with unsupported status {returncode}",
            stdout=stdout,
            stderr=stderr,
        )

    messages: list[dict[str, Any]] = []
    for line_number, line in enumerate(stdout.splitlines(), start=1):
        if not line.strip():
            continue
        try:
            message = json.loads(line)
        except json.JSONDecodeError as error:
            fail(
                f"stdout line {line_number} is not JSON-RPC: {error}",
                stdout=stdout,
                stderr=stderr,
            )
        if not isinstance(message, dict) or message.get("jsonrpc") != "2.0":
            fail(
                f"stdout line {line_number} is not a JSON-RPC 2.0 object",
                stdout=stdout,
                stderr=stderr,
            )
        messages.append(message)

    if not messages:
        fail("server emitted no protocol messages", stdout=stdout, stderr=stderr)

    initialize = response_by_id(messages, 1)
    if "result" not in initialize or "error" in initialize:
        fail("initialize did not succeed", stdout=stdout, stderr=stderr)
    server_info = initialize["result"].get("serverInfo", {})
    if not server_info.get("name") or not server_info.get("version"):
        fail("initialize omitted serverInfo name/version", stdout=stdout, stderr=stderr)

    tools_response = response_by_id(messages, 2)
    tools = tools_response.get("result", {}).get("tools")
    if not isinstance(tools, list):
        fail("tools/list did not return a tools array", stdout=stdout, stderr=stderr)
    tool_names = {tool.get("name") for tool in tools if isinstance(tool, dict)}
    missing_tools = sorted(REQUIRED_TOOLS - tool_names)
    if missing_tools:
        fail(
            f"tools/list is missing required tools: {', '.join(missing_tools)}",
            stdout=stdout,
            stderr=stderr,
        )

    repo_map = response_by_id(messages, 3)
    repo_map_result = repo_map.get("result", {})
    if repo_map_result.get("isError") is True:
        fail("repo_map unexpectedly returned isError=true", stdout=stdout, stderr=stderr)
    content = repo_map_result.get("content")
    if not isinstance(content, list) or not content:
        fail("repo_map returned no MCP content", stdout=stdout, stderr=stderr)
    text = "\n".join(
        item.get("text", "")
        for item in content
        if isinstance(item, dict) and item.get("type") == "text"
    )
    if "fiducia" not in text.lower():
        fail("repo_map content does not identify Fiducia", stdout=stdout, stderr=stderr)

    ping = response_by_id(messages, 4)
    if "result" not in ping or "error" in ping:
        fail("MCP ping did not succeed", stdout=stdout, stderr=stderr)

    unknown = response_by_id(messages, 5)
    error = unknown.get("error")
    if not isinstance(error, dict) or error.get("code") != -32601:
        fail(
            "unknown method did not fail with JSON-RPC method-not-found (-32601)",
            stdout=stdout,
            stderr=stderr,
        )

    lowered_stderr = stderr.lower()
    forbidden_stderr = [
        "panicked at",
        "thread 'main' panicked",
        "backtrace:",
        "fiducia_api_key=",
        "cloudflare_api_token=",
    ]
    leaked = [needle for needle in forbidden_stderr if needle in lowered_stderr]
    if leaked:
        fail(
            f"stderr contains forbidden crash/secret markers: {', '.join(leaked)}",
            stdout=stdout,
            stderr=stderr,
        )

    print(
        "MCP stdio lifecycle passed "
        f"({len(messages)} messages, {len(tools)} tools, timed_out={timed_out})"
    )


def _as_text(value: str | bytes | None) -> str:
    if value is None:
        return ""
    if isinstance(value, bytes):
        return value.decode("utf-8", errors="replace")
    return value


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit(f"usage: {sys.argv[0]} <container-image>")
    run(sys.argv[1])
